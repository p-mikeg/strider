//! Tests for the dominator-scoped integer range analysis.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use strider_ir::node::{NodeId, NodeKind, ValueId, ValueType};
use strider_ir::{IRBuilderExt, IRViewer, IntBinaryOp, IntCmpOp, control_dominators};
use strider_ir_test_utils::{RegisterSet, SENTINEL_LIFT_ADDR};

use super::compute_value_ranges;
use crate::analyze_known_bits;

// ---------------------------------------------------------------------------
// Helper: Build a "guarded dispatch" function:
//
//   entry_region:
//     cond = IntCmpOp::Less(idx, IntConst(N))
//     If(cond) -> dispatch_region (true), exit_region (false)
//   dispatch_region:
//     Return(idx)
//   exit_region:
//     Return(idx)
//
// Returns (Function, idx_value, dispatch_region_node_id, exit_region_node_id).
// ---------------------------------------------------------------------------
fn build_guarded_dispatch(
    bound: u64,
    ty: ValueType,
) -> (strider_ir::Function, ValueId, NodeId, NodeId) {
    let mut b = RegisterSet::new().build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    let entry = b.create_region().unwrap();
    let dispatch = b.create_region().unwrap();
    let exit = b.create_region().unwrap();

    b.set_entry_region(entry).unwrap();

    // entry region: build idx (non-const — use a bitwise-and to make it a real
    // computation), then branch on Less(idx, bound).
    b.set_region(entry);
    // idx = IntConst(0) | IntConst(0) — simple non-const proxy for an unknown idx.
    // Actually we want something that has no KB info so we use an unconstrained
    // value: load from a dummy address.
    let dummy_addr = b.build_int_const(0xDEAD_u64, ValueType::I64).unwrap();
    let idx = b.build_load(dummy_addr, rsleigh::VnSpace::RAM, ty).unwrap();
    let bound_c = b.build_int_const(bound, ty).unwrap();
    let cond = b
        .build_int_cmp_operation(idx, bound_c, IntCmpOp::Less, ty)
        .unwrap();
    b.build_if(cond, dispatch, exit).unwrap();

    // dispatch region: return idx (true successor of If).
    b.set_region(dispatch);
    let dispatch_ctrl_val = b.region_cur_ctrl(dispatch);
    b.build_return(Some(idx), &[]).unwrap();

    // exit region: return idx (false successor of If).
    b.set_region(exit);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_lift_addr(None);
    let f = b.build().unwrap();

    // Find dispatch_region NodeId by finding the Region whose control value was
    // `dispatch_ctrl_val` — that value is produced by the Region node itself.
    let dispatch_node = f.graph().producer(dispatch_ctrl_val);

    // Find exit_region NodeId: the Region node that is NOT the entry (entry has
    // no control inputs) and is NOT the dispatch node.
    let entry_node = f.entry().unwrap();
    let exit_node = f
        .graph()
        .all_node_ids()
        .find(|&n| {
            matches!(f.node_kind(n), NodeKind::Region) && n != entry_node && n != dispatch_node
        })
        .expect("exit_region must exist");

    (f, idx, dispatch_node, exit_node)
}

// ---------------------------------------------------------------------------
// Test 1: strict-less guard → [0, N-1]
// ---------------------------------------------------------------------------
#[test]
fn strict_less_guard_bounds_index_on_true_edge() {
    // if idx < 8 on true edge → idx ∈ [0, 7]
    let (f, idx, dispatch_region, _exit) = build_guarded_dispatch(8, ValueType::I32);
    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let ranges = compute_value_ranges(&f, &doms, &known);

    let iv = ranges.range_of(idx, dispatch_region);
    assert_eq!(iv.lo, 0, "lower bound must be 0");
    assert_eq!(iv.hi, 7, "upper bound must be 7 for idx < 8");
}

// A guard on `Add(X, const)` must propagate the bound back to `X` (shifted by
// `-const`).  This is the masked-Thumb shape: the guard bounds `(kind&7) - 1`,
// and the dispatch indexes `kind&7`, so `kind&7 = ((kind&7)-1) + 1` must inherit
// `[1, 7]` from a guard `((kind&7)-1) < 7`.
#[test]
fn guard_on_add_propagates_bound_back_to_operand() {
    let ty = ValueType::I32;
    let mut b = RegisterSet::new().build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let entry = b.create_region().unwrap();
    let dispatch = b.create_region().unwrap();
    let exit = b.create_region().unwrap();
    b.set_entry_region(entry).unwrap();
    b.set_region(entry);
    // X = load (no KB info), diff = X + (-1), guard `diff < 7`.
    let dummy_addr = b.build_int_const(0xDEAD_u64, ValueType::I64).unwrap();
    let x = b.build_load(dummy_addr, rsleigh::VnSpace::RAM, ty).unwrap();
    let neg1 = b.build_int_const((0u128).wrapping_sub(1), ty).unwrap();
    let diff = b
        .build_int_binary_operation(x, neg1, IntBinaryOp::Add, ty)
        .unwrap();
    let seven = b.build_int_const(7u64, ty).unwrap();
    let cond = b
        .build_int_cmp_operation(diff, seven, IntCmpOp::Less, ty)
        .unwrap();
    b.build_if(cond, dispatch, exit).unwrap();
    b.set_region(dispatch);
    let dispatch_ctrl = b.region_cur_ctrl(dispatch);
    b.build_return(Some(x), &[]).unwrap();
    b.set_region(exit);
    b.build_return(Some(x), &[]).unwrap();
    b.set_lift_addr(None);
    let f = b.build().unwrap();
    let dispatch_node = f.graph().producer(dispatch_ctrl);

    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let ranges = compute_value_ranges(&f, &doms, &known);

    let iv = ranges.range_of(x, dispatch_node);
    assert_eq!(
        (iv.lo, iv.hi),
        (1, 7),
        "guard on Add(X,-1) ∈ [0,6] must bound X ∈ [1,7]"
    );
}

// ---------------------------------------------------------------------------
// Test 2: trivial phi of guarded index still bounded
//
// Same shape, but we read `idx` through a trivial (single-input) Phi in the
// dispatch region.  The phi-chase must resolve to the underlying value.
// We approximate this by using a different SSA value (a truncate of idx)
// that the builder's phi-of-variable mechanism wraps.
//
// Actually a more direct approach: build the function with a tracked variable
// (vn), write idx into it in the entry, read it back in dispatch — the
// builder inserts a single-input Phi for tracked variables.
// ---------------------------------------------------------------------------
#[test]
fn trivial_phi_of_guarded_index_is_bounded() {
    use rsleigh::VnSpace;
    use strider_ir_test_utils::reg_vn;

    // Register variable for idx.
    let idx_vn = reg_vn(0x10, 4); // 4-byte register → I32
    let mut b = RegisterSet::new().tracked(idx_vn).build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    let entry = b.create_region().unwrap();
    let dispatch = b.create_region().unwrap();
    let exit = b.create_region().unwrap();

    b.set_entry_region(entry).unwrap();

    b.set_region(entry);
    // Write some concrete value as idx — use a load so it has no KB facts.
    let dummy = b.build_int_const(0xCAFEu64, ValueType::I64).unwrap();
    let raw_idx = b.build_load(dummy, VnSpace::RAM, ValueType::I32).unwrap();
    // Write raw_idx into the tracked variable.
    b.write_variable(&idx_vn, raw_idx).unwrap();
    let bound_c = b.build_int_const(8u64, ValueType::I32).unwrap();
    let cond = b
        .build_int_cmp_operation(raw_idx, bound_c, IntCmpOp::Less, ValueType::I32)
        .unwrap();
    b.build_if(cond, dispatch, exit).unwrap();

    b.set_region(dispatch);
    let dispatch_ctrl = b.region_cur_ctrl(dispatch);
    // Read idx back through the builder's phi mechanism.
    let phi_idx = b.read_variable(&idx_vn).unwrap();
    b.build_return(Some(phi_idx), &[]).unwrap();

    b.set_region(exit);
    b.build_return(Some(raw_idx), &[]).unwrap();

    b.set_lift_addr(None);
    let f = b.build().unwrap();

    let dispatch_node = f.graph().producer(dispatch_ctrl);

    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let ranges = compute_value_ranges(&f, &doms, &known);

    let iv = ranges.range_of(phi_idx, dispatch_node);
    assert_eq!(iv.lo, 0, "trivial phi: lower bound must be 0");
    assert_eq!(iv.hi, 7, "trivial phi: upper bound must be 7");
}

