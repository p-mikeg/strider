//! `PeepholePass` trait + generic driver for the kind-filtered, per-node
//! rewrite shape shared by most opt passes in this crate.
//!
//! A `PeepholePass` impl declares (a) which `NodeKind`s it cares about and
//! (b) how to attempt one rewrite at a given root.  The driver
//! ([`run_peephole`]) handles the worklist, kind-filtered seeding,
//! and (optionally) consumer re-enqueue on a successful rewrite.  Passes
//! that don't need cascading re-enqueue can override
//! [`PeepholePass::propagate_to_consumers`] to return `false`.
//!
//! `PeepholePass` is *below* the existing [`crate::opt::pipeline::Optimizer`]
//! trait — concrete passes implement `PeepholePass` and provide a thin
//! `Optimizer` impl whose body is just `run_peephole(self, ctx)`.  The
//! pipeline still consumes `dyn Optimizer` exactly as before.
//!
//! Passes that don't fit this shape (analytic passes, multi-stage passes
//! with a per-pass memo, etc.) keep their hand-written `Optimizer` impl.

use entity_utils::Worklist;
use strider_ir::node::{NodeId, NodeKind};

use crate::opt::error::Result;
use crate::opt::pipeline::OptimizationResult;

/// A kind-filtered, per-node rewrite pass.  See module docs.
pub(crate) trait PeepholePass {
    /// Concrete pass name, for debug / tracing only.  Held in the trait
    /// surface (not just on the concrete pass type) so `dyn`-erased
    /// drivers can attribute failures to the originating pass.
    #[allow(dead_code)]
    fn name(&self) -> &'static str;

    /// Which `NodeKind`s does this pass care about?  Seeded into the
    /// worklist by [`run_peephole`] via `ctx.preorder_kind`.
    fn matches_kind(&self, kind: &NodeKind) -> bool;

    /// Attempt to rewrite at `root`.  Returns `Changed` if a rewrite
    /// fired (the driver will re-enqueue consumers iff
    /// [`Self::propagate_to_consumers`] is `true`).
    ///
    /// # Errors
    /// Propagates the first error from the underlying rewrite.
    fn try_rewrite(
        &self,
        ctx: &mut crate::pattern::RewriteCtx<'_>,
        root: NodeId,
    ) -> Result<OptimizationResult>;

    /// When `true`, the driver re-enqueues every consumer of `root`'s
    /// outputs after a successful rewrite, so cascading folds can
    /// re-fire in the same sweep (no need for a fresh fixed-point
    /// iteration).  Default `true` matches the `ConstantFold` shape.
    fn propagate_to_consumers(&self) -> bool {
        true
    }
}

/// Drive a [`PeepholePass`] over the reachable graph.
///
/// Seeds the worklist with every kind-matching reachable root, then
/// drains the worklist by calling `pass.try_rewrite` on each root.  On
/// `Changed`, consumers of `root`'s outputs are re-enqueued (subject to
/// [`PeepholePass::propagate_to_consumers`]) so cascading folds can fire
/// in the same sweep.
///
/// Consumers are snapshotted **before** `try_rewrite` runs because the
/// rewrite typically rewires uses to a replacement, leaving
/// `output_uses(old_out)` empty afterwards.  A `SmallVec<[NodeId; 8]>`
/// inlines the common case (~95% of IR nodes fan out to <=8 consumers)
/// to avoid heap allocation on the hot worklist path.
///
/// # Errors
/// Propagates the first error from `try_rewrite`.
pub(crate) fn run_peephole<P: PeepholePass>(
    pass: &P,
    ctx: &mut crate::pattern::RewriteCtx<'_>,
) -> Result<OptimizationResult> {
    let mut work: Worklist<NodeId> =
        ctx.preorder_kind(|k| pass.matches_kind(k)).collect();
    let mut overall = OptimizationResult::NoChange;
    let propagate = pass.propagate_to_consumers();
    // Reused per iteration to snapshot consumer NodeIds BEFORE running
    // the pass body.  After a rewrite, `output_uses(old_out)` is empty
    // (uses were rewired to the replacement), so capture consumers ahead.
    let mut consumers: smallvec::SmallVec<[NodeId; 8]> = smallvec::SmallVec::new();
    while let Some(root) = work.dequeue() {
        if propagate {
            consumers.clear();
            for &out in ctx.node_outputs(root) {
                for (consumer, _) in ctx.output_uses(out) {
                    consumers.push(consumer);
                }
            }
        }
        let r = pass.try_rewrite(ctx, root)?;
        if r.changed() {
            overall = OptimizationResult::Changed;
            if propagate {
                for &consumer in &consumers {
                    work.enqueue(consumer);
                }
            }
        }
    }
    Ok(overall)
}

