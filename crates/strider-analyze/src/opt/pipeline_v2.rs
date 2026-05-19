//! v2 optimizer pipeline — interleaved destructive + nondestructive
//! fixed-point loop.  Phase 3 Task 3.8.
//!
//! # Design
//!
//! v1 splits the optimizer pipeline into two halves: a **stable** half
//! that the strider orchestrator runs in a fixed-point loop while the
//! IR graph is still growing under indirect-branch resolution, and a
//! **destructive** half that runs exactly once at the orchestrator's
//! exit (node-removal passes invalidate the per-iteration
//! `RegionIndex` if run mid-iteration).
//!
//! v2 replaces that split with a **single interleaved fixed-point
//! loop** that runs the egg-based value-slice rewrites and the
//! imperative control-simplification passes (`RedundantPhis`,
//! `DeadBranchElimination`) together.  Destructive cleanup unlocks
//! new nondestructive opportunities (e.g. `DeadBranchElimination`
//! removes an `If(true)` → `RedundantPhis` collapses now-single-pred
//! `VarPhi` → if the survivor is `IntConst`, `ConstantFoldEgg`
//! propagates further).
//!
//! Pipeline shape (matches v1's
//! [`crate::opt::default_pipeline`] +
//! [`crate::Strider::build_optimizer_pipeline`] composition,
//! lifted to egg-based passes):
//!
//! Inner loop (interleaved; runs until quiescence):
//! 1. [`ConstantFoldEgg`](crate::opt::constant_fold_egg::ConstantFoldEgg)
//! 2. [`KnownBitsEgg`](crate::opt::known_bits_egg::KnownBitsEgg)
//! 3. [`FlagCmpCanonicalizeEgg`](crate::opt::flag_cmp_canonicalize_egg::FlagCmpCanonicalizeEgg)
//! 4. [`IfCondInversionEgg`](crate::opt::if_cond_inversion_egg::IfCondInversionEgg)
//! 5. [`StackStoreDetectEgg`](crate::opt::stack_store_detect_egg::StackStoreDetectEgg)
//! 6. [`StackLoadForwardEgg`](crate::opt::stack_load_forward_egg::StackLoadForwardEgg)
//! 7. Optional [`LoadReadOnlyEgg`](crate::opt::load_readonly_egg::LoadReadOnlyEgg)
//!    when a ROM image is supplied.
//! 8. [`RedundantPhis`](crate::opt::RedundantPhis) (imperative — Phase 3
//!    deferred its egg port).
//! 9. [`DeadBranchElimination`](crate::opt::DeadBranchElimination)
//!    (imperative — Phase 3 deferred its egg port).
//!
//! Post-passes (run once after the loop converges):
//! - [`CallStackArgCollectEgg`](crate::opt::call_stack_arg_collect_egg::CallStackArgCollectEgg)
//! - [`FunctionArgDetectEgg`](crate::opt::function_arg_detect_egg::FunctionArgDetectEgg)
//!
//! # Why a single loop and not an `OptimizerPipeline`?
//!
//! The pre-existing [`crate::opt::OptimizerPipeline`] would in
//! principle work — every pass implements `OptimizerRaw`.  The reason
//! to author a dedicated `PipelineV2` is observability:
//!
//! * The fixed-point loop counter (`iters_to_convergence`) is exposed
//!   so the parity test can report average iterations across the 5
//!   representative fixtures.
//! * Future Phase 3.9 work will add an option to skip post-passes
//!   (e.g. for IR-only diagnostics), and `PipelineV2::run_with_options`
//!   is the natural seam.
//!
//! For now the pipeline shape matches v1 1-to-1, and v1 stays
//! untouched.  Production code still calls
//! [`crate::Strider::build_optimizer_pipeline`].  See Phase 3.9 / 6 for
//! the wire-into-production tasks.

use std::sync::Arc;

use strider_ir::node::NodeId;
use strider_ir::ReadOnlyMemory;
use target::{BuiltCallingConvention, Endianness};

use crate::opt::call_stack_arg_collect_egg::CallStackArgCollectEgg;
use crate::opt::constant_fold_egg::ConstantFoldEgg;
use crate::opt::dead_branch::DeadBranchElimination;
use crate::opt::flag_cmp_canonicalize_egg::FlagCmpCanonicalizeEgg;
use crate::opt::function_arg_detect_egg::FunctionArgDetectEgg;
use crate::opt::if_cond_inversion_egg::IfCondInversionEgg;
use crate::opt::known_bits_egg::KnownBitsEgg;
use crate::opt::load_readonly_egg::LoadReadOnlyEgg;
use crate::opt::pipeline::OptimizerRaw;
use crate::opt::redundant_phis::RedundantPhis;
use crate::opt::stack_load_forward_egg::StackLoadForwardEgg;
use crate::opt::stack_store_detect_egg::StackStoreDetectEgg;

