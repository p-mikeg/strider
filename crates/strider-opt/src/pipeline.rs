/// Drives the pipeline's decision to run another fixed-point iteration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptimizationResult {
    NoChange,
    /// At least one node was changed, added, or removed.
    Changed,
}

impl OptimizationResult {
    #[inline]
    #[must_use]
    pub fn changed(self) -> bool {
        matches!(self, OptimizationResult::Changed)
    }

    /// Maps the boolean return of [`strider_ir::Graph::replace_all_uses`].
    #[must_use]
    pub fn from_changed(changed: bool) -> Self {
        if changed {
            OptimizationResult::Changed
        } else {
            OptimizationResult::NoChange
        }
    }
}

/// Per-run, cross-pass context threaded through every [`Optimizer::apply`]
/// call.
pub struct OptCtx<'mem> {
    /// `None` cuts off every read of the image: `LoadReadOnly` bails, and a
    /// jump table stored in the image does not decode.
    pub rom: Option<&'mem dyn strider_ir::ReadOnlyMemory>,
    pub options: crate::OptOptions,
    /// Every live `IndirectBranch` placeholder AND seated `Switch` visited
    /// this run, mapped to `Some` when the dispatch target was recovered and
    /// `None` when it stays unresolvable.
    pub indirect_resolutions:
        rustc_hash::FxHashMap<strider_ir::node::NodeId, Option<strider_cfg::ResolvedTargets>>,
}

impl<'mem> OptCtx<'mem> {
    #[must_use]
    pub fn new(rom: Option<&'mem dyn strider_ir::ReadOnlyMemory>) -> Self {
        Self {
            rom,
            options: crate::OptOptions::default(),
            indirect_resolutions: rustc_hash::FxHashMap::default(),
        }
    }
}

/// A single IR optimization pass. Returns
/// [`OptimizationResult::Changed`] when it modified anything, which makes
/// the pipeline run another iteration.
///
/// ```
/// # use strider_opt::{OptCtx, OptimizationResult, Optimizer};
/// # use strider_opt::EditFunction;
/// #[derive(Clone)]
/// struct MyPass;
/// impl Optimizer for MyPass {
///     fn apply(
///         &self,
///         _edit: &mut EditFunction<'_>,
///         _ctx: &mut OptCtx<'_>,
///     ) -> anyhow::Result<OptimizationResult> {
///         // ... pass body operating on `_edit` ...
///         Ok(OptimizationResult::NoChange)
///     }
/// }
/// ```
///
pub trait Optimizer: OptimizerClone {
    /// `edit` wraps a built function, so its entry is a valid `NodeId`.
    ///
    /// # Errors
    ///
    /// Returns the first error the pass hits, typically an IR validation
    /// failure or a pattern-rewrite error.
    fn apply(
        &self,
        edit: &mut crate::EditFunction<'_>,
        ctx: &mut OptCtx<'_>,
    ) -> crate::Result<OptimizationResult>;

    /// The concrete struct's short name.
    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
            .rsplit("::")
            .next()
            .unwrap_or("UnknownPass")
    }
}

/// Runs one pass through a throwaway `EditFunction`, culling dead nodes
/// before and draining after.
///
/// `function` must be built (`function.entry()` is a valid `NodeId`).
///
/// # Errors
///
/// Returns the first error from [`Optimizer::apply`].
#[cfg(any(test, feature = "test-util"))]
pub fn run_one(
    pass: &dyn Optimizer,
    function: &mut strider_ir::Function,
    octx: &mut OptCtx<'_>,
) -> crate::Result<OptimizationResult> {
    let mut edit = crate::EditFunction::new(function);
    edit.cull_dead();
    let result = pass.apply(&mut edit, octx)?;
    edit.clean();
    Ok(result)
}

