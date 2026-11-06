/// Per-run context of [`Optimizer::apply`]. Claims live on the function
/// ([`strider_ir::Function::assume`]).
pub struct OptCtx<'mem> {
    /// `None` cuts off every read of the image.
    pub rom: Option<&'mem dyn strider_ir::ReadOnlyMemory>,
}

impl<'mem> OptCtx<'mem> {
    /// A context over `rom`, `None` for no image.
    #[must_use]
    pub fn new(rom: Option<&'mem dyn strider_ir::ReadOnlyMemory>) -> Self {
        Self { rom }
    }
}

/// A single IR optimization pass. A run that moves a use or kills a node
/// ([`EditFunction::generation`](crate::EditFunction::generation)) makes the
/// pipeline run another iteration.
pub trait Optimizer: Sync {
    /// `edit` wraps a built function, so its entry is a valid `NodeId`.
    fn apply(&self, edit: &mut crate::EditFunction<'_>, ctx: &OptCtx<'_>) -> crate::Result<()>;

    /// Whether the pass runs only once on a converged graph, which
    /// [`OptimizerPipeline::add`] refuses.
    fn is_post_pass(&self) -> bool {
        false
    }

    /// The concrete struct's short name.
    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
            .rsplit("::")
            .next()
            .unwrap_or("UnknownPass")
    }
}

/// Runs one pass through a throwaway `EditFunction`: drops the memory
/// analysis, culls dead nodes before and drains after. `true` when the pass
/// changed the graph.
///
/// `function` must be built (`function.entry()` is a valid `NodeId`).
///
/// # Panics
///
/// Panics with the rendered errors if the pass leaves `function` invalid.
#[cfg(any(test, feature = "test-util"))]
pub fn run_one(
    pass: &dyn Optimizer,
    function: &mut strider_ir::Function,
    octx: &OptCtx<'_>,
) -> crate::Result<bool> {
    let changed = run_one_unvalidated(pass, function, octx)?;
    if let Err(errors) = strider_ir::validate::validate(function) {
        panic!(
            "{} left invalid IR: {}",
            pass.name(),
            errors.report(function)
        );
    }
    Ok(changed)
}

/// [`run_one`] without the validation.
#[cfg(any(test, feature = "test-util"))]
pub fn run_one_unvalidated(
    pass: &dyn Optimizer,
    function: &mut strider_ir::Function,
    octx: &OptCtx<'_>,
) -> crate::Result<bool> {
    function.invalidate_analyses();
    let mut edit = crate::EditFunction::new(function);
    edit.cull_dead();
    let changed = apply_counted(pass, &mut edit, octx)?;
    edit.clean();
    if changed {
        edit.function().invalidate_analyses();
    }
    Ok(changed)
}

/// [`Optimizer::apply`], `true` when it moved a use or killed a node.
fn apply_counted(
    pass: &dyn Optimizer,
    edit: &mut crate::EditFunction<'_>,
    ctx: &OptCtx<'_>,
) -> crate::Result<bool> {
    let generation = edit.generation();
    pass.apply(edit, ctx)?;
    Ok(edit.generation() != generation)
}

/// Runs one post-pass as [`OptimizerPipeline::run`] does, without
/// validating: the memory classes survive a pass that changed nothing.
#[cfg(any(test, feature = "test-util"))]
pub fn run_post(
    pass: &dyn Optimizer,
    function: &mut strider_ir::Function,
    octx: &OptCtx<'_>,
) -> crate::Result<()> {
    let mut edit = crate::EditFunction::new(function);
    edit.cull_dead();
    apply_post(pass, &mut edit, octx)
}

