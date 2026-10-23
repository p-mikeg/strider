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

    /// Maps the boolean return of [`EditFunction::replace_value`](crate::EditFunction::replace_value).
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
/// call. It carries no claim: a pass reads the function's
/// ([`strider_ir::Function::assume`] adds them).
pub struct OptCtx<'mem> {
    /// `None` cuts off every read of the image: `LoadReadOnly` bails, and a
    /// jump table stored in the image does not decode.
    pub rom: Option<&'mem dyn strider_ir::ReadOnlyMemory>,
}

impl<'mem> OptCtx<'mem> {
    /// A context over `rom`, `None` for no image.
    #[must_use]
    pub fn new(rom: Option<&'mem dyn strider_ir::ReadOnlyMemory>) -> Self {
        Self { rom }
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
///         _ctx: &OptCtx<'_>,
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
        ctx: &OptCtx<'_>,
    ) -> crate::Result<OptimizationResult>;

    /// The concrete struct's short name.
    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
            .rsplit("::")
            .next()
            .unwrap_or("UnknownPass")
    }
}

/// Runs one pass through a throwaway `EditFunction`: drops the memory
/// analysis, culls dead nodes before and drains after.
///
/// `function` must be built (`function.entry()` is a valid `NodeId`).
///
/// # Errors
///
/// Returns the first error from [`Optimizer::apply`].
///
/// # Panics
///
/// Panics with the rendered errors if the pass leaves `function` invalid.
#[cfg(any(test, feature = "test-util"))]
pub fn run_one(
    pass: &dyn Optimizer,
    function: &mut strider_ir::Function,
    octx: &OptCtx<'_>,
) -> crate::Result<OptimizationResult> {
    let result = run_one_unvalidated(pass, function, octx)?;
    if let Err(errors) = strider_ir::validate::validate(function) {
        panic!(
            "{} left invalid IR: {}",
            pass.name(),
            errors.report(function)
        );
    }
    Ok(result)
}

/// [`run_one`] without the validation, for a fixture malformed on purpose and
/// for a bench timing the pass alone.
///
/// # Errors
///
/// Returns the first error from [`Optimizer::apply`].
#[cfg(any(test, feature = "test-util"))]
pub fn run_one_unvalidated(
    pass: &dyn Optimizer,
    function: &mut strider_ir::Function,
    octx: &OptCtx<'_>,
) -> crate::Result<OptimizationResult> {
    // As `run` starts: a memo an edit since the last run can have outdated is
    // dropped.
    function.side_tables().invalidate_analyses();
    let mut edit = crate::EditFunction::new(function);
    edit.cull_dead();
    let result = apply_checked(pass, &mut edit, octx)?;
    edit.clean();
    if result.changed() {
        edit.function().side_tables().invalidate_analyses();
    }
    Ok(result)
}

/// [`Optimizer::apply`], asserting in a debug build that a pass reporting
/// `NoChange` did not edit: the fixed point would exit on it.
fn apply_checked(
    pass: &dyn Optimizer,
    edit: &mut crate::EditFunction<'_>,
    ctx: &OptCtx<'_>,
) -> crate::Result<OptimizationResult> {
    let generation = edit.generation();
    let result = pass.apply(edit, ctx)?;
    debug_assert!(
        result.changed() || edit.generation() == generation,
        "{} reported NoChange after moving a use or killing a node",
        pass.name()
    );
    Ok(result)
}

/// Post-pass sibling of [`run_one`]: the memory classes survive a pass that
/// changed nothing, as between `run`'s post-passes.
///
/// # Errors
///
/// Returns the first error from [`PostOptimizer::apply`].
#[cfg(any(test, feature = "test-util"))]
pub fn run_post(
    pass: &dyn PostOptimizer,
    function: &mut strider_ir::Function,
    octx: &OptCtx<'_>,
) -> crate::Result<()> {
    let mut edit = crate::EditFunction::new(function);
    edit.cull_dead();
    apply_post(pass, &mut edit, octx)
}