/// Post-pass sibling of [`run_one`].
///
/// # Errors
///
/// Returns the first error from [`PostOptimizer::apply`].
#[cfg(any(test, feature = "test-util"))]
pub fn run_post(
    pass: &dyn PostOptimizer,
    function: &mut strider_ir::Function,
    octx: &mut OptCtx<'_>,
) -> crate::Result<()> {
    let mut edit = crate::EditFunction::new(function);
    edit.cull_dead();
    pass.apply(&mut edit, octx)?;
    edit.clean();
    Ok(())
}

/// Defines an object-safe clone shim trait `$shim` for `dyn $obj`, plus its
/// blanket impl for every `$obj + Clone + 'static`.
macro_rules! clone_box_shim {
    ($(#[$attr:meta])* $shim:ident for dyn $obj:ident) => {
        $(#[$attr])*
        pub trait $shim {
            fn clone_box(&self) -> Box<dyn $obj>;
        }

        impl<T: $obj + Clone + 'static> $shim for T {
            fn clone_box(&self) -> Box<dyn $obj> {
                Box::new(self.clone())
            }
        }
    };
}

clone_box_shim! {
    /// Object-safe clone shim for [`Optimizer`].
    OptimizerClone for dyn Optimizer
}

/// A pass that runs ONCE on the converged graph.
///
/// Assumes the optimizer has converged: may rely on any canonical shape the
/// in-loop passes settle on (e.g. `Add(_, Neg(_))` for subtraction) rather
/// than re-normalising.
pub trait PostOptimizer: PostOptimizerClone {
    /// `edit` wraps the converged function.
    ///
    /// # Errors
    ///
    /// Returns the first error the pass hits.
    fn apply(&self, edit: &mut crate::EditFunction<'_>, ctx: &mut OptCtx<'_>) -> crate::Result<()>;

    /// The concrete struct's short name.
    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
            .rsplit("::")
            .next()
            .unwrap_or("UnknownPass")
    }
}

clone_box_shim! {
    /// Object-safe clone shim for [`PostOptimizer`].
    PostOptimizerClone for dyn PostOptimizer
}

/// An ordered pass list run in a shared fixed-point loop: every pass is
/// called once per iteration in registration order, repeating until no pass
/// reports a change.
pub struct OptimizerPipeline {
    passes: Vec<Box<dyn Optimizer>>,
    post_passes: Vec<Box<dyn PostOptimizer>>,
}

impl Default for OptimizerPipeline {
    fn default() -> Self {
        Self::new()
    }
}

impl OptimizerPipeline {
    #[must_use]
    pub fn new() -> Self {
        Self {
            passes: Vec::new(),
            post_passes: Vec::new(),
        }
    }

