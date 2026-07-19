//! Classifies the placeholder targets the lifter inserts at `BranchIndirect`
//! sites.  The orchestrator drives the outer loop (CFG rebuild, cache
//! invalidation, iteration cap).
//!
//! [`classify_target`] tries an ordered list of sound recognisers, first match
//! wins: a literal `IntConst` address, an `InitialVar(lr)` return, then
//! [`table::classify_table_dispatch`] for rodata jump tables and on-stack label
//! arrays.  No match leaves the target unresolved, and the orchestrator either
//! retries or surfaces it at fixed point.
//!
//! Each recogniser reads its ABI facts (link-register and stack-pointer
//! varnodes, endianness) straight off the `Function`, so the only external
//! inputs are the optional rodata image and the value-range map.
//!
//! `ResolvedTargets` lives in `strider_cfg`, the lowest layer that needs it:
//! the cfg builder consumes it via `LiftOptions::known_targets` to seat
//! indirect-branch terminators.

#![allow(clippy::module_name_repetitions)]

use strider_ir::node::{NodeId, NodeKind, ValueId};
use strider_ir::{IRViewer, IRWalker};

use strider_cfg::ResolvedTargets;

use crate::pipeline::{OptCtx, PostOptimizer};
use crate::{EditFunction, ReadOnlyMemory};

mod eval;
pub mod table;

/// Without a cap, an all-ones KnownBits mask would force iteration through
/// 4 GiB of slots.  Real gcc/clang jump tables are bounded by the source-level
/// `switch` arm count and are almost always well under this, so anything
/// larger is better deferred than enumerated.
pub(crate) const MAX_TABLE_ENTRIES: u64 = 4096;

pub use table::classify_table_dispatch;

/// `None` when the dispatch value matches no known sound shape, which defers
/// the branch.  `rom` is consulted only by the rodata jump-table shape.
///
/// # Soundness
///
/// Every recogniser must identify the runtime target set UNAMBIGUOUSLY and
/// fails closed on a partial proof, never under-approximating.  `Load(sp)` for
/// `pop pc`-style returns is deliberately excluded: a `push X; pop pc` tail
/// call has the identical shape and would be misclassified as a return.  A
/// properly-popped return address instead reaches `link_register_return` as
/// `InitialVar(lr)`, courtesy of `LoadForward`.
#[must_use]
pub fn classify_target(
    function: &strider_ir::Function,
    branch: NodeId,
    rom: Option<&dyn ReadOnlyMemory>,
    ranges: &mut crate::value_range::RangeMap<'_>,
    alias_mode: crate::AliasMode,
) -> Option<ResolvedTargets> {
    // Taking the branch NODE rather than the bare value keeps the table
    // classifier's index-range query scoped to THIS branch, even when several
    // branches share one dispatch value.
    let target_value = function.indirect_branch_target(branch);
    single_const_target(function, target_value)
        .or_else(|| link_register_return(function, target_value))
        .or_else(|| table::classify_table_dispatch(function, branch, rom, ranges, alias_mode))
}

/// Sound because a literal in the IR only comes from a tracked `IntConst`
/// pcode insn, `ConstantFold`, or a `LoadReadOnly` rodata resolution, all
/// deterministic functions of the pcode, so `k` is the only possible target.
fn single_const_target(
    function: &strider_ir::Function,
    target_value: ValueId,
) -> Option<ResolvedTargets> {
    if !matches!(function.kind_of_value(target_value), NodeKind::IntConst(_)) {
        return None;
    }
    let k = function.int_const_u128(target_value)?;
    // A const with its high 64 bits set is never a valid jump target on a
    // 64-bit ISA, so reject it rather than truncate to a wrong address.
    Some(ResolvedTargets::Single(u64::try_from(k).ok()?))
}

/// `None` on stack-push ABIs (x86 / x86_64), which have no link register to
/// match against.  This is the shape `LoadForward` produces for a
/// properly-popped return address.
fn link_register_return(
    function: &strider_ir::Function,
    target_value: ValueId,
) -> Option<ResolvedTargets> {
    let lr = function.default_cc().link_register_vn?;
    match *function.kind_of_value(target_value) {
        NodeKind::InitialVar(id) if function.initial_vn(id) == lr => {
            Some(ResolvedTargets::LinkRegister)
        }
        _ => None,
    }
}

