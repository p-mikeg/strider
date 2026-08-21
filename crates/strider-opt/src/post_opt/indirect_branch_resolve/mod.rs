//! Classifies the placeholder targets the lifter inserts at `BranchIndirect`
//! sites.
//!
//! [`classify_target`] tries an ordered list of sound recognisers, first match
//! wins: a literal `IntConst` address, an `InitialVar(lr)` return, then
//! [`table::classify_table_dispatch`] for rodata jump tables and on-stack label
//! arrays.  No match leaves the target unresolved.

#![allow(clippy::module_name_repetitions)]

use strider_ir::node::{NodeId, NodeKind, ValueId};
use strider_ir::{IRViewer, IRWalker};

use strider_cfg::ResolvedTargets;

use crate::pipeline::{OptCtx, PostOptimizer};
use crate::{EditFunction, ReadOnlyMemory};

mod eval;
pub mod table;

/// Cap on enumerated table slots: without one, an all-ones KnownBits mask would
/// force iteration through 4 GiB of them.
pub(crate) const MAX_TABLE_ENTRIES: u64 = 4096;

pub use table::classify_table_dispatch;

/// `None` when the dispatch value matches no known sound shape, which defers
/// the branch.  `rom` is consulted only by the rodata jump-table shape.
///
/// # Soundness
///
/// Every recogniser must identify the runtime target set UNAMBIGUOUSLY and
/// fails closed on a partial proof, never under-approximating.  `Load(sp)` for
/// `pop pc`-style returns stays unclassified: a `push X; pop pc` tail call has
/// the identical shape and would be misclassified as a return.
#[must_use]
pub fn classify_target(
    function: &strider_ir::Function,
    branch: NodeId,
    rom: Option<&dyn ReadOnlyMemory>,
    ranges: &mut crate::value_range::RangeMap<'_>,
    alias_mode: crate::AliasMode,
) -> Option<ResolvedTargets> {
    let target_value = function.indirect_branch_target(branch);
    // The ISA mode this branch's instruction committed for its target(s), when
    // it is an interworking branch; carried on the branch as a live input and
    // evaluated per resolved target below.
    let mode_value = function.indirect_branch_isa_mode(branch);
    single_const_target(function, target_value, mode_value)
        .or_else(|| link_register_return(function, target_value))
        .or_else(|| {
            table::classify_table_dispatch(function, branch, rom, ranges, alias_mode, mode_value)
        })
}

/// The literal target of an `IntConst` dispatch value.  A literal is always a
/// deterministic function of the pcode, so it is the only possible target.
fn single_const_target(
    function: &strider_ir::Function,
    target_value: ValueId,
    mode_value: Option<ValueId>,
) -> Option<ResolvedTargets> {
    if !matches!(function.kind_of_value(target_value), NodeKind::IntConst(_)) {
        return None;
    }
    let k = function.int_const_u128(target_value)?;
    // A committed interworking mode MUST fold to a constant here; if it does
    // not, fail closed (`?` defers the whole branch) rather than decode the
    // const target in the flowing mode. Mirrors the table path (table.rs).
    let isa_bit = match mode_value {
        Some(mv) => Some(function.int_const_u128(mv)? != 0),
        None => None,
    };
    // Reject rather than truncate a const with its high 64 bits set.
    Some(ResolvedTargets::Single(strider_cfg::ResolvedTarget::new(
        u64::try_from(k).ok()?,
        isa_bit,
    )))
}

/// The value under an interworking mask, else `value` unchanged.
///
/// `BXWritePC` / `JXWritePC` emit `target & ~1`: the cleared bit is the ISA
/// mode, which the branch carries separately, so the mask hides the shape the
/// recognisers match.  Only the exact low-bit mask is stripped; any other
/// constant is real arithmetic on the target.
fn strip_isa_mode_mask(function: &strider_ir::Function, value: ValueId) -> ValueId {
    let producer = function.producer(value);
    if !matches!(
        function.node_kind(producer),
        NodeKind::IntBinaryOp(strider_ir::node::IntBinaryOp::And)
    ) {
        return value;
    }
    let Ok([a, b]) = function.producer_inputs_exact::<2>(value) else {
        return value;
    };
    let Some(ty) = function.value_type_opt(value) else {
        return value;
    };
    let want = ty.bit_mask_u128() ^ 1;
    for (operand, other) in [(a, b), (b, a)] {
        if function.int_const_u128(other) == Some(want) {
            return operand;
        }
    }
    value
}

/// `None` on stack-push ABIs (x86 / x86_64), which have no link register.
fn link_register_return(
    function: &strider_ir::Function,
    target_value: ValueId,
) -> Option<ResolvedTargets> {
    let lr = function.default_cc().link_register_vn?;
    match *function.kind_of_value(strip_isa_mode_mask(function, target_value)) {
        NodeKind::InitialVar(id) if function.initial_vn(id) == lr => {
            Some(ResolvedTargets::LinkRegister)
        }
        _ => None,
    }
}