// ---------------------------------------------------------------------------
// Test 3: KnownBits mask → [0, 7] flow-insensitively
//
// idx = arg & 7  → bits 3..31 known zero → max = 7 → range [0, 7] everywhere.
// ---------------------------------------------------------------------------
#[test]
fn known_bits_mask_bounds_index_everywhere() {
    let mut b = RegisterSet::new().build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    let entry = b.create_region().unwrap();
    let other = b.create_region().unwrap();

    b.set_entry_region(entry).unwrap();
    b.set_region(entry);

    // arg = unconstrained value (load from arbitrary address).
    let dummy = b.build_int_const(0x1000u64, ValueType::I64).unwrap();
    let arg = b
        .build_load(dummy, rsleigh::VnSpace::RAM, ValueType::I32)
        .unwrap();
    let mask = b.build_int_const(7u64, ValueType::I32).unwrap();
    let idx = b
        .build_int_binary_operation(arg, mask, IntBinaryOp::And, ValueType::I32)
        .unwrap();

    // Build a trivial branch to `other` so we can check the range there too.
    let one = b.build_boolean_const(true);
    b.build_if(one, other, other).unwrap();

    b.set_region(other);
    let other_ctrl = b.region_cur_ctrl(other);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_lift_addr(None);
    let f = b.build().unwrap();

    let other_node = f.graph().producer(other_ctrl);
    let entry_node = f.entry().unwrap();

    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let ranges = compute_value_ranges(&f, &doms, &known);

    // In entry region: KnownBits should see max_value = 7 → [0, 7].
    let iv_entry = ranges.range_of(idx, entry_node);
    assert_eq!(iv_entry.hi, 7, "KnownBits bound: hi must be 7 in entry");
    assert_eq!(iv_entry.lo, 0, "KnownBits bound: lo must be 0 in entry");

    // In other region: same flow-insensitive KnownBits bound.
    let iv_other = ranges.range_of(idx, other_node);
    assert_eq!(iv_other.hi, 7, "KnownBits bound: hi must be 7 in other");
}

// ---------------------------------------------------------------------------
// Test 4: unguarded predecessor → top (fail-closed)
//
// dispatch has two predecessors: one with idx<8 guard, one without.
// The union must be top.
// ---------------------------------------------------------------------------
#[test]
fn unguarded_predecessor_makes_range_top() {
    use rsleigh::VnSpace;
    use strider_ir_test_utils::reg_vn;

    let idx_vn = reg_vn(0x10, 4);
    let mut b = RegisterSet::new().tracked(idx_vn).build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    // Regions: entry → (guarded_region, unguarded_region) → dispatch
    let entry = b.create_region().unwrap();
    let guarded = b.create_region().unwrap();
    let unguarded = b.create_region().unwrap();
    let dispatch = b.create_region().unwrap();

    b.set_entry_region(entry).unwrap();

    b.set_region(entry);
    let dummy = b.build_int_const(0xBEEFu64, ValueType::I64).unwrap();
    let raw_idx = b.build_load(dummy, VnSpace::RAM, ValueType::I32).unwrap();
    b.write_variable(&idx_vn, raw_idx).unwrap();
    let bound_c = b.build_int_const(8u64, ValueType::I32).unwrap();
    let cond = b
        .build_int_cmp_operation(raw_idx, bound_c, IntCmpOp::Less, ValueType::I32)
        .unwrap();
    b.build_if(cond, guarded, unguarded).unwrap();

    // guarded_region: idx is known < 8 here, branch to dispatch.
    b.set_region(guarded);
    // Keep the same SSA write for idx_vn so the phi in dispatch has this value.
    b.write_variable(&idx_vn, raw_idx).unwrap();
    b.build_branch(dispatch).unwrap();

    // unguarded_region: no guard, branch to dispatch.
    b.set_region(unguarded);
    // Also write raw_idx so the phi has two inputs with the same SSA value.
    b.write_variable(&idx_vn, raw_idx).unwrap();
    b.build_branch(dispatch).unwrap();

    b.set_region(dispatch);
    let dispatch_ctrl = b.region_cur_ctrl(dispatch);
    let phi_idx = b.read_variable(&idx_vn).unwrap();
    b.build_return(Some(phi_idx), &[]).unwrap();

    b.set_lift_addr(None);
    let f = b.build().unwrap();
    let dispatch_node = f.graph().producer(dispatch_ctrl);

    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let ranges = compute_value_ranges(&f, &doms, &known);

    let iv = ranges.range_of(phi_idx, dispatch_node);
    let type_mask = ValueType::I32.bit_mask_u128();
    assert!(
        iv.is_top(type_mask),
        "phi with one unguarded predecessor must produce top, got [{}, {}]",
        iv.lo,
        iv.hi
    );
    assert!(
        iv.upper_exclusive(type_mask).is_none(),
        "upper_exclusive must be None for top"
    );
}

// ---------------------------------------------------------------------------
// Test 5: lowered <= guard → [0, N]
//
// Lowered `idx <= 15` shape = `If(Xor(Less(IntConst(15), idx), IntConst(1)):I1)`.
// On the true branch, idx ∈ [0, 15].
// ---------------------------------------------------------------------------
#[test]
fn lowered_le_guard_bounds_index() {
    let mut b = RegisterSet::new().build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    let entry = b.create_region().unwrap();
    let dispatch = b.create_region().unwrap();
    let exit = b.create_region().unwrap();

    b.set_entry_region(entry).unwrap();
    b.set_region(entry);

    let dummy = b.build_int_const(0xF00Du64, ValueType::I64).unwrap();
    let idx = b
        .build_load(dummy, rsleigh::VnSpace::RAM, ValueType::I32)
        .unwrap();

    // Build `Xor(Less(IntConst(15), idx), IntConst(1)):I1`
    // = the lowered form of `idx <= 15`.
    let n15 = b.build_int_const(15u64, ValueType::I32).unwrap();
    let inner_less = b
        .build_int_cmp_operation(n15, idx, IntCmpOp::Less, ValueType::I32)
        .unwrap();
    // Xor with IntConst(1) at I1 = logical NOT.
    let one_i1 = b.build_int_const(1u64, ValueType::I1).unwrap();
    let cond = b
        .build_int_binary_operation(inner_less, one_i1, IntBinaryOp::Xor, ValueType::I1)
        .unwrap();
    b.build_if(cond, dispatch, exit).unwrap();

    b.set_region(dispatch);
    let dispatch_ctrl = b.region_cur_ctrl(dispatch);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_region(exit);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_lift_addr(None);
    let f = b.build().unwrap();
    let dispatch_node = f.graph().producer(dispatch_ctrl);

    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let ranges = compute_value_ranges(&f, &doms, &known);

    let iv = ranges.range_of(idx, dispatch_node);
    assert_eq!(iv.lo, 0, "lowered <=: lower bound must be 0");
    assert_eq!(
        iv.hi, 15,
        "lowered <=: upper bound must be 15 for idx <= 15"
    );
}

