//! Tests for the dominator-scoped integer range analysis.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
)]

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
    let idx = b
        .build_load(dummy_addr, rsleigh::VnSpace::RAM, ty)
        .unwrap();
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
            matches!(f.node_kind(n), NodeKind::Region)
                && n != entry_node
                && n != dispatch_node
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
    let raw_idx = b
        .build_load(dummy, VnSpace::RAM, ValueType::I32)
        .unwrap();
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
    let raw_idx = b
        .build_load(dummy, VnSpace::RAM, ValueType::I32)
        .unwrap();
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
    assert_eq!(iv.hi, 15, "lowered <=: upper bound must be 15 for idx <= 15");
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
