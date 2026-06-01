/// Whether an optimization pass made any change to the IR graph.
///
/// Passes return this from `Optimizer::optimize`.  The pipeline uses it to
/// decide whether to run another fixed-point iteration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptimizationResult {
    /// The graph was not modified.
    NoChange,
    /// At least one node was changed, added, or removed.
    Changed,
}

impl OptimizationResult {
    /// Returns `true` when the result is [`Changed`](OptimizationResult::Changed).
    #[inline]
    #[must_use]
    pub fn changed(self) -> bool {
        matches!(self, OptimizationResult::Changed)
    }

    /// Maps the boolean return of [`strider_ir::Graph::replace_all_uses`] to
    /// an `OptimizationResult`: `true` → `Changed`, `false` → `NoChange`.
    #[must_use]
    pub fn from_changed(changed: bool) -> Self {
        if changed {
            OptimizationResult::Changed
        } else {
            OptimizationResult::NoChange
        }
    }

    /// Replaces every use of `old` with `new`, **absorbs** the producer
    /// of `old`'s asm-fingerprint into `new`'s producer, and folds the
    /// resulting `Changed`/`NoChange` into `self`.
    ///
    /// Delegates to [`strider_ir::Function::replace_value`], the single
    /// source of truth for the fingerprint-absorb + use-redirect pair.
    ///
    /// # Errors
    ///
    /// Propagates [`strider_ir::Function::replace_value`]'s `Err` arm as
    /// a typed error rather than panicking.
    pub fn after_replace(
        self,
        function: &mut strider_pattern::RewriteCtx<'_>,
        old: strider_ir::node::NodeOutputId,
        new: strider_ir::node::NodeOutputId,
    ) -> crate::opt::Result<Self> {
        // `RewriteCtx` derefs to `Function`; `replace_value` is the SSoT that
        // absorbs `old`'s fingerprint into `new` and redirects all uses.
        let changed = function.replace_value(old, new)?;
        Ok(self | OptimizationResult::from_changed(changed))
    }
}

impl std::ops::BitOr for OptimizationResult {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        if self.changed() || rhs.changed() {
            OptimizationResult::Changed
        } else {
            OptimizationResult::NoChange
        }
    }
}

impl std::ops::BitOrAssign for OptimizationResult {
    fn bitor_assign(&mut self, rhs: Self) {
        *self = *self | rhs;
    }
}

/// Per-run context threaded through every [`Optimizer::optimize`] call.
///
/// Currently carries the optional borrowed read-only memory image
/// consumed by [`crate::opt::LoadReadOnly`] (and a future home for any
/// other per-run, pass-agnostic state).  Passes that don't need the
/// context simply ignore it (`_ctx: &OptCtx<'_>`).
///
/// Borrowed (`&dyn ReadOnlyMemory`), not `Arc`-shared: strider runs
/// single-threaded and the orchestrator owns the rom for the whole
/// run, threading it down per pipeline invocation.
pub struct OptCtx<'mem> {
    /// Borrowed read-only memory image.  `None` disables every pass
    /// gated on rom availability ([`crate::opt::LoadReadOnly`]
    /// short-circuits to `NoChange`).
    pub rom: Option<&'mem dyn strider_ir::ReadOnlyMemory>,
}

impl<'mem> OptCtx<'mem> {
    /// Construct an empty context — no rom, used by passes that need
    /// the type but no per-run state, and by callers driving the
    /// pipeline without a rom image.
    #[must_use]
    pub const fn empty() -> Self {
        Self { rom: None }
    }

    /// Construct a context carrying a borrowed rom.  Passes that need
    /// the rom (e.g. [`crate::opt::LoadReadOnly`]) read it via
    /// `ctx.rom`; passes that don't ignore the ctx.
    #[must_use]
    pub const fn with_rom(rom: &'mem dyn strider_ir::ReadOnlyMemory) -> Self {
        Self { rom: Some(rom) }
    }
}

impl Default for OptCtx<'_> {
    fn default() -> Self {
        Self::empty()
    }
}

