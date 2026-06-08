//! IR-level indirect-branch resolver.
//!
//! Classifies placeholder anchors that the strider lifter inserts at
//! `BranchIndirect` sites.  The strider orchestrator drives the outer loop
//! (CFG rebuild, cache invalidation, iteration cap) and calls into the
//! classifier functions directly — there is no opt-pipeline pass for
//! indirect-branch resolution.
//!
//! ## Producer-shape classifier
//!
//! [`classify_anchor`] recognises the dispatch value feeding a placeholder
//! anchor as one of an ordered list of sound shapes, each a named
//! recogniser returning `Option<ResolvedTargets>`:
//!
//!   * `single_const_target` — the dispatch value is a literal address
//!     (`IntConst`); the branch goes to exactly one place.
//!   * `link_register_return` — the dispatch value is the function-entry
//!     value of the link register (`InitialVar(lr)`); the branch is a
//!     return.
//!   * [`table::classify_table_dispatch`] — the dispatch value is an
//!     indexed table load (rodata jump table or on-stack label array).
//!
//! The first recogniser that matches wins; if none does, the anchor stays
//! unresolved (the orchestrator retries next iteration or surfaces
//! `UnresolvedIndirectBranch` at fixed point).
//!
//! The ABI facts each recogniser needs — the link-register and
//! stack-pointer varnodes, the target endianness — are read straight off
//! the [`strider_ir::Function`] (`default_cc()` / `endianness()`), so the
//! only external input is the optional [`ReadOnlyMemory`] image (rodata)
//! and the precomputed value-range map.
//!
//! [`IndirectBranchClassify`] is the optimizer post-pass that drives this
//! classifier: it runs once on the converged graph, walks every live
//! `IndirectBranch` placeholder, and writes the per-placeholder
//! classification into [`OptCtx::indirect_resolutions`] for the
//! orchestrator to drain.
//!
//! ## Submodules
//!
//! - [`table`] — unified table-dispatch arm covering both the rodata
//!   jump-table (absolute base) and on-stack label-array (SP-rooted base)
//!   shapes ([`classify_table_dispatch`]).
//!
//! ## Where `ResolvedTargets` lives
//!
//! Defined in `strider_cfg::indirect_resolver` (the
//! lowest layer that needs the enum: the cfg builder consumes it via
//! `LiftOptions::known_targets` to seat indirect-branch terminators, and
//! it is the return type of [`classify_anchor`] itself).  Import it directly
//! from there.

#![allow(clippy::module_name_repetitions)]

use strider_ir::IRViewer;
use strider_ir::IRWalker;
use strider_ir::node::{NodeKind, ValueId};

use strider_cfg::ResolvedTargets;

use crate::EditFunction;
use crate::ReadOnlyMemory;
use crate::pipeline::{OptCtx, PostOptimizer};

pub mod table;

/// Per-anchor enumeration cap for the table-dispatch arm
/// (`table::classify_table_dispatch`), covering both the rodata jump-table
/// (absolute base) and on-stack label-array (SP-rooted base) shapes.
///
/// `u32::MAX + 1` if a known-bits mask were all-ones, so without this cap
/// a buggy KnownBits result could force iteration through 4 GiB of slots.
/// Real jump tables emitted by gcc/clang are bounded by the source-level
/// `switch` arm count, almost always well under 4096.  Tables larger than
/// this cap are unusual enough that we prefer `None` (defer to
/// `UnresolvedIndirectBranch`) over the pathological enumeration cost.
pub(crate) const MAX_TABLE_ENTRIES: u64 = 4096;

pub use table::classify_table_dispatch;

