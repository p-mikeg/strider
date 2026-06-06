//! [`IndirectBranchClassify`] — the post-optimization analysis pass that
//! classifies every live `IndirectBranch` placeholder.
//!
//! Runs once, after the optimizer pipeline has converged, over the
//! fully-optimised IR.  It is **analysis-only**: it never mutates the
//! graph (always returns [`OptimizationResult::NoChange`]).  Its output
//! is written to [`OptCtx::indirect_resolutions`] for the orchestrator to
//! drain after [`crate::OptimizerPipeline::run`] returns.
//!
//! ## Why post-optimization
//!
//! An `IndirectBranch`'s dispatch value is opaque at lift time (just
//! "whatever's in the register").  It only becomes classifiable once the
//! optimizer has folded it into a recognizable shape — a `LoadReadOnly`
//! /`ConstantFold` jump table, a `LoadForward`-resolved constant, an
//! `InitialVar(lr)` after `PhiCollapse`.  So the rewrites *are* the
//! resolution mechanism, and the classifier must run on their output.
//!
//! ## Why walk live nodes
//!
//! The pass reads each placeholder's **current** slot-2 input straight
//! from the live graph, so it never inspects a value the optimizer's
//! `replace_all_uses` rewires orphaned away.  Walking from the entry also
//! means a placeholder the node-removing passes proved unreachable simply
//! isn't visited — a dead indirect branch needs no resolution and is
//! silently dropped rather than reported unresolved.

use crate::pipeline::{OptCtx, OptimizationResult, Optimizer};
use crate::EditFunction;
use strider_ir::node::NodeKind;
use strider_ir::{IRViewer, IRWalker};

/// Post-optimization pass that classifies live `IndirectBranch`
/// placeholders into [`OptCtx::indirect_resolutions`].
///
/// Add it as a **post-pass** (`OptimizerPipeline::add_post_pass`) so it
/// runs once on the converged graph.
#[derive(Clone, Default)]
pub struct IndirectBranchClassify;

impl IndirectBranchClassify {
    /// Construct the pass.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Optimizer for IndirectBranchClassify {
    fn apply(
        &self,
        rctx: &mut EditFunction<'_>,
        ctx: &mut OptCtx<'_>,
    ) -> crate::Result<OptimizationResult> {
        let function = rctx.function();

        // Dominator-scoped value ranges, computed once for every anchor —
        // the graph doesn't change during this analysis-only pass.  The
        // classifier reads every other input (link-register / stack-pointer
        // varnodes, endianness) off the function itself.
        let known = crate::known_bits::analyze(function)?;
        let doms = strider_ir::control_dominators(function);
        let ranges = crate::value_range::compute_value_ranges(function, &doms, &known);

        let mut resolutions = Vec::new();
        for node in function.walk() {
            if !matches!(function.node_kind(node), NodeKind::IndirectBranch) {
                continue;
            }
            // Slot layout `[control, memory, target]` — slot 2 is the live
            // dispatch value the placeholder currently points at.
            let [_, _, anchor] = function.node_inputs_exact::<3>(node)?;
            let resolved = crate::classify_anchor(function, anchor, ctx.rom, &ranges);
            resolutions.push((node, resolved));
        }
        ctx.indirect_resolutions = resolutions;

        Ok(OptimizationResult::NoChange)
    }
}
