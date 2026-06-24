/// Whether an optimization pass made any change to the IR graph.
///
/// Passes return this from `Optimizer::apply`.  The pipeline uses it to
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
    /// Delegates to [`crate::EditFunction::replace_value`], the single
    /// source of truth for the fingerprint-absorb + use-redirect pair.
    ///
    /// # Errors
    ///
    /// Propagates [`crate::EditFunction::replace_value`]'s `Err` arm as
    /// a typed error rather than panicking.
    pub fn after_replace(
        self,
        function: &mut crate::EditFunction<'_>,
        old: strider_ir::node::ValueId,
        new: strider_ir::node::ValueId,
    ) -> crate::Result<Self> {
        // `replace_value` is the SSoT that absorbs `old`'s fingerprint into
        // `new` and redirects all uses; it now lives on `EditFunction`.
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

/// Per-run, cross-pass context threaded through every [`Optimizer::apply`]
/// call.  The shared home for configuration and caches that every pass in
/// one pipeline run agrees on, so individual passes stop carrying their
/// own copies:
///
/// * `rom` — the optional borrowed read-only memory image consumed by
///   [`crate::LoadReadOnly`].
/// * `options` — the [`crate::OptOptions`] struct holding all per-run tuning
///   knobs (`alias_mode`, `calls_clobber`).  The
///   SP-aware passes ([`crate::LoadForward`], [`crate::FunctionArgDetect`],
///   [`crate::CallStackArgCollect`]) read from it; set fields on
///   `ctx.options` after constructing via [`OptCtx::new`].
/// * `sp_memo` — a shared `ValueId → SpExpr` decomposition cache reused
///   across the SP-aware passes within a run.  The pipeline clears it at
///   every drain point (graph change), so a memoised decomposition is
///   never stale across an iteration that rewrote the graph.
///
/// Passes that don't need any of this simply ignore the context
/// (`_ctx: &mut OptCtx<'_>`).
///
/// Borrowed (`&dyn ReadOnlyMemory`), not `Arc`-shared: strider runs
/// single-threaded and the orchestrator owns the rom for the whole
/// run, threading it down per pipeline invocation.
///
/// The fields are `pub`: this is the shared config bag, and callers
/// (the orchestrator, tests) set fields on `options` directly after
/// constructing via [`OptCtx::new`].
pub struct OptCtx<'mem> {
    /// Borrowed read-only memory image.  `None` disables every pass
    /// gated on rom availability ([`crate::LoadReadOnly`]
    /// short-circuits to `NoChange`).
    pub rom: Option<&'mem dyn strider_ir::ReadOnlyMemory>,
    /// All per-run tuning knobs in one place.  See [`crate::OptOptions`].
    pub options: crate::OptOptions,
    /// Shared `ValueId → SpExpr` decomposition cache.  Cleared by the
    /// pipeline at every drain point (graph change), so a memoised entry
    /// is valid within a pass and never stale across a changed iteration.
    /// Crate-internal — only the SP-aware passes touch it.
    pub(crate) sp_memo: crate::sp_expr::SpExprMemo,
    /// Output channel for the [`crate::IndirectBranchClassify`] post-pass:
    /// maps each **live** `IndirectBranch` placeholder the pass visited to
    /// its classification (`Some` when the dispatch target was recovered,
    /// `None` when it remains unresolvable this iteration).  Keyed by the
    /// placeholder's [`strider_ir::node::NodeId`] — the orchestrator joins
    /// these back to the dispatch pcode address via the correlation it
    /// recorded at lift time.  Empty until the pass runs; the orchestrator
    /// drains it after `OptimizerPipeline::run` returns.  Dead placeholders
    /// (pruned by the node-removing passes) never appear here, so a branch
    /// optimisation proved unreachable is silently dropped rather than
    /// reported unresolved.
    pub indirect_resolutions:
        rustc_hash::FxHashMap<strider_ir::node::NodeId, Option<strider_cfg::ResolvedTargets>>,
}