// ---------------------------------------------------------------------------
// Test 5 (b): lowered <= guard with SWAPPED Xor operands — order independent
//
// Same semantic as test 5, but the Xor is built as
// `Xor(IntConst(1), Less(IntConst(15), idx)):I1`
// (constant-first operand order) instead of the canonical
// `Xor(Less(IntConst(15), idx), IntConst(1)):I1`.
// The guard extractor must recognise both orderings (Xor is commutative).
// ---------------------------------------------------------------------------
#[test]
fn lowered_le_guard_swapped_xor_operands_still_bounds_index() {
    let mut b = RegisterSet::new().build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    let entry = b.create_region().unwrap();
    let dispatch = b.create_region().unwrap();
    let exit = b.create_region().unwrap();

    b.set_entry_region(entry).unwrap();
    b.set_region(entry);

    let dummy = b.build_int_const(0xF00Du64, ValueType::I64).unwrap();
    let idx = b
        .build_load(dummy, rsleigh::VnSpace::RAM, ValueType::I32)
        .unwrap();

    // Build `Xor(IntConst(1), Less(IntConst(15), idx)):I1`
    // — operands SWAPPED relative to the canonical form — still equals `idx <= 15`.
    let n15 = b.build_int_const(15u64, ValueType::I32).unwrap();
    let inner_less = b
        .build_int_cmp_operation(n15, idx, IntCmpOp::Less, ValueType::I32)
        .unwrap();
    let one_i1 = b.build_int_const(1u64, ValueType::I1).unwrap();
    // Note: one_i1 is the FIRST operand here (swapped vs test 5).
    let cond = b
        .build_int_binary_operation(one_i1, inner_less, IntBinaryOp::Xor, ValueType::I1)
        .unwrap();
    b.build_if(cond, dispatch, exit).unwrap();

    b.set_region(dispatch);
    let dispatch_ctrl = b.region_cur_ctrl(dispatch);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_region(exit);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_lift_addr(None);
    let f = b.build().unwrap();
    let dispatch_node = f.graph().producer(dispatch_ctrl);

    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let ranges = compute_value_ranges(&f, &doms, &known);

    let iv = ranges.range_of(idx, dispatch_node);
    assert_eq!(iv.lo, 0, "swapped Xor: lower bound must be 0");
    assert_eq!(
        iv.hi, 15,
        "swapped Xor: upper bound must be 15 for idx <= 15 (operand-order-independent)"
    );
}

// ---------------------------------------------------------------------------
// Sless guard WITH KnownBits-proven sign-bit-zero ⇒ bounded
//
// `Sless(idx, 8)` where `idx = load & 0xFF` (sign bit of I32 known zero)
// can be trusted as an unsigned bound: the true edge gives [0, 7] — the
// guard interval intersected with the KB base bound [0, 255].
// ---------------------------------------------------------------------------
#[test]
fn sless_guard_with_known_zero_sign_bit_bounds_index() {
    let mut b = RegisterSet::new().build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    let entry = b.create_region().unwrap();
    let dispatch = b.create_region().unwrap();
    let exit = b.create_region().unwrap();

    b.set_entry_region(entry).unwrap();
    b.set_region(entry);

    let dummy = b.build_int_const(0xABCDu64, ValueType::I64).unwrap();
    let raw = b
        .build_load(dummy, rsleigh::VnSpace::RAM, ValueType::I32)
        .unwrap();
    // Mask to [0, 255] → KnownBits proves the I32 sign bit is zero.
    let mask = b.build_int_const(0xFFu64, ValueType::I32).unwrap();
    let idx = b
        .build_int_binary_operation(raw, mask, IntBinaryOp::And, ValueType::I32)
        .unwrap();
    let bound_c = b.build_int_const(8u64, ValueType::I32).unwrap();
    let cond = b
        .build_int_cmp_operation(idx, bound_c, IntCmpOp::Sless, ValueType::I32)
        .unwrap();
    b.build_if(cond, dispatch, exit).unwrap();

    b.set_region(dispatch);
    let dispatch_ctrl = b.region_cur_ctrl(dispatch);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_region(exit);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_lift_addr(None);
    let f = b.build().unwrap();
    let dispatch_node = f.graph().producer(dispatch_ctrl);

    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let ranges = compute_value_ranges(&f, &doms, &known);

    let iv = ranges.range_of(idx, dispatch_node);
    assert_eq!(iv.lo, 0, "Sless with known-zero sign bit: lower bound 0");
    assert_eq!(iv.hi, 7, "Sless with known-zero sign bit: idx s< 8 → [0, 7]");
}

// ---------------------------------------------------------------------------
// Inverted guard `If(!(idx < 8))` ⇒ bounds the FALSE edge
//
// `Xor(Less(idx, IntConst(8)), 1)` is the inverted-sense branch: the
// constraint `idx < 8` holds on the FALSE successor (where the condition is
// false), while the TRUE successor only knows `idx >= 8` (a lower-only bound,
// useless for table sizing → top).  The both-edge guard model peels the
// `Xor(_, 1)` as a sense flip and recognises `idx < 8` on the false edge,
// binding it to `[0, 7]` there.  This is the shape `IfCondInversion`
// normalises away in the production pipeline — but custom pipelines that omit
// that pass still get the correct false-edge bound here.
// ---------------------------------------------------------------------------
#[test]
fn inverted_less_guard_bounds_index_on_false_edge() {
    let mut b = RegisterSet::new().build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    let entry = b.create_region().unwrap();
    let oob = b.create_region().unwrap(); // true edge: idx >= 8
    let dispatch = b.create_region().unwrap(); // false edge: idx < 8

    b.set_entry_region(entry).unwrap();
    b.set_region(entry);

    let dummy = b.build_int_const(0xF00Du64, ValueType::I64).unwrap();
    let idx = b
        .build_load(dummy, rsleigh::VnSpace::RAM, ValueType::I32)
        .unwrap();
    let bound_c = b.build_int_const(8u64, ValueType::I32).unwrap();
    let less = b
        .build_int_cmp_operation(idx, bound_c, IntCmpOp::Less, ValueType::I32)
        .unwrap();
    let one_i1 = b.build_int_const(1u64, ValueType::I1).unwrap();
    let cond = b
        .build_int_binary_operation(less, one_i1, IntBinaryOp::Xor, ValueType::I1)
        .unwrap();
    b.build_if(cond, oob, dispatch).unwrap();

    b.set_region(oob);
    let oob_ctrl = b.region_cur_ctrl(oob);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_region(dispatch);
    let dispatch_ctrl = b.region_cur_ctrl(dispatch);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_lift_addr(None);
    let f = b.build().unwrap();
    let oob_node = f.graph().producer(oob_ctrl);
    let dispatch_node = f.graph().producer(dispatch_ctrl);

    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let ranges = compute_value_ranges(&f, &doms, &known);

    let type_mask = ValueType::I32.bit_mask_u128();
    // True edge: `idx >= 8` — lower-only, no useful upper bound → top.
    assert!(
        ranges.range_of(idx, oob_node).is_top(type_mask),
        "true edge of the inverted guard carries only a lower bound (idx >= 8) → top"
    );
    // False edge: `idx < 8` holds → [0, 7].
    let iv = ranges.range_of(idx, dispatch_node);
    assert_eq!(iv.lo, 0, "false edge of inverted guard: lower bound 0");
    assert_eq!(
        iv.hi, 7,
        "false edge of inverted guard: `idx < 8` holds → upper bound 7"
    );
}