/// Classify a placeholder anchor's dispatch value into a
/// [`ResolvedTargets`], or `None` when it matches no known sound shape
/// (the orchestrator then defers the branch).
///
/// `rom` is the binary's read-only image, consulted only by the rodata
/// jump-table shape (`None` disables it).  `ranges` is the precomputed
/// dominator-scoped value-range map.  Every other input the recognisers
/// need (link-register / stack-pointer varnodes, endianness) is read off
/// `ctx`.
///
/// # Soundness
///
/// Every recogniser must identify the branch's runtime target set
/// **unambiguously** on the optimised IR.  Shapes the prior in-place
/// heuristic tried (`Load(InitialVar(sp))` for `pop pc`-style returns) are
/// deliberately excluded: a `push X; pop pc` tail call has the same
/// Load-shape and would be misclassified as a return.  We rely on
/// `LoadForward` having simplified a properly-popped return address to
/// `InitialVar(lr)` directly — the shape `link_register_return` matches.
/// Every recogniser fails closed (`None`) on any partial proof, never
/// under-approximating the target set.
#[must_use]
pub fn classify_anchor(
    ctx: &strider_ir::Function,
    anchor_value: ValueId,
    rom: Option<&dyn ReadOnlyMemory>,
    ranges: &crate::value_range::RangeMap<'_>,
) -> Option<ResolvedTargets> {
    single_const_target(ctx, anchor_value)
        .or_else(|| link_register_return(ctx, anchor_value))
        .or_else(|| table::classify_table_dispatch(ctx, anchor_value, rom, ranges))
}

/// Recognise a single constant dispatch target: the anchor's producer is a
/// literal `IntConst(k)`, so the branch goes to exactly `k`.
///
/// SOUND: a literal constant in the IR comes from a tracked `IntConst`
/// pcode insn, `ConstantFold`, or a `LoadReadOnly` rodata resolution — all
/// deterministic functions of the function's pcode, so `k` is the only
/// possible runtime target.  Wide constants (high 64 bits set) are never
/// valid targets on a 64-bit ISA — defer rather than truncate to a wrong
/// address.
fn single_const_target(
    ctx: &strider_ir::Function,
    anchor_value: ValueId,
) -> Option<ResolvedTargets> {
    if !matches!(ctx.node_kind(ctx.producer(anchor_value)), NodeKind::IntConst(_)) {
        return None;
    }
    let k = ctx.int_const_u128(anchor_value)?;
    // A const whose high 64 bits are set is never a valid jump target on a
    // 64-bit ISA; `try_from` rejects it rather than silently truncating.
    Some(ResolvedTargets::Single(u64::try_from(k).ok()?))
}

/// Recognise a return-via-link-register: the anchor's producer is
/// `InitialVar(lr)`, the function-entry value of the calling convention's
/// link register, so the branch dispatches to the caller-provided return
/// address.
///
/// `None` on stack-push ABIs (x86 / x86_64) where `default_cc()` has no
/// link register — there can be no LR match without one.  This is the
/// shape `LoadForward` produces for a properly-popped return address.
fn link_register_return(
    ctx: &strider_ir::Function,
    anchor_value: ValueId,
) -> Option<ResolvedTargets> {
    let lr = ctx.default_cc().link_register_vn?;
    match *ctx.node_kind(ctx.producer(anchor_value)) {
        NodeKind::InitialVar(vn) if vn == lr => Some(ResolvedTargets::LinkRegister),
        _ => None,
    }
}

/// The post-optimization analysis pass that drives [`classify_anchor`]
/// over every live `IndirectBranch` placeholder.
///
/// Add it as a **post-pass** (`OptimizerPipeline::add_post_pass`) so it
/// runs once on the converged graph.  It is **analysis-only** — it never
/// mutates the graph — and writes its output to
/// [`OptCtx::indirect_resolutions`] for the orchestrator to drain after
/// [`crate::OptimizerPipeline::run`] returns.
///
/// ## Why post-optimization
///
/// An `IndirectBranch`'s dispatch value is opaque at lift time (just
/// "whatever's in the register").  It only becomes classifiable once the
/// optimizer has folded it into a recognizable shape — a `LoadReadOnly` /
/// `ConstantFold` jump table, a `LoadForward`-resolved constant, an
/// `InitialVar(lr)` after `PhiCollapse`.  So the rewrites *are* the
/// resolution mechanism, and the classifier must run on their output.
///
/// ## Why walk live nodes
///
/// The pass reads each placeholder's **current** slot-2 input straight
/// from the live graph, so it never inspects a value the optimizer's
/// `replace_all_uses` rewired orphaned away.  Walking from the entry also
/// means a placeholder the node-removing passes proved unreachable simply
/// isn't visited — a dead indirect branch needs no resolution and is
/// silently dropped rather than reported unresolved.
#[derive(Clone)]
pub struct IndirectBranchClassify;