/// One iteration of `run`'s post-pass loop: a pass that changed the graph
/// drops the memos, since it may have rewired an address. The frame-escape
/// verdict goes after every pass, since an edit through `function_mut` is
/// not counted.
fn apply_post(
    pass: &dyn PostOptimizer,
    edit: &mut crate::EditFunction<'_>,
    ctx: &OptCtx<'_>,
) -> crate::Result<()> {
    let edits = edit.edits();
    pass.apply(edit, ctx)?;
    edit.clean();
    if edit.edits() == edits {
        edit.function().side_tables().clear_frame_escape();
    } else {
        edit.function().side_tables().invalidate_analyses();
    }
    Ok(())
}

/// Defines an object-safe clone shim trait `$shim` for `dyn $obj`, plus its
/// blanket impl for every `$obj + Clone + 'static`.
macro_rules! clone_box_shim {
    ($(#[$attr:meta])* $shim:ident for dyn $obj:ident) => {
        $(#[$attr])*
        pub trait $shim {
            /// `Send` so a pipeline can be handed to another thread: the
            /// Python binding wraps one in a class that must be, and holding
            /// it back would keep the GIL held across a whole `analyze`.
            fn clone_box(&self) -> Box<dyn $obj + Send>;
        }

        impl<T: $obj + Clone + Send + 'static> $shim for T {
            fn clone_box(&self) -> Box<dyn $obj + Send> {
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
/// than re-normalising.  The memory classes survive a post-pass that changes
/// nothing; one that changes the graph drops them.
pub trait PostOptimizer: PostOptimizerClone {
    /// `edit` wraps the converged function.
    ///
    /// # Errors
    ///
    /// Returns the first error the pass hits.
    fn apply(&self, edit: &mut crate::EditFunction<'_>, ctx: &OptCtx<'_>) -> crate::Result<()>;

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
/// reports a change.  A pass whose last run changed nothing is skipped until
/// some edit changes the live graph, so a pass must be a function of the graph,
/// its options, the function's claims and the ROM; the claims cannot change
/// during a run.
pub struct OptimizerPipeline {
    passes: Vec<Box<dyn Optimizer + Send>>,
    post_passes: Vec<Box<dyn PostOptimizer + Send>>,
}

impl Default for OptimizerPipeline {
    fn default() -> Self {
        Self::new()
    }
}

/// Each pass cloned through its `clone_box`, so a clone runs the same passes
/// in the same order.
impl Clone for OptimizerPipeline {
    fn clone(&self) -> Self {
        Self {
            passes: self.passes.iter().map(|pass| pass.clone_box()).collect(),
            post_passes: self
                .post_passes
                .iter()
                .map(|pass| pass.clone_box())
                .collect(),
        }
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

    pub fn add<O: Optimizer + Send + 'static>(&mut self, opt: O) {
        self.add_boxed(Box::new(opt));
    }

    /// [`Self::add`] for a pass already boxed.
    pub fn add_boxed(&mut self, pass: Box<dyn Optimizer + Send>) {
        self.passes.push(pass);
    }

    pub fn add_post_pass<O: PostOptimizer + Send + 'static>(&mut self, opt: O) {
        self.add_post_boxed(Box::new(opt));
    }

    /// [`Self::add_post_pass`] for a pass already boxed.
    pub fn add_post_boxed(&mut self, pass: Box<dyn PostOptimizer + Send>) {
        self.post_passes.push(pass);
    }

    /// The fixed-point passes in registration order.
    #[must_use]
    pub fn passes(&self) -> &[Box<dyn Optimizer + Send>] {
        &self.passes
    }

    #[must_use]
    pub fn post_passes(&self) -> &[Box<dyn PostOptimizer + Send>] {
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
    /// pass error skips the final validation step and wins, leaving `function`
    /// mutated up to the failing rewrite.
    ///
    /// Exhausting the iteration cap also leaves `function` mutated, but drained
    /// and validated: the caller keeps the graph, so it is handed back in the
    /// shape every other exit hands it back in, and a validation failure there
    /// is reported ahead of the non-convergence.  The post-passes do not run.
    ///
    /// Every pass reads [`strider_ir::Function::assumptions`]; a caller adds
    /// claims with [`strider_ir::Function::assume`] before the run.
    pub fn run(&self, function: &mut strider_ir::Function, ctx: &OptCtx<'_>) -> crate::Result<()> {
        const MAX_ITERS: u32 = 1024;
        // A memo left by an earlier run or an edit since is dropped: a rewrite
        // can move the address it names.
        function.side_tables().invalidate_analyses();
        // Scoped so the borrow of `function` ends before validation.
        let converged = {
            let mut edit = crate::EditFunction::new(function);
            edit.cull_dead();
            let converged = self.fixed_point(&mut edit, ctx, MAX_ITERS)?;
            // The caller keeps a capped graph, so drain it; the post-passes
            // read a converged one and are skipped.
            edit.clean();
            if converged {
                for opt in &self.post_passes {
                    apply_post(opt.as_ref(), &mut edit, ctx)?;
                }
                // What `pattern`'s region filters read; every changing pass
                // above drained it.
                crate::mem_analysis::stamp_all(&mut edit);
            }
            converged
        };
        strider_ir::validate::validate(function).map_err(|e| e.report(function))?;
        anyhow::ensure!(
            converged,
            "optimizer pipeline did not converge after {MAX_ITERS} iterations"
        );
        Ok(())
    }

    /// Runs the passes until an iteration changes nothing (`true`) or
    /// `max_iters` iterations have each changed something (`false`).
    fn fixed_point(
        &self,
        edit: &mut crate::EditFunction<'_>,
        ctx: &OptCtx<'_>,
        max_iters: u32,
    ) -> crate::Result<bool> {
        // Per pass, `edits()` after its last run when that run changed nothing.
        let mut settled: Vec<Option<u64>> = vec![None; self.passes.len()];
        for _ in 0..max_iters {
            let mut changed = false;
            for (opt, settled) in self.passes.iter().zip(&mut settled) {
                if *settled == Some(edit.edits()) {
                    continue;
                }
                let result = apply_checked(opt.as_ref(), edit, ctx)?;
                *settled = (!result.changed()).then(|| edit.edits());
                if result.changed() {
                    changed = true;
                    // Drain so the next pass sees a culled graph, and drop the
                    // memos a rewrite can invalidate.
                    edit.clean();
                    edit.function().side_tables().invalidate_analyses();
                }
            }
            if !changed {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::{OptCtx, Optimizer};
    use strider_ir::node::ValueType;
    use strider_ir::{IRBuilderExt, IRViewer, IRWalker};
    use strider_ir_test_utils::SENTINEL_LIFT_ADDR;
    use strider_ir_test_utils::{IrBuilderEx, stack_args_at};

    /// Single-region function returning `IntConst(k)`.
    fn one_const_fn(k: u64) -> strider_ir::Function {
        let mut b = strider_ir_test_utils::empty_builder().unwrap();
        let region = b.create_region_all().unwrap();
        b.set_entry_region_all(region).unwrap();
        b.set_region(region);
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let v = b.build_int_const(k, ValueType::int(64)).unwrap();
        b.build_return(Some(v), &[]).unwrap();
        b.set_lift_addr(None);
        b.build().unwrap()
    }

    /// `Add(1, 2)` returned: ConstantFold folds it.
    fn folding_fn() -> strider_ir::Function {
        let mut b = strider_ir_test_utils::empty_builder().unwrap();
        let region = b.create_region_all().unwrap();
        b.set_entry_region_all(region).unwrap();
        b.set_region(region);
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let one = b.build_int_const(1u64, ValueType::int(64)).unwrap();
        let two = b.build_int_const(2u64, ValueType::int(64)).unwrap();
        let sum = b
            .build_int_binary_operation(one, two, strider_ir::IntBinaryOp::Add, ValueType::int(64))
            .unwrap();
        b.build_return(Some(sum), &[]).unwrap();
        b.set_lift_addr(None);
        b.build().unwrap()
    }

    /// A boxed pass reports its own name, and a clone runs the same passes in
    /// the same order: the copy every Python-driven analysis runs. Mutation:
    /// make `add_boxed` drop its pass.
    #[test]
    fn a_cloned_pipeline_of_boxed_passes_runs_as_the_original() -> crate::Result<()> {
        let mut pipeline = super::OptimizerPipeline::new();
        pipeline.add_boxed(Box::new(crate::ConstantFold::new()));
        pipeline.add_post_boxed(Box::new(crate::StackOffsetDetect));
        let names = |p: &super::OptimizerPipeline| {
            (
                p.passes().iter().map(|p| p.name()).collect::<Vec<_>>(),
                p.post_passes().iter().map(|p| p.name()).collect::<Vec<_>>(),
            )
        };
        let clone = pipeline.clone();
        assert_eq!(names(&clone), names(&pipeline));
        assert_eq!(
            names(&pipeline),
            (vec!["ConstantFold"], vec!["StackOffsetDetect"])
        );

        let (mut a, mut b) = (folding_fn(), folding_fn());
        pipeline.run(&mut a, &OptCtx::new(None))?;
        clone.run(&mut b, &OptCtx::new(None))?;
        assert!(
            !a.walk()
                .any(|n| matches!(a.node_kind(n), strider_ir::node::NodeKind::IntBinaryOp(_))),
            "precondition: the boxed ConstantFold ran",
        );
        assert_eq!(a.to_text(false), b.to_text(false));
        Ok(())
    }

    /// A valid input graph must survive `run`'s final validation.
    #[test]
    fn pipeline_run_validates_final_graph_on_clean_input() -> crate::Result<()> {
        let mut function = one_const_fn(3);
        let pipeline = crate::default_pipeline();
        let before = function.walk().count();
        pipeline.run(&mut function, &OptCtx::new(None))?;
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
                _ctx: &OptCtx<'_>,
            ) -> crate::Result<OptimizationResult> {
                Ok(OptimizationResult::Changed)
            }
        }

        let mut function = one_const_fn(0);
        let mut pipeline = OptimizerPipeline::new();
        pipeline.add(AlwaysChanged);
        let err = pipeline
            .run(&mut function, &OptCtx::new(None))
            .expect_err("pipeline must bail out on a non-monotone pass");
        assert!(
            err.to_string().contains("did not converge"),
            "expected 'did not converge' error, got {err:?}"
        );
    }

    /// The caller keeps the graph a capped run mutated, so it must not be one
    /// the crate's own validator rejects: the cap runs the final validation
    /// and reports its failure ahead of the non-convergence.
    #[test]
    fn fixed_point_limit_still_validates() {
        use super::{OptimizationResult, Optimizer, OptimizerPipeline};
        use strider_ir::node::NodeKind;
        /// Never converges, and leaves a reachable node with no
        /// asm-fingerprint, which `validate` refuses.
        #[derive(Clone)]
        struct AlwaysChangedUnattributed;
        impl Optimizer for AlwaysChangedUnattributed {
            fn apply(
                &self,
                edit: &mut crate::EditFunction<'_>,
                _ctx: &OptCtx<'_>,
            ) -> crate::Result<OptimizationResult> {
                let ret = edit
                    .live_of_kind(|k| matches!(k, NodeKind::Return))
                    .next()
                    .expect("the fixture returns");
                // Return inputs are [ctrl, mem, value]; a fourth is this
                // pass's own, added once.
                if edit.node_inputs(ret).len() == 3 {
                    // `build_int_const` carries no contributor stamp.
                    let value = edit.build_int_const(7u64, ValueType::int(64))?;
                    edit.add_node_input(ret, value);
                }
                Ok(OptimizationResult::Changed)
            }
        }

        let mut function = one_const_fn(0);
        let mut pipeline = OptimizerPipeline::new();
        pipeline.add(AlwaysChangedUnattributed);
        let err = pipeline
            .run(&mut function, &OptCtx::new(None))
            .expect_err("the pass never converges");
        assert!(
            err.to_string().contains("asm-fingerprint"),
            "a capped run must validate the graph it hands back, got {err:?}"
        );
    }

    /// `run_one` stands in for one pass of `run`'s fixed-point loop, which
    /// drops the memos an edit since the last run can have outdated.
    #[test]
    fn run_one_resets_the_memory_analysis() -> crate::Result<()> {
        use super::{OptimizationResult, Optimizer};
        #[derive(Clone)]
        struct NoOp;
        impl Optimizer for NoOp {
            fn apply(
                &self,
                _edit: &mut crate::EditFunction<'_>,
                _ctx: &OptCtx<'_>,
            ) -> crate::Result<OptimizationResult> {
                Ok(OptimizationResult::NoChange)
            }
        }

        let mut function = one_const_fn(5);
        let returned = crate::test_support::return_value(function.graph())?;
        function
            .side_tables()
            .set_stack_slot(returned, returned, 16);
        assert!(
            function.side_tables().memory_decomp(returned).1.is_some(),
            "the memo is installed"
        );
        crate::pipeline::run_one(&NoOp, &mut function, &OptCtx::new(None))?;
        assert!(
            function.side_tables().memory_decomp(returned).1.is_none(),
            "run_one must drop a memo an earlier edit left"
        );
        Ok(())
    }

    /// `run` drops a memo an outside edit left, even when it adds no claim.
    #[test]
    fn run_resets_the_memory_analysis() -> crate::Result<()> {
        let mut function = one_const_fn(5);
        let returned = crate::test_support::return_value(function.graph())?;
        function
            .side_tables()
            .set_stack_slot(returned, returned, 16);
        super::OptimizerPipeline::new().run(&mut function, &OptCtx::new(None))?;
        assert!(
            function.side_tables().memory_decomp(returned).1.is_none(),
            "run must drop a memo an earlier edit left"
        );
        Ok(())
    }

    /// `run`, `run_one_unvalidated` and `run_post` each keep the claims the
    /// function holds, the run folding a node.
    #[test]
    fn no_run_withdraws_a_claim() -> crate::Result<()> {
        let mut function = folding_fn();
        let claims = crate::AssumptionOptions {
            noalias_allocators: [0x1000].into_iter().collect(),
            stack_global_disjoint: true,
            escape_analysis: true,
            ..crate::AssumptionOptions::none()
        };
        function.assume(&claims);
        let mut folding = super::OptimizerPipeline::new();
        folding.add(crate::ConstantFold::new());
        folding.run(&mut function, &OptCtx::new(None))?;
        let pass = CountsRuns(std::sync::Arc::default());
        super::run_one_unvalidated(&pass, &mut function, &OptCtx::new(None))?;
        super::run_post(&crate::StackOffsetDetect, &mut function, &OptCtx::new(None))?;
        assert_eq!(function.assumptions(), &claims);
        Ok(())
    }

    /// `run_one` also drops the memos after a changing pass, as `run`'s
    /// fixed-point loop does.
    #[test]
    fn run_one_drops_the_memos_after_a_changing_pass() -> crate::Result<()> {
        /// Stamps the returned value, then replaces it.
        #[derive(Clone)]
        struct StampsThenRewrites;
        impl super::Optimizer for StampsThenRewrites {
            fn apply(
                &self,
                edit: &mut crate::EditFunction<'_>,
                _ctx: &OptCtx<'_>,
            ) -> crate::Result<super::OptimizationResult> {
                let old = crate::test_support::return_value(edit.function().graph())?;
                edit.function().side_tables().set_stack_slot(old, old, 16);
                let value = edit.build_int_const(9u64, ValueType::int(64))?;
                edit.absorb_fingerprint(value, old);
                edit.replace_value(old, value);
                Ok(super::OptimizationResult::Changed)
            }
        }

        let mut function = one_const_fn(5);
        let returned = crate::test_support::return_value(function.graph())?;
        crate::pipeline::run_one(&StampsThenRewrites, &mut function, &OptCtx::new(None))?;
        assert!(function.side_tables().memory_decomp(returned).1.is_none());
        Ok(())
    }

    /// `run_post` keeps a memo, as `run` does between its post-passes.
    #[test]
    fn run_post_keeps_the_memos() -> crate::Result<()> {
        #[derive(Clone)]
        struct NoOpPost;
        impl super::PostOptimizer for NoOpPost {
            fn apply(
                &self,
                _edit: &mut crate::EditFunction<'_>,
                _ctx: &OptCtx<'_>,
            ) -> crate::Result<()> {
                Ok(())
            }
        }

        let mut function = one_const_fn(5);
        let returned = crate::test_support::return_value(function.graph())?;
        function
            .side_tables()
            .set_stack_slot(returned, returned, 16);
        super::run_post(&NoOpPost, &mut function, &OptCtx::new(None))?;
        assert!(function.side_tables().memory_decomp(returned).1.is_some());
        Ok(())
    }

    /// An edit through `function_mut` leaves the edit count alone, so the
    /// frame-escape verdict is dropped after every post-pass all the same.
    #[test]
    fn run_post_drops_the_frame_escape_verdict() -> crate::Result<()> {
        /// Returns `Add(1, 2)`'s first operand instead, through the raw graph.
        #[derive(Clone)]
        struct RewiresThroughFunctionMut;
        impl super::PostOptimizer for RewiresThroughFunctionMut {
            fn apply(
                &self,
                edit: &mut crate::EditFunction<'_>,
                _ctx: &OptCtx<'_>,
            ) -> crate::Result<()> {
                let graph = edit.function_mut().graph_mut();
                let sum = crate::test_support::return_value(graph)?;
                let one = graph.nth_input(graph.producer(sum), 0).expect("Add(1, 2)");
                let (ret, slot) = graph.value_uses(sum).next().expect("returned");
                let slot = graph.node_input_id_at(ret, slot as usize)?;
                graph.update_input(slot, one);
                Ok(())
            }
        }

        let mut function = folding_fn();
        function.side_tables().set_frame_escape(true);
        super::run_post(
            &RewiresThroughFunctionMut,
            &mut function,
            &OptCtx::new(None),
        )?;
        assert_eq!(function.side_tables().frame_escape(), None);
        Ok(())
    }

    /// Moves the stored address `sp + c` to `sp + c - 4` by rewiring the
    /// `Add`'s constant, so the address value keeps its id.
    #[derive(Clone)]
    struct MovesStoreDown;

    impl super::PostOptimizer for MovesStoreDown {
        fn apply(
            &self,
            edit: &mut crate::EditFunction<'_>,
            _ctx: &OptCtx<'_>,
        ) -> crate::Result<()> {
            let store = edit
                .live_of_kind(|k| matches!(k, strider_ir::node::NodeKind::Store(_)))
                .next()
                .expect("the fixture stores");
            let addr = edit.access_address(store).expect("a store has an address");
            let add = edit.producer(addr);
            let old = edit.nth_input(add, 1).expect("an Add has two operands");
            let c = edit.int_const_u128(old).expect("the addend is constant");
            let moved = edit.build_int_const(c.wrapping_sub(4) as u64, ValueType::int(32))?;
            edit.absorb_fingerprint(moved, old);
            let slot = edit.node_input_id_at(add, 1)?;
            edit.update_input(slot, moved);
            Ok(())
        }
    }

    /// A post-pass that rewires an address's producers leaves no stale
    /// class: the stamp after the post-passes reads the moved slot.
    #[test]
    fn a_post_pass_rewiring_an_address_restamps_its_class() -> crate::Result<()> {
        let mut function =
            strider_ir_test_utils::make_sp_fn(strider_ir_test_utils::stack_vn_x86(), |b, sp_v| {
                let minus_four = b.build_int_const(0xFFFF_FFFCu64, ValueType::int(32))?;
                let addr = b.build_int_binary_operation(
                    sp_v,
                    minus_four,
                    strider_ir::IntBinaryOp::Add,
                    ValueType::int(32),
                )?;
                let data = b.build_int_const(0x42u64, ValueType::int(32))?;
                b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
                b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
                b.build_return(None, &[])?;
                Ok(())
            })?;

        let mut p = super::OptimizerPipeline::new();
        p.add(crate::PhiCollapse);
        p.add_post_pass(crate::StackOffsetDetect);
        p.add_post_pass(MovesStoreDown);
        p.run(&mut function, &OptCtx::new(None))?;

        let store = function
            .walk()
            .find(|&n| matches!(function.node_kind(n), strider_ir::node::NodeKind::Store(_)))
            .expect("the store survives");
        assert_eq!(function.stack_offset(store).map(|(_, k)| k), Some(-8));
        Ok(())
    }

    /// Rewrites the returned value to a fresh constant on its first `edits`
    /// runs.
    #[derive(Clone)]
    struct RewritesReturn {
        edits: usize,
        runs: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl super::Optimizer for RewritesReturn {
        fn apply(
            &self,
            edit: &mut crate::EditFunction<'_>,
            _ctx: &OptCtx<'_>,
        ) -> crate::Result<super::OptimizationResult> {
            let run = self.runs.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if run >= self.edits {
                return Ok(super::OptimizationResult::NoChange);
            }
            let ret = edit
                .live_of_kind(|k| matches!(k, strider_ir::node::NodeKind::Return))
                .next()
                .expect("the fixture returns");
            let value = edit.build_int_const(100 + run as u64, ValueType::int(64))?;
            let old = edit.node_inputs(ret)[strider_ir::node::NodeKind::Return.input_head_len()];
            edit.absorb_fingerprint(value, old);
            edit.replace_value(old, value);
            Ok(super::OptimizationResult::Changed)
        }
    }

    /// Counts its runs and changes nothing.
    #[derive(Clone)]
    struct CountsRuns(std::sync::Arc<std::sync::atomic::AtomicUsize>);

    impl super::Optimizer for CountsRuns {
        fn apply(
            &self,
            _edit: &mut crate::EditFunction<'_>,
            _ctx: &OptCtx<'_>,
        ) -> crate::Result<super::OptimizationResult> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(super::OptimizationResult::NoChange)
        }
    }

    /// Three rewrites take four iterations; the pass after the rewriter reruns
    /// after each rewrite, but not in the last iteration, where nothing changed
    /// since its previous run.
    #[test]
    fn a_pass_that_changed_nothing_is_skipped_until_the_graph_changes() -> crate::Result<()> {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let rewrites = Arc::new(AtomicUsize::new(0));
        let counted = Arc::new(AtomicUsize::new(0));
        let mut pipeline = super::OptimizerPipeline::new();
        pipeline.add(RewritesReturn {
            edits: 3,
            runs: Arc::clone(&rewrites),
        });
        pipeline.add(CountsRuns(Arc::clone(&counted)));
        pipeline.run(&mut one_const_fn(1), &OptCtx::new(None))?;
        assert_eq!(rewrites.load(Ordering::Relaxed), 4);
        assert_eq!(counted.load(Ordering::Relaxed), 3);
        Ok(())
    }

    /// An edit reported as no change would let the fixed point exit early.
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "reported NoChange after moving a use")]
    fn a_pass_hiding_an_edit_trips_the_debug_check() {
        use super::{OptimizationResult, Optimizer, OptimizerPipeline};
        use strider_ir::node::NodeKind;
        #[derive(Clone)]
        struct HidesAnEdit;
        impl Optimizer for HidesAnEdit {
            fn apply(
                &self,
                edit: &mut crate::EditFunction<'_>,
                _ctx: &OptCtx<'_>,
            ) -> crate::Result<OptimizationResult> {
                let ret = edit
                    .live_of_kind(|k| matches!(k, NodeKind::Return))
                    .next()
                    .expect("the fixture returns");
                let value = edit.build_int_const(9u64, ValueType::int(64))?;
                let slot = edit.node_input_id_at(ret, 2)?;
                edit.update_input(slot, value);
                Ok(OptimizationResult::NoChange)
            }
        }

        let mut function = one_const_fn(1);
        let mut pipeline = OptimizerPipeline::new();
        pipeline.add(HidesAnEdit);
        let _ = pipeline.run(&mut function, &OptCtx::new(None));
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
            .stack_args(stack_args_at(0, 8))
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
        p.run(&mut function, &OptCtx::new(None))?;
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
        let four = b.build_int_const(4u64, ValueType::int(32))?;
        let addr = b.build_sub_as_add_neg(sp_v, four, ValueType::int(32))?;
        let data = b.build_int_const(0x42u64, ValueType::int(32))?;
        b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::int(32))?;
        b.build_return(Some(loaded), &[])?;
        b.set_lift_addr(None);
        let mut function = b.build()?;

        let mut p = OptimizerPipeline::new();
        p.add(ConstantFold::new());
        p.add(KnownBits);
        p.add(PhiCollapse);
        p.add(RegionCollapse);
        p.add(DeadBranchElimination);
        p.add(LoadForward);
        p.run(&mut function, &OptCtx::new(None))?;

        let val = crate::test_support::return_value(function.graph())?;
        let kind = *function.kind_of_value(val);
        assert!(
            matches!(kind, NodeKind::IntConst(_)) && function.int_const_u128(val) == Some(0x42),
            "load must forward to stored value, got {kind:?} (value={:?})",
            function.int_const_u128(val)
        );
        Ok(())
    }

    /// A 50-deep `Add(_, 1)` chain must reach fixed point: no premature
    /// exit, no infinite loop.
    #[test]
    fn long_reassoc_chain_converges() -> crate::Result<()> {
        use strider_ir::IntBinaryOp;
        let mut function = strider_ir_test_utils::make_empty_fn(|b| {
            let mut acc = b.build_int_const(0u64, ValueType::int(64))?;
            for _ in 0..50 {
                let one = b.build_int_const(1u64, ValueType::int(64))?;
                acc =
                    b.build_int_binary_operation(acc, one, IntBinaryOp::Add, ValueType::int(64))?;
            }
            Ok(acc)
        })?;
        crate::default_pipeline().run(&mut function, &OptCtx::new(None))?;
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
        let p: Box<dyn Optimizer + Send> = Box::new(crate::opt::constant_fold::ConstantFold);
        assert_eq!(p.name(), "ConstantFold");
    }
}

#[cfg(test)]
mod clone_shim_tests {
    /// `clone_box` is reachable by NAME, not only through the blanket impl: a
    /// consumer holding a pass out of `OptimizerPipeline::passes` has to be
    /// able to clone it, and the shim traits live in this private module.
    #[test]
    fn a_pass_out_of_passes_can_be_cloned_by_name() {
        use crate::{Optimizer, OptimizerClone};
        let pipeline = crate::default_pipeline();
        let first: &(dyn Optimizer + Send) = &**pipeline.passes().first().expect("a pass");
        let cloned: Box<dyn Optimizer + Send> = OptimizerClone::clone_box(first);
        assert_eq!(cloned.name(), first.name());
    }
}