// ---------------------------------------------------------------------------
// Test 6: no guard, no KnownBits → top
// ---------------------------------------------------------------------------
#[test]
fn no_constraint_is_top() {
    let mut b = RegisterSet::new().build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    let entry = b.create_region().unwrap();
    b.set_entry_region(entry).unwrap();
    b.set_region(entry);

    let dummy = b.build_int_const(0x4242u64, ValueType::I64).unwrap();
    let idx = b
        .build_load(dummy, rsleigh::VnSpace::RAM, ValueType::I32)
        .unwrap();
    b.build_return(Some(idx), &[]).unwrap();

    b.set_lift_addr(None);
    let f = b.build().unwrap();
    let entry_node = f.entry().unwrap();

    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let ranges = compute_value_ranges(&f, &doms, &known);

    let iv = ranges.range_of(idx, entry_node);
    let type_mask = ValueType::I32.bit_mask_u128();
    assert!(
        iv.is_top(type_mask),
        "unconstrained load must produce top, got [{}, {}]",
        iv.lo,
        iv.hi
    );
}

// ---------------------------------------------------------------------------
// Test 7 (a): Sless guard WITHOUT sign-bit-known-zero ⇒ top
//
// `Sless(v, IntConst(N))` — the variable has no known-zero sign bit so
// the guard can't be trusted as unsigned; range_of must return top.
// ---------------------------------------------------------------------------
#[test]
fn sless_guard_without_known_sign_bit_is_top() {
    // Build: if (signed_idx s< 8) → dispatch else → exit.
    // idx comes from a raw load with no KnownBits info, so sign bit is unknown.
    let mut b = RegisterSet::new().build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    let entry = b.create_region().unwrap();
    let dispatch = b.create_region().unwrap();
    let exit = b.create_region().unwrap();

    b.set_entry_region(entry).unwrap();
    b.set_region(entry);

    let dummy = b.build_int_const(0xABCDu64, ValueType::I64).unwrap();
    let idx = b
        .build_load(dummy, rsleigh::VnSpace::RAM, ValueType::I32)
        .unwrap();
    let bound_c = b.build_int_const(8u64, ValueType::I32).unwrap();
    // Use Sless (signed less) — sign bit is NOT known-zero on idx.
    let cond = b
        .build_int_cmp_operation(idx, bound_c, IntCmpOp::Sless, ValueType::I32)
        .unwrap();
    b.build_if(cond, dispatch, exit).unwrap();

    b.set_region(dispatch);
    let dispatch_ctrl = b.region_cur_ctrl(dispatch);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_region(exit);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_lift_addr(None);
    let f = b.build().unwrap();
    let dispatch_node = f.graph().producer(dispatch_ctrl);

    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let ranges = compute_value_ranges(&f, &doms, &known);

    let iv = ranges.range_of(idx, dispatch_node);
    let type_mask = ValueType::I32.bit_mask_u128();
    assert!(
        iv.is_top(type_mask),
        "Sless without known-zero sign bit must yield top, got [{}, {}]",
        iv.lo,
        iv.hi
    );
}

// ---------------------------------------------------------------------------
// Test 7 (b): Query the FALSE-successor region of If(Less(v, N)) ⇒ top
//
// The guard `idx < 8` constrains the TRUE edge.  On the FALSE edge the
// only constraint is idx >= 8, which the analysis does not model — it
// must return top.
// ---------------------------------------------------------------------------
#[test]
fn false_successor_of_guard_is_top() {
    let (f, idx, _dispatch, exit) = build_guarded_dispatch(8, ValueType::I32);

    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let ranges = compute_value_ranges(&f, &doms, &known);

    let iv = ranges.range_of(idx, exit);
    let type_mask = ValueType::I32.bit_mask_u128();
    assert!(
        iv.is_top(type_mask),
        "false successor of guard must be top, got [{}, {}]",
        iv.lo,
        iv.hi
    );
    assert!(
        iv.upper_exclusive(type_mask).is_none(),
        "upper_exclusive must be None on false edge"
    );
}

// ---------------------------------------------------------------------------
// Test 7 (c): Query a SIBLING region — not dominated by the guard's
// true successor ⇒ top
//
// Layout:
//   entry → If(idx < 8) → dispatch (true), exit (false)
//   entry → also unconditionally jumps to sibling
//   (sibling is a separate region reachable only from entry, not through dispatch)
//
// Since we can't have two terminators on entry, we build a diamond
// where the guard is in the LEFT branch:
//   entry → If(flag) → left_branch, right_branch
//   left_branch: If(idx < 8) → dispatch, guarded_exit (both return)
//   right_branch: returns directly — sibling NOT dominated by the guard true-succ
// ---------------------------------------------------------------------------
#[test]
fn sibling_region_not_dominated_is_top() {
    use rsleigh::VnSpace;
    use strider_ir_test_utils::reg_vn;

    let idx_vn = reg_vn(0x10, 4);
    let mut b = RegisterSet::new().tracked(idx_vn).build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    let entry = b.create_region().unwrap();
    let left = b.create_region().unwrap();
    let right = b.create_region().unwrap();
    let dispatch = b.create_region().unwrap();
    let guarded_exit = b.create_region().unwrap();

    b.set_entry_region(entry).unwrap();

    b.set_region(entry);
    // Load idx (unconstrained).
    let dummy = b.build_int_const(0x1000u64, ValueType::I64).unwrap();
    let raw_idx = b.build_load(dummy, VnSpace::RAM, ValueType::I32).unwrap();
    b.write_variable(&idx_vn, raw_idx).unwrap();
    // Use a constant true to always take the left branch (makes structure simpler).
    let flag = b.build_boolean_const(true);
    b.build_if(flag, left, right).unwrap();

    // left branch: the guarded if.
    b.set_region(left);
    let bound_c = b.build_int_const(8u64, ValueType::I32).unwrap();
    let cond = b
        .build_int_cmp_operation(raw_idx, bound_c, IntCmpOp::Less, ValueType::I32)
        .unwrap();
    b.build_if(cond, dispatch, guarded_exit).unwrap();

    // dispatch: idx is bounded < 8 here.
    b.set_region(dispatch);
    let dispatch_ctrl = b.region_cur_ctrl(dispatch);
    b.build_return(Some(raw_idx), &[]).unwrap();

    // guarded_exit: just return.
    b.set_region(guarded_exit);
    b.build_return(Some(raw_idx), &[]).unwrap();

    // right branch: sibling — NOT dominated by dispatch.
    b.set_region(right);
    let right_ctrl = b.region_cur_ctrl(right);
    b.build_return(Some(raw_idx), &[]).unwrap();

    b.set_lift_addr(None);
    let f = b.build().unwrap();
    let dispatch_node = f.graph().producer(dispatch_ctrl);
    let right_node = f.graph().producer(right_ctrl);

    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let ranges = compute_value_ranges(&f, &doms, &known);

    // dispatch is dominated by guard true-succ → bounded.
    let iv_dispatch = ranges.range_of(raw_idx, dispatch_node);
    assert_eq!(
        iv_dispatch.hi, 7,
        "dispatch must have bound 7, got {}",
        iv_dispatch.hi
    );

    // right branch is NOT dominated by the guard's true-successor → top.
    let iv_right = ranges.range_of(raw_idx, right_node);
    let type_mask = ValueType::I32.bit_mask_u128();
    assert!(
        iv_right.is_top(type_mask),
        "sibling region must be top, got [{}, {}]",
        iv_right.lo,
        iv_right.hi
    );
}

