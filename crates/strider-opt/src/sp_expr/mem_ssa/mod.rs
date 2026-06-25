//! Unified backward memory-SSA walk shared by the SP-aware analyses.
//!
//! A memory-SSA walk starts at a `Load`'s memory-token input and walks
//! the memory-token chain backward looking for the nearest memory
//! definition (a `Store` / `Call` / `CallOther` memory output, or a
//! `MemPhi`) that **may alias** the location the load reads.  The
//! aliasing question is delegated to a pluggable [`MemorySSAWalker`]
//! oracle so the consumers — `function_args` (does any def shadow a
//! stack-arg slot?) and `load_forward` (is the live def an exact-match
//! store I can forward?) — share the traversal plumbing while supplying
//! their own alias predicate.
//!
//! [`MemorySSAWalker::find_nearest_clobber`] returns the nearest clobbering
//! definition NODE, or the function's `InitialMemory` node when the chain
//! reaches it with no aliasing def on any path.  Callers distinguish "clean"
//! by the returned node's kind.  The walk is **read-only**: a caller that
//! wants to shorten a load's memory edge onto the returned clobber calls the
//! separate [`narrow_load_to`] step afterward (see its docs).
//!
//! ## Semantics
//!
//! Starting at the load's memory input, iterate the memory-token chain
//! backward (input slot 0 of `Load` / `Store` / `MemPhi`, the call's
//! memory input for `Call` / `CallOther`):
//!
//! * if a def aliases the load (the oracle calls it a clobber) → return it
//!   (the nearest clobber);
//! * if it does NOT alias and is not a phi → advance the cursor to that
//!   def's own memory input and continue;
//! * at a `MemPhi` → resolve every predecessor independently, then JOIN
//!   the per-predecessor results:
//!   * if every predecessor resolves to the **same** result (all `None`,
//!     or all `Some(x)` for one shared `x`) → the merge is transparent:
//!     return that shared result.  This is the dominator case — the
//!     branches agree on the live definition (e.g. a store that
//!     dominates the merge, reached identically through every arm, or
//!     branches that all leave the slot untouched), so the search
//!     continues past the phi cleanly.
//!   * if the predecessors **disagree** (different clobbers, or one
//!     clobbers while another is clean) → there is no single live
//!     definition across the merge, so the `MemPhi` itself is the
//!     clobber boundary: return the phi's own memory output.
//!
//! This JOIN rule is correct for BOTH consumers:
//!
//! * `function_args` only asks `is_some()` ("does any path clobber this
//!   slot?").  A disagreeing phi (any arm clobbers) returns the phi →
//!   `Some` → dirty; an all-clean phi returns `None` → not dirty.
//! * `load_forward` forwards only when the result is a single exact-match
//!   `Store`.  A transparent agreeing phi yields that store; a
//!   disagreeing phi yields the `MemPhi` (not a `Store`) → it bails.  No
//!   value-`Phi` is ever synthesized.
//!
//! ## Memoization + cycle guard
//!
//! Two `MemPhi` arms can share a subchain (a store that dominates the
//! merge, reached identically through every arm — the user-visible
//! "both branches don't clobber, so keep searching through the common
//! ancestor" case).  A plain visited-set would let only the first arm
//! resolve the shared node and hand the second arm a spurious "clean",
//! producing a false disagreement.  So the walk **memoizes the resolved
//! result per memory-output** (`Resolve::Done`): a node reached on a
//! second path reuses its already-computed result instead of recomputing
//! (or short-circuiting) it.
//!
//! Loop-header `MemPhi`s feed their own region indirectly, so a node can
//! also appear on its own resolution path.  A node currently being
//! resolved is marked `Resolve::InProgress`; re-encountering it (a genuine
//! cycle) contributes `None` for that edge.  The combination is sound:
//! `InProgress` breaks cycles, `Done` shares DAG fan-in.
//!
//! ## Stack safety
//!
//! The walk is an iterative enter/exit DFS (explicit work stack), so it
//! is heap-bounded, not call-stack-bounded — a 10k-deep store prologue or
//! a deep phi fan-out costs O(1) host stack.

use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use strider_ir::node::{NodeId, NodeKind, ValueId};
use strider_ir::{Function, IRViewer};

/// Pluggable aliasing oracle for the memory-SSA walk.
pub(crate) trait MemorySSAWalker {
    /// Does the memory definition `def` clobber (overlap) the location the
    /// walk is analysing?  The oracle holds the analysed location itself
    /// (e.g. the load's precomputed address class), so the walk does not
    /// pass it in.
    ///
    /// `def` is never a `MemPhi` or `InitialMemory`: the walker handles
    /// phis structurally (joining per-predecessor results) and treats
    /// `InitialMemory` as the clean chain root, so the oracle classifies
    /// every other producer it meets on the chain — `Store` / `Call` /
    /// `CallOther` and any opaque memory producer.  A conservative oracle
    /// returns `true` for producers it cannot reason about.
    ///
    /// Returning `true` terminates the branch with `def` as the nearest
    /// clobber; returning `false` advances the cursor past `def` to its
    /// own memory input (or terminates the branch cleanly when the
    /// producer has no incoming memory edge).
    fn def_clobbers(&mut self, function: &Function, def: NodeId) -> bool;

