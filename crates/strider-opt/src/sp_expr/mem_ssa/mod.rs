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

use strider_ir::Function;
use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind};

mod engine;
use engine::MemSsaWalk;

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

    /// Like [`find_nearest_clobber`](Self::find_nearest_clobber) but
    /// **narrows** the originating load's memory edge onto the returned
    /// clobber (Phase 2), so it takes `&mut EditFunction`.
    ///
    /// # Narrowing side effect
    ///
    /// When `load` is a `Load` node, its memory input is repointed onto the
    /// returned clobber's memory output (skipping every proven-disjoint def
    /// in between), shortening the chain for this load and every future walk
    /// through it.  Only a `Load` is rewired — a pure consumer (no memory
    /// output), so moving its single incoming memory edge is invisible to
    /// every other node.  Idempotent, and monotone-safe across fixed-point
    /// iterations (`MayAlias → Disjoint` only).  A non-`Load` handle gets the
    /// clobber result with no rewrite.
    fn may_clobber(
        &mut self,
        ctx: &mut crate::EditFunction<'_>,
        load: NodeId,
        mem: NodeId,
    ) -> NodeId
    where
        Self: Sized,
    {
        // Phase 1 — analysis: walk backward to the nearest clobber via the
        // read-only engine.  The shared borrow of `ctx.function()` ends with
        // the walk, before the Phase-2 mutation below.
        let clobber = MemSsaWalk::new(ctx.function(), self).nearest_clobber(mem);

        // Phase 2 — narrowing: repoint the originating `Load`'s memory edge
        // onto `clobber`'s memory output when the walk proved the intervening
        // defs disjoint.  Scoped so the read-only borrow ends before the
        // mutation.
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
}

#[cfg(test)]
mod tests;