// ---------------------------------------------------------------------------
// Test 7 (d): Cyclic phi (loop-carried index) ⇒ top
//
// Loop shape:
//   entry → loop_header (branch)
//   loop_header: idx_phi = Phi(idx_initial, idx_next)
//     if idx_phi < 16 → loop_body else → loop_exit
//   loop_body: idx_next = idx_phi + 1 → back to loop_header
//   loop_exit: return idx_phi
//
// range_of(idx_phi, loop_header) must return top (depth-cap hit or
// cycle detected via the multi-input phi fail-closed path).
// ---------------------------------------------------------------------------
#[test]
fn cyclic_phi_is_top() {
    use strider_ir_test_utils::reg_vn;

    let idx_vn = reg_vn(0x10, 4); // 4-byte register → I32
    let mut b = RegisterSet::new().tracked(idx_vn).build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    let entry = b.create_region().unwrap();
    let header = b.create_region().unwrap();
    let body = b.create_region().unwrap();
    let loop_exit = b.create_region().unwrap();

    b.set_entry_region(entry).unwrap();

    // entry: idx = 0, branch to header.
    b.set_region(entry);
    let zero = b.build_int_const(0u64, ValueType::I32).unwrap();
    b.write_variable(&idx_vn, zero).unwrap();
    b.build_branch(header).unwrap();

    // header: read idx_phi from variable (will be a Phi after linking),
    //         check idx_phi < 16, branch to body / loop_exit.
    b.set_region(header);
    let header_ctrl = b.region_cur_ctrl(header);
    let idx_phi = b.read_variable(&idx_vn).unwrap();
    let bound = b.build_int_const(16u64, ValueType::I32).unwrap();
    let cond = b
        .build_int_cmp_operation(idx_phi, bound, IntCmpOp::Less, ValueType::I32)
        .unwrap();
    b.build_if(cond, body, loop_exit).unwrap();

    // body: idx_next = idx_phi + 1, write back, branch to header.
    b.set_region(body);
    let one = b.build_int_const(1u64, ValueType::I32).unwrap();
    let idx_next = b
        .build_int_binary_operation(idx_phi, one, IntBinaryOp::Add, ValueType::I32)
        .unwrap();
    b.write_variable(&idx_vn, idx_next).unwrap();
    b.build_branch(header).unwrap();

    // loop_exit: return idx_phi.
    b.set_region(loop_exit);
    b.build_return(Some(idx_phi), &[]).unwrap();

    b.set_lift_addr(None);
    let f = b.build().unwrap();
    let header_node = f.graph().producer(header_ctrl);

    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let ranges = compute_value_ranges(&f, &doms, &known);

    // The loop-carried phi has two data inputs (zero and idx_next).
    // The back-edge arm (from body) has a loop-relative predecessor
    // whose range is bounded but whose phi input is itself loop-carried,
    // causing a recursive cycle that the depth-cap terminates as top.
    let iv = ranges.range_of(idx_phi, header_node);
    let type_mask = ValueType::I32.bit_mask_u128();
    assert!(
        iv.is_top(type_mask),
        "cyclic phi must yield top, got [{}, {}]",
        iv.lo,
        iv.hi
    );
}

// ---------------------------------------------------------------------------
// Test 7 (e): Nested guards — inner region gets intersection
//
// Layout:
//   entry: if idx < 16 → outer_guard else → exit
//   outer_guard: if idx < 8 → dispatch else → mid_exit
//   dispatch: range_of(idx) must be [0, 7]  (intersection of both guards)
// ---------------------------------------------------------------------------
#[test]
fn nested_guards_intersect_at_inner_region() {
    let mut b = RegisterSet::new().build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    let entry = b.create_region().unwrap();
    let outer_guard = b.create_region().unwrap();
    let dispatch = b.create_region().unwrap();
    let mid_exit = b.create_region().unwrap();
    let exit = b.create_region().unwrap();

    b.set_entry_region(entry).unwrap();

    // entry: load idx, check idx < 16.
    b.set_region(entry);
    let dummy = b.build_int_const(0xF00Du64, ValueType::I64).unwrap();
    let idx = b
        .build_load(dummy, rsleigh::VnSpace::RAM, ValueType::I32)
        .unwrap();
    let bound16 = b.build_int_const(16u64, ValueType::I32).unwrap();
    let cond16 = b
        .build_int_cmp_operation(idx, bound16, IntCmpOp::Less, ValueType::I32)
        .unwrap();
    b.build_if(cond16, outer_guard, exit).unwrap();

    // outer_guard: check idx < 8.
    b.set_region(outer_guard);
    let bound8 = b.build_int_const(8u64, ValueType::I32).unwrap();
    let cond8 = b
        .build_int_cmp_operation(idx, bound8, IntCmpOp::Less, ValueType::I32)
        .unwrap();
    b.build_if(cond8, dispatch, mid_exit).unwrap();

    // dispatch: idx is bounded by BOTH guards → [0, 7].
    b.set_region(dispatch);
    let dispatch_ctrl = b.region_cur_ctrl(dispatch);
    b.build_return(Some(idx), &[]).unwrap();

    // mid_exit: idx is only bounded by the outer guard.
    b.set_region(mid_exit);
    b.build_return(Some(idx), &[]).unwrap();

    // exit: no guard at all.
    b.set_region(exit);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_lift_addr(None);
    let f = b.build().unwrap();
    let dispatch_node = f.graph().producer(dispatch_ctrl);

    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let ranges = compute_value_ranges(&f, &doms, &known);

    let iv = ranges.range_of(idx, dispatch_node);
    assert_eq!(
        iv.lo, 0,
        "nested guards: lower bound must be 0, got {}",
        iv.lo
    );
    assert_eq!(
        iv.hi, 7,
        "nested guards: inner dispatch must see [0,7] from intersection, got {}",
        iv.hi
    );
    assert_eq!(
        iv.upper_exclusive(ValueType::I32.bit_mask_u128()),
        Some(8),
        "upper_exclusive must be Some(8)"
    );
}

// ---------------------------------------------------------------------------
// Test 7 (f): Strict Less(idx, 0) ⇒ top  (Fix 1 regression guard)
//
// `v < 0` is impossible for an unsigned value; the guard can never fire,
// so the analysis must not yield the spurious [0, 0] that
// `saturating_sub(1)` would produce for N = 0.
// ---------------------------------------------------------------------------
#[test]
fn strict_less_zero_bound_is_top() {
    // Build: if idx < 0 (bound = 0) → dispatch else → exit.
    // This is a degenerate guard that can never be true.
    let (f, idx, dispatch_region, _exit) = build_guarded_dispatch(0, ValueType::I32);

    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let ranges = compute_value_ranges(&f, &doms, &known);

    let iv = ranges.range_of(idx, dispatch_region);
    let type_mask = ValueType::I32.bit_mask_u128();
    assert!(
        iv.is_top(type_mask),
        "Less(idx, 0) guard must yield top (impossible guard), got [{}, {}] — \
         this is the Fix-1 regression: saturating_sub(1) must not produce [0,0]",
        iv.lo,
        iv.hi
    );
}

// ---------------------------------------------------------------------------
// Test 7 (g): Less(idx, type_mask) narrows by exactly one
//
// For I32: type_mask = 0xFFFF_FFFF.  `Less(idx, type_mask)` yields
// hi = type_mask - 1 = 0xFFFF_FFFE, so upper_exclusive = Some(type_mask).
// This verifies that `saturating_sub(1)` works correctly at the boundary —
// the result is NOT top (the guard does narrow), just by a single step.
//
// Note: a bound of N = type_mask + 1 cannot be represented in I32
// (build_int_const masks to the type width, wrapping to 0, which is
// the impossible-guard case covered by test_f).  The true "no-narrowing"
// scenario for an overflowing bound is therefore unreachable via
// the strict-Less shape and is not tested here.
#[test]
fn strict_less_at_type_mask_narrows_by_one() {
    // For I32: type_mask = 0xFFFF_FFFF.  Less(idx, type_mask) → [0, type_mask-1].
    // This is NOT top — it narrows by 1.  Verify upper_exclusive = Some(type_mask).
    let type_mask_u64 = 0xFFFF_FFFFu64;
    let (f, idx, dispatch_region, _exit) = build_guarded_dispatch(type_mask_u64, ValueType::I32);

    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let ranges = compute_value_ranges(&f, &doms, &known);

    let iv = ranges.range_of(idx, dispatch_region);
    let type_mask = ValueType::I32.bit_mask_u128();
    // hi must be type_mask - 1 (one below the maximum).
    assert_eq!(
        iv.hi,
        type_mask - 1,
        "Less(idx, type_mask) must narrow by 1, got hi={}",
        iv.hi
    );
    // upper_exclusive must be Some(type_mask) — it points just past hi.
    assert_eq!(
        iv.upper_exclusive(type_mask),
        Some(type_mask_u64),
        "upper_exclusive must be Some(type_mask)"
    );
    // Must NOT be top.
    assert!(
        !iv.is_top(type_mask),
        "Less(idx, type_mask) must NOT be top (it narrows by 1)"
    );
}