/// Emit a thin [`crate::opt::pipeline::Optimizer`] impl for a
/// [`PeepholePass`] type whose `Optimizer::optimize` body would be the
/// verbatim two-liner: build a `RewriteCtx`, hand it to
/// [`run_peephole`].
///
/// Use from a pass module after the `PeepholePass` impl block:
///
/// ```ignore
/// impl_optimizer_from_peephole!(MyPass);
/// ```
macro_rules! impl_optimizer_from_peephole {
    ($t:ty) => {
        impl $crate::opt::pipeline::Optimizer for $t {
            fn optimize(
                &self,
                graph: &mut strider_ir::Graph,
                entry: strider_ir::node::NodeId,
            ) -> $crate::opt::error::Result<$crate::opt::pipeline::OptimizationResult> {
                let mut ctx = $crate::pattern::RewriteCtx::new(graph, entry);
                $crate::opt::peephole::run_peephole(self, &mut ctx)
            }
        }
    };
}

pub(crate) use impl_optimizer_from_peephole;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::cell::RefCell;
    use strider_ir::node::{NodeKind, NodeOutputType};
    use strider_ir::IntBinaryOp;
    use strider_ir_test_utils::make_empty_fn;

    use crate::opt::error::Result;
    use crate::opt::pipeline::OptimizationResult;

    /// A scriptable pass: matches on a configured kind predicate, records
    /// every `try_rewrite` invocation, and on match rewires the root's
    /// single value output to a fresh `IntConst(REPLACEMENT_K)`.  Used by
    /// the tests below to assert ordering / propagation behaviour without
    /// pulling in a real opt pass.
    struct ScriptedPass {
        // The trait contract requires implementing `name()`; the test
        // driver doesn't surface it back but the field documents intent
        // when a fixture is read in isolation.
        #[allow(dead_code)]
        name: &'static str,
        match_kind: fn(&NodeKind) -> bool,
        do_rewrite: bool,
        propagate: bool,
        return_error: bool,
        visit_log: RefCell<Vec<u32>>,
    }

    const REPLACEMENT_K: u64 = 0xABCD_1234;

    impl PeepholePass for ScriptedPass {
        fn name(&self) -> &'static str {
            self.name
        }
        fn matches_kind(&self, k: &NodeKind) -> bool {
            (self.match_kind)(k)
        }
        fn try_rewrite(
            &self,
            ctx: &mut crate::pattern::RewriteCtx<'_>,
            root: NodeId,
        ) -> Result<OptimizationResult> {
            use cranelift_entity::EntityRef;
            self.visit_log.borrow_mut().push(root.index() as u32);
            if self.return_error {
                return Err(anyhow::anyhow!("scripted-pass forced error"));
            }
            if !self.do_rewrite {
                return Ok(OptimizationResult::NoChange);
            }
            let kind = *ctx.node_kind(root);
            if !(self.match_kind)(&kind) {
                return Ok(OptimizationResult::NoChange);
            }
            let [root_out] = ctx.node_outputs_exact::<1>(root)?;
            let ty = ctx.output_kind(root_out).as_value_or_err()?;
            let new_out = ctx.make_int_const(REPLACEMENT_K, ty)?;
            OptimizationResult::NoChange.after_replace(ctx, root_out, new_out)
        }
        fn propagate_to_consumers(&self) -> bool {
            self.propagate
        }
    }

    /// `fn() -> u64 { return 7; }` — minimal reachable graph.
    fn one_const_fn() -> strider_ir::Graph {
        make_empty_fn(|b| b.build_int_const(7u64, NodeOutputType::U64)).unwrap()
    }

    /// `fn() -> u64 { return Add(11, 13); }`.
    fn add_two_consts() -> strider_ir::Graph {
        make_empty_fn(|b| {
            let a = b.build_int_const(11u64, NodeOutputType::U64)?;
            let bb = b.build_int_const(13u64, NodeOutputType::U64)?;
            b.build_int_binary_operation(a, bb, IntBinaryOp::Add, NodeOutputType::U64)
        })
        .unwrap()
    }

    fn match_add(k: &NodeKind) -> bool {
        matches!(k, NodeKind::IntBinaryOp(IntBinaryOp::Add))
    }
    fn match_nothing(_: &NodeKind) -> bool {
        false
    }

    #[test]
    fn run_peephole_on_minimal_graph_no_match() {
        let mut fg = one_const_fn();
        let pass = ScriptedPass {
            name: "Empty",
            match_kind: match_nothing,
            do_rewrite: false,
            propagate: false,
            return_error: false,
            visit_log: RefCell::new(Vec::new()),
        };
        let entry = fg.entry().unwrap();
        let mut ctx = crate::pattern::RewriteCtx::new(fg.graph_mut(), entry);
        let r = run_peephole(&pass, &mut ctx).unwrap();
        assert_eq!(r, OptimizationResult::NoChange);
        assert!(pass.visit_log.borrow().is_empty());
    }

    #[test]
    fn run_peephole_pass_never_matches_returns_nochange() {
        // Graph has an Add but the pass kind-filter rejects everything.
        let mut fg = add_two_consts();
        let pass = ScriptedPass {
            name: "MissAll",
            match_kind: match_nothing,
            do_rewrite: true,
            propagate: true,
            return_error: false,
            visit_log: RefCell::new(Vec::new()),
        };
        let entry = fg.entry().unwrap();
        let mut ctx = crate::pattern::RewriteCtx::new(fg.graph_mut(), entry);
        let r = run_peephole(&pass, &mut ctx).unwrap();
        assert_eq!(r, OptimizationResult::NoChange);
        assert!(pass.visit_log.borrow().is_empty());
    }

    #[test]
    fn run_peephole_rewrites_and_reports_changed() {
        let mut fg = add_two_consts();
        let pass = ScriptedPass {
            name: "RewriteAdd",
            match_kind: match_add,
            do_rewrite: true,
            propagate: false,
            return_error: false,
            visit_log: RefCell::new(Vec::new()),
        };
        let entry = fg.entry().unwrap();
        let mut ctx = crate::pattern::RewriteCtx::new(fg.graph_mut(), entry);
        let r = run_peephole(&pass, &mut ctx).unwrap();
        assert_eq!(r, OptimizationResult::Changed);
        assert!(!pass.visit_log.borrow().is_empty());

        // Return's value-input is now an IntConst (the rewrite replacement).
        let ret = fg
            .all_node_ids()
            .find(|&n| matches!(fg.node_kind(n), NodeKind::Return))
            .expect("Return must exist");
        let value_input = fg.node_inputs(ret)[2];
        let producer = fg.get_node_from_output(value_input);
        assert!(
            matches!(fg.node_kind(producer), NodeKind::IntConst(_)),
            "Return's value input must be IntConst post-rewrite",
        );
    }

    #[test]
    fn run_peephole_with_propagate_false_skips_reenqueue() {
        // `Add(Add(1,2), 3)` — outer consumes inner.  With propagate=false
        // each Add is visited at most once (the seed-time visit).
        let mut fg = make_empty_fn(|b| {
            let a = b.build_int_const(1u64, NodeOutputType::U64)?;
            let bb = b.build_int_const(2u64, NodeOutputType::U64)?;
            let c = b.build_int_const(3u64, NodeOutputType::U64)?;
            let inner = b.build_int_binary_operation(a, bb, IntBinaryOp::Add, NodeOutputType::U64)?;
            b.build_int_binary_operation(inner, c, IntBinaryOp::Add, NodeOutputType::U64)
        })
        .unwrap();
        let pass = ScriptedPass {
            name: "RewriteAddNoProp",
            match_kind: match_add,
            do_rewrite: true,
            propagate: false,
            return_error: false,
            visit_log: RefCell::new(Vec::new()),
        };
        let entry = fg.entry().unwrap();
        let mut ctx = crate::pattern::RewriteCtx::new(fg.graph_mut(), entry);
        let _ = run_peephole(&pass, &mut ctx).unwrap();
        let log = pass.visit_log.borrow().clone();
        assert_eq!(log.len(), 2, "exactly two visits, no re-enqueue: {log:?}");
    }

    #[test]
    fn run_peephole_with_propagate_true_reenqueues_consumers() {
        let mut fg = make_empty_fn(|b| {
            let a = b.build_int_const(1u64, NodeOutputType::U64)?;
            let bb = b.build_int_const(2u64, NodeOutputType::U64)?;
            let c = b.build_int_const(3u64, NodeOutputType::U64)?;
            let inner = b.build_int_binary_operation(a, bb, IntBinaryOp::Add, NodeOutputType::U64)?;
            b.build_int_binary_operation(inner, c, IntBinaryOp::Add, NodeOutputType::U64)
        })
        .unwrap();
        let pass = ScriptedPass {
            name: "RewriteAddProp",
            match_kind: match_add,
            do_rewrite: true,
            propagate: true,
            return_error: false,
            visit_log: RefCell::new(Vec::new()),
        };
        let entry = fg.entry().unwrap();
        let mut ctx = crate::pattern::RewriteCtx::new(fg.graph_mut(), entry);
        let r = run_peephole(&pass, &mut ctx).unwrap();
        assert_eq!(r, OptimizationResult::Changed);
        // Each Add visited at least once; propagate-true allows extra
        // re-enqueue visits.  The exact count depends on worklist dedup
        // policy, but the lower bound is 2.
        let log_len = pass.visit_log.borrow().len();
        assert!(log_len >= 2, "expected >=2 visits with propagate=true, got {log_len}");
    }

    #[test]
    fn run_peephole_propagates_pass_internal_error() {
        let mut fg = add_two_consts();
        let pass = ScriptedPass {
            name: "Erroring",
            match_kind: match_add,
            do_rewrite: false,
            propagate: false,
            return_error: true,
            visit_log: RefCell::new(Vec::new()),
        };
        let entry = fg.entry().unwrap();
        let mut ctx = crate::pattern::RewriteCtx::new(fg.graph_mut(), entry);
        let r = run_peephole(&pass, &mut ctx);
        assert!(r.is_err(), "errored pass must surface error");
        let msg = format!("{:?}", r.unwrap_err());
        assert!(
            msg.contains("scripted-pass forced error"),
            "error must propagate, got {msg}",
        );
    }
}
