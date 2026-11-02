//! These build nodes directly rather than lifting, so the classifier's
//! match arms are exercised in isolation.

use super::*;
use crate::test_support::{COLLAPSE, at, run_passes};
use strider_ir::node::{IntBinaryOp, NodeKind, ValueType};
use strider_ir::{AssumptionOptions, FunctionBuilder, IRBuilderExt, IRViewer, UBig};
use strider_ir_test_utils::{IrWalkerEx, RegisterSet, reg_vn as fake_reg_vn, sp_frame};

/// Runs the two value-based recognisers on a dispatch value directly.
///
/// The entry stands in for the branch site: these targets are an
/// `InitialVar` or an `IntConst`, so no slot is followed from it.
fn classify_target_bare(
    function: &strider_ir::Function,
    target_value: ValueId,
) -> Option<ResolvedTargets> {
    let mut sc = SiteClassifier::new(function, None);
    sc.single_const_target(target_value, None)
        .or_else(|| sc.link_register_return(function.entry(), target_value))
}

/// A register-free builder in its sole, entry, region.
fn bare() -> FunctionBuilder {
    RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn_single_region")
}

/// `b` built and brought to the converged shape (no single-input `VarPhi`s)
/// under `claims`.
fn finish(mut b: FunctionBuilder, claims: &AssumptionOptions) -> strider_ir::Function {
    b.set_lift_addr(None);
    let mut function = b.build().expect("build");
    function.assume(claims);
    run_passes(&mut function, COLLAPSE).expect("prep");
    function
}

/// An empty region terminated by a Return over the caller-supplied value,
/// so the classifier sees a real validation-passing graph.
fn empty_graph_returning(
    target_inputs: impl FnOnce(&mut FunctionBuilder) -> ValueId,
) -> (strider_ir::Function, ValueId) {
    let mut builder = bare();
    let target = target_inputs(&mut builder);
    builder
        .build_return(Some(target), &[])
        .expect("build_return");
    builder.set_lift_addr(None);
    // `build` is a move, but `ValueId` is a stable entity index.
    (builder.build().expect("build"), target)
}

/// An interworking branch carries its ISA-mode bit as slot 3; the resolver
/// surfaces it, read live off the placeholder. A 3-input branch has no mode,
/// so it keeps the flowing mode.
/// MIPS64 `ext i,w,26,6; andi i,i,0xff` on the sign-extended word `w`:
/// `((sext64(w) << 32) >> 58) & 0xff`.  Shared with the table and range
/// tests.
pub(super) fn ext_26_6(b: &mut FunctionBuilder, w: ValueId) -> ValueId {
    let i64t = ValueType::int(64);
    let wide = b
        .extend_if_needed(w, i64t, strider_ir::ExtendOp::SignExtend)
        .unwrap();
    let up = b
        .build_const_binop(32u64, wide, IntBinaryOp::ShiftLeft, i64t)
        .unwrap();
    let field = b
        .build_const_binop(58u64, up, IntBinaryOp::ShiftRight, i64t)
        .unwrap();
    b.build_const_binop(0xffu64, field, IntBinaryOp::And, i64t)
        .unwrap()
}

/// MIPS64 `srl t,w,lo; sltiu t,t,bound` on the `I32` word `w`:
/// `sext64(w >> lo) < bound`.  Shared with the table and range tests.
pub(super) fn srl_sltiu(b: &mut FunctionBuilder, w: ValueId, lo: u64, bound: u64) -> ValueId {
    let (i32t, i64t) = (ValueType::int(32), ValueType::int(64));
    let field = b
        .build_const_binop(lo, w, IntBinaryOp::ShiftRight, i32t)
        .unwrap();
    let wide = b
        .extend_if_needed(field, i64t, strider_ir::ExtendOp::SignExtend)
        .unwrap();
    let n = b.build_int_const(bound, i64t).unwrap();
    b.build_int_cmp_operation(wide, n, strider_ir::IntCmpOp::Less, i64t)
        .unwrap()
}