// ---------------------------------------------------------------------------
// Test 7 (h): Same value guarded differently in two sibling regions
//
// Layout:
//   entry → If(flag) → branch_a, branch_b
//   branch_a: if idx < 8 → dispatch_a (narrow [0,7]) else → sink_a
//   branch_b: if idx < 16 → dispatch_b (narrow [0,15]) else → sink_b
//
//   range_of(idx, dispatch_a) == [0, 7]
//   range_of(idx, dispatch_b) == [0, 15]
// ---------------------------------------------------------------------------
#[test]
fn two_sibling_guard_regions_give_independent_bounds() {
    let mut b = RegisterSet::new().build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    let entry = b.create_region().unwrap();
    let branch_a = b.create_region().unwrap();
    let branch_b = b.create_region().unwrap();
    let dispatch_a = b.create_region().unwrap();
    let sink_a = b.create_region().unwrap();
    let dispatch_b = b.create_region().unwrap();
    let sink_b = b.create_region().unwrap();

    b.set_entry_region(entry).unwrap();

    // entry: load idx, use constant true flag to split to a/b.
    b.set_region(entry);
    let dummy = b.build_int_const(0xCAFEu64, ValueType::I64).unwrap();
    let idx = b
        .build_load(dummy, rsleigh::VnSpace::RAM, ValueType::I32)
        .unwrap();
    let flag = b.build_boolean_const(true);
    b.build_if(flag, branch_a, branch_b).unwrap();

    // branch_a: if idx < 8 → dispatch_a else → sink_a.
    b.set_region(branch_a);
    let bound8 = b.build_int_const(8u64, ValueType::I32).unwrap();
    let cond8 = b
        .build_int_cmp_operation(idx, bound8, IntCmpOp::Less, ValueType::I32)
        .unwrap();
    b.build_if(cond8, dispatch_a, sink_a).unwrap();

    // dispatch_a: idx bounded < 8 → [0, 7].
    b.set_region(dispatch_a);
    let dispatch_a_ctrl = b.region_cur_ctrl(dispatch_a);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_region(sink_a);
    b.build_return(Some(idx), &[]).unwrap();

    // branch_b: if idx < 16 → dispatch_b else → sink_b.
    b.set_region(branch_b);
    let bound16 = b.build_int_const(16u64, ValueType::I32).unwrap();
    let cond16 = b
        .build_int_cmp_operation(idx, bound16, IntCmpOp::Less, ValueType::I32)
        .unwrap();
    b.build_if(cond16, dispatch_b, sink_b).unwrap();

    // dispatch_b: idx bounded < 16 → [0, 15].
    b.set_region(dispatch_b);
    let dispatch_b_ctrl = b.region_cur_ctrl(dispatch_b);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_region(sink_b);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_lift_addr(None);
    let f = b.build().unwrap();
    let da_node = f.graph().producer(dispatch_a_ctrl);
    let db_node = f.graph().producer(dispatch_b_ctrl);

    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let ranges = compute_value_ranges(&f, &doms, &known);

    let iv_a = ranges.range_of(idx, da_node);
    assert_eq!(iv_a.lo, 0, "dispatch_a lo must be 0");
    assert_eq!(iv_a.hi, 7, "dispatch_a hi must be 7 (idx < 8)");

    let iv_b = ranges.range_of(idx, db_node);
    assert_eq!(iv_b.lo, 0, "dispatch_b lo must be 0");
    assert_eq!(iv_b.hi, 15, "dispatch_b hi must be 15 (idx < 16)");
}

// ---------------------------------------------------------------------------
// Multi-input phi output carries a dominating guard ⇒ bounded
//
// Layout:
//   entry → If(flag) → path_a / path_b   [both write the same SSA idx]
//   path_a → join (branch)
//   path_b → join (branch)
//   join: phi_idx = Phi(idx_a, idx_b)   [MULTI-input phi]
//         If(phi_idx < 8) → dispatch / exit
//   dispatch: range_of(phi_idx) must be [0, 7]
//
// The guard `phi_idx < 8` is recorded against the phi's OWN output value
// (a multi-input phi is not chased through).  The dispatch region is
// dominated by the guard's true edge.  At query time `range_of(phi_idx,
// dispatch)` dispatches to the multi-input-phi resolver, which unions the
// (unconstrained) arms — so without intersecting the guard recorded on the
// phi output the bound is silently dropped and the result is top.
// ---------------------------------------------------------------------------
#[test]
fn multi_input_phi_output_guard_bounds_index() {
    use rsleigh::VnSpace;
    use strider_ir_test_utils::reg_vn;

    let idx_vn = reg_vn(0x10, 4); // 4-byte register → I32
    let mut b = RegisterSet::new().tracked(idx_vn).build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    let entry = b.create_region().unwrap();
    let path_a = b.create_region().unwrap();
    let path_b = b.create_region().unwrap();
    let join = b.create_region().unwrap();
    let dispatch = b.create_region().unwrap();
    let exit = b.create_region().unwrap();

    b.set_entry_region(entry).unwrap();

    // entry: load idx, write it, then split unconditionally to path_a / path_b.
    b.set_region(entry);
    let dummy = b.build_int_const(0xDEADu64, ValueType::I64).unwrap();
    let raw_idx = b.build_load(dummy, VnSpace::RAM, ValueType::I32).unwrap();
    b.write_variable(&idx_vn, raw_idx).unwrap();
    let flag = b.build_boolean_const(true);
    b.build_if(flag, path_a, path_b).unwrap();

    // path_a: write idx and branch to join.
    b.set_region(path_a);
    b.write_variable(&idx_vn, raw_idx).unwrap();
    b.build_branch(join).unwrap();

    // path_b: write idx and branch to join — gives the join phi TWO data inputs.
    b.set_region(path_b);
    b.write_variable(&idx_vn, raw_idx).unwrap();
    b.build_branch(join).unwrap();

    // join: read the (multi-input) phi, guard it with `phi_idx < 8`.
    b.set_region(join);
    let phi_idx = b.read_variable(&idx_vn).unwrap();
    let bound_c = b.build_int_const(8u64, ValueType::I32).unwrap();
    let cond = b
        .build_int_cmp_operation(phi_idx, bound_c, IntCmpOp::Less, ValueType::I32)
        .unwrap();
    b.build_if(cond, dispatch, exit).unwrap();

    // dispatch: phi_idx is guarded < 8 here.
    b.set_region(dispatch);
    let dispatch_ctrl = b.region_cur_ctrl(dispatch);
    b.build_return(Some(phi_idx), &[]).unwrap();

    b.set_region(exit);
    b.build_return(Some(phi_idx), &[]).unwrap();

    b.set_lift_addr(None);
    let f = b.build().unwrap();
    let dispatch_node = f.graph().producer(dispatch_ctrl);

    // Sanity: the guarded value really is a multi-input Phi.
    let phi_producer = f.graph().producer(phi_idx);
    assert!(
        matches!(f.node_kind(phi_producer), NodeKind::Phi),
        "guarded value must be a Phi"
    );
    assert!(
        f.phi_data_inputs(phi_producer).count() >= 2,
        "the join phi must have multiple data inputs for this test to exercise the bug"
    );

    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let ranges = compute_value_ranges(&f, &doms, &known);

    let iv = ranges.range_of(phi_idx, dispatch_node);
    assert_eq!(iv.lo, 0, "multi-phi output guard: lower bound must be 0");
    assert_eq!(
        iv.hi, 7,
        "multi-phi output guard: a dominating `phi_idx < 8` guard must bound the \
         multi-input phi output to [0, 7]"
    );
}