/// A single IR optimization pass.
///
/// Implement this trait to add a new pass.  The pass receives the
/// `function` (whose entry [`strider_ir::node::NodeId`] is reachable
/// via `function.entry()`) plus an [`OptCtx`] of per-run state, applies
/// whatever transformations it can in one sweep, and returns
/// [`OptimizationResult::Changed`] if anything was modified (causing
/// the pipeline to run another iteration) or
/// [`OptimizationResult::NoChange`] if the graph is already in normal
/// form for this pass.
///
/// # Why `&mut Function` and not `&mut strider_pattern::RewriteCtx<'_>`
///
/// `RewriteCtx<'_>` carries a lifetime parameter, which prevents it
/// appearing as the receiver type of a trait object
/// (`Box<dyn Optimizer>`).  The pipeline stores type-erased passes, so
/// the trait must be object-safe with no lifetime parameter.  Pass
/// authors that want the ergonomic `RewriteCtx` API construct one
/// internally at the top of `optimize`:
///
/// ```
/// # use strider_analyze::opt::{OptCtx, OptimizationResult, Optimizer};
/// # use strider_pattern::RewriteCtx;
/// # use strider_ir::Function;
/// #[derive(Clone)]
/// struct MyPass;
/// impl Optimizer for MyPass {
///     fn optimize(
///         &self,
///         function: &mut Function,
///         _ctx: &OptCtx<'_>,
///     ) -> anyhow::Result<OptimizationResult> {
///         let _ctx = RewriteCtx::try_for_built(function)?;
///         // ... pass body operating on `_ctx` ...
///         Ok(OptimizationResult::NoChange)
///     }
/// }
/// ```
///
/// Passes that need the entry [`strider_ir::node::NodeId`] directly
/// (for `function.preorder(entry)` or
/// `strider_ir::walk::cfg_reachable(function, entry)`) derive it from
/// `function.entry().expect("Optimizer::optimize: function must be built")`
/// — the pipeline only ever runs over a built function, so the entry
/// is guaranteed to be `Some(_)`.
pub trait Optimizer: OptimizerClone {
    /// Run one sweep of this pass over the IR `function`.
    ///
    /// The function is guaranteed to be in its built form (i.e.
    /// `function.entry()` is `Some(_)`); passes that need the entry
    /// derive it via `function.entry().expect(...)`.
    ///
    /// `ctx` carries per-run state (currently the borrowed rom image);
    /// passes that don't consume the ctx ignore it (`_ctx: &OptCtx<'_>`).
    ///
    /// # Errors
    ///
    /// Returns the first error encountered by the pass — typically an IR
    /// validation failure or a pattern-rewrite error propagated up through
    /// `anyhow::Error`.
    fn optimize(
        &self,
        function: &mut strider_ir::Function,
        ctx: &OptCtx<'_>,
    ) -> crate::opt::Result<OptimizationResult>;

    /// Symbolic name of this pass.  Defaults to
    /// `std::any::type_name::<Self>()`, which yields fully-qualified
    /// paths like `strider_analyze::opt::constant_fold::ConstantFold`
    /// — sufficient for substring-match assertions in tests pinning
    /// pipeline composition.  Override only if you need a friendlier
    /// short name (and document why).
    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }
}

/// Object-safe clone shim for [`Optimizer`].
///
/// Enables external iteration over the canonical default pipelines:
/// downstream crates (e.g. `strider-py`) snapshot the pass list via
/// [`OptimizerPipeline::passes`] / [`OptimizerPipeline::post_passes`] and
/// `clone_box` each entry into their own storage, rather than
/// hand-mirroring the pass list and risking silent drift.
///
/// Every concrete `Optimizer + Clone + 'static` gets a blanket
/// `OptimizerClone` impl for free, so pass authors never write
/// `clone_box` by hand — `#[derive(Clone)]` on the pass type is
/// sufficient.  ZST passes get `Clone` via `#[derive(Clone, Copy)]`.
pub trait OptimizerClone {
    /// Clone the pass behind a `Box<dyn Optimizer>`.
    fn clone_box(&self) -> Box<dyn Optimizer>;
}

impl<T: Optimizer + Clone + 'static> OptimizerClone for T {
    fn clone_box(&self) -> Box<dyn Optimizer> {
        Box::new(self.clone())
    }
}

/// An ordered list of `Optimizer` passes that are run in a shared fixed-point
/// loop.
///
/// On each iteration every pass is called once in registration order.  The loop
/// repeats until no pass reports a change.  Use [`OptimizerPipeline::add`] to
/// register passes and [`OptimizerPipeline::run`] to execute them.
///
/// Internally the pipeline stores passes as `Box<dyn Optimizer>` so it
/// can dispatch on `(&mut Function, NodeId)` directly.
pub struct OptimizerPipeline {
    passes: Vec<Box<dyn Optimizer>>,
    post_passes: Vec<Box<dyn Optimizer>>,
}