/// Classifies every live `IndirectBranch` placeholder into
/// [`OptCtx::indirect_resolutions`].  Never mutates the graph.
///
/// Must run as a post-pass: a dispatch value only becomes classifiable once the
/// optimizer has folded it into a recognizable shape.
#[derive(Clone)]
pub struct IndirectBranchClassify;

impl PostOptimizer for IndirectBranchClassify {
    fn apply(&self, edit: &mut EditFunction<'_>, ctx: &mut OptCtx<'_>) -> crate::Result<()> {
        let function = edit.function();

        // Most functions have neither a placeholder nor a seated `Switch`, so
        // one walk collects both kinds and the KnownBits / dominator /
        // value-range setup below is skipped when it finds nothing.
        let sites: Vec<NodeId> = function
            .walk_kind(|k| matches!(k, NodeKind::IndirectBranch | NodeKind::Switch))
            .collect();

        // Off leaves every site a placeholder, but each one still has to be
        // REPORTED: the orchestrator derives its live-placeholder set from
        // `indirect_resolutions`, so returning before the inserts below would
        // publish an empty `unresolved_indirect_branches` while placeholders
        // are live. A site the caller seated via `known_targets` is already a
        // `Switch` by now, so it reports nothing.
        if !ctx.options.resolve_indirect_branches {
            ctx.indirect_resolutions = sites
                .into_iter()
                .filter(|&node| matches!(function.node_kind(node), NodeKind::IndirectBranch))
                .map(|node| (node, None))
                .collect();
            return Ok(());
        }

        let mut resolutions: rustc_hash::FxHashMap<_, _> = rustc_hash::FxHashMap::default();
        if !sites.is_empty() {
            // Computed once, since the graph does not change during this pass.
            let known = crate::opt::known_bits::analyze(function)?;
            let doms = strider_ir::control_dominators(function);
            let mut ranges = crate::value_range::compute_value_ranges(function, &doms, &known);

            for node in sites {
                // An already-seated `Switch` is re-derived from its retained
                // selector: a site that resolved before the CFG finished
                // growing (a switch whose loop back-edge runs through its own
                // arms) reads as a wider table now that the loop is closed. The
                // orchestrator REPLACES the seated set with this one, adopting
                // per address the ISA mode it already proved, so widening never
                // re-decides a decode. A `Switch` carries no ISA-mode input, so
                // `mode_value` is `None` there and every re-derived target
                // reports no mode. Returning `None` keeps the seated arms and
                // REPORTS the site unresolved: they may be a proper subset.
                let resolved = if matches!(function.node_kind(node), NodeKind::Switch) {
                    let Some(&selector) = function.node_inputs(node).get(1) else {
                        continue;
                    };
                    table::classify_dispatch_value(
                        function,
                        node,
                        selector,
                        ctx.rom,
                        &mut ranges,
                        ctx.options.alias_mode,
                        None,
                    )
                } else {
                    classify_target(function, node, ctx.rom, &mut ranges, ctx.options.alias_mode)
                };
                resolutions.insert(node, resolved);
            }
        }
        ctx.indirect_resolutions = resolutions;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    //! These build nodes directly rather than lifting, so the classifier's
    //! match arms are exercised in isolation.

    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use strider_ir::node::{IntBinaryOp, NodeKind, ValueType};
    use strider_ir::{FunctionBuilder, IRBuilderExt, IRViewer};
    use strider_ir_test_utils::{RegisterSet, SENTINEL_LIFT_ADDR, reg_vn as fake_reg_vn};

    /// Runs the two value-based recognisers on a dispatch value directly.
    fn classify_target_bare(
        function: &strider_ir::Function,
        target_value: ValueId,
    ) -> anyhow::Result<Option<ResolvedTargets>> {
        Ok(single_const_target(function, target_value, None)
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
        // `build` is a move, but `ValueId` is a stable entity index.
        (function, target)
    }

    /// PhiCollapse has not run on these hand-built fixtures, so the tracked
    /// read sits behind single-input `VarPhi`s the classifier arms look past.
    fn skip_trivial_var_phis(function: &strider_ir::Function, mut value: ValueId) -> ValueId {
        loop {
            let pid = function.producer(value);
            let is_var_phi = matches!(function.node_kind(pid), NodeKind::Phi)
                && function
                    .get_vn_for_value(function.node_outputs(pid)[0])
                    .is_some();
            if !is_var_phi {
                return value;
            }
            // Inputs are [phi_token, ...per-pred values], so a single
            // predecessor puts the value at slot 1.
            if function.node_inputs(pid).len() != 2 {
                return value;
            }
            let Some(slot1) = function.nth_input(pid, 1) else {
                return value;
            };
            value = slot1;
        }
    }

    #[test]
    fn indirect_branch_carries_its_mode_input() {
        // An interworking branch carries its ISA-mode bit as slot 3; the resolver
        // surfaces it, read live off the placeholder.
        let mut builder = strider_ir_test_utils::empty_builder().expect("empty_builder");
        let region = builder.create_region_all().expect("create_region");
        builder.set_entry_region_all(region).expect("set_entry");
        builder.set_region(region);
        builder.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let mode = builder.build_int_const(1u64, ValueType::I64).unwrap();
        let target = builder.build_int_const(0x8000u64, ValueType::I64).unwrap();
        let branch = builder
            .build_indirect_branch_with_mode(target, Some(mode))
            .expect("build_branch");
        builder.set_lift_addr(None);
        let function = builder.build().expect("build");

        assert_eq!(function.indirect_branch_isa_mode(branch), Some(mode));
    }

    #[test]
    fn non_switching_branch_has_no_mode_input() {
        // A 3-input branch: no mode, so it keeps the flowing mode.
        let mut builder = strider_ir_test_utils::empty_builder().expect("empty_builder");
        let region = builder.create_region_all().expect("create_region");
        builder.set_entry_region_all(region).expect("set_entry");
        builder.set_region(region);
        builder.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let target = builder.build_int_const(0x8000u64, ValueType::I64).unwrap();
        let branch = builder.build_indirect_branch(target).expect("build_branch");
        builder.set_lift_addr(None);
        let function = builder.build().expect("build");

        assert_eq!(function.indirect_branch_isa_mode(branch), None);
    }

    #[test]
    fn const_target_with_unfoldable_mode_defers() {
        // A `bx <const>` whose committed ISA mode does not fold to a constant
        // must fail closed: the callee's mode is unknown, so defer rather than
        // decode the const target in the flowing mode (the table path already
        // fails closed here via `?`).
        let mut builder = strider_ir_test_utils::empty_builder().expect("empty_builder");
        let region = builder.create_region_all().expect("create_region");
        builder.set_entry_region_all(region).expect("set_entry");
        builder.set_region(region);
        builder.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let target = builder.build_int_const(0x8000u64, ValueType::I64).unwrap();
        // A symbolic (non-IntConst) mode, as a runtime `entry & 1` would be.
        let one = builder.build_int_const(1u64, ValueType::I64).unwrap();
        let mode = builder
            .build_int_binary_operation(target, one, IntBinaryOp::And, ValueType::I64)
            .unwrap();
        builder
            .build_indirect_branch_with_mode(target, Some(mode))
            .expect("build_branch");
        builder.set_lift_addr(None);
        let function = builder.build().expect("build");

        assert_eq!(single_const_target(&function, target, Some(mode)), None);
    }

    #[test]
    fn classify_int_const_returns_single() {
        let (function, target) = empty_graph_returning(|fb| {
            // I64 because BranchIndirect targets are pointer-sized.
            fb.build_int_const(0x1234u64, ValueType::I64).unwrap()
        });
        let result = classify_target_bare(&function, target).expect("classify");
        assert_eq!(result, Some(ResolvedTargets::Single(0x1234.into())));
    }

    #[test]
    fn classify_int_const_when_lr_unset_still_returns_single() {
        // The IntConst arm must not consult `link_register_vn`.
        let (function, target) = empty_graph_returning(|fb| {
            fb.build_int_const(0xfeed_face_u64, ValueType::I64).unwrap()
        });
        assert_eq!(
            classify_target_bare(&function, target).expect("classify"),
            Some(ResolvedTargets::Single(0xfeed_face.into())),
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

        let producer_value = skip_trivial_var_phis(&function, target);

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

        let producer_value = skip_trivial_var_phis(&function, target);

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

        let producer_value = skip_trivial_var_phis(&function, target);

        let result = classify_target_bare(&function, producer_value).expect("classify");
        assert_eq!(result, None);
    }

    /// The pass publishes what it found on every exit, so a second run over an
    /// `OptCtx` carrying an earlier run's map never leaves a stale site behind.
    #[test]
    fn classify_pass_republishes_on_a_site_free_function() {
        let (mut function, _target) =
            empty_graph_returning(|fb| fb.build_int_const(0x1234u64, ValueType::I64).unwrap());
        let stale = function.entry();
        let mut ctx = crate::OptCtx::new(None);
        ctx.indirect_resolutions.insert(stale, None);
        crate::run_post(&IndirectBranchClassify, &mut function, &mut ctx).expect("classify");
        assert!(
            ctx.indirect_resolutions.is_empty(),
            "a function with no branch site publishes an empty map"
        );
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