    /// Read-only walk: the nearest clobbering memory-definition node
    /// reachable backward from the memory output of `mem` — a `Store` /
    /// `Call` / `CallOther` (or a disagreeing `MemPhi` boundary) — or the
    /// function's `InitialMemory` node when every path is clean (callers
    /// distinguish by the returned node's kind).  Takes a shared
    /// `&Function` and performs **no narrowing**; use it from a read-only
    /// context.  Default method: `self` is the aliasing oracle and the
    /// traversal runs through the internal [`MemSsaWalk`] engine.
    fn find_nearest_clobber(&mut self, function: &Function, mem: NodeId) -> NodeId
    where
        Self: Sized,
    {
        MemSsaWalk::new(function, self).nearest_clobber(mem)
    }
}

/// Narrows a `Load`'s memory edge onto `clobber` (its nearest clobbering
/// definition) — the perf side-step the read-only walk no longer performs
/// itself, lifted out so the walk stays `&Function`-only and the mutation is
/// an explicit caller step.
///
/// When `load` is a `Load` node, its memory input is repointed onto
/// `clobber`'s memory output (skipping every proven-disjoint def in between),
/// shortening the chain for this load and every future walk through it.  Only
/// a `Load` is rewired — a pure consumer (no memory output), so moving its
/// single incoming memory edge is invisible to every other node.  Idempotent,
/// and monotone-safe across fixed-point iterations (`MayAlias → Disjoint`
/// only).  A non-`Load` handle is left untouched.
pub(crate) fn narrow_load_to(ctx: &mut crate::EditFunction<'_>, load: NodeId, clobber: NodeId) {
    let rewire = {
        let function = ctx.function();
        if matches!(function.node_kind(load), NodeKind::Load(_)) {
            let target_mem = function
                .memory_output_of(clobber)
                .expect("a clobber node has a memory output");
            let mem_use = function
                .node_input_id_at(load, 0)
                .expect("a Load has a memory input at slot 0");
            let cur_mem = function.graph().value_of_use(mem_use);
            // Skip the no-op move when the load already points at its nearest
            // clobber (keeps the narrowing idempotent / convergent).
            (cur_mem != target_mem).then_some((mem_use, target_mem))
        } else {
            None
        }
    };
    if let Some((mem_use, target_mem)) = rewire {
        ctx.update_input(mem_use, target_mem);
    }
}

// ── DFS engine ───────────────────────────────────────────────────────────────
//
// The private engine behind the trait above: an iterative, memoized backward
// memory-token walk that resolves the nearest clobbering definition.  The
// trait's default methods construct a `MemSsaWalk` and call `nearest_clobber`;
// everything below is engine-private (the work-stack frames, the per-output
// memo, the phi-join).  See the module-level docs for the walk semantics,
// memoization + cycle guard, and stack-safety rationale.

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

/// Memoization state for a memory output during one walk.  An absent key in
/// the memo map means "not yet entered on this walk"; only `InProgress` and
/// `Done` are ever stored.
#[derive(Clone, Copy)]
enum Resolve {
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
/// rewrite is a separate caller-side mutating step (see [`narrow_load_to`]).
struct MemSsaWalk<'f, 'w, W: MemorySSAWalker> {
    function: &'f Function,
    walker: &'w mut W,
}

impl<'f, 'w, W: MemorySSAWalker> MemSsaWalk<'f, 'w, W> {
    fn new(function: &'f Function, walker: &'w mut W) -> Self {
        Self { function, walker }
    }

    /// Nearest clobbering memory-definition node reachable backward from
    /// `mem`'s memory output — a `Store` / `Call` / `CallOther` (or a
    /// disagreeing `MemPhi` boundary) — or the `InitialMemory` root when
    /// every path is clean (callers distinguish by the returned node kind).
    fn nearest_clobber(&mut self, mem: NodeId) -> NodeId {
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
    ///
    /// # Performance: cross-call worst case
    ///
    /// The memo is **per-call** (allocated fresh here), so a *single* walk is
    /// linear, but each caller (one per `Load` in `LoadForward`, one per stack
    /// slot in `CallStackArgCollect`) re-walks the chain from its own cursor.
    /// With N loads over an N-deep store chain that gives O(N × chain) per
    /// fixed-point iteration, and `LoadForward` repeats it every iteration.
    /// This is bounded in practice — `LoadForward`'s narrowing side-effect
    /// repoints each forwarded load's memory edge onto its nearest clobber,
    /// collapsing proven-disjoint runs so later iterations are cheap — and the
    /// chain length is bounded by the function's store count.  No cross-load
    /// nearest-clobber cache is kept; add one (keyed by `mem_value`) only if a
    /// profile on a real binary shows this walk dominating.
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
        // key means "not yet entered"; only `InProgress` and `Done` are stored.
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
                    if memo.contains_key(&cur) {
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
                        // Record the clean chain root so a caller can name it
                        // when no def aliases on any path.
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
                        .map(|s| match memo.get(&s).copied() {
                            Some(Resolve::Done(r)) => r,
                            _ => None,
                        })
                        .collect();
                    let result = self.combine(cur, &succ_results);
                    memo.insert(cur, Resolve::Done(result));
                }
            }
        }

        match memo.get(&start_mem).copied() {
            Some(Resolve::Done(r)) => r,
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
            // Linear node: its single memory predecessor (slot 0 for
            // `Store` / `Load`; slot 1 for `Call` / `CallOther`).
            _ => self.function.memory_input_of(node).into_iter().collect(),
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
}

#[cfg(test)]
mod tests;