impl PostOptimizer for IndirectBranchClassify {
    fn apply(&self, edit: &mut EditFunction<'_>, ctx: &mut OptCtx<'_>) -> crate::Result<()> {
        let function = edit.function();

        // Dominator-scoped value ranges, computed once for every anchor —
        // the graph doesn't change during this analysis-only pass.  The
        // classifier reads every other input (link-register / stack-pointer
        // varnodes, endianness) off the function itself.
        let known = crate::known_bits::analyze(function)?;
        let doms = strider_ir::control_dominators(function);
        let ranges = crate::value_range::compute_value_ranges(function, &doms, &known);

        let mut resolutions: rustc_hash::FxHashMap<_, _> = rustc_hash::FxHashMap::default();
        for node in function.walk() {
            if !matches!(function.node_kind(node), NodeKind::IndirectBranch) {
                continue;
            }
            // Slot layout `[control, memory, target]` — slot 2 is the live
            // dispatch value the placeholder currently points at.  The walk
            // visits each node once, so every key is unique.
            let [_, _, anchor] = function.node_inputs_exact::<3>(node)?;
            let resolved = classify_anchor(function, anchor, ctx.rom, &ranges);
            resolutions.insert(node, resolved);
        }
        ctx.indirect_resolutions = resolutions;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests for [`classify_anchor`].
    //!
    //! Each test constructs a minimal [`strider_ir::Graph`]
    //! via [`strider_ir::FunctionBuilder::new_raw`], appends nodes
    //! directly via `graph.create_node` to control the producer shape
    //! exactly, and then invokes the classifier on the targeted output.
    //! These tests intentionally bypass the strider IR-lift path so the
    //! classifier's match arms are exercised in isolation without
    //! depending on the optimiser pipeline producing the expected
    //! shape.

    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use strider_ir::FunctionBuilder;
    use strider_ir::IRBuilderExt;
    use strider_ir::IRViewer;
    use strider_ir::node::{NodeKind, ValueType};
    use strider_ir_test_utils::{RegisterSet, SENTINEL_LIFT_ADDR, reg_vn as fake_reg_vn};

    /// Unit-test convenience: computes the range analysis and
    /// calls [`classify_anchor`] with no rom.  The
    /// integration-style tests in `tests/indirect_resolve_classify.rs`
    /// drive the rom/SP arms; these unit tests only exercise the
    /// IntConst / InitialVar / Load-fallthrough arms.
    fn classify_anchor_bare(
        ctx: &strider_ir::Function,
        anchor_value: ValueId,
    ) -> anyhow::Result<Option<ResolvedTargets>> {
        let known = crate::analyze_known_bits(ctx)?;
        let doms = strider_ir::control_dominators(ctx);
        let ranges = crate::value_range::compute_value_ranges(ctx, &doms, &known);
        Ok(classify_anchor(ctx, anchor_value, None, &ranges))
    }

    /// Build a minimal `Graph` with one tracked
    /// variable and an empty body region terminated by a Return
    /// whose single value-input is the caller-supplied
    /// `ValueId`.  Used as a scaffold for the unit tests so
    /// the classifier sees a real, validation-passing graph.
    fn empty_graph_returning(
        anchor_inputs: impl FnOnce(&mut FunctionBuilder) -> ValueId,
    ) -> (strider_ir::Function, ValueId) {
        // No tracked variables, no calling convention plumbing.
        let mut builder = strider_ir_test_utils::empty_builder().expect("FunctionBuilder::new_raw");
        let region = builder.create_region().expect("create_region");
        builder.set_entry_region(region).expect("set_entry_region");
        builder.set_region(region);
        builder.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let anchor = anchor_inputs(&mut builder);
        // Re-stamp in case `anchor_inputs` cleared the lift_addr.
        builder.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        builder
            .build_return(Some(anchor), &[])
            .expect("build_return");
        builder.set_lift_addr(None);
        let function = builder.build().expect("build");
        // Re-locate the anchor in the built graph: the build step
        // is a move, but `ValueId` is a stable cranelift-entity
        // index so the same id continues to point at the same
        // output in the resulting graph.
        (function, anchor)
    }

    #[test]
    fn classify_int_const_returns_single() {
        let (function, anchor) = empty_graph_returning(|fb| {
            // Single IntConst node.  Output type is I64 — chosen
            // because BranchIndirect targets are pointer-sized on
            // every supported 64-bit arch; smaller widths would
            // also fold via the `as u64` cast in the classifier.
            fb.build_int_const(0x1234u64, ValueType::I64).unwrap()
        });
        let result = classify_anchor_bare(&function, anchor).expect("classify");
        assert_eq!(result, Some(ResolvedTargets::Single(0x1234)));
    }

    #[test]
    fn classify_int_const_when_lr_unset_still_returns_single() {
        // Pinned: the IntConst arm does not consult
        // `link_register_vn`.  A None lr (x86 / x86_64) must not
        // suppress IntConst classification.
        let (function, anchor) = empty_graph_returning(|fb| {
            fb.build_int_const(0xfeed_face_u64, ValueType::I64).unwrap()
        });
        assert_eq!(
            classify_anchor_bare(&function, anchor).expect("classify"),
            Some(ResolvedTargets::Single(0xfeed_face)),
        );
    }

    #[test]
    fn classify_initial_var_with_matching_lr_returns_link_register() {
        let lr_vn = fake_reg_vn(0x4c, 4);
        // Build a graph where the only tracked variable IS the
        // link register; reading it produces an `InitialVar(lr)`
        // output.
        let mut builder = RegisterSet::new()
            .tracked(lr_vn)
            .link_register(lr_vn)
            .build_fn_single_region()
            .expect("RegisterSet::build_fn_single_region");
        // `read_variable` in the entry region's only predecessor
        // (the function entry) returns the InitialVar.
        let anchor = builder.read_variable(&lr_vn).expect("read_variable(lr)");
        builder
            .build_return(Some(anchor), &[])
            .expect("build_return");
        builder.set_lift_addr(None);
        let function = builder.build().expect("build");

        // PhiCollapse hasn't run, so the producer might be a
        // VarPhi rather than InitialVar directly.  Inspect.
        // For the LinkRegister arm to match, the producer must be
        // `InitialVar(lr_vn)` — we walk past a single-input
        // VarPhi in the test if we hit one, since
        // PhiCollapse would have done that in production.
        let mut producer_value = anchor;
        loop {
            let pid = function.producer(producer_value);
            let is_var_phi = matches!(function.node_kind(pid), NodeKind::Phi)
                && function
                    .get_vn_for_value(function.node_outputs(pid)[0])
                    .is_some();
            if !is_var_phi {
                break;
            }
            // VarPhi inputs: [phi_token, ...per-pred values].
            // With one predecessor, slot 1 is the value.
            if function.node_inputs(pid).len() != 2 {
                break;
            }
            let Some(slot1) = function.graph().nth_input(pid, 1) else {
                break;
            };
            producer_value = slot1;
        }

        let result = classify_anchor_bare(&function, producer_value).expect("classify");
        assert_eq!(result, Some(ResolvedTargets::LinkRegister));
    }

    #[test]
    fn classify_initial_var_with_non_matching_vn_returns_none() {
        let lr_vn = fake_reg_vn(0x4c, 4);
        let other_vn = fake_reg_vn(0x10, 4);
        // Track only `other_vn`; reading it gives `InitialVar(other_vn)`.
        // Pass `lr_vn` as the link register — they don't match, so
        // the classifier must return None.
        let mut builder = RegisterSet::new()
            .tracked(other_vn)
            .link_register(lr_vn)
            .build_fn_single_region()
            .expect("RegisterSet::build_fn_single_region");
        let anchor = builder
            .read_variable(&other_vn)
            .expect("read_variable(other)");
        builder
            .build_return(Some(anchor), &[])
            .expect("build_return");
        builder.set_lift_addr(None);
        let function = builder.build().expect("build");

        let mut producer_value = anchor;
        loop {
            let pid = function.producer(producer_value);
            let is_var_phi = matches!(function.node_kind(pid), NodeKind::Phi)
                && function
                    .get_vn_for_value(function.node_outputs(pid)[0])
                    .is_some();
            if !is_var_phi {
                break;
            }
            if function.node_inputs(pid).len() != 2 {
                break;
            }
            let Some(slot1) = function.graph().nth_input(pid, 1) else {
                break;
            };
            producer_value = slot1;
        }

        let result = classify_anchor_bare(&function, producer_value).expect("classify");
        assert_eq!(result, None);
    }

    #[test]
    fn classify_initial_var_with_lr_unset_returns_none() {
        // InitialVar(lr_vn), but `link_register_vn = None` (the
        // x86 / x86_64 case).  The `Some(vn) == None` guard fails;
        // classifier returns None.
        let lr_vn = fake_reg_vn(0x4c, 4);
        let mut builder = RegisterSet::new()
            .tracked(lr_vn)
            .build_fn_single_region()
            .expect("RegisterSet::build_fn_single_region");
        let anchor = builder.read_variable(&lr_vn).expect("read_variable(lr)");
        builder
            .build_return(Some(anchor), &[])
            .expect("build_return");
        builder.set_lift_addr(None);
        let function = builder.build().expect("build");

        let mut producer_value = anchor;
        loop {
            let pid = function.producer(producer_value);
            let is_var_phi = matches!(function.node_kind(pid), NodeKind::Phi)
                && function
                    .get_vn_for_value(function.node_outputs(pid)[0])
                    .is_some();
            if !is_var_phi {
                break;
            }
            if function.node_inputs(pid).len() != 2 {
                break;
            }
            let Some(slot1) = function.graph().nth_input(pid, 1) else {
                break;
            };
            producer_value = slot1;
        }

        let result = classify_anchor_bare(&function, producer_value).expect("classify");
        assert_eq!(result, None);
    }

    #[test]
    fn classify_unrelated_node_kind_returns_none() {
        // An IntAdd node — not IntConst, not InitialVar, not
        // ValuePhi — must classify as None.
        let (function, anchor) = empty_graph_returning(|fb| {
            let lhs = fb.build_int_const(1u64, ValueType::I64).unwrap();
            let rhs = fb.build_int_const(2u64, ValueType::I64).unwrap();
            fb.build_int_binary_operation(lhs, rhs, strider_ir::IntBinaryOp::Add, ValueType::I64)
                .expect("build_int_binary_operation")
        });
        // Note: ConstantFold would turn 1+2 into IntConst(3), but
        // we don't run the optimiser here — the unit tests use the
        // raw builder output.  The returned anchor's producer is
        // an IntBinaryOp node, which the `_ => None` arm catches.
        let producer_kind = *function.kind_of_value(anchor);
        assert!(
            matches!(producer_kind, NodeKind::IntBinaryOp(_)),
            "fixture must produce an IntBinaryOp; got {producer_kind:?}"
        );
        assert_eq!(
            classify_anchor_bare(&function, anchor).expect("classify"),
            None
        );
    }
}