impl<'mem> OptCtx<'mem> {
    /// Construct an optimization context with an optional read-only-memory
    /// image (the rom the `LoadReadOnly` / indirect-branch passes fold against).
    /// Callers that already hold an `Option<&dyn ReadOnlyMemory>` pass it straight
    /// through.
    #[must_use]
    pub fn new(rom: Option<&'mem dyn strider_ir::ReadOnlyMemory>) -> Self {
        Self {
            rom,
            options: crate::OptOptions::default(),
            sp_memo: crate::sp_expr::SpExprMemo::default(),
            indirect_resolutions: rustc_hash::FxHashMap::default(),
        }
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
/// # Why `apply(&mut EditFunction)` is the only entry point
///
/// The pipeline runs many passes over one function per run.  Each pass
/// mutates the IR through a [`crate::EditFunction`], so building
/// a fresh ctx inside every pass would reconstruct the same wrapper
/// once per pass per fixed-point iteration.  Instead the pipeline builds
/// **one** self-cleaning `EditFunction` for the whole run and hands it to
/// every pass via [`Optimizer::apply`] — the single entry point.
///
/// One-off callers (tests, benches) that hold a `&mut Function` and want
/// to run a single pass use the [`crate::run_one`] helper, which builds a
/// throwaway [`crate::EditFunction`] (populate → cull → `apply` → drain) for
/// that function.
///
/// `EditFunction<'_>` carries a lifetime parameter, which would prevent it
/// appearing as the receiver type of a trait object
/// (`Box<dyn Optimizer>`).  The pipeline stores type-erased passes, so
/// the trait itself must stay object-safe with no lifetime parameter on
/// `Self`.  `apply` keeps the trait object-safe by late-binding the ctx
/// lifetime on the method (`EditFunction<'_>`) rather than on the trait.
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
/// Passes that need the entry [`strider_ir::node::NodeId`] directly
/// (for `edit.walk()` or
/// `strider_ir::walk::cfg_reachable(edit.graph_ref(), edit.entry())`)
/// derive it via `edit.entry()` — `EditFunction::new` enforces
/// the post-build invariant, so the entry is guaranteed `Some(_)`.
pub trait Optimizer: OptimizerClone {
    /// Real entry point: passes mutate the function through the shared
    /// `EditFunction` the pipeline built once for this run.
    ///
    /// `edit` wraps the built function (entry is `Some(_)`); passes
    /// mutate through `edit`'s curated mutation-façade methods
    /// (`create_node`, `update_input`, `set_stack_offset`, …) and read
    /// through `edit`'s deref to `Function` / `Graph`.
    ///
    /// `ctx` carries per-run state (currently the borrowed rom image);
    /// passes that don't consume the ctx ignore it (`_ctx: &mut OptCtx<'_>`).
    ///
    /// # Errors
    ///
    /// Returns the first error encountered by the pass — typically an IR
    /// validation failure or a pattern-rewrite error propagated up through
    /// `anyhow::Error`.
    fn apply(
        &self,
        edit: &mut crate::EditFunction<'_>,
        ctx: &mut OptCtx<'_>,
    ) -> crate::Result<OptimizationResult>;
}

/// Run a single pass against `function` through a throwaway self-cleaning
/// [`crate::EditFunction`] — the one-off replacement for the (removed)
/// `Optimizer::optimize` default.
///
/// Constructs a [`crate::EditFunction::new`], runs the initial dead-node cull
/// via [`crate::EditFunction::cull_dead`], calls
/// [`Optimizer::apply`], then drains the maybe-dead queue
/// ([`crate::EditFunction::clean`]) so the post-run graph reflects the same
/// eager cull the pipeline applies.  Direct/one-off callers (tests, benches)
/// that hold a `&mut Function` use this; the pipeline shares one ctx across
/// all passes and never calls it.
///
/// `function` must be in its built form (`function.entry()` is `Some(_)`).
///
/// # Errors
///
/// Returns an error if `function` has not been built (no entry node — the
/// built invariant `EditFunction::new` enforces), or the first error returned
/// by [`Optimizer::apply`].
pub fn run_one(
    pass: &dyn Optimizer,
    function: &mut strider_ir::Function,
    octx: &mut OptCtx<'_>,
) -> crate::Result<OptimizationResult> {
    let mut edit = crate::EditFunction::new(function)?;
    edit.cull_dead();
    let result = pass.apply(&mut edit, octx)?;
    edit.clean();
    Ok(result)
}

/// Test-only ergonomic shim mirroring the removed `Optimizer::optimize`:
/// `pass.run_one(&mut fg, &mut octx)` delegates to the free [`run_one`].
#[cfg(test)]
pub(crate) trait OptimizerTestExt {
    /// Build a throwaway self-cleaning ctx, apply `self`, drain, and return.
    ///
    /// # Errors
    /// Propagates the first error returned by [`Optimizer::apply`].
    fn run_one(
        &self,
        function: &mut strider_ir::Function,
        octx: &mut OptCtx<'_>,
    ) -> crate::Result<OptimizationResult>;
}

#[cfg(test)]
impl<T: Optimizer> OptimizerTestExt for T {
    fn run_one(
        &self,
        function: &mut strider_ir::Function,
        octx: &mut OptCtx<'_>,
    ) -> crate::Result<OptimizationResult> {
        run_one(self, function, octx)
    }
}

/// Run a single [`PostOptimizer`] against `function` through a throwaway
/// self-cleaning [`crate::EditFunction`] — the post-pass sibling of
/// [`run_one`].  Post-passes return no `Change`/`NoChange`, so this yields
/// `crate::Result<()>`.  Test-only: the production driver is
/// [`OptimizerPipeline::run`]'s post-pass loop.
///
/// `function` must be in its built form (`function.entry()` is `Some(_)`).
///
/// # Errors
///
/// Returns an error if `function` has not been built, or the first error
/// returned by [`PostOptimizer::apply`].
pub fn run_post(
    pass: &dyn PostOptimizer,
    function: &mut strider_ir::Function,
    octx: &mut OptCtx<'_>,
) -> crate::Result<()> {
    let mut edit = crate::EditFunction::new(function)?;
    edit.cull_dead();
    pass.apply(&mut edit, octx)?;
    edit.clean();
    Ok(())
}

/// Test-only ergonomic shim for [`PostOptimizer`], mirroring
/// [`OptimizerTestExt`]: `pass.run_one(&mut fg, &mut octx)` delegates to the
/// free [`run_post`].
#[cfg(test)]
pub(crate) trait PostOptimizerTestExt {
    /// Build a throwaway self-cleaning ctx, apply `self`, drain, and return.
    ///
    /// # Errors
    /// Propagates the first error returned by [`PostOptimizer::apply`].
    fn run_one(
        &self,
        function: &mut strider_ir::Function,
        octx: &mut OptCtx<'_>,
    ) -> crate::Result<()>;
}

#[cfg(test)]
impl<T: PostOptimizer> PostOptimizerTestExt for T {
    fn run_one(
        &self,
        function: &mut strider_ir::Function,
        octx: &mut OptCtx<'_>,
    ) -> crate::Result<()> {
        run_post(self, function, octx)
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

/// A post-optimization pass that runs **once** after the fixed-point loop
/// converges (not iterated), so — unlike [`Optimizer`] — it returns no
/// `Change`/`NoChange`.
///
/// Post-passes are side-table-annotation / wiring passes
/// ([`crate::StackOffsetDetect`], [`crate::FunctionArgDetect`],
/// [`crate::CallStackArgCollect`], [`crate::IndirectBranchClassify`]) that
/// run on the converged graph and never feed back into the loop.  Splitting
/// them onto their own trait type-enforces "post-only": a post pass cannot
/// be registered via [`OptimizerPipeline::add`], and the absence of a
/// `Change`/`NoChange` return makes the single-shot contract explicit.
///
/// Register via [`OptimizerPipeline::add_post_pass`].
pub trait PostOptimizer: PostOptimizerClone {
    /// Runs ONCE after the fixed-point loop converges (not iterated), so it
    /// returns no Change/NoChange.
    ///
    /// `edit` wraps the converged function; `ctx` carries the same per-run
    /// state the fixed-point passes saw.
    ///
    /// # Errors
    ///
    /// Returns the first error encountered by the pass, propagated up through
    /// `anyhow::Error`.
    fn apply(&self, edit: &mut crate::EditFunction<'_>, ctx: &mut OptCtx<'_>) -> crate::Result<()>;
}

/// Object-safe clone shim for [`PostOptimizer`], mirroring [`OptimizerClone`].
///
/// Every concrete `PostOptimizer + Clone + 'static` gets this blanket impl
/// for free, so pass authors never write `clone_box` by hand —
/// `#[derive(Clone)]` on the pass type is sufficient.
pub trait PostOptimizerClone {
    /// Clone the pass behind a `Box<dyn PostOptimizer>`.
    fn clone_box(&self) -> Box<dyn PostOptimizer>;
}

impl<T: PostOptimizer + Clone + 'static> PostOptimizerClone for T {
    fn clone_box(&self) -> Box<dyn PostOptimizer> {
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
/// Internally the pipeline stores passes as `Box<dyn Optimizer>` and
/// dispatches each via `apply(&mut EditFunction, &mut OptCtx)` against the
/// shared self-cleaning `EditFunction` built once per run.
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
    /// registration order, after the fixed-point loop converges, and return
    /// no `Change`/`NoChange` (no re-entry into the fixed-point loop).
    pub fn add_post_pass<O: PostOptimizer + 'static>(&mut self, opt: O) {
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
    pub fn post_passes(&self) -> &[Box<dyn PostOptimizer>] {
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
    /// callers use [`OptCtx::new`].
    ///
    /// Returns `Ok(())` when no pass changed the graph in a full iteration
    /// and all post-passes completed without error.  Propagates the first
    /// error returned by any pass.
    ///
    /// # Errors
    ///
    /// Returns an error if the function is not built (`EditFunction::new`
    /// rejects a function whose `entry()` is `None`).
    /// Otherwise, returns the first `anyhow::Error` reported by any pass.
    /// If every pass and post-pass succeeds, the graph is then re-validated
    /// and any validation error is returned.  When a post-pass returns
    /// `Err`, the final validation step is skipped — the pass error wins.
    pub fn run(
        &self,
        function: &mut strider_ir::Function,
        ctx: &mut OptCtx<'_>,
    ) -> crate::Result<()> {
        const MAX_ITERS: u32 = 1024;
        {
            // Build ONE self-cleaning EditFunction for the whole run and share
            // it across every pass, instead of each pass reconstructing one.
            // `EditFunction::new` seeds the live/roots bookkeeping, and the
            // explicit `cull_dead()` removes any pre-existing dead nodes.  The
            // borrow of `function` (via the ctx) is held for the duration of
            // this scope and released before the final validation step below.
            let mut edit = crate::EditFunction::new(function)?;
            edit.cull_dead();
            let mut iters: u32 = 0;
            loop {
                let mut changed = false;
                for opt in &self.passes {
                    if opt.apply(&mut edit, ctx)?.changed() {
                        changed = true;
                        // Drain + reset immediately after every pass that
                        // changed the graph, so the NEXT pass in this same
                        // iteration sees a culled graph and a fresh
                        // SP-decomposition cache (a `ValueId → SpExpr` entry
                        // may now be stale — its ValueId culled or its producer
                        // rewritten).
                        edit.clean();
                        ctx.sp_memo.clear();
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
                // Post-passes run exactly once, in order, after convergence and
                // return no Change/NoChange.  Drain + reset unconditionally
                // after each one so the next post-pass sees a culled graph and
                // a fresh SP-decomposition cache (a post-pass that edited the
                // graph — e.g. CallStackArgCollect appending Call inputs — may
                // have left dead nodes or stale memoised decompositions).
                opt.apply(&mut edit, ctx)?;
                edit.clean();
                ctx.sp_memo.clear();
            }
        } // edit + state dropped here → function borrow released
        strider_ir::validate::validate(function)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests for [`OptimizerPipeline::run`].

    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::OptCtx;
    use strider_ir::IRBuilderExt;
    use strider_ir::node::ValueType;
    use strider_ir::{IRViewer, IRWalker};
    use strider_ir_test_utils::SENTINEL_LIFT_ADDR;

    /// Build a tiny single-region function returning `IntConst(K)`.
    fn one_const_fn(k: u64) -> strider_ir::Function {
        let mut b = strider_ir_test_utils::empty_builder().unwrap();
        let region = b.create_region().unwrap();
        b.set_entry_region(region).unwrap();
        b.set_region(region);
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let v = b.build_int_const(k, ValueType::I64).unwrap();
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
    fn pipeline_run_validates_final_graph_on_clean_input() -> crate::Result<()> {
        let mut function = one_const_fn(3);
        let pipeline = crate::default_pipeline();
        let before = function.walk().count();
        pipeline.run(&mut function, &mut OptCtx::new(None))?;
        let after = function.walk().count();
        // The default pipeline on an already-folded constant cannot fold
        // further; the reachable-count is stable.  This pins that
        // `run(graph, entry)` doesn't accidentally mutate the graph
        // beyond what the underlying passes produce.
        assert!(
            after <= before,
            "default pipeline must not GROW the reachable set"
        );
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

    /// `default_pipeline().run` invokes `validate` at the end on a
    /// trivial valid input — pins that the validate-on-finish step
    /// is wired and accepts a clean graph (smoke).
    #[test]
    fn run_validates_after_default_pipeline() -> crate::Result<()> {
        let mut function = one_const_fn(0);
        crate::default_pipeline().run(&mut function, &mut OptCtx::new(None))?;
        Ok(())
    }

    /// `run` calls `validate` after every post-pass too — pin that a
    /// pipeline carrying a post-pass produces a graph that still
    /// validates.  Uses ConstantFold + CallStackArgCollect (post-pass)
    /// — the same plumbing the orchestrator relies on.
    #[test]
    fn run_with_post_passes_validates() -> crate::Result<()> {
        use crate::{CallStackArgCollect, ConstantFold, OptimizerPipeline};
        // Use a synthetic SP varnode in REGISTER space.
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
        b.set_stack_args(Some(strider_target::StackArgs {
            base_offset: 0,
            increment: 8,
        }));
        let region = b.create_region()?;
        b.set_entry_region(region)?;
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

    /// `LoadForward` must forward a SP-relative store to the subsequent
    /// load at the same offset.
    /// Build `store sp-4 = 0x42; load sp-4` and assert the load is
    /// forwarded to `IntConst(0x42)`.  Pins the in-pipeline ordering
    /// the orchestrator depends on.
    #[test]
    fn store_then_load_at_same_offset_forwarded() -> crate::Result<()> {
        use crate::{
            ConstantFold, DeadBranchElimination, KnownBits, LoadForward, OptimizerPipeline,
            PhiCollapse, RegionCollapse,
        };
        use strider_ir::node::NodeKind;
        use strider_ir::IRViewer;

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
        let region = b.create_region()?;
        b.set_entry_region(region)?;
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
        p.add(LoadForward);
        p.run(&mut function, &mut OptCtx::new(None))?;

        let val = crate::test_support::return_value(function.graph())?;
        let kind = *function.kind_of_value(val);
        assert!(
            matches!(kind, NodeKind::IntConst(_))
                && function.int_const_val(val) == Some(0x42),
            "load must forward to stored value, got {kind:?} (value={:?})",
            function.int_const_val(val)
        );
        Ok(())
    }

    /// `CallStackArgCollect` post-pass must extend a Call's input list
    /// with positional stack arg values pushed before it.
    /// Pins the orchestrator's full SP-aware pipeline.
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
        let mut b = strider_ir_test_utils::builder(
            vec![sp],
            &[],
            &[sp],
            &[],
            Some(sp),
            0,
            strider_target::Endianness::Little,
        )?;
        b.set_stack_args(Some(strider_target::StackArgs {
            base_offset: 0,
            increment: 4,
        }));
        let region = b.create_region()?;
        b.set_entry_region(region)?;
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
        b.build_call(target, None)?;
        b.build_return(None, &[])?;
        b.set_lift_addr(None);
        let mut function = b.build()?;

        let mut p = OptimizerPipeline::new();
        p.add(ConstantFold::new());
        p.add(KnownBits);
        p.add(PhiCollapse);
        p.add(RegionCollapse);
        p.add(DeadBranchElimination);
        p.add(LoadForward);
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

    /// A 50-deep chain of `Add(_, 1)` ops must reach fixed point via
    /// the default pipeline — no premature exit, no infinite loop.
    /// Pins the convergence side of the fixed-point loop.
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