    pub fn add<O: Optimizer + 'static>(&mut self, opt: O) {
        self.passes.push(Box::new(opt));
    }

    pub fn add_post_pass<O: PostOptimizer + 'static>(&mut self, opt: O) {
        self.post_passes.push(Box::new(opt));
    }

    /// The fixed-point passes in registration order.
    #[must_use]
    pub fn passes(&self) -> &[Box<dyn Optimizer>] {
        &self.passes
    }

    #[must_use]
    pub fn post_passes(&self) -> &[Box<dyn PostOptimizer>] {
        &self.post_passes
    }

    /// Runs the passes to convergence, then each post-pass once in
    /// registration order, then re-validates the graph.
    ///
    /// `function` must be built (`function.entry()` is a valid `NodeId`).
    ///
    /// # Errors
    ///
    /// Returns the first error from any pass, or from the final validation. A
    /// pass error skips the final validation step and wins.
    pub fn run(
        &self,
        function: &mut strider_ir::Function,
        ctx: &mut OptCtx<'_>,
    ) -> crate::Result<()> {
        const MAX_ITERS: u32 = 1024;
        // Publish the pure-allocator set onto the function so every `decompose`
        // sees one consistent set. Config, not a memo, so it persists across the
        // per-pass memo drains below.
        function
            .side_tables_mut()
            .set_noalias_allocators(ctx.options.assumptions.noalias_allocators.clone());
        {
            // Scoped so the borrow of `function` is released before the
            // validation step below.
            let mut edit = crate::EditFunction::new(function);
            edit.cull_dead();
            let mut iters: u32 = 0;
            loop {
                let mut changed = false;
                for opt in &self.passes {
                    if opt.apply(&mut edit, ctx)?.changed() {
                        changed = true;
                        // Drain after every changing pass so the next pass in
                        // this iteration sees a culled graph, and invalidate the
                        // SP-decomposition and frame-escape memos: a rewrite can
                        // change or cull the value a cached verdict was computed
                        // for.
                        edit.clean();
                        edit.function().side_tables().clear_memory_slots();
                        edit.function().side_tables().clear_frame_escape();
                    }
                }
                if !changed {
                    break;
                }
                iters += 1;
                if iters >= MAX_ITERS {
                    anyhow::bail!(
                        "optimizer pipeline did not converge after {MAX_ITERS} iterations"
                    );
                }
            }
            for opt in &self.post_passes {
                // `memory_offsets` survives between post-passes: no post-pass
                // mutation changes an address value's decomposition, so the
                // filled slots stay valid as the user-facing per-node SSoT.
                opt.apply(&mut edit, ctx)?;
                edit.clean();
                // `CallStackArgCollect` rewrites `Call` inputs, which is what
                // the frame-escape walk reads.
                edit.function().side_tables().clear_frame_escape();
            }
            // That SSoT is what `pattern`'s region filters read, and every
            // changing pass above drained it.  A caller pipeline need not
            // register `StackOffsetDetect`, so refill it here or `stack_only`
            // matches nothing and says nothing.
            crate::post_opt::stack_offset_detect::stamp_all(&mut edit);
        }
        strider_ir::validate::validate(function)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::{OptCtx, Optimizer};
    use strider_ir::node::ValueType;
    use strider_ir::{IRBuilderExt, IRViewer, IRWalker};
    use strider_ir_test_utils::IrBuilderEx;
    use strider_ir_test_utils::SENTINEL_LIFT_ADDR;

    /// Single-region function returning `IntConst(k)`.
    fn one_const_fn(k: u64) -> strider_ir::Function {
        let mut b = strider_ir_test_utils::empty_builder().unwrap();
        let region = b.create_region_all().unwrap();
        b.set_entry_region_all(region).unwrap();
        b.set_region(region);
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let v = b.build_int_const(k, ValueType::I64).unwrap();
        b.build_return(Some(v), &[]).unwrap();
        b.set_lift_addr(None);
        b.build().unwrap()
    }

    /// A valid input graph must survive `run`'s final validation.
    #[test]
    fn pipeline_run_validates_final_graph_on_clean_input() -> crate::Result<()> {
        let mut function = one_const_fn(3);
        let pipeline = crate::default_pipeline();
        let before = function.walk().count();
        pipeline.run(&mut function, &mut OptCtx::new(None))?;
        let after = function.walk().count();
        // An already-folded constant cannot fold further, so the reachable
        // count is stable. Pins that `run` does not mutate the graph beyond
        // what the passes themselves produce.
        assert!(
            after <= before,
            "default pipeline must not GROW the reachable set"
        );
        Ok(())
    }

    /// A non-monotone pass that always claims a change must hit the
    /// iteration cap instead of spinning forever.
    #[test]
    fn fixed_point_limit_exceeded() {
        use super::{OptimizationResult, Optimizer, OptimizerPipeline};
        #[derive(Clone)]
        struct AlwaysChanged;
        impl Optimizer for AlwaysChanged {
            fn apply(
                &self,
                _edit: &mut crate::EditFunction<'_>,
                _ctx: &mut OptCtx<'_>,
            ) -> crate::Result<OptimizationResult> {
                Ok(OptimizationResult::Changed)
            }
        }

        let mut function = one_const_fn(0);
        let mut pipeline = OptimizerPipeline::new();
        pipeline.add(AlwaysChanged);
        let err = pipeline
            .run(&mut function, &mut OptCtx::new(None))
            .expect_err("pipeline must bail out on a non-monotone pass");
        assert!(
            err.to_string().contains("did not converge"),
            "expected 'did not converge' error, got {err:?}"
        );
    }

    /// Pins that the validate-on-finish step is wired and accepts a clean
    /// graph.
    #[test]
    fn run_validates_after_default_pipeline() -> crate::Result<()> {
        let mut function = one_const_fn(0);
        crate::default_pipeline().run(&mut function, &mut OptCtx::new(None))?;
        Ok(())
    }

    /// A pipeline carrying a post-pass must still produce a graph that
    /// validates.
    #[test]
    fn run_with_post_passes_validates() -> crate::Result<()> {
        use crate::{CallStackArgCollect, ConstantFold, OptimizerPipeline};
        let sp = rsleigh::Vn {
            addr_off: 0x20,
            addr_space: rsleigh::VnSpace::REGISTER,
            size: 4,
        };
        let mut b = strider_ir_test_utils::RegisterSet::new()
            .tracked(sp)
            .callee_saved(sp)
            .stack_args(Some(strider_target::StackArgs {
                base_offset: 0,
                increment: 8,
            }))
            .build_fn()?;
        let region = b.create_region_all()?;
        b.set_entry_region_all(region)?;
        b.set_region(region);
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        b.build_return(None, &[])?;
        b.set_lift_addr(None);
        let mut function = b.build()?;

        let mut p = OptimizerPipeline::new();
        p.add(ConstantFold::new());
        p.add_post_pass(CallStackArgCollect);
        p.run(&mut function, &mut OptCtx::new(None))?;
        Ok(())
    }

    /// `store sp-4 = 0x42; load sp-4` must forward to `IntConst(0x42)`.
    #[test]
    fn store_then_load_at_same_offset_forwarded() -> crate::Result<()> {
        use crate::{
            ConstantFold, DeadBranchElimination, KnownBits, LoadForward, OptimizerPipeline,
            PhiCollapse, RegionCollapse,
        };
        use strider_ir::IRViewer;
        use strider_ir::node::NodeKind;

        let sp = rsleigh::Vn {
            addr_off: 0x20,
            addr_space: rsleigh::VnSpace::REGISTER,
            size: 4,
        };
        let mut b = strider_ir_test_utils::builder(
            vec![sp],
            &[],
            &[sp],
            &[],
            None,
            0,
            strider_target::Endianness::Little,
        )?;
        let region = b.create_region_all()?;
        b.set_entry_region_all(region)?;
        b.set_region(region);
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let sp_v = b.read_variable(&sp)?;
        let four = b.build_int_const(4u64, ValueType::I32)?;
        let addr = b.build_sub_as_add_neg(sp_v, four, ValueType::I32)?;
        let data = b.build_int_const(0x42u64, ValueType::I32)?;
        b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::I32)?;
        b.build_return(Some(loaded), &[])?;
        b.set_lift_addr(None);
        let mut function = b.build()?;

        let mut p = OptimizerPipeline::new();
        p.add(ConstantFold::new());
        p.add(KnownBits);
        p.add(PhiCollapse);
        p.add(RegionCollapse);
        p.add(DeadBranchElimination);
        p.add(LoadForward::default());
        p.run(&mut function, &mut OptCtx::new(None))?;

        let val = crate::test_support::return_value(function.graph())?;
        let kind = *function.kind_of_value(val);
        assert!(
            matches!(kind, NodeKind::IntConst(_)) && function.int_const_u128(val) == Some(0x42),
            "load must forward to stored value, got {kind:?} (value={:?})",
            function.int_const_u128(val)
        );
        Ok(())
    }

    /// `CallStackArgCollect` must extend a Call's inputs with the
    /// positional stack args pushed before it.
    #[test]
    fn full_call_pipeline_collects_args() -> crate::Result<()> {
        use crate::{
            CallStackArgCollect, ConstantFold, DeadBranchElimination, KnownBits, LoadForward,
            OptimizerPipeline, PhiCollapse, RegionCollapse,
        };
        use strider_ir::node::NodeKind;

        let sp = rsleigh::Vn {
            addr_off: 0x20,
            addr_space: rsleigh::VnSpace::REGISTER,
            size: 4,
        };
        let mut b = strider_ir_test_utils::RegisterSet::new()
            .tracked(sp)
            .callee_saved(sp)
            .stack_vn(sp)
            .stack_args(Some(strider_target::StackArgs {
                base_offset: 0,
                increment: 4,
            }))
            .build_fn()?;
        let region = b.create_region_all()?;
        b.set_entry_region_all(region)?;
        b.set_region(region);
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let sp_v0 = b.read_variable(&sp)?;
        let four = b.build_int_const(4u64, ValueType::I32)?;
        let sp_v1 = b.build_sub_as_add_neg(sp_v0, four, ValueType::I32)?;
        b.write_variable(&sp, sp_v1)?;
        let arg1 = b.build_int_const(22u64, ValueType::I32)?;
        b.build_store(sp_v1, arg1, rsleigh::VnSpace::RAM)?;
        let sp_v2 = b.build_sub_as_add_neg(sp_v1, four, ValueType::I32)?;
        b.write_variable(&sp, sp_v2)?;
        let arg0 = b.build_int_const(11u64, ValueType::I32)?;
        b.build_store(sp_v2, arg0, rsleigh::VnSpace::RAM)?;
        let target = b.build_int_const(0x1000u64, ValueType::I32)?;
        b.build_call_cc(target, None)?;
        b.build_return(None, &[])?;
        b.set_lift_addr(None);
        let mut function = b.build()?;

        let mut p = OptimizerPipeline::new();
        p.add(ConstantFold::new());
        p.add(KnownBits);
        p.add(PhiCollapse);
        p.add(RegionCollapse);
        p.add(DeadBranchElimination);
        p.add(LoadForward::default());
        p.add_post_pass(CallStackArgCollect);
        p.run(&mut function, &mut OptCtx::new(None))?;

        let call = function
            .graph()
            .all_node_ids()
            .find(|&n| matches!(function.node_kind(n), NodeKind::Call))
            .expect("Call present");
        let inputs = function.node_inputs(call);
        assert_eq!(
            inputs.len(),
            6,
            "ctrl + mem + target + sp + 2 collected args = 6 inputs"
        );
        Ok(())
    }

    /// A 50-deep `Add(_, 1)` chain must reach fixed point: no premature
    /// exit, no infinite loop.
    #[test]
    fn long_reassoc_chain_converges() -> crate::Result<()> {
        use strider_ir::IntBinaryOp;
        let mut function = strider_ir_test_utils::make_empty_fn(|b| {
            let mut acc = b.build_int_const(0u64, ValueType::I64)?;
            for _ in 0..50 {
                let one = b.build_int_const(1u64, ValueType::I64)?;
                acc = b.build_int_binary_operation(acc, one, IntBinaryOp::Add, ValueType::I64)?;
            }
            Ok(acc)
        })?;
        crate::default_pipeline().run(&mut function, &mut OptCtx::new(None))?;
        // The chain folds to a single `IntConst(50)`.
        assert!(
            function.walk().count() < 20,
            "50-deep chain should fold; reachable={}",
            function.walk().count()
        );
        Ok(())
    }

    #[test]
    fn optimizer_name_is_the_concrete_struct_name() {
        let p: Box<dyn Optimizer> = Box::new(crate::opt::constant_fold::ConstantFold::default());
        assert_eq!(p.name(), "ConstantFold");
    }
}