#[test]
fn indirect_branch_carries_its_mode_input() {
    for with_mode in [true, false] {
        let mut builder = bare();
        let mode = builder.build_int_const(1u64, ValueType::int(64)).unwrap();
        let target = builder
            .build_int_const(0x8000u64, ValueType::int(64))
            .unwrap();
        let mode = with_mode.then_some(mode);
        let branch = builder
            .build_indirect_branch_with_mode(target, mode)
            .expect("build_branch");
        builder.set_lift_addr(None);
        let function = builder.build().expect("build");
        assert_eq!(function.indirect_branch_isa_mode(branch), mode);
    }
}

#[test]
fn const_target_with_unfoldable_mode_defers() {
    // A `bx <const>` whose committed ISA mode does not fold to a constant
    // must fail closed: the callee's mode is unknown, so defer rather than
    // decode the const target in the flowing mode (the table path already
    // fails closed here via `?`).
    let mut builder = bare();
    let target = builder
        .build_int_const(0x8000u64, ValueType::int(64))
        .unwrap();
    // A symbolic (non-IntConst) mode, as a runtime `entry & 1` would be.
    let mode = builder
        .build_const_binop(1u64, target, IntBinaryOp::And, ValueType::int(64))
        .unwrap();
    builder
        .build_indirect_branch_with_mode(target, Some(mode))
        .expect("build_branch");
    builder.set_lift_addr(None);
    let function = builder.build().expect("build");

    let sc = SiteClassifier::new(&function, None);
    assert_eq!(sc.single_const_target(target, Some(mode)), None);
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

/// The link-register recogniser over a returned register read: only the
/// declared link register's own entry value answers `LinkRegister`. `lr + 1`
/// under a declared link register reaches the recogniser's fallback arm, not
/// the missing-register exit.
///
/// Mutation: answer `LinkRegister` from that fallback arm.
#[test]
fn classify_initial_var_against_the_link_register() {
    let lr = fake_reg_vn(0x4c, 4);
    let other = fake_reg_vn(0x10, 4);
    let rows = [
        (
            "the link register",
            lr,
            Some(lr),
            false,
            Some(ResolvedTargets::LinkRegister),
        ),
        ("another register", other, Some(lr), false, None),
        ("no link register (x86)", lr, None, false, None),
        ("lr + 1", lr, Some(lr), true, None),
    ];
    for (name, read, declared, plus_one, want) in rows {
        let mut regs = RegisterSet::new().tracked(read);
        if let Some(declared) = declared {
            regs = regs.link_register(declared);
        }
        let mut b = regs
            .build_fn_single_region()
            .expect("build_fn_single_region");
        let mut target = b.read_variable(&read).expect("read_variable");
        if plus_one {
            target = b
                .build_const_binop(1u64, target, IntBinaryOp::Add, ValueType::int(32))
                .expect("lr + 1");
        }
        b.build_return(Some(target), &[]).expect("build_return");
        let function = finish(b, &AssumptionOptions::none());
        let target = function.returned();
        assert_eq!(
            plus_one,
            matches!(function.kind_of_value(target), NodeKind::IntBinaryOp(_)),
            "{name}: fixture shape"
        );
        assert_eq!(classify_target_bare(&function, target), want, "{name}");
    }
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
    let mut b = bare();
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

/// `sp_frame(sp)` with `lr` tracked as the link register.
fn lr_frame(sp: rsleigh::Vn, lr: rsleigh::Vn) -> RegisterSet {
    sp_frame(sp).tracked(lr).link_register(lr)
}

/// Branches through `slot`'s reload and finishes `b` under `claims`.
fn return_through(
    mut b: FunctionBuilder,
    slot: ValueId,
    ty: ValueType,
    claims: &AssumptionOptions,
) -> (strider_ir::Function, NodeId, ValueId) {
    let target = b.build_load(slot, rsleigh::VnSpace::RAM, ty).unwrap();
    let branch = b.build_indirect_branch(target).unwrap();
    (finish(b, claims), branch, target)
}

/// A 32-bit link-register function saving `lr` at `sp + 0x8000_0000` (off an
/// `And`-aligned SP when `aligned`), writing 4 bytes at `sp + store` when
/// given, then branching through the reloaded slot.
fn lr_saved_across_the_wrap(
    aligned: bool,
    store: Option<i64>,
) -> (strider_ir::Function, NodeId, ValueId) {
    let sp = strider_ir_test_utils::stack_vn_x86();
    let lr = fake_reg_vn(0x4c, 4);
    let word = ValueType::int(32);
    let mut b = lr_frame(sp, lr).build_fn_single_region().unwrap();
    let sp_val = b.read_variable(&sp).unwrap();
    let lr_val = b.read_variable(&lr).unwrap();
    let frame = if aligned {
        b.build_const_binop(0xFFFF_FFF0u64, sp_val, IntBinaryOp::And, word)
            .unwrap()
    } else {
        sp_val
    };
    let slot = at(&mut b, frame, 0x8000_0000, word).unwrap();
    b.build_store(slot, lr_val, rsleigh::VnSpace::RAM).unwrap();
    if let Some(off) = store {
        let near = at(&mut b, sp_val, off, word).unwrap();
        let junk = b.build_int_const(0xDEAD_BEEFu64, word).unwrap();
        b.build_store(near, junk, rsleigh::VnSpace::RAM).unwrap();
    }
    return_through(b, slot, word, &AssumptionOptions::none())
}

#[test]
fn a_store_wrapping_onto_the_saved_return_slot_clobbers_it() {
    let verified = |aligned, store| {
        let (f, branch, target) = lr_saved_across_the_wrap(aligned, store);
        ReturnTargets::new(&f).returns_to_caller(branch, target)
    };
    for aligned in [false, true] {
        assert!(
            verified(aligned, None),
            "control, aligned={aligned}: the untouched slot holds lr"
        );
        assert!(
            !verified(aligned, Some(0x7FFF_FFFE)),
            "aligned={aligned}: sp+0x7FFF_FFFE overwrites the slot's low half mod 2^32"
        );
    }
    assert!(
        verified(false, Some(0x7FFF_FFFC)),
        "sp+0x7FFF_FFFC ends where the slot starts"
    );
    assert!(
        !verified(true, Some(0x7FFF_FFFC)),
        "the alignment mask can put the slot up to 15 bytes lower, where \
         that same store reaches it"
    );
}

/// A 128-bit frame saving `lr` at `(sp & -16) + 2^127`, the bottom of the
/// `i128` carrier, then, when `junk`, a 16-byte store at `sp`.
fn lr_saved_at_the_carrier_limit(junk: bool) -> (strider_ir::Function, NodeId, ValueId) {
    let sp = fake_reg_vn(0x20, 16);
    let lr = fake_reg_vn(0x4c, 16);
    let wide = ValueType::int(128);
    let mut b = lr_frame(sp, lr).build_fn_single_region().unwrap();
    let sp_val = b.read_variable(&sp).unwrap();
    let lr_val = b.read_variable(&lr).unwrap();
    let frame = b
        .build_const_binop(!0xFu128, sp_val, IntBinaryOp::And, wide)
        .unwrap();
    let slot = b
        .build_const_binop(1u128 << 127, frame, IntBinaryOp::Add, wide)
        .unwrap();
    b.build_store(slot, lr_val, rsleigh::VnSpace::RAM).unwrap();
    if junk {
        let junk = b.build_int_const(0xDEAD_BEEFu64, wide).unwrap();
        b.build_store(sp_val, junk, rsleigh::VnSpace::RAM).unwrap();
    }
    return_through(b, slot, wide, &AssumptionOptions::none())
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
    claims: &AssumptionOptions,
) -> (strider_ir::Function, NodeId, ValueId) {
    const ALLOCATOR: u64 = 0x1000;
    let sp = strider_ir_test_utils::stack_vn_x86();
    let lr = fake_reg_vn(0x4c, 4);
    let r0 = fake_reg_vn(0x00, 4);
    let word = ValueType::int(32);
    let mut b = lr_frame(sp, lr)
        .tracked(r0)
        .ret(r0)
        .build_fn_single_region()
        .unwrap();
    let sp_val = b.read_variable(&sp).unwrap();
    let lr_val = b.read_variable(&lr).unwrap();
    let callee = b.build_int_const(ALLOCATOR, word).unwrap();
    let (_, rets) = b.build_call(callee, &[], &[r0], 0).unwrap();
    let slot = at(&mut b, sp_val, -4, word).unwrap();
    b.build_store(slot, lr_val, rsleigh::VnSpace::RAM).unwrap();
    let to = if heap {
        rets[0]
    } else {
        b.build_int_const(0x4000u64, word).unwrap()
    };
    let junk = b.build_int_const(0xDEAD_BEEFu64, word).unwrap();
    b.build_store(to, junk, rsleigh::VnSpace::RAM).unwrap();
    return_through(b, slot, word, claims)
}

/// With `return_slots_survive_untracked_writes` off, a write a held claim
/// puts off the stack passes the slot without `LoadForward` having
/// stepped the reload past it; at the floor it clobbers.
///
/// Mutations: drop `store_effect`'s constant arm; let its heap spine clobber.
#[test]
fn an_off_stack_write_passes_the_slot_under_its_claim() {
    let verified = |heap, claims: &AssumptionOptions| {
        let (f, branch, target) = lr_saved_past_an_off_stack_write(heap, claims);
        ReturnTargets::new(&f).returns_to_caller(branch, target)
    };
    for heap in [false, true] {
        let claims = if heap {
            AssumptionOptions {
                noalias_allocators: [0x1000].into_iter().collect(),
                ..AssumptionOptions::none()
            }
        } else {
            AssumptionOptions {
                stack_global_disjoint: true,
                ..AssumptionOptions::none()
            }
        };
        assert!(
            !verified(heap, &AssumptionOptions::none()),
            "heap={heap}: at the floor the write may land on the slot"
        );
        assert!(
            verified(heap, &claims),
            "heap={heap}: the claim keeps it off"
        );
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
    let wide = ValueType::int(128);
    let mut b = lr_frame(sp, lr).build_fn_single_region().unwrap();
    let sp_val = b.read_variable(&sp).unwrap();
    let lr_val = b.read_variable(&lr).unwrap();
    let add = |b: &mut FunctionBuilder, base: ValueId, off: i128| {
        b.build_const_binop(off as u128, base, IntBinaryOp::Add, wide)
            .unwrap()
    };
    let save = add(&mut b, sp_val, 0x40);
    let saved = add(&mut b, sp_val, saved);
    b.build_store(save, saved, rsleigh::VnSpace::RAM).unwrap();
    let fp = b.build_load(save, rsleigh::VnSpace::RAM, wide).unwrap();
    let slot = addends.iter().fold(fp, |at, &off| add(&mut b, at, off));
    b.build_store(slot, lr_val, rsleigh::VnSpace::RAM).unwrap();
    if let Some(off) = junk {
        let near = add(&mut b, fp, off);
        let junk = b.build_int_const(0xDEAD_BEEFu64, wide).unwrap();
        b.build_store(near, junk, rsleigh::VnSpace::RAM).unwrap();
    }
    return_through(b, slot, wide, &AssumptionOptions::none())
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

/// Two return sites of one function whose verifications both reach the
/// stack value `sp + 0x20`.
///
/// Arm A loads a frame pointer out of the slot at `sp + 0x60`, which only
/// ever held that value, saves `lr` through it and branches back: it
/// returns to the caller. Arm B loads one out of the slot at `sp + 0x50`,
/// which a store through a spine-less address overwrote with the same
/// value: the optimistic walk steps past that store and guesses `0x18`,
/// the exhaustive one locates it and finds `sp + 0x20` there, so the site
/// does not verify.
///
/// Built at the floor, with no claim, which is what makes the spine-less
/// store address located rather than passed.
fn two_sites_claiming_one_stack_value()
-> (strider_ir::Function, (NodeId, ValueId), (NodeId, ValueId)) {
    let sp = strider_ir_test_utils::stack_vn_x86();
    let lr = fake_reg_vn(0x4c, 4);
    let word = ValueType::int(32);
    let mut b = lr_frame(sp, lr).build_fn().expect("build_fn");
    let entry = b.create_region_all().expect("entry");
    let arm_a = b.create_region_all().expect("arm_a");
    let arm_b = b.create_region_all().expect("arm_b");
    b.set_entry_region_all(entry).expect("set_entry_region_all");
    b.record_register_arg_carriers();
    b.set_region(entry);
    let sp_val = b.read_variable(&sp).expect("read sp");
    let lr_val = b.read_variable(&lr).expect("read lr");
    let sp_at = |b: &mut FunctionBuilder, off| at(b, sp_val, off, word).unwrap();
    let slot_b = sp_at(&mut b, 0x50);
    let indirect = sp_at(&mut b, 0x70);
    let other = sp_at(&mut b, 0x18);
    let shared = sp_at(&mut b, 0x20);
    let slot_a = sp_at(&mut b, 0x60);
    b.build_store(indirect, slot_b, rsleigh::VnSpace::RAM)
        .expect("store the address");
    // `sp + 0x50`, off the address spine: a guess steps past a store
    // through it, a proof locates it.
    let opaque = b
        .build_load(indirect, rsleigh::VnSpace::RAM, word)
        .expect("load the address");
    b.build_store(slot_a, shared, rsleigh::VnSpace::RAM)
        .expect("store A");
    b.build_store(slot_b, other, rsleigh::VnSpace::RAM)
        .expect("store B");
    b.build_store(opaque, shared, rsleigh::VnSpace::RAM)
        .expect("store through the opaque address");
    let cond = b
        .build_int_cmp_operation(shared, other, strider_ir::IntCmpOp::Less, word)
        .expect("cond");
    b.build_if(cond, arm_a, arm_b).expect("if");

    let site = |b: &mut FunctionBuilder, region, slot| {
        b.set_region(region);
        let frame = b.build_load(slot, rsleigh::VnSpace::RAM, word).unwrap();
        b.build_store(frame, lr_val, rsleigh::VnSpace::RAM).unwrap();
        let target = b.build_load(frame, rsleigh::VnSpace::RAM, word).unwrap();
        (b.build_indirect_branch(target).unwrap(), target)
    };
    let a = site(&mut b, arm_a, slot_a);
    let bb = site(&mut b, arm_b, slot_b);
    (finish(b, &AssumptionOptions::none()), a, bb)
}

/// One verifier answers each return site the same whichever site it was
/// asked about first.
///
/// Mutation: lift the contradiction ledger back to struct scope.
#[test]
fn a_shared_verifier_answers_each_return_site_the_same() {
    let (f, a, b) = two_sites_claiming_one_stack_value();
    let alone =
        |(ret, target): (NodeId, ValueId)| ReturnTargets::new(&f).returns_to_caller(ret, target);
    assert!(alone(a), "fixture: arm A returns to the caller");
    assert!(!alone(b), "fixture: arm B cannot be shown to");

    let mut first_a = ReturnTargets::new(&f);
    let a_first = (
        first_a.returns_to_caller(a.0, a.1),
        first_a.returns_to_caller(b.0, b.1),
    );
    let mut first_b = ReturnTargets::new(&f);
    let b_first = (
        first_b.returns_to_caller(b.0, b.1),
        first_b.returns_to_caller(a.0, a.1),
    );
    assert_eq!(
        (a_first.0, a_first.1),
        (b_first.1, b_first.0),
        "one verifier's verdicts depend on the order the sites were asked"
    );
}

/// At I256 the `u128` constant `2^128 - 2` also clears bits 128..256, so
/// it is arithmetic on the target, not the low-bit ISA-mode mask.
#[test]
fn a_mask_past_the_u128_carrier_is_not_the_isa_mode_mask() {
    let wide = ValueType::int(256);
    let (function, masked) = empty_graph_returning(|fb| {
        let target = fb
            .build_int_const(UBig::from_words(&[1, 2, 3, 4]), wide)
            .unwrap();
        let low_bit_clear = fb.build_int_const(u128::MAX ^ 1, wide).unwrap();
        fb.build_int_binary_operation(target, low_bit_clear, IntBinaryOp::And, wide)
            .unwrap()
    });
    assert_eq!(strip_isa_mode_mask(&function, masked), masked);
}
