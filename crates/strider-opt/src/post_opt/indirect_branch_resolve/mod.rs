//! Classifies the placeholder targets the lifter inserts at `BranchIndirect`
//! sites.
//!
//! [`classify_target`] tries an ordered list of sound recognisers, first match
//! wins: a literal `IntConst` address, an `InitialVar(lr)` return, then
//! `table::classify_table_dispatch` for rodata jump tables and on-stack label
//! arrays.  No match leaves the target unresolved.

use strider_ir::node::{NodeId, NodeKind, ValueId};
use strider_ir::{IRViewer, IRWalker};

use strider_cfg::ResolvedTargets;

use crate::ReadOnlyMemory;

pub(crate) mod eval;
mod return_target;
pub(crate) mod table;

pub use return_target::ReturnTargets;

/// Cap on enumerated table slots: a range narrowed only by a KnownBits stride
/// still spans billions of them.
pub(crate) const MAX_TABLE_ENTRIES: u64 = 4096;

/// `None` when the dispatch value matches no known sound shape, which defers
/// the branch.  `rom` is consulted only by the rodata jump-table shape.
///
/// # Soundness
///
/// Every recogniser must identify the runtime target set UNAMBIGUOUSLY and
/// fails closed on a partial proof, never under-approximating.  `Load(sp)` for
/// a `pop pc`-style return, which a `push X; pop pc` tail call shares the shape
/// of, is classified only where [`ReturnTargets`] follows the slot back to a
/// store of the entry link register.  That walk is the one place a
/// call-boundary relaxation reaches a decoded target, and it is the orchestrator
/// that keeps the answer honest: the seated `Return` carries a return site, so
/// every round re-checks the seat against its own IR. A verdict that rests on
/// `return_slots_survive_untracked_writes` is not reported; clearing the claim
/// and re-running is how a caller audits one.
///
/// Of `assumptions` the table walk honours `stack_global_disjoint` and the
/// allocation identity `noalias_allocators` gives `decompose`.  The
/// call-boundary relaxations are deliberately not among them
/// (`eval::table_walk_options`): this walk decides a decoded branch target,
/// so a relaxation that is wrong produces a wrong edge rather than a wrong
/// fold.
#[must_use]
pub fn classify_target(
    function: &strider_ir::Function,
    branch: NodeId,
    rom: Option<&dyn ReadOnlyMemory>,
    ranges: &mut crate::value_range::RangeMap<'_>,
    assumptions: &crate::AssumptionOptions,
) -> Option<ResolvedTargets> {
    let target_value = function.indirect_branch_target(branch);
    // The ISA mode this branch's instruction committed for its target(s), when
    // it is an interworking branch; carried on the branch as a live input and
    // evaluated per resolved target below.
    let mode_value = function.indirect_branch_isa_mode(branch);
    single_const_target(function, target_value, mode_value)
        .or_else(|| link_register_return(function, branch, target_value, assumptions))
        .or_else(|| {
            table::classify_table_dispatch(function, branch, rom, ranges, assumptions, mode_value)
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
/// mode, which the branch carries separately, so the mask hides the
/// `InitialVar(lr)` shape under it.  Only the exact low-bit mask is stripped;
/// any other constant is real arithmetic on the target.
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
///
/// A single-register pop into the PC (ARM `ldr pc,[sp],#4`) lifts to a
/// `BranchIndirect` over a stack load, not a `Return`, and once a call sits
/// between the push and the pop nothing folds that load back to the link
/// register.  [`ReturnTargets`] is what separates it from the `push X; pop pc`
/// tail call it looks like.
fn link_register_return(
    function: &strider_ir::Function,
    branch: NodeId,
    target_value: ValueId,
    assumptions: &crate::AssumptionOptions,
) -> Option<ResolvedTargets> {
    let lr = function.default_cc().link_register_vn?;
    match *function.kind_of_value(strip_isa_mode_mask(function, target_value)) {
        NodeKind::InitialVar(id) if function.initial_vn(id) == lr => {
            Some(ResolvedTargets::LinkRegister)
        }
        NodeKind::Load(space) if space == rsleigh::VnSpace::RAM => {
            ReturnTargets::new(function, assumptions)
                .returns_to_caller(branch, target_value)
                .then_some(ResolvedTargets::LinkRegister)
        }
        _ => None,
    }
}

/// Every live dispatch site of a function, and what was recovered for each.
#[derive(Debug, Default)]
pub struct ClassifiedSites {
    /// Every reachable `IndirectBranch` placeholder and seated `Switch`.
    pub sites: Vec<NodeId>,
    /// The sites a target set was recovered for.
    pub resolved: rustc_hash::FxHashMap<NodeId, ResolvedTargets>,
}

/// Classifies every live `IndirectBranch` placeholder, and re-derives every
/// live `Switch` from its retained selector. Reads `f` only.
///
/// `options` are the ones `f` was optimised under: the table walk reads the
/// memory-class memo the pipeline stamped under `options.assumptions`. With
/// `!options.resolve_indirect_branches`, `resolved` is empty.
///
/// A dispatch value only becomes classifiable once the optimizer has folded
/// it into a recognizable shape, so `f` is the converged function.
///
/// # Errors
///
/// The known-bits analysis failing.
pub fn classify_sites(
    f: &strider_ir::Function,
    rom: Option<&dyn ReadOnlyMemory>,
    options: &crate::OptOptions,
) -> crate::Result<ClassifiedSites> {
    let sites: Vec<NodeId> = f
        .walk_kind(|k| matches!(k, NodeKind::IndirectBranch | NodeKind::Switch(_)))
        .collect();
    let mut resolved = rustc_hash::FxHashMap::default();
    // Most functions have neither kind, so the setup below is skipped then.
    if !options.resolve_indirect_branches || sites.is_empty() {
        return Ok(ClassifiedSites { sites, resolved });
    }
    // Computed once, since the graph does not change here.
    let known = crate::opt::known_bits::analyze(f)?;
    let doms = strider_ir::control_dominator_tree(f);
    let mut ranges = crate::value_range::compute_value_ranges(f, &doms, &known);
    for &node in &sites {
        // A seated `Switch` is re-derived from its retained selector, which
        // reads as a wider table once a loop back-edge through its own arms
        // closes; it carries the mode its instruction commits, so a widened
        // arm is evaluated in its own mode.
        let targets = if matches!(f.node_kind(node), NodeKind::Switch(_)) {
            table::classify_dispatch_value(
                f,
                node,
                f.switch_selector(node),
                rom,
                &mut ranges,
                &options.assumptions,
                f.switch_isa_mode(node),
            )
        } else {
            classify_target(f, node, rom, &mut ranges, &options.assumptions)
        };
        if let Some(targets) = targets {
            resolved.insert(node, targets);
        }
    }
    Ok(ClassifiedSites { sites, resolved })
}

#[cfg(test)]
mod tests {
    //! These build nodes directly rather than lifting, so the classifier's
    //! match arms are exercised in isolation.

    use super::*;
    use strider_ir::node::{IntBinaryOp, NodeKind, ValueType};
    use strider_ir::{FunctionBuilder, IRBuilderExt, IRViewer};
    use strider_ir_test_utils::{RegisterSet, SENTINEL_LIFT_ADDR, reg_vn as fake_reg_vn};

    /// Runs the two value-based recognisers on a dispatch value directly.
    ///
    /// The entry stands in for the branch site: these targets are an
    /// `InitialVar` or an `IntConst`, so no slot is followed from it.
    fn classify_target_bare(
        function: &strider_ir::Function,
        target_value: ValueId,
    ) -> Option<ResolvedTargets> {
        single_const_target(function, target_value, None).or_else(|| {
            link_register_return(
                function,
                function.entry(),
                target_value,
                &crate::AssumptionOptions::default(),
            )
        })
    }

    /// An empty region terminated by a Return over the caller-supplied value,
    /// so the classifier sees a real validation-passing graph.
    fn empty_graph_returning(
        target_inputs: impl FnOnce(&mut FunctionBuilder) -> ValueId,
    ) -> (strider_ir::Function, ValueId) {
        let mut builder = strider_ir_test_utils::empty_builder().expect("empty_builder");
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
        let result = classify_target_bare(&function, target);
        assert_eq!(result, Some(ResolvedTargets::Single(0x1234.into())));
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

        let result = classify_target_bare(&function, producer_value);
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

        let result = classify_target_bare(&function, producer_value);
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

        let result = classify_target_bare(&function, producer_value);
        assert_eq!(result, None);
    }

    /// Mutation: walk every node (`f.walk().collect()`) instead of the two
    /// dispatch kinds.
    #[test]
    fn classify_sites_lists_nothing_on_a_site_free_function() {
        let (function, _target) =
            empty_graph_returning(|fb| fb.build_int_const(0x1234u64, ValueType::I64).unwrap());
        let found =
            classify_sites(&function, None, &crate::OptOptions::default()).expect("classify");
        assert!(found.sites.is_empty() && found.resolved.is_empty());
    }

    /// `jmp rax` over an entry value: listed, with nothing recovered.
    /// Mutation: insert `ResolvedTargets::LinkRegister` for every site the
    /// recognisers leave `None`.
    #[test]
    fn classify_sites_lists_a_site_it_cannot_resolve() {
        let rax = fake_reg_vn(0x0, 8);
        let mut b = RegisterSet::new()
            .tracked(rax)
            .build_fn_single_region()
            .expect("RegisterSet::build_fn_single_region");
        let target = b.read_variable(&rax).expect("read rax");
        let branch = b.build_indirect_branch(target).expect("indirect");
        b.set_lift_addr(None);
        let function = b.build().expect("build");

        let found =
            classify_sites(&function, None, &crate::OptOptions::default()).expect("classify");
        assert_eq!(found.sites, vec![branch]);
        assert!(found.resolved.is_empty());
    }

    /// With resolution off every site is still listed, so the orchestrator
    /// still reports it, and nothing is recovered. Mutation: drop the
    /// `!options.resolve_indirect_branches ||` guard.
    #[test]
    fn classify_sites_resolves_nothing_with_resolution_off() {
        let mut b = strider_ir_test_utils::empty_builder().expect("empty_builder");
        let region = b.create_region_all().expect("create_region");
        b.set_entry_region_all(region).expect("set_entry");
        b.set_region(region);
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let target = b.build_int_const(0x8000u64, ValueType::I64).unwrap();
        let branch = b.build_indirect_branch(target).expect("build_branch");
        b.set_lift_addr(None);
        let function = b.build().expect("build");

        let on = classify_sites(&function, None, &crate::OptOptions::default()).expect("on");
        assert_eq!(
            on.resolved.get(&branch),
            Some(&ResolvedTargets::Single(0x8000.into()))
        );
        let off = classify_sites(
            &function,
            None,
            &crate::OptOptions {
                resolve_indirect_branches: false,
                ..crate::OptOptions::default()
            },
        )
        .expect("off");
        assert_eq!(off.sites, vec![branch]);
        assert!(off.resolved.is_empty());
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
        assert_eq!(classify_target_bare(&function, target), None);
    }

    /// A 32-bit link-register function saving `lr` at `sp + 0x8000_0000` (off an
    /// `And`-aligned SP when `aligned`), writing 4 bytes at `sp + store` when
    /// given, then branching through the reloaded slot.
    fn lr_saved_across_the_wrap(
        aligned: bool,
        store: Option<u64>,
    ) -> (strider_ir::Function, NodeId, ValueId) {
        let sp = strider_ir_test_utils::stack_vn_x86();
        let lr = fake_reg_vn(0x4c, 4);
        let mut b = strider_ir_test_utils::sp_frame(sp)
            .tracked(lr)
            .link_register(lr)
            .build_fn_single_region()
            .expect("build_fn_single_region");
        let sp_val = b.read_variable(&sp).unwrap();
        let lr_val = b.read_variable(&lr).unwrap();
        let frame = if aligned {
            let mask = b.build_int_const(0xFFFF_FFF0u64, ValueType::I32).unwrap();
            b.build_int_binary_operation(sp_val, mask, IntBinaryOp::And, ValueType::I32)
                .unwrap()
        } else {
            sp_val
        };
        let at = |b: &mut FunctionBuilder, base: ValueId, off: u64| {
            let c = b.build_int_const(off, ValueType::I32).unwrap();
            b.build_int_binary_operation(base, c, IntBinaryOp::Add, ValueType::I32)
                .unwrap()
        };
        let slot = at(&mut b, frame, 0x8000_0000);
        b.build_store(slot, lr_val, rsleigh::VnSpace::RAM).unwrap();
        if let Some(off) = store {
            let near = at(&mut b, sp_val, off);
            let junk = b.build_int_const(0xDEAD_BEEFu64, ValueType::I32).unwrap();
            b.build_store(near, junk, rsleigh::VnSpace::RAM).unwrap();
        }
        let target = b
            .build_load(slot, rsleigh::VnSpace::RAM, ValueType::I32)
            .unwrap();
        let branch = b.build_indirect_branch(target).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();
        // The hand-built reads sit behind single-input `VarPhi`s.
        let mut prep = crate::OptimizerPipeline::new();
        prep.add(crate::PhiCollapse);
        prep.add(crate::RegionCollapse);
        prep.run(&mut function, &mut crate::OptCtx::at_floor(None))
            .expect("prep");
        (function, branch, target)
    }

    #[test]
    fn a_store_wrapping_onto_the_saved_return_slot_clobbers_it() {
        let none = crate::AssumptionOptions::none();
        for aligned in [false, true] {
            let (f, branch, target) = lr_saved_across_the_wrap(aligned, None);
            assert!(
                ReturnTargets::new(&f, &none).returns_to_caller(branch, target),
                "control, aligned={aligned}: the untouched slot holds lr"
            );
            let (f, branch, target) = lr_saved_across_the_wrap(aligned, Some(0x7FFF_FFFE));
            assert!(
                !ReturnTargets::new(&f, &none).returns_to_caller(branch, target),
                "aligned={aligned}: sp+0x7FFF_FFFE overwrites the slot's low half mod 2^32"
            );
        }
        let (f, branch, target) = lr_saved_across_the_wrap(false, Some(0x7FFF_FFFC));
        assert!(
            ReturnTargets::new(&f, &none).returns_to_caller(branch, target),
            "sp+0x7FFF_FFFC ends where the slot starts"
        );
    }
}
