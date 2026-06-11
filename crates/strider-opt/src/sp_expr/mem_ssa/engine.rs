//! The private DFS engine behind the [`MemorySSAWalker`](super::MemorySSAWalker)
//! trait: an iterative, memoized backward memory-token walk that resolves the
//! nearest clobbering definition.  The trait's default methods construct a
//! [`MemSsaWalk`] and call [`MemSsaWalk::nearest_clobber`]; everything else here
//! is engine-private (the work-stack frames, the per-output memo, the
//! phi-join).  See the module-level docs on `super` for the walk semantics,
//! memoization + cycle guard, and stack-safety rationale.

use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use strider_ir::Function;
use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind, ValueId};

use super::MemorySSAWalker;

/// Joins the per-predecessor results of one `MemPhi` into a single
/// result for the phi.  Agreement (all results equal) passes the shared
/// value through transparently; disagreement makes the phi itself the
/// boundary clobber (`phi_value`).
fn join_phi_results(phi_value: ValueId, preds: &[Option<ValueId>]) -> Option<ValueId> {
    let Some((&first, rest)) = preds.split_first() else {
        // A phi with no value predecessors carries no definition.
        return None;
    };
    if rest.iter().all(|&p| p == first) {
        // Every arm agrees on the same live definition (all `None`, or
        // all `Some(x)`): the merge is transparent.
        first
    } else {
        // Arms disagree → no single live definition across the merge.
        Some(phi_value)
    }
}

/// Memoization state for a memory output during one walk.  Keyed densely
/// by `ValueId` in the walk's memo map, so an absent key reads back as the
/// default `Unseen` — the state of every output not yet entered.
#[derive(Clone, Copy, Default)]
enum Resolve {
    /// Not yet entered on this walk.
    #[default]
    Unseen,
    /// Currently on the resolution path — re-encountering it is a cycle.
    InProgress,
    /// Fully resolved to this (nearest-clobber) result.
    Done(Option<ValueId>),
}

/// Enter/exit work-stack frame for the iterative memoized DFS.
enum Frame {
    /// First visit to `mem`: classify it (oracle short-circuit at an
    /// aliasing def), else push an `Exit` continuation and its successors.
    Enter(ValueId),
    /// All successors of `mem` are resolved; combine and memoize.
    Exit(ValueId),
}

/// Backward memory-SSA walk bound to a `Function` + an aliasing oracle.
/// The traversal reads `self.function` and consults `self.walker` instead
/// of threading them through every helper.  Read-only: the narrowing
/// rewrite is a separate mutating step (see [`may_clobber`](super::MemorySSAWalker::may_clobber)).
pub(super) struct MemSsaWalk<'f, 'w, W: MemorySSAWalker> {
    function: &'f Function,
    walker: &'w mut W,
}

impl<'f, 'w, W: MemorySSAWalker> MemSsaWalk<'f, 'w, W> {
    pub(super) fn new(function: &'f Function, walker: &'w mut W) -> Self {
        Self { function, walker }
    }

    /// Nearest clobbering memory-definition node reachable backward from
    /// `mem`'s memory output — a `Store` / `Call` / `CallOther` (or a
    /// disagreeing `MemPhi` boundary) — or the `InitialMemory` root when
    /// every path is clean (callers distinguish by the returned node kind).
    pub(super) fn nearest_clobber(&mut self, mem: NodeId) -> NodeId {
        let start_mem = self
            .function
            .memory_output_of(mem)
            .expect("memory-chain start node has a memory output");
        let mut initial_memory: Option<NodeId> = None;
        match self.walk_from(start_mem, &mut initial_memory) {
            Some(clobber_value) => self.function.producer(clobber_value),
            None => initial_memory.expect("a clean memory chain bottoms out at InitialMemory"),
        }
    }