/// Hard cap on the v2 outer loop iterations.  Matches
/// [`crate::opt::OptimizerPipeline::run`]'s `MAX_ITERS` for parity —
/// v1's pipeline has the same defence-in-depth bound.  A loop hitting
/// this cap is a real bug (rewrites un-do each other); the caller
/// surfaces an error rather than spinning forever.
const MAX_ITERS: u32 = 1024;

/// v2 optimizer pipeline.  See module-level docs for the algorithm.
///
/// Construct via [`PipelineV2::new`] (no ROM) or
/// [`PipelineV2::with_rom`] (with `LoadReadOnlyEgg` enabled).
pub struct PipelineV2 {
    constant_fold: ConstantFoldEgg,
    known_bits: KnownBitsEgg,
    flag_cmp: FlagCmpCanonicalizeEgg,
    if_cond_inv: IfCondInversionEgg,
    stack_store: StackStoreDetectEgg,
    stack_load: StackLoadForwardEgg,
    /// `Some(_)` when a ROM image was supplied at construction time;
    /// `None` otherwise.  The ROM erases its concrete `M` parameter
    /// via [`Arc<dyn ReadOnlyMemory>`] so the pipeline doesn't have to
    /// be generic over the ROM type — strider-py and the example both
    /// supply different concrete `M`s, and only the dyn-safe surface
    /// matters here.
    load_readonly: Option<LoadReadOnlyEgg<Arc<dyn ReadOnlyMemory>>>,
    /// Imperative control-simplification: collapses single-pred phis
    /// and ControlState joins.  Stays imperative — Phase 3 didn't
    /// port it to egg (control nodes live outside the value slice).
    redundant_phis: RedundantPhis,
    /// Imperative `If(const)` stripper.  Same reason as
    /// `redundant_phis` — control rewrite, not in the egraph.
    dead_branch: DeadBranchElimination,
    /// Post-passes (run once after the inner loop converges).
    call_stack_arg_collect: CallStackArgCollectEgg,
    function_arg_detect: FunctionArgDetectEgg,
}

impl PipelineV2 {
    /// Builds a v2 pipeline configured for `cc` and `endianness` but
    /// without a ROM image.  Loads from constant ROM addresses are
    /// left as-is.
    #[must_use]
    pub fn new(cc: &BuiltCallingConvention, endianness: Endianness) -> Self {
        Self {
            constant_fold: ConstantFoldEgg::new(),
            known_bits: KnownBitsEgg::new(),
            flag_cmp: FlagCmpCanonicalizeEgg::new(),
            if_cond_inv: IfCondInversionEgg::new(),
            stack_store: StackStoreDetectEgg::new(cc.stack_ptr_vn()),
            stack_load: StackLoadForwardEgg::new(cc.stack_ptr_vn(), endianness),
            load_readonly: None,
            redundant_phis: RedundantPhis,
            dead_branch: DeadBranchElimination,
            call_stack_arg_collect: CallStackArgCollectEgg::from_convention(cc),
            function_arg_detect: FunctionArgDetectEgg::from_convention(cc),
        }
    }

    /// Builds a v2 pipeline with a ROM image.  `LoadReadOnlyEgg` is
    /// enabled and runs inside the fixed-point loop.  This mirrors how
    /// the example layers `LoadReadOnly` on top of
    /// [`crate::Strider::build_optimizer_pipeline`].
    #[must_use]
    pub fn with_rom(
        cc: &BuiltCallingConvention,
        endianness: Endianness,
        rom: Arc<dyn ReadOnlyMemory>,
    ) -> Self {
        let mut p = Self::new(cc, endianness);
        p.load_readonly = Some(LoadReadOnlyEgg::new(rom));
        p
    }

    /// Runs the v2 optimizer to convergence + post-passes + final
    /// IR validation.
    ///
    /// Returns the number of outer-loop iterations to convergence
    /// (informational — the parity test reports the average).
    ///
    /// # Errors
    ///
    /// * Propagates any pass error from
    ///   [`OptimizerRaw::optimize_raw`].
    /// * Returns an error if the loop hits [`MAX_ITERS`] without
    ///   converging.
    /// * Returns the final-graph validation error (same as v1's
    ///   [`crate::opt::OptimizerPipeline::run`] does).
    pub fn run(
        &self,
        graph: &mut strider_ir::Graph,
        entry: NodeId,
    ) -> crate::opt::Result<u32> {
        let mut iters: u32 = 0;
        loop {
            let mut changed = false;

            // Value-slice egg passes — each one independently
            // re-snapshots the egraph from the current graph state.
            changed |= self.constant_fold.optimize_raw(graph, entry)?.changed();
            changed |= self.known_bits.optimize_raw(graph, entry)?.changed();
            changed |= self.flag_cmp.optimize_raw(graph, entry)?.changed();
            changed |= self.if_cond_inv.optimize_raw(graph, entry)?.changed();
            changed |= self.stack_store.optimize_raw(graph, entry)?.changed();
            changed |= self.stack_load.optimize_raw(graph, entry)?.changed();
            if let Some(ref load_ro) = self.load_readonly {
                changed |= load_ro.optimize_raw(graph, entry)?.changed();
            }

            // Imperative control-simplification (destructive).
            changed |= self.redundant_phis.optimize_raw(graph, entry)?.changed();
            changed |= self.dead_branch.optimize_raw(graph, entry)?.changed();

            if !changed {
                break;
            }
            iters += 1;
            if iters >= MAX_ITERS {
                anyhow::bail!(
                    "PipelineV2 did not converge after {MAX_ITERS} iterations"
                );
            }
        }

        // Post-passes — run once, in registration order.
        self.call_stack_arg_collect.optimize_raw(graph, entry)?;
        self.function_arg_detect.optimize_raw(graph, entry)?;

        // Final IR validation — same end-of-pipeline check v1 does.
        strider_ir::validate::validate(graph, entry)?;
        Ok(iters)
    }