impl Default for OptimizerPipeline {
    fn default() -> Self {
        Self::new()
    }
}

impl OptimizerPipeline {
    /// Creates an empty pipeline with no passes registered.
    #[must_use]
    pub fn new() -> Self {
        Self {
            passes: Vec::new(),
            post_passes: Vec::new(),
        }
    }

    /// Appends `opt` to the end of the pass list.
    pub fn add<O: Optimizer + 'static>(&mut self, opt: O) {
        self.passes.push(Box::new(opt));
    }

    /// Appends `opt` to the post-pass list.  Post-passes run once, in
    /// registration order, after the fixed-point loop converges.  Their return
    /// value is ignored (no re-entry into the fixed-point loop).
    pub fn add_post_pass<O: Optimizer + 'static>(&mut self, opt: O) {
        self.post_passes.push(Box::new(opt));
    }

    /// Borrow the fixed-point passes as a slice in registration order.
    ///
    /// Lets downstream crates snapshot the canonical pipeline without
    /// hand-mirroring the pass list.  Combine with the
    /// `OptimizerClone::clone_box` supertrait method to materialise an
    /// independent copy of each pass.
    #[must_use]
    pub fn passes(&self) -> &[Box<dyn Optimizer>] {
        &self.passes
    }

    /// Borrow the post-passes as a slice in registration order.  See
    /// [`OptimizerPipeline::passes`] for the use-case.
    #[must_use]
    pub fn post_passes(&self) -> &[Box<dyn Optimizer>] {
        &self.post_passes
    }

    /// Runs all registered passes in a fixed-point loop until convergence,
    /// then runs each post-pass exactly once in registration order.
    ///
    /// `function` must be in its built form (i.e. `function.entry()` is
    /// `Some(_)`); each pass derives the entry [`strider_ir::node::NodeId`]
    /// internally as needed, and the final validation step requires it.
    /// `ctx` carries per-run pass-agnostic state (currently the borrowed
    /// rom image); the orchestrator constructs one per pipeline run, ad-hoc
    /// callers use [`OptCtx::empty`].
    ///
    /// Returns `Ok(())` when no pass changed the graph in a full iteration
    /// and all post-passes completed without error.  Propagates the first
    /// error returned by any pass.
    ///
    /// # Errors
    ///
    /// Returns an error if `function.entry()` is `None` (graph not built).
    /// Otherwise, returns the first `anyhow::Error` reported by any pass.
    /// If every pass and post-pass succeeds, the graph is then re-validated
    /// and any validation error is returned.  When a post-pass returns
    /// `Err`, the final validation step is skipped — the pass error wins.
    pub fn run(
        &self,
        function: &mut strider_ir::Function,
        ctx: &OptCtx<'_>,
    ) -> crate::opt::Result<()> {
        const MAX_ITERS: u32 = 1024;
        let mut iters: u32 = 0;
        loop {
            let mut changed = false;
            for opt in &self.passes {
                if opt.optimize(function, ctx)?.changed() {
                    changed = true;
                }
            }
            if !changed {
                break;
            }
            iters += 1;
            if iters >= MAX_ITERS {
                anyhow::bail!("optimizer pipeline did not converge after {MAX_ITERS} iterations");
            }
        }
        for opt in &self.post_passes {
            opt.optimize(function, ctx)?;
        }
        let entry = function.entry().ok_or_else(|| {
            anyhow::anyhow!(
                "OptimizerPipeline::run: function must be built (entry is None)"
            )
        })?;
        strider_ir::validate::validate(function, entry)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests for [`OptimizerPipeline::run`].

    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::OptCtx;
    use strider_ir::FunctionBuilder;
    use strider_ir::node::NodeOutputType;
    use strider_ir_test_utils::SENTINEL_LIFT_ADDR;

    /// Build a tiny single-region function returning `IntConst(K)`.
    fn one_const_fn(k: u64) -> strider_ir::Function {
        let mut b = FunctionBuilder::empty().unwrap();
        let region = b.create_region().unwrap();
        b.set_entry_region(region).unwrap();
        b.set_region(region);
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let v = b.build_int_const(k, NodeOutputType::I64).unwrap();
        b.build_return(Some(v), &[]).unwrap();
        b.set_lift_addr(None);
        b.build().unwrap()
    }

    /// `run(graph, entry)` validates the final graph — an invalid graph
    /// in the post-pass output surfaces as a `ValidationErrors`-bearing
    /// `anyhow::Error` (downcastable) (downcastable via `anyhow::Error::
    /// downcast_ref::<strider_ir::validate::ValidationErrors>()`).  Smoke test
    /// using an empty post-pass list and a valid input — run must
    /// succeed (no validation error) and the graph must be unchanged.
    #[test]
    fn pipeline_run_validates_final_graph_on_clean_input() -> crate::opt::Result<()> {
        let mut function = one_const_fn(3);
        let pipeline = crate::opt::default_pipeline();
        let before = function.walk().count();
        pipeline.run(&mut function, &OptCtx::empty())?;
        let after = function.walk().count();
        // The default pipeline on an already-folded constant cannot fold
        // further; the reachable-count is stable.  This pins that
        // `run(graph, entry)` doesn't accidentally mutate the graph
        // beyond what the underlying passes produce.
        assert!(after <= before, "default pipeline must not GROW the reachable set");
        Ok(())
    }

    /// A non-monotone pass that always claims the graph changed must be
    /// caught by the pipeline's iteration cap rather than spinning
    /// forever.  Pins the divergence-guard contract on
    /// `MAX_ITERS = 1024`.
    #[test]
    fn fixed_point_limit_exceeded() {
        use super::{OptimizationResult, Optimizer, OptimizerPipeline};
        #[derive(Clone)]
        struct AlwaysChanged;
        impl Optimizer for AlwaysChanged {
            fn optimize(
                &self,
                _function: &mut strider_ir::Function,
                _ctx: &OptCtx<'_>,
            ) -> crate::opt::Result<OptimizationResult> {
                Ok(OptimizationResult::Changed)
            }
        }

        let mut function = one_const_fn(0);
        let mut pipeline = OptimizerPipeline::new();
        pipeline.add(AlwaysChanged);
        let err = pipeline
            .run(&mut function, &OptCtx::empty())
            .expect_err("pipeline must bail out on a non-monotone pass");
        assert!(
            err.to_string().contains("did not converge"),
            "expected 'did not converge' error, got {err:?}"
        );
    }

    /// `default_pipeline().run` invokes `validate` at the end on a
    /// trivial valid input — pins that the validate-on-finish step
    /// is wired and accepts a clean graph (smoke).
    #[test]
    fn run_validates_after_default_pipeline() -> crate::opt::Result<()> {
        let mut function = one_const_fn(0);
        crate::opt::default_pipeline().run(&mut function, &OptCtx::empty())?;
        Ok(())
    }

    /// `run` calls `validate` after every post-pass too — pin that a
    /// pipeline carrying a post-pass produces a graph that still
    /// validates.  Uses ConstantFold + CallStackArgCollect (post-pass)
    /// — the same plumbing the orchestrator relies on.
    #[test]
    fn run_with_post_passes_validates() -> crate::opt::Result<()> {
        use crate::opt::{CallStackArgCollect, ConstantFold, OptimizerPipeline};
        // Use a synthetic SP varnode in REGISTER space.
        let sp = rsleigh::Vn {
            addr_off: 0x20,
            addr_space: rsleigh::VnSpace::REGISTER,
            size: 4,
        };
        let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        b.build_return(None, &[])?;
        b.set_lift_addr(None);
        let mut function = b.build()?;

        let mut p = OptimizerPipeline::new();
        p.add(ConstantFold);
        p.add_post_pass(CallStackArgCollect::new(vec![0], sp));
        p.run(&mut function, &OptCtx::empty())?;
        Ok(())
    }

    /// `LoadForward` must forward a SP-relative store to the subsequent
    /// load at the same offset.
    /// Build `store sp-4 = 0x42; load sp-4` and assert the load is
    /// forwarded to `IntConst(0x42)`.  Pins the in-pipeline ordering
    /// the orchestrator depends on.
    #[test]
    fn store_then_load_at_same_offset_forwarded() -> crate::opt::Result<()> {
        use crate::opt::{
            ConstantFold, DeadBranchElimination, KnownBits, OptimizerPipeline, RedundantPhis,
            LoadForward,
        };
        use strider_ir::node::NodeKind;
        use strider_target::Endianness;

        let sp = rsleigh::Vn {
            addr_off: 0x20,
            addr_space: rsleigh::VnSpace::REGISTER,
            size: 4,
        };
        let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let sp_v = b.read_variable(&sp)?;
        let four = b.build_int_const(4u64, NodeOutputType::I32)?;
        let addr = b.build_sub_as_add_neg(sp_v, four, NodeOutputType::I32)?;
        let data = b.build_int_const(0x42u64, NodeOutputType::I32)?;
        b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::I32)?;
        b.build_return(Some(loaded), &[])?;
        b.set_lift_addr(None);
        let mut function = b.build()?;

        let mut p = OptimizerPipeline::new();
        p.add(ConstantFold);
        p.add(KnownBits);
        p.add(RedundantPhis);
        p.add(DeadBranchElimination);
        p.add(LoadForward::new(sp, Endianness::Little));
        p.run(&mut function, &OptCtx::empty())?;

        let ret = function
            .all_node_ids()
            .find(|&n| matches!(function.node_kind(n), NodeKind::Return))
            .expect("Return present");
        let val = function.node_inputs(ret)[2];
        let kind = *function.kind_of_output(val);
        assert!(
            matches!(kind, NodeKind::IntConst(0x42)),
            "load must forward to stored value, got {kind:?}"
        );
        Ok(())
    }

    /// `CallStackArgCollect` post-pass must extend a Call's input list
    /// with positional stack arg values pushed before it.
    /// Pins the orchestrator's full SP-aware pipeline.
    #[test]
    fn full_call_pipeline_collects_args() -> crate::opt::Result<()> {
        use crate::opt::{
            CallStackArgCollect, ConstantFold, DeadBranchElimination, KnownBits,
            OptimizerPipeline, RedundantPhis, LoadForward,
        };
        use strider_ir::node::NodeKind;
        use strider_target::Endianness;

        let sp = rsleigh::Vn {
            addr_off: 0x20,
            addr_space: rsleigh::VnSpace::REGISTER,
            size: 4,
        };
        let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let sp_v0 = b.read_variable(&sp)?;
        let four = b.build_int_const(4u64, NodeOutputType::I32)?;
        let sp_v1 = b.build_sub_as_add_neg(sp_v0, four, NodeOutputType::I32)?;
        b.write_variable(&sp, sp_v1)?;
        let arg1 = b.build_int_const(22u64, NodeOutputType::I32)?;
        b.build_store(sp_v1, arg1, rsleigh::VnSpace::RAM)?;
        let sp_v2 = b.build_sub_as_add_neg(sp_v1, four, NodeOutputType::I32)?;
        b.write_variable(&sp, sp_v2)?;
        let arg0 = b.build_int_const(11u64, NodeOutputType::I32)?;
        b.build_store(sp_v2, arg0, rsleigh::VnSpace::RAM)?;
        let target = b.build_int_const(0x1000u64, NodeOutputType::I32)?;
        b.build_call(target)?;
        b.build_return(None, &[])?;
        b.set_lift_addr(None);
        let mut function = b.build()?;

        let mut p = OptimizerPipeline::new();
        p.add(ConstantFold);
        p.add(KnownBits);
        p.add(RedundantPhis);
        p.add(DeadBranchElimination);
        p.add(LoadForward::new(sp, Endianness::Little));
        p.add_post_pass(CallStackArgCollect::new(vec![0, 4], sp));
        p.run(&mut function, &OptCtx::empty())?;

        let call = function
            .all_node_ids()
            .find(|&n| matches!(function.node_kind(n), NodeKind::Call))
            .expect("Call present");
        let inputs = function.node_inputs(call);
        assert_eq!(
            inputs.len(),
            5,
            "ctrl + mem + target + 2 collected args = 5 inputs"
        );
        Ok(())
    }

    /// A 50-deep chain of `Add(_, 1)` ops must reach fixed point via
    /// the default pipeline — no premature exit, no infinite loop.
    /// Pins the convergence side of the fixed-point loop.
    #[test]
    fn long_reassoc_chain_converges() -> crate::opt::Result<()> {
        use strider_ir::IntBinaryOp;
        let mut function = strider_ir_test_utils::make_empty_fn(|b| {
            let mut acc = b.build_int_const(0u64, NodeOutputType::I64)?;
            for _ in 0..50 {
                let one = b.build_int_const(1u64, NodeOutputType::I64)?;
                acc = b.build_int_binary_operation(
                    acc,
                    one,
                    IntBinaryOp::Add,
                    NodeOutputType::I64,
                )?;
            }
            Ok(acc)
        })?;
        crate::opt::default_pipeline().run(&mut function, &OptCtx::empty())?;
        // After fixed point, the 50-deep chain has folded to a single
        // `IntConst(50)`; the reachable set is small.
        assert!(
            function.walk().count() < 20,
            "50-deep chain should fold; reachable={}",
            function.walk().count()
        );
        Ok(())
    }
}