/// Analysis-only: never mutates the graph, and writes its output to
/// [`OptCtx::indirect_resolutions`] for the orchestrator to drain.
///
/// It must run as a post-pass because a dispatch value is opaque at lift time
/// and only becomes classifiable once the optimizer has folded it into a
/// recognizable shape.  The rewrites ARE the resolution mechanism, so the
/// classifier has to see their output.
///
/// Walking live nodes reads each placeholder's CURRENT slot-2 input, so it
/// never inspects a value `replace_all_uses` orphaned away.  A placeholder the
/// node-removing passes proved unreachable is simply never visited, so a dead
/// indirect branch is dropped rather than reported unresolved.
#[derive(Clone)]
pub struct IndirectBranchClassify;

impl PostOptimizer for IndirectBranchClassify {
    fn apply(&self, edit: &mut EditFunction<'_>, ctx: &mut OptCtx<'_>) -> crate::Result<()> {
        let function = edit.function();

        // Computed once for every target, since the graph does not change
        // during this analysis-only pass.
        let known = crate::opt::known_bits::analyze(function)?;
        let doms = strider_ir::control_dominators(function);
        let mut ranges = crate::value_range::compute_value_ranges(function, &doms, &known);

        let mut resolutions: rustc_hash::FxHashMap<_, _> = rustc_hash::FxHashMap::default();
        for node in function.walk_kind(|k| matches!(k, NodeKind::IndirectBranch)) {
            // The walk visits each node once, so every key is unique.
            let resolved =
                classify_target(function, node, ctx.rom, &mut ranges, ctx.options.alias_mode);
            resolutions.insert(node, resolved);
        }
        ctx.indirect_resolutions = resolutions;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    //! These deliberately bypass the IR-lift path and build nodes directly, so
    //! the classifier's match arms are exercised in isolation rather than
    //! depending on the optimiser pipeline to produce the expected shape.

    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use strider_ir::node::{NodeKind, ValueType};
    use strider_ir::{FunctionBuilder, IRBuilderExt, IRViewer};
    use strider_ir_test_utils::{RegisterSet, SENTINEL_LIFT_ADDR, reg_vn as fake_reg_vn};

    /// Runs the two value-based recognisers on a dispatch value directly.  The
    /// table arm is the only one needing a branch node plus dominator-scoped
    /// ranges, and `table_tests` covers it against a real `IndirectBranch`.
    fn classify_target_bare(
        function: &strider_ir::Function,
        target_value: ValueId,
    ) -> anyhow::Result<Option<ResolvedTargets>> {
        Ok(single_const_target(function, target_value)
            .or_else(|| link_register_return(function, target_value)))
    }

    /// An empty region terminated by a Return over the caller-supplied value,
    /// so the classifier sees a real validation-passing graph.
    fn empty_graph_returning(
        target_inputs: impl FnOnce(&mut FunctionBuilder) -> ValueId,
    ) -> (strider_ir::Function, ValueId) {
        let mut builder = strider_ir_test_utils::empty_builder().expect("FunctionBuilder::new_raw");
        let region = builder.create_region_all().expect("create_region");
        builder
            .set_entry_region_all(region)
            .expect("set_entry_region");
        builder.set_region(region);
        builder.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let target = target_inputs(&mut builder);
        // Re-stamp in case the closure cleared the lift_addr.
        builder.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        builder
            .build_return(Some(target), &[])
            .expect("build_return");
        builder.set_lift_addr(None);
        let function = builder.build().expect("build");
        // `build` is a move, but `ValueId` is a stable entity index, so the
        // same id still names the same output afterward.
        (function, target)
    }

    #[test]
    fn classify_int_const_returns_single() {
        let (function, target) = empty_graph_returning(|fb| {
            // I64 because BranchIndirect targets are pointer-sized on every
            // supported 64-bit arch; narrower widths would fold too.
            fb.build_int_const(0x1234u64, ValueType::I64).unwrap()
        });
        let result = classify_target_bare(&function, target).expect("classify");
        assert_eq!(result, Some(ResolvedTargets::Single(0x1234)));
    }

    #[test]
    fn classify_int_const_when_lr_unset_still_returns_single() {
        // The IntConst arm must not consult `link_register_vn`, so a None lr
        // (x86 / x86_64) cannot suppress its classification.
        let (function, target) = empty_graph_returning(|fb| {
            fb.build_int_const(0xfeed_face_u64, ValueType::I64).unwrap()
        });
        assert_eq!(
            classify_target_bare(&function, target).expect("classify"),
            Some(ResolvedTargets::Single(0xfeed_face)),
        );
    }

    #[test]
    fn classify_initial_var_with_matching_lr_returns_link_register() {
        let lr_vn = fake_reg_vn(0x4c, 4);
        // The only tracked variable IS the link register, so reading it gives
        // an `InitialVar(lr)`.
        let mut builder = RegisterSet::new()
            .tracked(lr_vn)
            .link_register(lr_vn)
            .build_fn_single_region()
            .expect("RegisterSet::build_fn_single_region");
        let target = builder.read_variable(&lr_vn).expect("read_variable(lr)");
        builder
            .build_return(Some(target), &[])
            .expect("build_return");
        builder.set_lift_addr(None);
        let function = builder.build().expect("build");

        // PhiCollapse has not run, so walk past a single-input VarPhi by hand
        // to reach the `InitialVar(lr_vn)` the LinkRegister arm needs.
        let mut producer_value = target;
        loop {
            let pid = function.producer(producer_value);
            let is_var_phi = matches!(function.node_kind(pid), NodeKind::Phi)
                && function
                    .get_vn_for_value(function.node_outputs(pid)[0])
                    .is_some();
            if !is_var_phi {
                break;
            }
            // Inputs are [phi_token, ...per-pred values], so a single
            // predecessor puts the value at slot 1.
            if function.node_inputs(pid).len() != 2 {
                break;
            }
            let Some(slot1) = function.nth_input(pid, 1) else {
                break;
            };
            producer_value = slot1;
        }

        let result = classify_target_bare(&function, producer_value).expect("classify");
        assert_eq!(result, Some(ResolvedTargets::LinkRegister));
    }

    #[test]
    fn classify_initial_var_with_non_matching_vn_returns_none() {
        let lr_vn = fake_reg_vn(0x4c, 4);
        let other_vn = fake_reg_vn(0x10, 4);
        // The tracked variable and the declared link register differ, so no
        // match is possible.
        let mut builder = RegisterSet::new()
            .tracked(other_vn)
            .link_register(lr_vn)
            .build_fn_single_region()
            .expect("RegisterSet::build_fn_single_region");
        let target = builder
            .read_variable(&other_vn)
            .expect("read_variable(other)");
        builder
            .build_return(Some(target), &[])
            .expect("build_return");
        builder.set_lift_addr(None);
        let function = builder.build().expect("build");

        let mut producer_value = target;
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
            let Some(slot1) = function.nth_input(pid, 1) else {
                break;
            };
            producer_value = slot1;
        }

        let result = classify_target_bare(&function, producer_value).expect("classify");
        assert_eq!(result, None);
    }

