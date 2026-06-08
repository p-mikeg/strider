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
//! [`may_clobber`] returns the nearest clobbering definition NODE, or the
//! function's `InitialMemory` node when the chain reaches it with no
//! aliasing def on any path.  Callers distinguish "clean" by the returned
//! node's kind.  As a side effect it **narrows** the originating `Load`'s
//! memory edge onto that nearest clobber — see [`may_clobber`]'s
//! "Narrowing side effect" docs.
//!
//! ## Semantics
//!
//! Starting at the load's memory input, iterate the memory-token chain
//! backward (input slot 0 of `Load` / `Store` / `MemPhi`, the call's
//! memory input for `Call` / `CallOther`):
//!
//! * if a def `may_clobber` the load → return it (the nearest clobber);
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

use cranelift_entity::SecondaryMap;
use smallvec::SmallVec;
use strider_ir::Function;
use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind, ValueId};

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
}

/// Read-only variant of [`may_clobber`] that performs only the backward
/// analysis walk (Phase 1) without the narrowing rewrite (Phase 2).
///
/// Returns the nearest clobbering memory-definition node reachable backward
/// from the memory output of `mem` — a `Store` / `Call` / `CallOther` (or a
/// disagreeing `MemPhi` boundary) — or the function's `InitialMemory` node
/// when every path is clean.  Callers distinguish "clean" by the returned
/// node's kind (`InitialMemory`).
///
/// Unlike [`may_clobber`] this accepts a plain `&Function` rather than
/// `&mut EditFunction` and therefore performs **no narrowing** of the load's
/// memory edge.  Use this when the caller holds only a shared reference —
/// e.g. the indirect-branch stack-array classifier, which runs in a
/// read-only context.  Thin wrapper over [`MemSsaWalk`].
pub(crate) fn find_nearest_clobber<W: MemorySSAWalker>(
    function: &Function,
    walker: &mut W,
    mem: NodeId,
) -> NodeId {
    MemSsaWalk::new(function, walker).nearest_clobber(mem)
}

/// Finds the nearest memory-definition NODE reachable backward from the
/// memory output of `mem` that may clobber `load` (per `walker`), and
/// **narrows** the load's memory edge onto it.
///
/// Returns that clobber node — a `Store` / `Call` / `CallOther` (or a
/// `MemPhi` boundary where control-flow arms disagree) — or the function's
/// `InitialMemory` node when the chain is clean on every path (no aliasing
/// def reachable).  Callers distinguish "clean" by the returned node's
/// kind (`InitialMemory`).
///
/// `mem` is the memory-definition node whose output the load reads (the
/// producer of the load's memory input); the walk starts from its memory
/// output.  It walks only memory-token edges (input slot 0 of `Load` /
/// `Store` / `MemPhi`; the memory input of `Call` / `CallOther`).  At a
/// `MemPhi`, per-predecessor results are joined: agreeing predecessors
/// pass the shared result through, disagreeing predecessors make the phi
/// the boundary clobber.  See the module docs for the full semantics and
/// the cycle-guard contract.
///
/// # Narrowing side effect
///
/// When `load` is a `Load` node, its memory input is repointed onto the
/// returned clobber's memory output (skipping every proven-disjoint def in
/// between), shortening the chain for this load and every future walk that
/// passes through it.  Only a `Load` is rewired — it is a pure consumer
/// (no memory output), so moving its single incoming memory edge is
/// invisible to every other node; the `MemPhi`, its arms, and the
/// intervening stores stay in place for other consumers.  The narrowing is
/// idempotent (a load already at its nearest clobber is left untouched) and
/// stays valid across fixed-point iterations because alias precision is
/// monotone (`MayAlias → Disjoint` only).  A caller that passes a non-`Load`
/// handle (e.g. a chain node doubling as the load handle) gets the clobber
/// result with no rewrite.
pub(crate) fn may_clobber<W: MemorySSAWalker>(
    ctx: &mut crate::EditFunction<'_>,
    walker: &mut W,
    load: NodeId,
    mem: NodeId,
) -> NodeId {
    // Phase 1 — analysis: walk the chain backward to the nearest clobber
    // `T` (a `Store` / `Call` / `CallOther`, a disagreeing `MemPhi`, or the
    // clean `InitialMemory` root).  Delegates to the read-only helper so the
    // analysis logic lives in one place; the immutable borrow ends before the
    // narrowing rewrite below.
    let clobber = find_nearest_clobber(ctx.function(), walker, mem);

    // Phase 2 — narrowing: repoint the originating `Load`'s memory edge onto
    // `clobber`'s memory output when the walk proved the intervening defs
    // disjoint.  Only a `Load` is narrowed: it is a pure consumer (no memory
    // output), so moving its single incoming memory edge is invisible to
    // every other node — the `MemPhi`, its arms, and all intervening stores
    // stay in place for any other consumer.  The verdict is monotone
    // (`MayAlias → Disjoint` only), so this permanent rewrite stays valid
    // across fixed-point iterations.  Scoped so the read-only borrow ends
    // before the mutation.
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
            // Skip the no-op move when the load already points at its
            // nearest clobber (keeps the walk idempotent / convergent).
            (cur_mem != target_mem).then_some((mem_use, target_mem))
        } else {
            None
        }
    };
    if let Some((mem_use, target_mem)) = rewire {
        ctx.update_input(mem_use, target_mem);
    }

    clobber
}

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
/// by `ValueId` in a [`SecondaryMap`], so the default `Unseen` is the
/// state of every output not yet entered.
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
/// rewrite is a separate mutating step (see [`may_clobber`]).
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
    fn walk_from(
        &mut self,
        start_mem: ValueId,
        initial_memory: &mut Option<NodeId>,
    ) -> Option<ValueId> {
        // Dense per-output memo (entity-keyed, not a hash map): `Unseen` is
        // the default for every output not yet entered.
        let mut memo: SecondaryMap<ValueId, Resolve> = SecondaryMap::new();
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
                    if !matches!(memo[cur], Resolve::Unseen) {
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
                        memo[cur] = Resolve::Done(Some(cur));
                        continue;
                    }
                    memo[cur] = Resolve::InProgress;
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
                        .map(|s| match memo[s] {
                            Resolve::Done(r) => r,
                            _ => None,
                        })
                        .collect();
                    let result = self.combine(cur, &succ_results);
                    memo[cur] = Resolve::Done(result);
                }
            }
        }

        match memo[start_mem] {
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
                self.function.node_inputs(node).into_iter().skip(1).collect()
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
    /// `Store` / `Load` / `MemPhi`; the call's memory input (slot 1) for
    /// `Call` / `CallOther`.  `None` for `InitialMemory` and anything that
    /// does not carry an incoming memory edge.
    fn prev_mem(&self, node: NodeId) -> Option<ValueId> {
        let inputs = self.function.node_inputs(node);
        match *self.function.node_kind(node) {
            NodeKind::Store(_) | NodeKind::Load(_) => inputs.into_iter().next(),
            NodeKind::Call | NodeKind::CallOther { .. } => inputs.into_iter().nth(1),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests;