    /// Iterative, memoized backward walk from a single memory cursor.  Uses
    /// an explicit enter/exit work stack so the host call stack stays O(1)
    /// regardless of chain depth or phi fan-out; per-output memoization
    /// makes shared DAG fan-in correct (and turns the walk linear in the
    /// number of reachable memory outputs).
    fn walk_from(
        &mut self,
        start_mem: ValueId,
        initial_memory: &mut Option<NodeId>,
    ) -> Option<ValueId> {
        // Sparse per-output memo: one walk visits only O(chain-length) memory
        // outputs out of O(function) total ValueIds, and callers loop this once
        // per stack slot / table entry over the same chain — so a hash map
        // (O(visited) space/init) is the right structure here, not an
        // entity-keyed dense `SecondaryMap` (O(function) per walk).  An absent
        // key reads back as `Resolve::Unseen` (the enum's `Default`).
        let mut memo: FxHashMap<ValueId, Resolve> = FxHashMap::default();
        let mut work: Vec<Frame> = vec![Frame::Enter(start_mem)];

        while let Some(frame) = work.pop() {
            match frame {
                Frame::Enter(cur) => {
                    // Skip a node already seen on this walk:
                    //  * `Done` — fully resolved on another path; reuse its
                    //    memoised result (DAG fan-in).
                    //  * `InProgress` — on the current path; a genuine cycle.
                    //    Leave it `InProgress` so the `combine` that consumes
                    //    it reads `None` for this edge (the cycle adds no new
                    //    clobber).
                    if !matches!(
                        memo.get(&cur).copied().unwrap_or(Resolve::Unseen),
                        Resolve::Unseen
                    ) {
                        continue;
                    }
                    let node = self.function.producer(cur);
                    // Aliasing-def short-circuit: a `Store` / `Call` /
                    // `CallOther` (or opaque producer) the oracle calls a
                    // clobber resolves to itself with no successor walk.
                    let node_kind = self.function.node_kind(node);
                    let is_phi = matches!(node_kind, NodeKind::MemPhi);
                    let is_initial = matches!(node_kind, NodeKind::InitialMemory);
                    if is_initial {
                        // Record the clean chain root so `may_clobber` can name
                        // it when no def aliases on any path.
                        *initial_memory = Some(node);
                    }
                    if !is_phi && !is_initial && self.walker.def_clobbers(self.function, node) {
                        memo.insert(cur, Resolve::Done(Some(cur)));
                        continue;
                    }
                    memo.insert(cur, Resolve::InProgress);
                    work.push(Frame::Exit(cur));
                    for succ in self.successors(cur) {
                        work.push(Frame::Enter(succ));
                    }
                }
                Frame::Exit(cur) => {
                    // Gather successor results from the memo.  A successor
                    // still `InProgress` (back-edge to an ancestor on the
                    // current path) contributes `None` — a cycle adds no new
                    // clobber on that edge.
                    let succ_results: SmallVec<[Option<ValueId>; 4]> = self
                        .successors(cur)
                        .into_iter()
                        .map(|s| match memo.get(&s).copied().unwrap_or(Resolve::Unseen) {
                            Resolve::Done(r) => r,
                            _ => None,
                        })
                        .collect();
                    let result = self.combine(cur, &succ_results);
                    memo.insert(cur, Resolve::Done(result));
                }
            }
        }

        match memo.get(&start_mem).copied().unwrap_or(Resolve::Unseen) {
            Resolve::Done(r) => r,
            _ => None,
        }
    }

    /// The successors whose results a node's own result depends on.  Empty
    /// for a terminal (clean) node; one element for a linear step; the
    /// predecessors for a `MemPhi`.
    fn successors(&self, cur: ValueId) -> SmallVec<[ValueId; 4]> {
        let node = self.function.producer(cur);
        match *self.function.node_kind(node) {
            NodeKind::MemPhi => {
                // Inputs: [phi_token, mem_pred_0, mem_pred_1, ...].
                self.function.phi_data_inputs(node).collect()
            }
            NodeKind::InitialMemory => SmallVec::new(),
            _ => self.prev_mem(node).into_iter().collect(),
        }
    }

    /// Combines a node's already-resolved successor results into the node's
    /// own result.  A `MemPhi` joins (agree → pass through, disagree →
    /// boundary); a linear node forwards its single predecessor's result; a
    /// terminal node is clean.  The oracle's per-`Store`/`Call` alias verdict
    /// is applied at enter-time (see [`Self::walk_from`]) and short-circuits
    /// before this combine ever runs, so here a non-phi node simply forwards.
    fn combine(&self, cur: ValueId, succ_results: &[Option<ValueId>]) -> Option<ValueId> {
        let node = self.function.producer(cur);
        match *self.function.node_kind(node) {
            NodeKind::MemPhi => join_phi_results(cur, succ_results),
            // Linear step: forward the single predecessor's result (or clean
            // when terminal).
            _ => succ_results.first().copied().flatten(),
        }
    }

    /// The memory-token input of a memory-chain node, if any.  Slot 0 for
    /// `Store` / `Load`; the call's memory input (slot 1) for `Call` /
    /// `CallOther`.  `None` for `MemPhi` (its variadic memory predecessors are
    /// reached via [`Self::successors`], not here), `InitialMemory`, and
    /// anything that does not carry an incoming memory edge.
    fn prev_mem(&self, node: NodeId) -> Option<ValueId> {
        self.function.memory_input_of(node)
    }
}