    #[test]
    fn classify_initial_var_with_lr_unset_returns_none() {
        // InitialVar(lr_vn) with `link_register_vn = None`, the x86 case.
        let lr_vn = fake_reg_vn(0x4c, 4);
        let mut builder = RegisterSet::new()
            .tracked(lr_vn)
            .build_fn_single_region()
            .expect("RegisterSet::build_fn_single_region");
        let target = builder.read_variable(&lr_vn).expect("read_variable(lr)");
        builder
            .build_return(Some(target), &[])
            .expect("build_return");
        builder.set_lift_addr(None);
        let function = builder.build().expect("build");

        let mut producer_value = target;
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
            let Some(slot1) = function.nth_input(pid, 1) else {
                break;
            };
            producer_value = slot1;
        }

        let result = classify_target_bare(&function, producer_value).expect("classify");
        assert_eq!(result, None);
    }

    #[test]
    fn classify_unrelated_node_kind_returns_none() {
        let (function, target) = empty_graph_returning(|fb| {
            let lhs = fb.build_int_const(1u64, ValueType::I64).unwrap();
            let rhs = fb.build_int_const(2u64, ValueType::I64).unwrap();
            fb.build_int_binary_operation(lhs, rhs, strider_ir::IntBinaryOp::Add, ValueType::I64)
                .expect("build_int_binary_operation")
        });
        // ConstantFold would turn 1+2 into IntConst(3), but the optimiser does
        // not run here, so the producer stays an IntBinaryOp.
        let producer_kind = *function.kind_of_value(target);
        assert!(
            matches!(producer_kind, NodeKind::IntBinaryOp(_)),
            "fixture must produce an IntBinaryOp; got {producer_kind:?}"
        );
        assert_eq!(
            classify_target_bare(&function, target).expect("classify"),
            None
        );
    }
}
