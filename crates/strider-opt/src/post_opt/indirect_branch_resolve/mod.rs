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
/// store of the entry link register.  That walk honours a call-boundary claim
/// of its own (`return_slots_survive_untracked_writes`), and it is the
/// orchestrator that keeps the answer honest: the seated `Return` carries a
/// return site, so every round re-checks the seat against its own IR. A
/// verdict that rests on that claim is not reported; analysing again with the
/// claim cleared is how a caller audits one.
///
/// Of the function's claims ([`strider_ir::Function::assumptions`]) the table
/// walk's own memory queries honour `stack_global_disjoint` and the allocation
/// identity `noalias_allocators` gives `decompose`, and no call-boundary
/// relaxation (`MemPurpose::TableWalk`).  That keeps the relaxations out of
/// the walk's own steps, not off the edge: every recogniser
/// reads the IR the pipeline left, whose `LoadForward` forwards across a call
/// under `escape_analysis`, `callee_preserves_stack_args` or a listed
/// allocator.  A dispatch value forwarded that way arrives here folded, so a
/// claim that is wrong for the input seats a wrong edge.
#[must_use]
pub fn classify_target(
    function: &strider_ir::Function,
    branch: NodeId,
    rom: Option<&dyn ReadOnlyMemory>,
    ranges: &mut crate::value_range::RangeMap<'_>,
) -> Option<ResolvedTargets> {
    let target_value = function.indirect_branch_target(branch);
    // The ISA mode this branch's instruction committed for its target(s), when
    // it is an interworking branch; carried on the branch as a live input and
    // evaluated per resolved target below.
    let mode_value = function.indirect_branch_isa_mode(branch);
    single_const_target(function, target_value, mode_value)
        .or_else(|| link_register_return(function, branch, target_value))
        .or_else(|| table::classify_table_dispatch(function, branch, rom, ranges, mode_value))
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
    let Some(want) = ty.int_mask_u128().map(|m| m ^ 1) else {
        return value;
    };
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
) -> Option<ResolvedTargets> {
    let lr = function.default_cc().link_register_vn?;
    match *function.kind_of_value(strip_isa_mode_mask(function, target_value)) {
        NodeKind::InitialVar(id) if function.initial_vn(id) == lr => {
            Some(ResolvedTargets::LinkRegister)
        }
        NodeKind::Load(space) if space == rsleigh::VnSpace::RAM => ReturnTargets::new(function)
            .returns_to_caller(branch, target_value)
            .then_some(ResolvedTargets::LinkRegister),
        _ => None,
    }
}

/// Every live dispatch site of a function, and what was recovered for each.
#[derive(Debug)]
pub struct ClassifiedSites {
    /// Every reachable `IndirectBranch` placeholder and seated `Switch`.
    pub sites: Vec<NodeId>,
    /// The sites a target set was recovered for.
    pub resolved: rustc_hash::FxHashMap<NodeId, ResolvedTargets>,
}

impl ClassifiedSites {
    /// Every live dispatch site of `f`, none resolved: what a caller that does
    /// not classify reports.
    #[must_use]
    pub fn unresolved(f: &strider_ir::Function) -> Self {
        Self {
            sites: f
                .walk_kind(|k| matches!(k, NodeKind::IndirectBranch | NodeKind::Switch(_)))
                .collect(),
            resolved: rustc_hash::FxHashMap::default(),
        }
    }
}

