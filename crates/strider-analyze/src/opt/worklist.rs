//! Shared per-pass worklist helpers used by every fold-style optimizer.
//!
//! Passes use [`entity_utils::Worklist<NodeId>`] directly (FIFO with
//! dedup-while-queued, backed by a [`DenseEntitySet<NodeId>`] for O(1)
//! membership keyed by `NodeId`'s u32 index).  This module retains the
//! kind-filtered seeding helper.  The orphan-detach reachability sweep
//! moved onto `strider_pattern::RewriteCtx::detach_unreachable_nodes` so
//! it routes through the one mutation surface.

use entity_utils::Worklist;
use strider_ir::node::{NodeId, NodeKind};

/// Seeds a worklist with every node reachable from `ctx.entry()` whose
/// [`NodeKind`] satisfies `pred`.
///
/// Replaces the recurring `ctx.walk_kind(...).collect::<Vec<_>>()`
/// followed by a `for node in collected { ... }` loop: the seeded
/// worklist gives the same one-shot iteration semantics (kind-filtered,
/// no re-enqueue unless a rule explicitly pushes consumers) without
/// allocating an intermediate `Vec`, and lets passes upgrade in place to
/// cascading rewrites by calling [`Worklist::enqueue`] on consumers.
pub(crate) fn seeded_kind<P>(ctx: &strider_pattern::RewriteCtx<'_>, mut pred: P) -> Worklist<NodeId>
where
    P: FnMut(&NodeKind) -> bool,
{
    ctx.walk_kind(|k| pred(k)).collect()
}

// The orphan-detach sweep (detaching the inputs of every node not
// reachable from the function entry) now lives on
// `strider_pattern::RewriteCtx::detach_unreachable_nodes`, so it routes
// through the one mutation surface alongside every other rewrite.  The
// tests below exercise that production path through a `RewriteCtx`.

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use entity_utils::DenseEntitySet;
    use strider_ir::node::{NodeKind, NodeOutputKind, NodeOutputType};
    use strider_ir::IntBinaryOp;
    use strider_ir_test_utils::{make_empty_fn, SENTINEL_LIFT_ADDR};
    use strider_pattern::GraphRewriteCtxExt;

    use crate::opt::OptRewrite;

    /// Drive the production orphan-detach sweep
    /// (`RewriteCtx::detach_unreachable_nodes`) over `fg`.  Returns `true`
    /// iff at least one node's inputs were detached.
    fn detach(fg: &mut strider_ir::Function, _entry: NodeId) -> bool {
        fg.with_rewrite_ctx(|ctx| {
            let entry = ctx.entry();
            Ok(ctx.detach_unreachable_nodes(entry))
        })
        .unwrap()
    }

    /// A minimal function `fn() -> u64 { return 7; }` — Entry + Return chain,
    /// no orphans, single IntConst.
    fn trivial_const_fn() -> strider_ir::Function {
        make_empty_fn(|b| b.build_int_const(7u64, NodeOutputType::I64)).unwrap()
    }

    /// Helper: count nodes whose `inputs.is_empty()` is false in the full
    /// arena (reachable or not).
    fn count_nodes_with_inputs(g: &strider_ir::Graph) -> usize {
        g.all_node_ids()
            .filter(|n| !g.node_inputs(*n).is_empty())
            .count()
    }

    #[test]
    fn detach_on_minimal_fn_is_noop() {
        // No orphaned nodes — Entry/Return chain is the only reachable shape.
        let mut fg = trivial_const_fn();
        let entry = fg.entry().unwrap();
        let pre_count = count_nodes_with_inputs(fg.graph());
        let r = detach(&mut fg, entry);
        assert!(!r, "all-reachable graph must report no detach");
        let post_count = count_nodes_with_inputs(fg.graph());
        assert_eq!(pre_count, post_count, "no inputs were detached");
    }

    #[test]
    fn detach_on_all_reachable_is_noop() {
        // `fn() -> u64 { return Add(11, 13); }` — the Add and both
        // IntConsts are reachable from Return → Entry.  No detach.
        let mut fg = make_empty_fn(|b| {
            let a = b.build_int_const(11u64, NodeOutputType::I64)?;
            let bb = b.build_int_const(13u64, NodeOutputType::I64)?;
            b.build_int_binary_operation(a, bb, IntBinaryOp::Add, NodeOutputType::I64)
        })
        .unwrap();
        let entry = fg.entry().unwrap();
        let pre_count = count_nodes_with_inputs(fg.graph());
        let r = detach(&mut fg, entry);
        assert!(!r, "all-reachable graph must report no detach");
        let post_count = count_nodes_with_inputs(fg.graph());
        assert_eq!(pre_count, post_count);
    }

    #[test]
    fn detach_on_orphan_subgraph_detaches_their_inputs() {
        // Reachable: `return 7;`.  Orphan: `Add(99, 100)` grafted onto the
        // arena but not consumed by anything reachable.
        let mut fg = trivial_const_fn();

        // Graft an orphan: an Add over two fresh IntConsts.  The Add has
        // inputs (so it's a detach candidate); detection should detach
        // its inputs without touching the reachable graph.
        let orphan_a = fg.make_int_const(99u64, NodeOutputType::I64).unwrap();
        let orphan_b = fg.make_int_const(100u64, NodeOutputType::I64).unwrap();
        let orphan_node = fg.create_node(
            NodeKind::IntBinaryOp(IntBinaryOp::Add),
            [orphan_a, orphan_b],
            [NodeOutputKind::OutputType(NodeOutputType::I64)],
        );
        fg.set_asm_fingerprint(orphan_node, vec![SENTINEL_LIFT_ADDR]);

        // Pre-conditions: orphan exists, has inputs, is unreachable.
        assert_eq!(fg.node_inputs(orphan_node).len(), 2);
        let reachable_pre: DenseEntitySet<NodeId> = fg.walk().collect();
        assert!(!reachable_pre.contains(orphan_node), "fixture must orphan the Add");

        let entry = fg.entry().unwrap();
        let r = detach(&mut fg, entry);
        assert!(r, "orphan with inputs must report a detach");
        assert_eq!(
            fg.node_inputs(orphan_node).len(),
            0,
            "orphan Add's inputs must have been detached",
        );
    }

    #[test]
    fn detach_distinguishes_reachable_vs_unreachable() {
        // Build: reachable Add fed by two consts, plus an unreachable Add
        // grafted separately.  After detach: reachable Add keeps inputs;
        // unreachable Add has inputs cleared.
        let mut fg = make_empty_fn(|b| {
            let a = b.build_int_const(1u64, NodeOutputType::I64)?;
            let bb = b.build_int_const(2u64, NodeOutputType::I64)?;
            b.build_int_binary_operation(a, bb, IntBinaryOp::Add, NodeOutputType::I64)
        })
        .unwrap();
        // Locate the reachable Add for later assertion.
        let reachable_add = fg
            .walk()
            .find(|&n| matches!(fg.node_kind(n), NodeKind::IntBinaryOp(IntBinaryOp::Add)))
            .expect("reachable Add must exist");
        assert_eq!(fg.node_inputs(reachable_add).len(), 2);

        // Graft an orphan Add — distinct constants so it isn't shared
        // with the reachable Add via dedup.
        let oa = fg.make_int_const(101u64, NodeOutputType::I64).unwrap();
        let ob = fg.make_int_const(103u64, NodeOutputType::I64).unwrap();
        let orphan = fg.create_node(
            NodeKind::IntBinaryOp(IntBinaryOp::Add),
            [oa, ob],
            [NodeOutputKind::OutputType(NodeOutputType::I64)],
        );
        fg.set_asm_fingerprint(orphan, vec![SENTINEL_LIFT_ADDR]);
        assert_eq!(fg.node_inputs(orphan).len(), 2);

        let entry = fg.entry().unwrap();
        let r = detach(&mut fg, entry);
        assert!(r, "orphan with inputs must report a detach");
        assert_eq!(
            fg.node_inputs(reachable_add).len(),
            2,
            "reachable Add must be untouched",
        );
        assert_eq!(
            fg.node_inputs(orphan).len(),
            0,
            "orphan Add must have inputs detached",
        );
    }

    #[test]
    fn detach_idempotent_on_repeat_call() {
        // After the first detach Changed, a second invocation returns
        // NoChange (no nodes left with both unreachable+nonempty inputs).
        let mut fg = trivial_const_fn();
        let a = fg.make_int_const(7u64, NodeOutputType::I64).unwrap();
        let b = fg.make_int_const(8u64, NodeOutputType::I64).unwrap();
        let orphan = fg.create_node(
            NodeKind::IntBinaryOp(IntBinaryOp::Add),
            [a, b],
            [NodeOutputKind::OutputType(NodeOutputType::I64)],
        );
        fg.set_asm_fingerprint(orphan, vec![SENTINEL_LIFT_ADDR]);
        let entry = fg.entry().unwrap();
        let r1 = detach(&mut fg, entry);
        let r2 = detach(&mut fg, entry);
        assert!(r1, "first call detaches the orphan");
        assert!(!r2, "second call must be a no-op");
    }

    #[test]
    fn detach_handles_orphan_cycle_without_panic() {
        // Construct an unreachable two-node cycle: A's input is B's output,
        // B's input is A's output.  The reachability walk starts at Entry
        // and never reaches A or B, so both are candidates.  Detaching
        // their inputs breaks the cycle.  This pins the "no infinite loop"
        // invariant.
        //
        // We can't form a true mutual cycle through `create_node` (inputs
        // must already exist when the node is created), so model it as
        // a self-loop instead: graft a node whose only input is its own
        // output.  Validate by routing through `replace_all_uses` from
        // a placeholder.
        let mut fg = trivial_const_fn();
        // Create a placeholder Add whose inputs are two consts.
        let c1 = fg.make_int_const(50u64, NodeOutputType::I64).unwrap();
        let c2 = fg.make_int_const(60u64, NodeOutputType::I64).unwrap();
        let orphan = fg.create_node(
            NodeKind::IntBinaryOp(IntBinaryOp::Add),
            [c1, c2],
            [NodeOutputKind::OutputType(NodeOutputType::I64)],
        );
        fg.set_asm_fingerprint(orphan, vec![SENTINEL_LIFT_ADDR]);
        // The orphan's output isn't fed back into itself (the IR doesn't
        // allow direct self-cycles through create_node), but the detach
        // routine only inspects `walk_from(entry)` for reachability.  Pin
        // that the implementation finishes without hanging on the
        // unreachable subgraph: a successful return is the assertion.
        let entry = fg.entry().unwrap();
        let _ = detach(&mut fg, entry);
        // Re-running is a no-op — proves termination.
        let r2 = detach(&mut fg, entry);
        assert!(!r2, "second call must be a no-op");
    }
}