    /// Convenience: runs on a [`strider_ir::BuiltFunctionGraph`].
    /// Mirrors [`crate::opt::OptimizerPipeline::run_on_built`].
    ///
    /// # Errors
    ///
    /// Propagates [`Self::run`].
    pub fn run_on_built(
        &self,
        function: &mut strider_ir::BuiltFunctionGraph,
    ) -> crate::opt::Result<u32> {
        let entry = function.entry;
        self.run(&mut function.graph, entry)
    }
}

#[cfg(test)]
mod tests {
    //! White-box smoke tests.  The end-to-end parity test against v1
    //! lives in `crates/strider-analyze/tests/pipeline_v2_parity.rs`.

    use super::*;
    use strider_ir::node::{NodeKind, NodeOutputType};
    use strider_ir::test_utils::SENTINEL_LIFT_ADDR;
    use strider_ir::{FunctionBuilder, IntBinaryOp};

    fn cc_x86_64() -> target::BuiltCallingConvention {
        let arch = target::SleighArch::x86_64();
        let regs = arch.probe_regs().expect("probe regs");
        target::CallingConvention::x86_64_systemv()
            .build(&regs)
            .expect("build cc")
    }

    #[test]
    fn smoke_run_on_constant_add() {
        let mut fg = {
            let mut b = FunctionBuilder::empty().expect("empty fb");
            let region = b.create_region().expect("region");
            b.set_entry_region(region).expect("set entry");
            b.set_region(region);
            b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
            let a = b
                .build_int_const(3u64, NodeOutputType::U64)
                .expect("const a");
            let bb = b
                .build_int_const(4u64, NodeOutputType::U64)
                .expect("const b");
            let sum = b
                .build_int_binary_operation(a, bb, IntBinaryOp::Add, NodeOutputType::U64)
                .expect("add");
            b.build_return(Some(sum), &[]).expect("return");
            b.set_lift_addr(None);
            b.build().expect("build")
        };

        let cc = cc_x86_64();
        let pipeline = PipelineV2::new(&cc, Endianness::Little);
        let iters = pipeline.run_on_built(&mut fg).expect("run");

        // Constant folding converges in 1-2 outer iterations.
        assert!(iters >= 1, "expected at least one iteration");
        assert!(iters < 16, "constant fold should not need many iters");

        // Result should be a single IntConst(7) return.
        let ret = fg
            .preorder()
            .find(|nid| matches!(fg.graph.node_kind(*nid), NodeKind::Return))
            .expect("Return node");
        let ret_inputs = fg.graph.node_inputs(ret);
        // Return inputs: [Control, Memory, ...ret_values].
        let val_out = ret_inputs[2];
        let val_producer = fg.graph.get_node_from_output(val_out);
        assert_eq!(*fg.graph.node_kind(val_producer), NodeKind::IntConst(7));
    }

    #[test]
    fn empty_graph_converges_quickly() {
        // Smallest possible function: a single Return with no value.
        // `RedundantPhis` may still fire on the entry boundary's phi
        // nodes (collapsing single-pred VarPhis), so we don't pin
        // iters == 0 — only that convergence happens fast.
        let mut fg = {
            let mut b = FunctionBuilder::empty().expect("empty fb");
            let region = b.create_region().expect("region");
            b.set_entry_region(region).expect("set entry");
            b.set_region(region);
            b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
            b.build_return(None, &[]).expect("return");
            b.set_lift_addr(None);
            b.build().expect("build")
        };
        let cc = cc_x86_64();
        let pipeline = PipelineV2::new(&cc, Endianness::Little);
        let iters = pipeline.run_on_built(&mut fg).expect("run");
        assert!(iters < 8, "trivial function should converge fast, got {iters}");
    }
}