// ---------------------------------------------------------------------------
// Test 8: join with one guarded and one UNGUARDED predecessor → top
//
// This is the soundness-critical fail-closed test.  A guard that holds on
// only ONE incoming path to a join must NOT be treated as bounding the phi.
//
// Layout:
//   entry → If(flag) → path_a / path_b
//   path_a → If(idx < 4) → dispatch / exit_a    [guarded: idx∈[0,3] on this arm]
//   path_b → dispatch                            [unconditional: idx UNCONSTRAINED]
//   dispatch: Phi(idx_a, idx_b) used as jump-table index
//
// Both phi arms ultimately refer to the same underlying InitialVar (the register
// read from function entry).  With the buggy "query all arms in joining_region"
// approach, `dominates(dispatch, dispatch)` is reflexively true so the guard
// applies to BOTH arms → returns [0,3].  The correct answer is TOP, because
// path_b carries an unconstrained idx.
// ---------------------------------------------------------------------------
#[test]
fn join_fails_closed_when_one_predecessor_unguarded() {
    use rsleigh::VnSpace;
    use strider_ir::IntCmpOp;
    use strider_ir_test_utils::reg_vn;

    let idx_vn = reg_vn(0x10, 4); // 4-byte register → I32
    let mut b = RegisterSet::new().tracked(idx_vn).build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    let entry = b.create_region().unwrap();
    let path_a = b.create_region().unwrap();
    let path_b = b.create_region().unwrap();
    let dispatch = b.create_region().unwrap();
    let exit_a = b.create_region().unwrap();

    b.set_entry_region(entry).unwrap();

    // entry: load idx into the tracked variable, then split to path_a / path_b.
    b.set_region(entry);
    let dummy = b.build_int_const(0xDEADu64, ValueType::I64).unwrap();
    let raw_idx = b.build_load(dummy, VnSpace::RAM, ValueType::I32).unwrap();
    b.write_variable(&idx_vn, raw_idx).unwrap();
    // flag = dummy comparison, splits unconditionally for test purposes.
    let zero = b.build_int_const(0u64, ValueType::I32).unwrap();
    let flag = b
        .build_int_cmp_operation(raw_idx, zero, IntCmpOp::Equal, ValueType::I32)
        .unwrap();
    b.build_if(flag, path_a, path_b).unwrap();

    // path_a: guarded — If(idx < 4) → dispatch / exit_a.
    // true_succ_region = dispatch for this guard.
    b.set_region(path_a);
    let idx_a = b.read_variable(&idx_vn).unwrap();
    let four = b.build_int_const(4u64, ValueType::I32).unwrap();
    let cond_a = b
        .build_int_cmp_operation(idx_a, four, IntCmpOp::Less, ValueType::I32)
        .unwrap();
    b.build_if(cond_a, dispatch, exit_a).unwrap();

    // path_b: unconditional branch to dispatch — idx is UNCONSTRAINED on this path.
    b.set_region(path_b);
    b.build_branch(dispatch).unwrap();

    // dispatch: Phi merges idx from both predecessors.
    b.set_region(dispatch);
    let dispatch_ctrl = b.region_cur_ctrl(dispatch);
    let phi_idx = b.read_variable(&idx_vn).unwrap();
    b.build_return(Some(phi_idx), &[]).unwrap();

    // exit_a: the false branch of the guarded If.
    b.set_region(exit_a);
    b.build_return(Some(raw_idx), &[]).unwrap();

    b.set_lift_addr(None);
    let f = b.build().unwrap();
    let dispatch_node = f.graph().producer(dispatch_ctrl);

    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let ranges = compute_value_ranges(&f, &doms, &known);

    // The phi at dispatch merges a guarded arm (path_a) and an unguarded arm
    // (path_b).  The correct result is TOP — one unconstrained arm poisons the
    // union.  The buggy code returns [0,3] because it queries both arms in
    // `dispatch` where the guard's true_succ_region dominates reflexively.
    let iv = ranges.range_of(phi_idx, dispatch_node);
    let type_mask = ValueType::I32.bit_mask_u128();
    assert!(
        iv.is_top(type_mask),
        "join with one unguarded predecessor must be top (fail-closed), \
         got [{}, {}] — soundness bug: guard on one path must not bound the phi",
        iv.lo,
        iv.hi
    );
    assert!(
        iv.upper_exclusive(type_mask).is_none(),
        "upper_exclusive must be None when join is top"
    );
}

// ---------------------------------------------------------------------------
// Const-on-LHS guard `If(Less(IntConst(N), v))` ⇒ false edge bounds `[0, N]`
//
// `Less(IntConst(8), v)` is `8 < v`.  On the FALSE edge the negation
// `!(8 < v)` = `v <= 8` holds → `v ∈ [0, 8]`.  On the TRUE edge only the
// lower bound `v >= 9` is known (useless for table sizing) → top.  This is
// the `ja`-after-canonicalisation shape the jump-table classifier needs.
// ---------------------------------------------------------------------------
#[test]
fn const_lhs_less_guard_bounds_false_edge() {
    let mut b = RegisterSet::new().build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    let entry = b.create_region().unwrap();
    let above = b.create_region().unwrap(); // true edge: v > 8
    let at_or_below = b.create_region().unwrap(); // false edge: v <= 8

    b.set_entry_region(entry).unwrap();
    b.set_region(entry);

    let dummy = b.build_int_const(0xF00Du64, ValueType::I64).unwrap();
    let idx = b
        .build_load(dummy, rsleigh::VnSpace::RAM, ValueType::I32)
        .unwrap();
    let n8 = b.build_int_const(8u64, ValueType::I32).unwrap();
    // Less(IntConst(8), idx) — const on the LHS.
    let cond = b
        .build_int_cmp_operation(n8, idx, IntCmpOp::Less, ValueType::I32)
        .unwrap();
    b.build_if(cond, above, at_or_below).unwrap();

    b.set_region(above);
    let above_ctrl = b.region_cur_ctrl(above);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_region(at_or_below);
    let below_ctrl = b.region_cur_ctrl(at_or_below);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_lift_addr(None);
    let f = b.build().unwrap();
    let above_node = f.graph().producer(above_ctrl);
    let below_node = f.graph().producer(below_ctrl);

    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let ranges = compute_value_ranges(&f, &doms, &known);

    let type_mask = ValueType::I32.bit_mask_u128();
    // True edge: v > 8 — lower-only → top.
    assert!(
        ranges.range_of(idx, above_node).is_top(type_mask),
        "const-LHS Less true edge carries only a lower bound (v >= 9) → top"
    );
    // False edge: v <= 8 → [0, 8].
    let iv = ranges.range_of(idx, below_node);
    assert_eq!(iv.lo, 0, "const-LHS Less false edge: lower bound 0");
    assert_eq!(iv.hi, 8, "const-LHS Less false edge: v <= 8 → upper bound 8");
}