/// Classifies every live `IndirectBranch` placeholder, and re-derives every
/// live `Switch` from its retained selector, under the claims `f` holds.
/// Reads `f` only.
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
) -> crate::Result<ClassifiedSites> {
    let ClassifiedSites {
        sites,
        mut resolved,
    } = ClassifiedSites::unresolved(f);
    // Most functions have neither kind, so the setup below is skipped then.
    if sites.is_empty() {
        return Ok(ClassifiedSites { sites, resolved });
    }
    // Computed once, since the graph does not change here.
    let mut ranges = crate::value_range::RangeMap::new(f)?;
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
                f.switch_isa_mode(node),
            )
        } else {
            classify_target(f, node, rom, &mut ranges)
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
        single_const_target(function, target_value, None)
            .or_else(|| link_register_return(function, function.entry(), target_value))
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
        let mode = builder.build_int_const(1u64, ValueType::int(64)).unwrap();
        let target = builder
            .build_int_const(0x8000u64, ValueType::int(64))
            .unwrap();
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
        let target = builder
            .build_int_const(0x8000u64, ValueType::int(64))
            .unwrap();
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
        let target = builder
            .build_int_const(0x8000u64, ValueType::int(64))
            .unwrap();
        // A symbolic (non-IntConst) mode, as a runtime `entry & 1` would be.
        let one = builder.build_int_const(1u64, ValueType::int(64)).unwrap();
        let mode = builder
            .build_int_binary_operation(target, one, IntBinaryOp::And, ValueType::int(64))
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
            fb.build_int_const(0x1234u64, ValueType::int(64)).unwrap()
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

    /// `unresolved` first: there a mutation listing every node
    /// (`f.walk().collect()`) reaches the assertion, where `classify_sites`
    /// panics on the first non-dispatch node.
    #[test]
    fn classify_sites_lists_nothing_on_a_site_free_function() {
        let (function, _target) =
            empty_graph_returning(|fb| fb.build_int_const(0x1234u64, ValueType::int(64)).unwrap());
        let unresolved = ClassifiedSites::unresolved(&function);
        let classified = classify_sites(&function, None).expect("classify");
        for (name, found) in [("unresolved", unresolved), ("classified", classified)] {
            assert!(
                found.sites.is_empty() && found.resolved.is_empty(),
                "{name}"
            );
        }
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

        let found = classify_sites(&function, None).expect("classify");
        assert_eq!(found.sites, vec![branch]);
        assert!(found.resolved.is_empty());
    }

    /// `unresolved` still lists every site, so the orchestrator still reports
    /// it, and recovers nothing where `classify_sites` does. Mutation:
    /// `unresolved` lists no site.
    #[test]
    fn unresolved_lists_the_sites_classify_sites_resolves() {
        let mut b = strider_ir_test_utils::empty_builder().expect("empty_builder");
        let region = b.create_region_all().expect("create_region");
        b.set_entry_region_all(region).expect("set_entry");
        b.set_region(region);
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let target = b.build_int_const(0x8000u64, ValueType::int(64)).unwrap();
        let branch = b.build_indirect_branch(target).expect("build_branch");
        b.set_lift_addr(None);
        let function = b.build().expect("build");

        let on = classify_sites(&function, None).expect("on");
        assert_eq!(
            on.resolved.get(&branch),
            Some(&ResolvedTargets::Single(0x8000.into()))
        );
        let off = ClassifiedSites::unresolved(&function);
        assert_eq!(off.sites, vec![branch]);
        assert!(off.resolved.is_empty());
    }

    #[test]
    fn classify_unrelated_node_kind_returns_none() {
        let (function, target) = empty_graph_returning(|fb| {
            let lhs = fb.build_int_const(1u64, ValueType::int(64)).unwrap();
            let rhs = fb.build_int_const(2u64, ValueType::int(64)).unwrap();
            fb.build_int_binary_operation(
                lhs,
                rhs,
                strider_ir::IntBinaryOp::Add,
                ValueType::int(64),
            )
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
            let mask = b
                .build_int_const(0xFFFF_FFF0u64, ValueType::int(32))
                .unwrap();
            b.build_int_binary_operation(sp_val, mask, IntBinaryOp::And, ValueType::int(32))
                .unwrap()
        } else {
            sp_val
        };
        let at = |b: &mut FunctionBuilder, base: ValueId, off: u64| {
            let c = b.build_int_const(off, ValueType::int(32)).unwrap();
            b.build_int_binary_operation(base, c, IntBinaryOp::Add, ValueType::int(32))
                .unwrap()
        };
        let slot = at(&mut b, frame, 0x8000_0000);
        b.build_store(slot, lr_val, rsleigh::VnSpace::RAM).unwrap();
        if let Some(off) = store {
            let near = at(&mut b, sp_val, off);
            let junk = b
                .build_int_const(0xDEAD_BEEFu64, ValueType::int(32))
                .unwrap();
            b.build_store(near, junk, rsleigh::VnSpace::RAM).unwrap();
        }
        let target = b
            .build_load(slot, rsleigh::VnSpace::RAM, ValueType::int(32))
            .unwrap();
        let branch = b.build_indirect_branch(target).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();
        // The hand-built reads sit behind single-input `VarPhi`s.
        let mut prep = crate::OptimizerPipeline::new();
        prep.add(crate::PhiCollapse);
        prep.add(crate::RegionCollapse);
        prep.run(&mut function, &crate::OptCtx::new(None))
            .expect("prep");
        (function, branch, target)
    }

    #[test]
    fn a_store_wrapping_onto_the_saved_return_slot_clobbers_it() {
        for aligned in [false, true] {
            let (f, branch, target) = lr_saved_across_the_wrap(aligned, None);
            assert!(
                ReturnTargets::new(&f).returns_to_caller(branch, target),
                "control, aligned={aligned}: the untouched slot holds lr"
            );
            let (f, branch, target) = lr_saved_across_the_wrap(aligned, Some(0x7FFF_FFFE));
            assert!(
                !ReturnTargets::new(&f).returns_to_caller(branch, target),
                "aligned={aligned}: sp+0x7FFF_FFFE overwrites the slot's low half mod 2^32"
            );
        }
        let (f, branch, target) = lr_saved_across_the_wrap(false, Some(0x7FFF_FFFC));
        assert!(
            ReturnTargets::new(&f).returns_to_caller(branch, target),
            "sp+0x7FFF_FFFC ends where the slot starts"
        );
    }

    /// A 128-bit frame saving `lr` at `(sp & -16) + 2^127`, the bottom of the
    /// `i128` carrier, then, when `junk`, a 16-byte store at `sp`.
    fn lr_saved_at_the_carrier_limit(junk: bool) -> (strider_ir::Function, NodeId, ValueId) {
        let sp = fake_reg_vn(0x20, 16);
        let lr = fake_reg_vn(0x4c, 16);
        let mut b = strider_ir_test_utils::sp_frame(sp)
            .tracked(lr)
            .link_register(lr)
            .build_fn_single_region()
            .expect("build_fn_single_region");
        let sp_val = b.read_variable(&sp).unwrap();
        let lr_val = b.read_variable(&lr).unwrap();
        let mask = b.build_int_const(!0xFu128, ValueType::int(128)).unwrap();
        let frame = b
            .build_int_binary_operation(sp_val, mask, IntBinaryOp::And, ValueType::int(128))
            .unwrap();
        let bottom = b
            .build_int_const(1u128 << 127, ValueType::int(128))
            .unwrap();
        let slot = b
            .build_int_binary_operation(frame, bottom, IntBinaryOp::Add, ValueType::int(128))
            .unwrap();
        b.build_store(slot, lr_val, rsleigh::VnSpace::RAM).unwrap();
        if junk {
            let junk = b
                .build_int_const(0xDEAD_BEEFu64, ValueType::int(128))
                .unwrap();
            b.build_store(sp_val, junk, rsleigh::VnSpace::RAM).unwrap();
        }
        let target = b
            .build_load(slot, rsleigh::VnSpace::RAM, ValueType::int(128))
            .unwrap();
        let branch = b.build_indirect_branch(target).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();
        let mut prep = crate::OptimizerPipeline::new();
        prep.add(crate::PhiCollapse);
        prep.add(crate::RegionCollapse);
        prep.run(&mut function, &crate::OptCtx::new(None))
            .expect("prep");
        (function, branch, target)
    }

    /// Placing the store at `sp` against the slot's span runs past the
    /// carrier, which fails closed.
    #[test]
    fn a_slot_at_the_carrier_limit_fails_closed() {
        let (f, branch, target) = lr_saved_at_the_carrier_limit(false);
        assert!(
            ReturnTargets::new(&f).returns_to_caller(branch, target),
            "control: the untouched slot holds lr"
        );
        let (f, branch, target) = lr_saved_at_the_carrier_limit(true);
        assert!(!ReturnTargets::new(&f).returns_to_caller(branch, target));
    }

    /// A 32-bit link-register function that calls `ALLOCATOR`, saves `lr` at
    /// `sp - 4`, writes through the call's result or to a global, and branches
    /// through the reloaded slot.  No `LoadForward` runs, so the walk meets
    /// the write itself.
    fn lr_saved_past_an_off_stack_write(
        heap: bool,
        claims: &strider_ir::AssumptionOptions,
    ) -> (strider_ir::Function, NodeId, ValueId) {
        const ALLOCATOR: u64 = 0x1000;
        let sp = strider_ir_test_utils::stack_vn_x86();
        let lr = fake_reg_vn(0x4c, 4);
        let r0 = fake_reg_vn(0x00, 4);
        let mut b = strider_ir_test_utils::sp_frame(sp)
            .tracked(lr)
            .link_register(lr)
            .tracked(r0)
            .ret(r0)
            .build_fn_single_region()
            .expect("build_fn_single_region");
        let sp_val = b.read_variable(&sp).unwrap();
        let lr_val = b.read_variable(&lr).unwrap();
        let callee = b.build_int_const(ALLOCATOR, ValueType::int(32)).unwrap();
        let (_, rets) = b.build_call(callee, &[], &[r0], 0).unwrap();
        let four = b
            .build_int_const((-4i64) as u64, ValueType::int(32))
            .unwrap();
        let slot = b
            .build_int_binary_operation(sp_val, four, IntBinaryOp::Add, ValueType::int(32))
            .unwrap();
        b.build_store(slot, lr_val, rsleigh::VnSpace::RAM).unwrap();
        let at = if heap {
            rets[0]
        } else {
            b.build_int_const(0x4000u64, ValueType::int(32)).unwrap()
        };
        let junk = b
            .build_int_const(0xDEAD_BEEFu64, ValueType::int(32))
            .unwrap();
        b.build_store(at, junk, rsleigh::VnSpace::RAM).unwrap();
        let target = b
            .build_load(slot, rsleigh::VnSpace::RAM, ValueType::int(32))
            .unwrap();
        let branch = b.build_indirect_branch(target).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();
        function.assume(claims);
        let mut prep = crate::OptimizerPipeline::new();
        prep.add(crate::PhiCollapse);
        prep.add(crate::RegionCollapse);
        prep.run(&mut function, &crate::OptCtx::new(None))
            .expect("prep");
        (function, branch, target)
    }

    /// With `return_slots_survive_untracked_writes` off, a write a held claim
    /// puts off the stack passes the slot without `LoadForward` having
    /// stepped the reload past it; at the floor it clobbers.
    ///
    /// Mutations: drop the constant arm of `off_stack`; drop its heap arm.
    #[test]
    fn an_off_stack_write_passes_the_slot_under_its_claim() {
        let none = strider_ir::AssumptionOptions::none();
        let claimed = |heap| {
            let claims = if heap {
                strider_ir::AssumptionOptions {
                    noalias_allocators: [0x1000].into_iter().collect(),
                    ..strider_ir::AssumptionOptions::none()
                }
            } else {
                strider_ir::AssumptionOptions {
                    stack_global_disjoint: true,
                    ..strider_ir::AssumptionOptions::none()
                }
            };
            let (f, branch, target) = lr_saved_past_an_off_stack_write(heap, &claims);
            ReturnTargets::new(&f).returns_to_caller(branch, target)
        };
        for heap in [false, true] {
            let (f, branch, target) = lr_saved_past_an_off_stack_write(heap, &none);
            assert!(
                !ReturnTargets::new(&f).returns_to_caller(branch, target),
                "heap={heap}: at the floor the write may land on the slot"
            );
            assert!(claimed(heap), "heap={heap}: the claim keeps it off");
        }
    }

    /// A 128-bit frame saving `sp + saved` at `sp + 0x40` and `lr` at
    /// `Add(..Add(fp, addends[0]).., addends[n])`, `fp` the reloaded save, so
    /// the slot is reached only through a guess.  When `junk` is given, a
    /// store at `fp + junk` follows.
    fn lr_saved_off_a_reloaded_sp(
        saved: i128,
        addends: &[i128],
        junk: Option<i128>,
    ) -> (strider_ir::Function, NodeId, ValueId) {
        let sp = fake_reg_vn(0x20, 16);
        let lr = fake_reg_vn(0x4c, 16);
        let mut b = strider_ir_test_utils::sp_frame(sp)
            .tracked(lr)
            .link_register(lr)
            .build_fn_single_region()
            .expect("build_fn_single_region");
        let sp_val = b.read_variable(&sp).unwrap();
        let lr_val = b.read_variable(&lr).unwrap();
        let add = |b: &mut FunctionBuilder, base: ValueId, off: i128| {
            let c = b.build_int_const(off as u128, ValueType::int(128)).unwrap();
            b.build_int_binary_operation(base, c, IntBinaryOp::Add, ValueType::int(128))
                .unwrap()
        };
        let save = add(&mut b, sp_val, 0x40);
        let saved = add(&mut b, sp_val, saved);
        b.build_store(save, saved, rsleigh::VnSpace::RAM).unwrap();
        let fp = b
            .build_load(save, rsleigh::VnSpace::RAM, ValueType::int(128))
            .unwrap();
        let slot = addends.iter().fold(fp, |at, &off| add(&mut b, at, off));
        b.build_store(slot, lr_val, rsleigh::VnSpace::RAM).unwrap();
        if let Some(off) = junk {
            let near = add(&mut b, fp, off);
            let junk = b
                .build_int_const(0xDEAD_BEEFu64, ValueType::int(128))
                .unwrap();
            b.build_store(near, junk, rsleigh::VnSpace::RAM).unwrap();
        }
        let target = b
            .build_load(slot, rsleigh::VnSpace::RAM, ValueType::int(128))
            .unwrap();
        let branch = b.build_indirect_branch(target).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();
        let mut prep = crate::OptimizerPipeline::new();
        prep.add(crate::PhiCollapse);
        prep.add(crate::RegionCollapse);
        prep.run(&mut function, &crate::OptCtx::new(None))
            .expect("prep");
        (function, branch, target)
    }

    /// Guessing and proving an offset past the `i128` carrier fails closed,
    /// at each place the arithmetic can leave it; the controls stand at the
    /// same edge without crossing it.
    #[test]
    fn an_offset_guessed_past_the_carrier_fails_closed() {
        let verified = |saved, addends: &[i128], junk| {
            let (f, branch, target) = lr_saved_off_a_reloaded_sp(saved, addends, junk);
            ReturnTargets::new(&f).returns_to_caller(branch, target)
        };
        assert!(verified(16, &[8, 8], Some(-64)), "control");
        assert!(
            verified(i128::MAX, &[-16], Some(-64)),
            "control at the edge"
        );
        assert!(
            !verified(i128::MAX, &[1], None),
            "the slot's offset off the spine runs past the carrier"
        );
        assert!(
            !verified(0, &[i128::MAX, 1], None),
            "the addends sum past the carrier"
        );
        assert!(
            !verified(i128::MAX, &[-16], Some(1)),
            "a remembered guess plus the junk store's addend runs past it"
        );
        assert!(
            !verified(i128::MAX, &[1, -1], None),
            "the slot is in range, but the inner sum and its claim are not"
        );
    }

    /// At I256 the `u128` constant `2^128 - 2` also clears bits 128..256, so
    /// it is arithmetic on the target, not the low-bit ISA-mode mask.
    #[test]
    fn a_mask_past_the_u128_carrier_is_not_the_isa_mode_mask() {
        let wide = ValueType::int(256);
        let (function, masked) = empty_graph_returning(|fb| {
            let target = fb.build_int_const_limbs(&[1, 2, 3, 4], wide).unwrap();
            let low_bit_clear = fb.build_int_const(u128::MAX ^ 1, wide).unwrap();
            fb.build_int_binary_operation(target, low_bit_clear, IntBinaryOp::And, wide)
                .unwrap()
        });
        assert_eq!(strip_isa_mode_mask(&function, masked), masked);
    }
}