/// Runs one post-pass. A changing pass drops every memo; the frame-escape
/// verdict is dropped either way, since `function_mut` edits are not counted.
fn apply_post(
    pass: &dyn Optimizer,
    edit: &mut crate::EditFunction<'_>,
    ctx: &OptCtx<'_>,
) -> crate::Result<()> {
    let edits = edit.edits();
    pass.apply(edit, ctx)?;
    edit.clean();
    if edit.edits() == edits {
        edit.function().clear_frame_escape();
    } else {
        edit.function().invalidate_analyses();
    }
    Ok(())
}

/// The iterations [`OptimizerPipeline::run`] allows before it stops with a
/// partly folded graph.
const MAX_ITERS: u32 = 1024;

#[cfg(test)]
thread_local! {
    /// Fixed-point iterations run.
    static ITERATIONS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// An ordered pass list run in a shared fixed-point loop until no pass moves
/// a use or kills a node. A pass whose last run changed nothing is skipped
/// until some edit changes the graph, so a pass must be a function of the
/// graph, its options, the function's claims and the ROM.
#[derive(Clone, Default)]
pub struct OptimizerPipeline {
    passes: Vec<&'static dyn Optimizer>,
    post_passes: Vec<&'static dyn Optimizer>,
}

impl OptimizerPipeline {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// # Panics
    ///
    /// Panics on a post-pass; [`Self::add_post_pass`] takes it.
    pub fn add(&mut self, pass: &'static dyn Optimizer) {
        assert!(
            !pass.is_post_pass(),
            "{} is a post-pass: add it with add_post_pass",
            pass.name()
        );
        self.passes.push(pass);
    }

    /// Adds a pass run once on the converged graph.
    pub fn add_post_pass(&mut self, pass: &'static dyn Optimizer) {
        self.post_passes.push(pass);
    }

    /// The fixed-point passes in registration order.
    #[must_use]
    pub fn passes(&self) -> &[&'static dyn Optimizer] {
        &self.passes
    }

    #[must_use]
    pub fn post_passes(&self) -> &[&'static dyn Optimizer] {
        &self.post_passes
    }

    /// Runs the passes to convergence or for `MAX_ITERS` iterations, then
    /// each post-pass once, then validates; every exit drains the graph and
    /// restamps memory classes. Errs with the first pass error (unvalidated,
    /// `function` mutated up to it), else a validation error. Records whether
    /// the loop converged in [`strider_ir::Function::optimizer_converged`],
    /// `false` after an error.
    pub fn run(&self, function: &mut strider_ir::Function, ctx: &OptCtx<'_>) -> crate::Result<()> {
        function.invalidate_analyses();
        function.set_optimizer_converged(false);
        let outcome = {
            let mut edit = crate::EditFunction::new(function);
            edit.cull_dead();
            let outcome = self.converge(&mut edit, ctx);
            edit.clean();
            // A failing pass can have moved an address after a stamp.
            if outcome.is_err() {
                edit.function().invalidate_analyses();
            }
            crate::mem_analysis::stamp_all(&mut edit);
            outcome
        };
        let converged = outcome?;
        strider_ir::validate::validate(function).map_err(|e| e.report(function))?;
        function.set_optimizer_converged(converged);
        Ok(())
    }

    /// The fixed-point loop, then the post-passes; whether the loop converged.
    fn converge(
        &self,
        edit: &mut crate::EditFunction<'_>,
        ctx: &OptCtx<'_>,
    ) -> crate::Result<bool> {
        let converged = self.fixed_point(edit, ctx)?;
        edit.clean();
        for opt in &self.post_passes {
            apply_post(*opt, edit, ctx)?;
        }
        Ok(converged)
    }

    /// Runs the passes until an iteration changes nothing (`true`) or
    /// [`MAX_ITERS`] iterations have each changed something (`false`).
    fn fixed_point(
        &self,
        edit: &mut crate::EditFunction<'_>,
        ctx: &OptCtx<'_>,
    ) -> crate::Result<bool> {
        // Per pass, `edits()` after its last run when that run changed nothing.
        let mut settled: Vec<Option<u64>> = vec![None; self.passes.len()];
        for _ in 0..MAX_ITERS {
            #[cfg(test)]
            ITERATIONS.with(|c| c.set(c.get() + 1));
            let mut changed = false;
            for (opt, settled) in self.passes.iter().zip(&mut settled) {
                if *settled == Some(edit.edits()) {
                    continue;
                }
                let pass_changed = apply_counted(*opt, edit, ctx)?;
                *settled = (!pass_changed).then(|| edit.edits());
                if pass_changed {
                    changed = true;
                    edit.clean();
                    edit.function().invalidate_analyses();
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
    use strider_ir_test_utils::{IrBuilderEx, IrWalkerEx, stack_args_at};

    /// Single-region function returning `IntConst(k)`.
    fn one_const_fn(k: u64) -> strider_ir::Function {
        crate::test_support::make_fn(|b| b.build_int_const(k, ValueType::int(64))).unwrap()
    }

    /// `Add(1, 2)` returned: ConstantFold folds it.
    fn folding_fn() -> strider_ir::Function {
        crate::test_support::make_fn(|b| {
            let one = b.build_int_const(1u64, ValueType::int(64))?;
            b.build_const_binop(2u64, one, strider_ir::IntBinaryOp::Add, ValueType::int(64))
        })
        .unwrap()
    }

    /// The fixed-point loop refuses a post-pass. Mutation: drop the check.
    #[test]
    #[should_panic(expected = "FunctionArgDetect is a post-pass")]
    fn add_refuses_a_post_pass() {
        super::OptimizerPipeline::new().add(&crate::FunctionArgDetect);
    }

    /// Mutation: leave `CallStackArgCollect` a fixed-point pass.
    #[test]
    #[should_panic(expected = "CallStackArgCollect is a post-pass")]
    fn add_refuses_the_call_argument_collector() {
        super::OptimizerPipeline::new().add(&crate::CallStackArgCollect);
    }

    /// A pass reports its own name, and a clone runs the same passes in the
    /// same order. Mutation: make `add` drop its pass.
    #[test]
    fn a_cloned_pipeline_runs_as_the_original() -> crate::Result<()> {
        let mut pipeline = super::OptimizerPipeline::new();
        pipeline.add(&crate::ConstantFold);
        pipeline.add_post_pass(&crate::CallStackArgCollect);
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
            (vec!["ConstantFold"], vec!["CallStackArgCollect"])
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
        assert!(
            after <= before,
            "default pipeline must not GROW the reachable set"
        );
        Ok(())
    }

    /// Redirects the returned constant `k` to `k + 1`: a change every run.
    fn bump_returned_const(edit: &mut crate::EditFunction<'_>) -> crate::Result<()> {
        let ret = edit
            .live_of_kind(|k| matches!(k, strider_ir::node::NodeKind::Return))
            .next()
            .expect("the fixture returns");
        // Return inputs are [ctrl, mem, value, ...].
        let value = edit.node_inputs(ret)[2];
        let k = edit
            .int_const_u128(value)
            .expect("the fixture returns a constant");
        let next = edit.build_int_const(k + 1, ValueType::int(64))?;
        assert!(edit.replace_value(value, next), "the return reads it");
        Ok(())
    }

    /// A chain of guards each reloading a constant past the previous guard's
    /// call settles in a bounded number of iterations.
    #[test]
    fn a_dependent_guard_chain_settles_in_bounded_iterations() -> crate::Result<()> {
        use crate::test_support::memory_shapes::guard_chain;
        let is_call =
            |k: &strider_ir::node::NodeKind| matches!(k, strider_ir::node::NodeKind::Call { .. });
        let mut f = guard_chain(200, 0, false)?;
        let iterations = crate::test_support::steps(&[&super::ITERATIONS], || {
            crate::default_pipeline().run(&mut f, &OptCtx::new(None))
        })?;
        assert!(iterations <= 10, "{iterations} iterations");
        assert!(!f.has_kind(is_call));
        let mut f = guard_chain(1500, 0, false)?;
        crate::default_pipeline().run(&mut f, &OptCtx::new(None))?;
        Ok(())
    }

    /// A store between guards costs each reload a bounded number of settle
    /// rounds, not one per guard above it.
    #[test]
    fn a_guard_chain_over_a_spill_costs_near_linear_work() -> crate::Result<()> {
        use crate::mem_analysis::WALK_STEPS;
        use crate::mem_ssa::CLIMB_STEPS;
        use crate::opt::load_forward::SETTLE_ROUNDS;
        use crate::test_support::memory_shapes::guard_chain;
        let work = |n| {
            let mut f = guard_chain(n, 0, true)?;
            let work =
                crate::test_support::steps(&[&SETTLE_ROUNDS, &WALK_STEPS, &CLIMB_STEPS], || {
                    crate::default_pipeline().run(&mut f, &OptCtx::new(None))
                })?;
            assert!(!f.has_kind(|k| matches!(k, strider_ir::node::NodeKind::Call { .. })));
            Ok(work)
        };
        crate::test_support::assert_near_linear("guard chain over a spill", 200, work, 260)
    }

    /// A guard whose constant takes the call keeps every later reload behind
    /// it. Mutation: skip the arm the `If` takes instead.
    #[test]
    fn a_taken_guard_call_blocks_the_reload() -> crate::Result<()> {
        let mut f = crate::test_support::memory_shapes::guard_chain(3, 1, false)?;
        crate::default_pipeline().run(&mut f, &OptCtx::new(None))?;
        let count = |k: fn(&strider_ir::node::NodeKind) -> bool| {
            f.walk().filter(|&n| k(f.node_kind(n))).count()
        };
        assert_eq!(
            count(|k| matches!(k, strider_ir::node::NodeKind::Call { .. })),
            3
        );
        assert_eq!(
            count(|k| matches!(k, strider_ir::node::NodeKind::Load(_))),
            2
        );
        Ok(())
    }

    /// A non-monotone pass that changes the graph every run stops at the
    /// iteration cap, recorded on the function, instead of spinning forever.
    #[test]
    fn fixed_point_limit_exceeded() {
        use super::{Optimizer, OptimizerPipeline};
        #[derive(Clone)]
        struct AlwaysChanged;
        impl Optimizer for AlwaysChanged {
            fn apply(
                &self,
                edit: &mut crate::EditFunction<'_>,
                _ctx: &OptCtx<'_>,
            ) -> crate::Result<()> {
                bump_returned_const(edit)?;
                Ok(())
            }
        }

        let mut function = one_const_fn(0);
        let mut pipeline = OptimizerPipeline::new();
        pipeline.add(&AlwaysChanged);
        pipeline
            .run(&mut function, &OptCtx::new(None))
            .expect("a capped run hands back a valid graph");
        assert!(!function.optimizer_converged());
        pipeline = OptimizerPipeline::new();
        pipeline.run(&mut function, &OptCtx::new(None)).unwrap();
        assert!(function.optimizer_converged(), "a converged run resets it");
    }

    /// A failed run leaves the graph part-optimized, so it is not converged.
    #[test]
    fn a_failed_run_is_not_converged() -> crate::Result<()> {
        let mut function = sp_store_fn()?;
        assert!(function.optimizer_converged());
        let mut pipeline = super::OptimizerPipeline::new();
        pipeline.add(&StampsMovesThenFails);
        assert!(pipeline.run(&mut function, &OptCtx::new(None)).is_err());
        assert!(!function.optimizer_converged());
        Ok(())
    }

    /// A capped run validates the graph it hands back.
    #[test]
    fn fixed_point_limit_still_validates() {
        use super::{Optimizer, OptimizerPipeline};
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
            ) -> crate::Result<()> {
                let ret = edit
                    .live_of_kind(|k| matches!(k, NodeKind::Return))
                    .next()
                    .expect("the fixture returns");
                // Return inputs are [ctrl, mem, value]; a fourth is this
                // pass's own, added once.
                if edit.node_inputs(ret).len() == 3 {
                    // `build_int_const` carries no contributor stamp, and the
                    // bumped constant never reaches this one.
                    let value = edit.build_int_const(0xDEAD_BEEFu64, ValueType::int(64))?;
                    edit.add_node_input(ret, value);
                }
                bump_returned_const(edit)?;
                Ok(())
            }
        }

        let mut function = one_const_fn(0);
        let mut pipeline = OptimizerPipeline::new();
        pipeline.add(&AlwaysChangedUnattributed);
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
        use super::Optimizer;
        #[derive(Clone)]
        struct NoOp;
        impl Optimizer for NoOp {
            fn apply(
                &self,
                _edit: &mut crate::EditFunction<'_>,
                _ctx: &OptCtx<'_>,
            ) -> crate::Result<()> {
                Ok(())
            }
        }

        let mut function = one_const_fn(5);
        let returned = function.returned();
        function.set_stack_slot(returned, returned, 16);
        assert!(
            function.memory_decomp(returned).1.is_some(),
            "the memo is installed"
        );
        crate::pipeline::run_one(&NoOp, &mut function, &OptCtx::new(None))?;
        assert!(
            function.memory_decomp(returned).1.is_none(),
            "run_one must drop a memo an earlier edit left"
        );
        Ok(())
    }

    /// `run` drops a memo an outside edit left, even when it adds no claim.
    #[test]
    fn run_resets_the_memory_analysis() -> crate::Result<()> {
        let mut function = one_const_fn(5);
        let returned = function.returned();
        function.set_stack_slot(returned, returned, 16);
        super::OptimizerPipeline::new().run(&mut function, &OptCtx::new(None))?;
        assert!(
            function.memory_decomp(returned).1.is_none(),
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
        folding.add(&crate::ConstantFold);
        folding.run(&mut function, &OptCtx::new(None))?;
        let pass = CountsRuns(std::sync::Arc::default());
        super::run_one_unvalidated(&pass, &mut function, &OptCtx::new(None))?;
        super::run_post(
            &crate::CallStackArgCollect,
            &mut function,
            &OptCtx::new(None),
        )?;
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
            ) -> crate::Result<()> {
                let old = crate::test_support::return_value(edit.function().graph())?;
                edit.function().set_stack_slot(old, old, 16);
                let value = edit.build_int_const(9u64, ValueType::int(64))?;
                edit.absorb_fingerprint(value, old);
                edit.replace_value(old, value);
                Ok(())
            }
        }

        let mut function = one_const_fn(5);
        let returned = function.returned();
        crate::pipeline::run_one(&StampsThenRewrites, &mut function, &OptCtx::new(None))?;
        assert!(function.memory_decomp(returned).1.is_none());
        Ok(())
    }

    /// `run_post` keeps a memo, as `run` does between its post-passes.
    #[test]
    fn run_post_keeps_the_memos() -> crate::Result<()> {
        #[derive(Clone)]
        struct NoOpPost;
        impl super::Optimizer for NoOpPost {
            fn apply(
                &self,
                _edit: &mut crate::EditFunction<'_>,
                _ctx: &OptCtx<'_>,
            ) -> crate::Result<()> {
                Ok(())
            }
        }

        let mut function = one_const_fn(5);
        let returned = function.returned();
        function.set_stack_slot(returned, returned, 16);
        super::run_post(&NoOpPost, &mut function, &OptCtx::new(None))?;
        assert!(function.memory_decomp(returned).1.is_some());
        Ok(())
    }

    /// An edit through `function_mut` leaves the edit count alone, so the
    /// frame-escape verdict is dropped after every post-pass all the same.
    #[test]
    fn run_post_drops_the_frame_escape_verdict() -> crate::Result<()> {
        /// Returns `Add(1, 2)`'s first operand instead, through the raw graph.
        #[derive(Clone)]
        struct RewiresThroughFunctionMut;
        impl super::Optimizer for RewiresThroughFunctionMut {
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
        function.set_frame_escape(true);
        super::run_post(
            &RewiresThroughFunctionMut,
            &mut function,
            &OptCtx::new(None),
        )?;
        assert_eq!(function.frame_escape(), None);
        Ok(())
    }

    /// Fills the memory classes, so a later post-pass can leave one stale.
    #[derive(Clone)]
    struct Stamps;

    impl super::Optimizer for Stamps {
        fn apply(
            &self,
            edit: &mut crate::EditFunction<'_>,
            _ctx: &OptCtx<'_>,
        ) -> crate::Result<()> {
            crate::mem_analysis::stamp_all(edit);
            Ok(())
        }
    }

    /// Moves the stored address `sp + c` to `sp + c - 4` by rewiring the
    /// `Add`'s constant, so the address value keeps its id.
    #[derive(Clone)]
    struct MovesStoreDown;

    impl super::Optimizer for MovesStoreDown {
        fn apply(
            &self,
            edit: &mut crate::EditFunction<'_>,
            _ctx: &OptCtx<'_>,
        ) -> crate::Result<()> {
            move_store_down(edit)
        }
    }

    fn move_store_down(edit: &mut crate::EditFunction<'_>) -> crate::Result<()> {
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

    /// Stamps the memory classes, moves the store, then fails.
    #[derive(Clone)]
    struct StampsMovesThenFails;

    impl super::Optimizer for StampsMovesThenFails {
        fn apply(
            &self,
            edit: &mut crate::EditFunction<'_>,
            _ctx: &OptCtx<'_>,
        ) -> crate::Result<()> {
            crate::mem_analysis::stamp_all(edit);
            move_store_down(edit)?;
            anyhow::bail!("failed after the move")
        }
    }

    /// `store sp - 4 = 0x42; return`, its entry phis collapsed so the address
    /// reads the entry SP.
    fn sp_store_fn() -> crate::Result<strider_ir::Function> {
        let mut function = raw_sp_store_fn()?;
        super::run_one(&crate::PhiCollapse, &mut function, &OptCtx::new(None))?;
        Ok(function)
    }

    fn raw_sp_store_fn() -> crate::Result<strider_ir::Function> {
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
        })
    }

    /// The stack offset the class of `function`'s store names.
    fn store_offset(function: &strider_ir::Function) -> Option<i128> {
        let store = function
            .walk()
            .find(|&n| matches!(function.node_kind(n), strider_ir::node::NodeKind::Store(_)))
            .expect("the store survives");
        function.stack_offset(store).map(|(_, k)| k)
    }

    /// A post-pass that rewires an address's producers leaves no stale
    /// class: the stamp after the post-passes reads the moved slot.
    #[test]
    fn a_post_pass_rewiring_an_address_restamps_its_class() -> crate::Result<()> {
        let mut function = raw_sp_store_fn()?;
        let mut p = super::OptimizerPipeline::new();
        p.add(&crate::PhiCollapse);
        p.add_post_pass(&Stamps);
        p.add_post_pass(&MovesStoreDown);
        p.run(&mut function, &OptCtx::new(None))?;
        assert_eq!(store_offset(&function), Some(-8));
        Ok(())
    }

    /// A pass failing after it moved an address, in the loop or after it,
    /// leaves the class of the moved slot, not the one it stamped before.
    /// Mutation: return the error before the restamp.
    #[test]
    fn a_failing_pass_leaves_no_stale_class() -> crate::Result<()> {
        for post in [false, true] {
            let mut function = sp_store_fn()?;
            let mut p = super::OptimizerPipeline::new();
            if post {
                p.add_post_pass(&StampsMovesThenFails);
            } else {
                p.add(&StampsMovesThenFails);
            }
            let err = p.run(&mut function, &OptCtx::new(None)).unwrap_err();
            assert!(err.to_string().contains("after the move"), "{err}");
            assert_eq!(store_offset(&function), Some(-8), "post-pass: {post}");
        }
        Ok(())
    }

    /// A capped run hands back its graph with every access classed, as a
    /// converged one does.
    #[test]
    fn a_capped_run_classes_every_access() -> crate::Result<()> {
        /// Moves the store down a slot every run.
        #[derive(Clone)]
        struct KeepsMoving;
        impl super::Optimizer for KeepsMoving {
            fn apply(
                &self,
                edit: &mut crate::EditFunction<'_>,
                _ctx: &OptCtx<'_>,
            ) -> crate::Result<()> {
                move_store_down(edit)
            }
        }
        let mut function = sp_store_fn()?;
        let mut p = super::OptimizerPipeline::new();
        p.add(&KeepsMoving);
        p.run(&mut function, &OptCtx::new(None))?;
        assert!(!function.optimizer_converged());
        let moved = -4 - 4 * i128::from(super::MAX_ITERS);
        assert_eq!(store_offset(&function), Some(moved));
        Ok(())
    }

    /// A rule failing after an earlier one moved an address still hands the
    /// function back with the moved slot's class. Mutation: return the
    /// error before the refresh.
    #[test]
    fn a_failing_rule_leaves_no_stale_class() -> crate::Result<()> {
        use strider_ir::node::{NodeId, NodeKind, ValueId};
        let mut function = sp_store_fn()?;
        let mut edit = crate::EditFunction::new(&mut function);
        edit.cull_dead();
        crate::mem_analysis::stamp_all(&mut edit);
        let rule =
            |edit: &mut crate::EditFunction<'_>, node: NodeId| -> crate::Result<Option<ValueId>> {
                match edit.node_kind(node) {
                    NodeKind::Store(_) => {
                        move_store_down(edit)?;
                        Ok(None)
                    }
                    NodeKind::InitialVar(_) => anyhow::bail!("failed after the move"),
                    _ => Ok(None),
                }
            };
        let err = crate::apply_rules_count(&mut edit, &[rule]).unwrap_err();
        assert!(err.to_string().contains("after the move"), "{err}");
        drop(edit);
        assert_eq!(store_offset(&function), Some(-8));
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
        ) -> crate::Result<()> {
            let run = self.runs.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if run >= self.edits {
                return Ok(());
            }
            let ret = edit
                .live_of_kind(|k| matches!(k, strider_ir::node::NodeKind::Return))
                .next()
                .expect("the fixture returns");
            let value = edit.build_int_const(100 + run as u64, ValueType::int(64))?;
            let old = edit.node_inputs(ret)[strider_ir::node::NodeKind::Return.input_head_len()];
            edit.absorb_fingerprint(value, old);
            edit.replace_value(old, value);
            Ok(())
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
        ) -> crate::Result<()> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(())
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
        // A pass is held `&'static`; each counter leaks one box.
        pipeline.add(Box::leak(Box::new(RewritesReturn {
            edits: 3,
            runs: Arc::clone(&rewrites),
        })));
        pipeline.add(Box::leak(Box::new(CountsRuns(Arc::clone(&counted)))));
        pipeline.run(&mut one_const_fn(1), &OptCtx::new(None))?;
        assert_eq!(rewrites.load(Ordering::Relaxed), 4);
        assert_eq!(counted.load(Ordering::Relaxed), 3);
        Ok(())
    }

    /// A pass reports nothing: the loop reruns exactly while it edits.
    /// Mutation: have the loop see no change.
    #[test]
    fn the_loop_reruns_while_a_pass_edits() -> crate::Result<()> {
        use super::{Optimizer, OptimizerPipeline};
        #[derive(Clone)]
        struct BumpsBelowThree;
        impl Optimizer for BumpsBelowThree {
            fn apply(
                &self,
                edit: &mut crate::EditFunction<'_>,
                _ctx: &OptCtx<'_>,
            ) -> crate::Result<()> {
                let returned = crate::test_support::return_value(edit.function().graph())?;
                if edit.int_const_u128(returned) < Some(3) {
                    bump_returned_const(edit)?;
                }
                Ok(())
            }
        }

        let mut function = one_const_fn(0);
        let mut pipeline = OptimizerPipeline::new();
        pipeline.add(&BumpsBelowThree);
        pipeline.run(&mut function, &OptCtx::new(None))?;
        let returned = function.returned();
        assert_eq!(function.int_const_u128(returned), Some(3));
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
            .stack_args(stack_args_at(0, 8))
            .build_fn()?;
        let region = b.create_region_all()?;
        b.set_entry_region_all(region)?;
        b.set_region(region);
        b.build_return(None, &[])?;
        let mut function = b.build()?;

        let mut p = OptimizerPipeline::new();
        p.add(&ConstantFold);
        p.add_post_pass(&CallStackArgCollect);
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
        let mut function = b.build()?;

        let mut p = OptimizerPipeline::new();
        p.add(&ConstantFold);
        p.add(&KnownBits);
        p.add(&PhiCollapse);
        p.add(&RegionCollapse);
        p.add(&DeadBranchElimination);
        p.add(&LoadForward);
        p.run(&mut function, &OptCtx::new(None))?;

        let val = function.returned();
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

    /// `x = 0; n times: if (x == 0) { x = 0 } else { x = 1 }; return x`.
    fn constant_branch_chain(n: usize) -> crate::Result<strider_ir::Function> {
        use strider_ir::IntCmpOp;
        let vn = strider_ir_test_utils::reg_vn(0x1000, 4);
        let i32t = ValueType::int(32);
        let mut b = strider_ir_test_utils::RegisterSet::new()
            .tracked(vn)
            .arg(vn)
            .build_fn()?;
        let r0 = b.create_region_all()?;
        b.set_entry_region_all(r0)?;
        b.set_region(r0);
        let z = b.build_int_const(0u64, i32t)?;
        b.write_variable(&vn, z)?;
        for _ in 0..n {
            let [a, e, j] = [(); 3].map(|_| b.create_region_all());
            let (a, e, j) = (a?, e?, j?);
            let x = b.read_variable(&vn)?;
            let zero = b.build_int_const(0u64, i32t)?;
            let c = b.build_int_cmp_operation(x, zero, IntCmpOp::Equal, i32t)?;
            b.build_if(c, a, e)?;
            for (r, k) in [(a, 0u64), (e, 1)] {
                b.set_region(r);
                let v = b.build_int_const(k, i32t)?;
                b.write_variable(&vn, v)?;
                b.build_branch(j)?;
            }
            b.set_region(j);
        }
        let x = b.read_variable(&vn)?;
        b.build_return(None, &[x])?;
        b.set_lift_addr(None);
        b.build()
    }

    /// Each link of a dependent constant-branch chain costs a fixed-point
    /// iteration, so a long one stops at the cap with a valid graph.
    #[test]
    fn a_capped_default_run_is_valid_and_not_converged() -> crate::Result<()> {
        for (n, converged) in [(50, true), (800, false)] {
            let mut function = constant_branch_chain(n)?;
            crate::default_pipeline().run(&mut function, &OptCtx::new(None))?;
            assert_eq!(function.optimizer_converged(), converged, "n={n}");
            strider_ir::validate::validate(&function).map_err(|e| e.report(&function))?;
        }
        Ok(())
    }

    #[test]
    fn optimizer_name_is_the_concrete_struct_name() {
        let p: &dyn Optimizer = &crate::opt::constant_fold::ConstantFold;
        assert_eq!(p.name(), "ConstantFold");
    }
}