// ---------------------------------------------------------------------------
// Guard whose true-edge consumer is a control MERGE (multi-pred Region) ⇒
// NOT applied (soundness gate)
//
// The guard `idx < 8` on the If's true edge feeds a Region that ALSO has a
// second predecessor (an unconditional branch from a sibling).  The guard
// does not hold for paths arriving via that other predecessor, so the
// soundness gate must skip recording it: the merge region (and anything it
// dominates) sees top.
// ---------------------------------------------------------------------------
#[test]
fn guard_into_control_merge_is_not_applied() {
    let mut b = RegisterSet::new().build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    let entry = b.create_region().unwrap();
    let guarded = b.create_region().unwrap(); // entry's true edge → also branches to merge
    let other = b.create_region().unwrap(); // entry's false edge → branches to merge
    let merge = b.create_region().unwrap(); // 2 predecessors

    b.set_entry_region(entry).unwrap();
    b.set_region(entry);
    let dummy = b.build_int_const(0xF00Du64, ValueType::I64).unwrap();
    let idx = b
        .build_load(dummy, rsleigh::VnSpace::RAM, ValueType::I32)
        .unwrap();
    let n8 = b.build_int_const(8u64, ValueType::I32).unwrap();
    let cond = b
        .build_int_cmp_operation(idx, n8, IntCmpOp::Less, ValueType::I32)
        .unwrap();
    // True edge: guarded (idx < 8 holds); false edge: other (unconstrained).
    b.build_if(cond, guarded, other).unwrap();

    // guarded branches into the merge.
    b.set_region(guarded);
    b.build_branch(merge).unwrap();

    // other branches into the merge too — making merge a 2-pred control merge.
    b.set_region(other);
    b.build_branch(merge).unwrap();

    b.set_region(merge);
    let merge_ctrl = b.region_cur_ctrl(merge);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_lift_addr(None);
    let f = b.build().unwrap();
    let merge_node = f.graph().producer(merge_ctrl);

    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let ranges = compute_value_ranges(&f, &doms, &known);

    // The guard's true-edge consumer is the 2-pred merge Region; the gate
    // skips it, so `idx` is unbounded at the merge.
    let iv = ranges.range_of(idx, merge_node);
    let type_mask = ValueType::I32.bit_mask_u128();
    assert!(
        iv.is_top(type_mask),
        "guard whose edge feeds a control merge must not be applied (soundness gate), \
         got [{}, {}]",
        iv.lo,
        iv.hi
    );
}

// ---------------------------------------------------------------------------
// Guard whose true edge is consumed DIRECTLY by a 2-predecessor merge Region
// ⇒ TOP at and below the merge (soundness gate fires here)
//
// This is the shape the gate exists to reject.  Unlike
// `guard_into_control_merge_is_not_applied` (where the true edge first passes
// through a single-pred Region, so it is the dominance filter — not the gate —
// that yields top at the eventual merge), here the If's true control output
// feeds the merge DIRECTLY:
//
//   entry → If(idx < 8) → merge (TRUE), other (FALSE)
//   other → branch → merge       [the merge's 2nd predecessor; no bound]
//   merge: return idx
//
// `single_control_consumer(true_ctrl)` is therefore the 2-predecessor merge
// Region itself, so the gate's "consumer is a multi-pred merge" check fires
// and the guard is NOT recorded.  Without the gate, the old region-keyed model
// would record the guard against the merge and `dominates(merge, query)` would
// then wrongly bound `idx` at or below the merge.  Querying `idx` at the merge
// (and below) must be TOP.
// ---------------------------------------------------------------------------
#[test]
fn guard_on_edge_into_merge_is_top_below_merge() {
    let mut b = RegisterSet::new().build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    let entry = b.create_region().unwrap();
    let other = b.create_region().unwrap(); // false edge → branches to merge
    let merge = b.create_region().unwrap(); // 2 preds: If true edge + other

    b.set_entry_region(entry).unwrap();
    b.set_region(entry);
    let dummy = b.build_int_const(0xF00Du64, ValueType::I64).unwrap();
    let idx = b
        .build_load(dummy, rsleigh::VnSpace::RAM, ValueType::I32)
        .unwrap();
    let n8 = b.build_int_const(8u64, ValueType::I32).unwrap();
    let cond = b
        .build_int_cmp_operation(idx, n8, IntCmpOp::Less, ValueType::I32)
        .unwrap();
    // True edge feeds `merge` DIRECTLY; false edge feeds `other`.
    b.build_if(cond, merge, other).unwrap();

    // other branches into merge too — making merge a 2-pred control merge whose
    // first predecessor is the If's true edge with NO intervening region.
    b.set_region(other);
    b.build_branch(merge).unwrap();

    b.set_region(merge);
    let merge_ctrl = b.region_cur_ctrl(merge);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_lift_addr(None);
    let f = b.build().unwrap();
    let merge_node = f.graph().producer(merge_ctrl);

    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let ranges = compute_value_ranges(&f, &doms, &known);

    // The If's true edge is consumed directly by the 2-pred merge, so the gate
    // skips recording the guard; `idx` is unbounded at the merge.
    let iv = ranges.range_of(idx, merge_node);
    let type_mask = ValueType::I32.bit_mask_u128();
    assert!(
        iv.is_top(type_mask),
        "guard on an If edge feeding a control merge directly must be top at \
         the merge (the soundness gate must skip it), got [{}, {}]",
        iv.lo,
        iv.hi
    );
}

// ---------------------------------------------------------------------------
// Guard survives the COLLAPSED shape: If true edge consumed directly by a
// non-Region control node ⇒ bound found at that node
//
// `RegionCollapse` deletes the single-predecessor dispatch Region, leaving
// the If's true control output feeding the next control node (here a
// `Return`) directly.  The both-edge guard model keys the guard on that
// non-Region consumer, so the bound is still found there — this is the
// regression the production jump-table resolution depends on.
// ---------------------------------------------------------------------------
#[test]
fn guard_survives_region_collapse_at_nonregion_consumer() {
    use crate::pipeline::OptimizerTestExt;
    use crate::RegionCollapse;

    let mut b = RegisterSet::new().build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    let entry = b.create_region().unwrap();
    let dispatch = b.create_region().unwrap(); // single-pred → collapses
    let exit = b.create_region().unwrap();

    b.set_entry_region(entry).unwrap();
    b.set_region(entry);
    let dummy = b.build_int_const(0xF00Du64, ValueType::I64).unwrap();
    let idx = b
        .build_load(dummy, rsleigh::VnSpace::RAM, ValueType::I32)
        .unwrap();
    let n8 = b.build_int_const(8u64, ValueType::I32).unwrap();
    let cond = b
        .build_int_cmp_operation(idx, n8, IntCmpOp::Less, ValueType::I32)
        .unwrap();
    b.build_if(cond, dispatch, exit).unwrap();

    // dispatch region: its sole consumer (the Return) rewires past it on
    // collapse, so the If's true edge then feeds the Return directly.
    b.set_region(dispatch);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_region(exit);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_lift_addr(None);
    let mut f = b.build().unwrap();

    // Capture the If's true control output BEFORE collapse so we can find its
    // post-collapse consumer.
    let if_node = f
        .graph()
        .all_node_ids()
        .find(|&n| matches!(f.node_kind(n), NodeKind::If))
        .expect("If node");
    let true_ctrl = f.node_outputs(if_node)[0];

    // Collapse the single-predecessor dispatch Region.
    let changed = RegionCollapse
        .run_one(&mut f, &mut crate::OptCtx::new(None))
        .unwrap()
        .changed();
    assert!(changed, "dispatch Region must collapse");

    // The If's true control output now feeds a non-Region node directly.
    let consumer = f
        .graph()
        .value_uses(true_ctrl)
        .map(|(n, _)| n)
        .next()
        .expect("a control consumer of the If true edge after collapse");
    assert!(
        !matches!(f.node_kind(consumer), NodeKind::Region),
        "after collapse, the If true edge feeds a non-Region node"
    );

    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let ranges = compute_value_ranges(&f, &doms, &known);

    // The guard keys on the non-Region consumer; querying there finds [0, 7].
    let iv = ranges.range_of(idx, consumer);
    assert_eq!(iv.lo, 0, "collapsed-shape guard: lower bound 0");
    assert_eq!(
        iv.hi, 7,
        "collapsed-shape guard: bound survives RegionCollapse → [0, 7]"
    );
}
