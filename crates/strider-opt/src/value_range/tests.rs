use strider_ir::node::{NodeId, NodeKind, ValueId, ValueType};
use strider_ir::{IRBuilderExt, IRViewer, IRWalker, IntBinaryOp, IntCmpOp, control_dominators};
use strider_ir_test_utils::{RegisterSet, SENTINEL_LIFT_ADDR};

use super::compute_value_ranges;
use crate::analyze_known_bits;
use crate::value_range::Interval;

/// The AP-meet must keep the stride phase: intersecting a dense `[5, 40]` with
/// a multiples-of-8 progression yields `{8, 16, 24, 32, 40}` (lo rounded up to
/// the stride phase, stride 8), NOT `{5, 13, 21, ...}`.
#[test]
fn intersect_preserves_stride_phase() {
    let dense = Interval::dense(5, 40);
    let mul8 = Interval {
        lo: 0,
        hi: 64,
        stride: 8,
    };
    let m = dense.intersect(mul8);
    assert_eq!((m.lo, m.hi, m.stride), (8, 40, 8));
    assert_eq!(m.count(), 5);
    // Equal strides with opposite phases have no common element.
    let odd = Interval {
        lo: 1,
        hi: 100,
        stride: 2,
    };
    let ev = Interval {
        lo: 0,
        hi: 100,
        stride: 2,
    };
    assert_eq!(odd.intersect(ev).count(), 0);
    // Compatible strides combine to lcm with the shared phase.
    let a = Interval {
        lo: 0,
        hi: 200,
        stride: 4,
    };
    let b = Interval {
        lo: 0,
        hi: 200,
        stride: 6,
    };
    let m = a.intersect(b);
    assert_eq!((m.lo, m.stride), (0, 12));
}

/// Brings a hand-built fixture to the converged IR shape the range analysis
/// assumes: no single-input phis, no single-predecessor regions, no
/// `If(Xor(C,1))` conds (rewritten to `If(C)` with the branches swapped).
fn canonicalize(f: &mut strider_ir::Function) {
    let mut p = crate::OptimizerPipeline::new();
    p.add(crate::IfCondInversion::new());
    p.add(crate::PhiCollapse);
    p.add(crate::RegionCollapse);
    p.run(f, &mut crate::OptCtx::new(None)).unwrap();
}

/// `(true_edge_consumer, false_edge_consumer)` of the sole `If`: the
/// post-collapse query points where each edge's guard holds.  `IfCondInversion`
/// swaps the branches of an `Xor(C,1)` cond, so the original "true" dispatch
/// becomes the false edge in that case.
fn if_edge_consumers(f: &strider_ir::Function) -> (NodeId, NodeId) {
    let if_node = f
        .walk()
        .find(|&n| matches!(f.node_kind(n), NodeKind::If))
        .expect("an If node");
    let outs: Vec<ValueId> = f.node_outputs(if_node).to_vec();
    let consumer = |ctrl: ValueId| {
        f.graph()
            .value_uses(ctrl)
            .next()
            .expect("each If edge has a consumer")
            .0
    };
    (consumer(outs[0]), consumer(outs[1]))
}

/// ```text
///   entry:    If(Less(idx, bound)) -> dispatch (true), exit (false)
///   dispatch: Return(idx)
///   exit:     Return(idx)
/// ```
/// Returns `(function, idx, dispatch_node, exit_node)`.
fn build_guarded_dispatch(
    bound: u64,
    ty: ValueType,
) -> (strider_ir::Function, ValueId, NodeId, NodeId) {
    let mut b = RegisterSet::new().build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    let entry = b.create_region_all().unwrap();
    let dispatch = b.create_region_all().unwrap();
    let exit = b.create_region_all().unwrap();

    b.set_entry_region_all(entry).unwrap();

    b.set_region(entry);
    // A load stands in for an idx with no KnownBits facts at all.
    let dummy_addr = b.build_int_const(0xDEAD_u64, ValueType::I64).unwrap();
    let idx = b.build_load(dummy_addr, rsleigh::VnSpace::RAM, ty).unwrap();
    let bound_c = b.build_int_const(bound, ty).unwrap();
    let cond = b
        .build_int_cmp_operation(idx, bound_c, IntCmpOp::Less, ty)
        .unwrap();
    b.build_if(cond, dispatch, exit).unwrap();

    b.set_region(dispatch);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_region(exit);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_lift_addr(None);
    let mut f = b.build().unwrap();
    canonicalize(&mut f);

    // A bare `Less` cond is not swapped, so dispatch is the true-edge consumer.
    let (dispatch_node, exit_node) = if_edge_consumers(&f);
    (f, idx, dispatch_node, exit_node)
}

#[test]
fn strict_less_guard_bounds_index_on_true_edge() {
    let (f, idx, dispatch_region, _exit) = build_guarded_dispatch(8, ValueType::I32);
    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let mut ranges = compute_value_ranges(&f, &doms, &known);

    let iv = ranges.range_of(idx, dispatch_region);
    assert_eq!(iv.lo, 0, "lower bound must be 0");
    assert_eq!(iv.hi, 7, "upper bound must be 7 for idx < 8");
}

/// `build_guarded_dispatch` but the dispatch returns `idx <op> c`, the scaled
/// value a table index feeds into its address arithmetic.  Returns
/// `(function, scaled_value, dispatch_node)`.
fn build_guarded_scaled(
    bound: u64,
    ty: ValueType,
    op: IntBinaryOp,
    c: u64,
) -> (strider_ir::Function, ValueId, NodeId) {
    let mut b = RegisterSet::new().build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let entry = b.create_region_all().unwrap();
    let dispatch = b.create_region_all().unwrap();
    let exit = b.create_region_all().unwrap();
    b.set_entry_region_all(entry).unwrap();

    b.set_region(entry);
    let dummy_addr = b.build_int_const(0xDEAD_u64, ValueType::I64).unwrap();
    let idx = b.build_load(dummy_addr, rsleigh::VnSpace::RAM, ty).unwrap();
    let bound_c = b.build_int_const(bound, ty).unwrap();
    let cond = b
        .build_int_cmp_operation(idx, bound_c, IntCmpOp::Less, ty)
        .unwrap();
    b.build_if(cond, dispatch, exit).unwrap();

    b.set_region(dispatch);
    let c_val = b.build_int_const(c, ty).unwrap();
    let scaled = b.build_int_binary_operation(idx, c_val, op, ty).unwrap();
    b.build_return(Some(scaled), &[]).unwrap();

    b.set_region(exit);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_lift_addr(None);
    let mut f = b.build().unwrap();
    canonicalize(&mut f);
    let (dispatch_node, _exit) = if_edge_consumers(&f);
    (f, scaled, dispatch_node)
}

fn scaled_range(bound: u64, op: IntBinaryOp, c: u64) -> Interval {
    let (f, scaled, dispatch) = build_guarded_scaled(bound, ValueType::I32, op, c);
    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let mut ranges = compute_value_ranges(&f, &doms, &known);
    ranges.range_of(scaled, dispatch)
}

/// The guard must forward-propagate through `idx << k`, the value a table index
/// feeds into `table + idx*stride`: `idx < 4` gives `idx << 2  in  {0,4,8,12}`.
#[test]
fn guard_propagates_through_shift_left() {
    let iv = scaled_range(4, IntBinaryOp::ShiftLeft, 2);
    assert_eq!((iv.lo, iv.hi, iv.stride), (0, 12, 4));
    assert_eq!(iv.count(), 4);
}

/// Same through a non-power-of-two `idx * c`: `idx < 4` gives `idx*3  in
/// {0,3,6,9}`.
#[test]
fn guard_propagates_through_mul_const() {
    let iv = scaled_range(4, IntBinaryOp::Mul, 3);
    assert_eq!((iv.lo, iv.hi, iv.stride), (0, 9, 3));
    assert_eq!(iv.count(), 4);
}

/// The floor shapes reduce the range: `idx < 8` gives `idx >> 1  in  [0,3]` and
/// `idx / 2  in  [0,3]`.
#[test]
fn guard_propagates_through_shift_right() {
    let iv = scaled_range(8, IntBinaryOp::ShiftRight, 1);
    assert_eq!((iv.lo, iv.hi), (0, 3));
}

#[test]
fn guard_propagates_through_udiv_const() {
    let iv = scaled_range(8, IntBinaryOp::Div, 2);
    assert_eq!((iv.lo, iv.hi), (0, 3));
}

/// A scale's operand guard gates the propagation and then bounds the operand,
/// which is one scan, not two: each is a linear walk of the operand's guard
/// list with a dominance test per entry.
#[test]
fn scaled_range_scans_the_operand_guards_once() {
    let (f, scaled, dispatch) = build_guarded_scaled(8, ValueType::I32, IntBinaryOp::Div, 2);
    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let mut ranges = compute_value_ranges(&f, &doms, &known);
    crate::value_range::GUARD_SCANS.with(|c| c.set(0));
    let iv = ranges.range_of(scaled, dispatch);
    assert_eq!((iv.lo, iv.hi), (0, 3));
    assert_eq!(
        crate::value_range::GUARD_SCANS.with(std::cell::Cell::get),
        2,
        "one scan for the scaled value, one for its operand"
    );
}

/// A diamond whose two arms, `v < hi_a` and a second guard, each branch to a
/// merge on their true edge.  `second_arm_bounds_v` picks whether that second
/// guard bounds `v` or an unrelated load.  Returns `(function, v, merge_region)`.
fn build_two_arm_merge(
    hi_a: u64,
    hi_b: u64,
    second_arm_bounds_v: bool,
) -> (strider_ir::Function, ValueId, NodeId) {
    let mut b = RegisterSet::new().build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let entry = b.create_region_all().unwrap();
    let mid = b.create_region_all().unwrap();
    let merge = b.create_region_all().unwrap();
    let exit = b.create_region_all().unwrap();
    b.set_entry_region_all(entry).unwrap();

    let ty = ValueType::I32;
    b.set_region(entry);
    let va = b.build_int_const(0xDEAD_u64, ValueType::I64).unwrap();
    let v = b.build_load(va, rsleigh::VnSpace::RAM, ty).unwrap();
    let other = if second_arm_bounds_v {
        v
    } else {
        let wa = b.build_int_const(0xBEEF_u64, ValueType::I64).unwrap();
        b.build_load(wa, rsleigh::VnSpace::RAM, ty).unwrap()
    };
    let ca = b.build_int_const(hi_a, ty).unwrap();
    let lt_a = b
        .build_int_cmp_operation(v, ca, IntCmpOp::Less, ty)
        .unwrap();
    b.build_if(lt_a, merge, mid).unwrap();

    b.set_region(mid);
    let cb = b.build_int_const(hi_b, ty).unwrap();
    let lt_b = b
        .build_int_cmp_operation(other, cb, IntCmpOp::Less, ty)
        .unwrap();
    b.build_if(lt_b, merge, exit).unwrap();

    b.set_region(merge);
    b.build_return(Some(v), &[]).unwrap();
    b.set_region(exit);
    b.build_return(Some(v), &[]).unwrap();

    b.set_lift_addr(None);
    let mut f = b.build().unwrap();
    canonicalize(&mut f);

    let merge_node = f
        .walk()
        .find(|&n| {
            matches!(f.node_kind(n), NodeKind::Region)
                && f.graph()
                    .node_inputs(n)
                    .iter()
                    .filter(|&x| f.value_kind(x).is_control())
                    .count()
                    >= 2
        })
        .expect("the merge region");
    (f, v, merge_node)
}

/// A value bounded on EVERY predecessor of a merge is bounded at the merge by
/// the union of the arms, even though no single guard dominates it.  `v < 8` on
/// one arm and `v < 6` on the other give `v <= 7` at the merge.
#[test]
fn guard_survives_a_merge_of_two_bounded_arms() {
    let (f, v, merge) = build_two_arm_merge(8, 6, true);
    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let mut ranges = compute_value_ranges(&f, &doms, &known);
    let iv = ranges.range_of(v, merge);
    assert_eq!(
        (iv.lo, iv.hi),
        (0, 7),
        "v <= 7 at the merge (union of the <8 and <6 arms)",
    );
}

/// The soundness guard: if even one predecessor of the merge leaves `v` free
/// (here the second arm bounds an unrelated value), `v` is NOT bounded at the
/// merge.
#[test]
fn guard_does_not_survive_a_merge_with_an_unbounded_arm() {
    let (f, v, merge) = build_two_arm_merge(8, 6, false);
    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let mut ranges = compute_value_ranges(&f, &doms, &known);
    let iv = ranges.range_of(v, merge);
    assert!(
        iv.is_top(ValueType::I32.bit_mask_u128()),
        "v must stay unbounded: one merge arm does not bound it, got [{},{}]",
        iv.lo,
        iv.hi,
    );
}

/// `shl`/`mul` must WIDEN to top when the scaled top overflows the carrier:
/// `checked_shl` alone only rejects `k >= 128` and otherwise wraps, which would
/// under-approximate (unsound) for a `u128::MAX` width mask.
#[test]
fn shl_and_mul_widen_to_top_on_carrier_overflow() {
    let hi = 1u128 << 127;
    assert!(
        Interval::dense(0, hi).shl(1, u128::MAX).is_top(u128::MAX),
        "2^127 << 1 overflows u128 -> must be top, not a wrapped [0,0]",
    );
    assert!(
        Interval::dense(0, hi).mul(4, u128::MAX).is_top(u128::MAX),
        "2^127 * 4 overflows u128 -> must be top",
    );
    // The common in-type case is unaffected by routing shl through mul.
    let iv = Interval::dense(0, 3).shl(2, 0xFFFF_FFFF);
    assert_eq!((iv.lo, iv.hi, iv.stride), (0, 12, 4));
}

/// Floored `shr`/`udiv` recover `stride/divisor` when the divisor divides the
/// stride (exact), and widen to 1 otherwise (a non-dividing divisor breaks the
/// progression, so the floored image is not an AP and must over-approximate).
#[test]
fn shr_udiv_recover_stride_only_on_clean_division() {
    // c | stride: x/c walks exactly stride/c.
    let clean = Interval {
        lo: 0,
        hi: 12,
        stride: 4,
    };
    let d = clean.udiv(2);
    assert_eq!((d.lo, d.hi, d.stride), (0, 6, 2), "4 / 2 -> stride 2");
    let s = clean.shr(1);
    assert_eq!((s.lo, s.hi, s.stride), (0, 6, 2), "4 >> 1 -> stride 2");

    // c does NOT divide stride: {0,5,10,15}/2 = {0,2,5,7}, which is not an AP;
    // widening to stride 1 keeps it sound (stride/c = 2 would drop 5 and 7).
    let ragged = Interval {
        lo: 0,
        hi: 15,
        stride: 5,
    };
    let r = ragged.udiv(2);
    assert_eq!(
        (r.lo, r.hi, r.stride),
        (0, 7, 1),
        "non-dividing stride widens"
    );
    for real in [0u128, 2, 5, 7] {
        assert!(
            r.lo <= real && real <= r.hi && (real - r.lo).is_multiple_of(r.stride),
            "real value {real} dropped from the widened result",
        );
    }
}

/// Exhaustively over small (lo, stride, divisor), every real value of the floored
/// image lands inside the returned interval at the returned stride: the reported
/// stride is always a true divisor of the real spacing.
#[test]
fn shr_udiv_stride_never_lies() {
    for stride in 1u128..=8 {
        for lo in 0u128..=3 {
            let src = Interval {
                lo,
                hi: lo + stride * 6,
                stride,
            };
            let elems: Vec<u128> = (0..=6).map(|i| lo + i * stride).collect();
            for c in 1u128..=8 {
                let r = src.udiv(c);
                for &x in &elems {
                    let v = x / c;
                    assert!(
                        r.lo <= v && v <= r.hi && (v - r.lo).is_multiple_of(r.stride),
                        "udiv lied: lo={lo} stride={stride} c={c} x={x} -> {v} outside {r:?}",
                    );
                }
                if c.is_power_of_two() {
                    let k = c.trailing_zeros();
                    let r = src.shr(k);
                    for &x in &elems {
                        let v = x >> k;
                        assert!(
                            r.lo <= v && v <= r.hi && (v - r.lo).is_multiple_of(r.stride),
                            "shr lied: lo={lo} stride={stride} k={k} x={x} -> {v} outside {r:?}",
                        );
                    }
                }
            }
        }
    }
}

// The masked-Thumb shape: the guard bounds `(kind&7) - 1` while the dispatch
// indexes `kind&7`, so `kind&7` must inherit `[1, 7]` from `((kind&7)-1) < 7`.
#[test]
fn guard_on_add_propagates_bound_back_to_operand() {
    let ty = ValueType::I32;
    let mut b = RegisterSet::new().build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let entry = b.create_region_all().unwrap();
    let dispatch = b.create_region_all().unwrap();
    let exit = b.create_region_all().unwrap();
    b.set_entry_region_all(entry).unwrap();
    b.set_region(entry);
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
    b.build_return(Some(x), &[]).unwrap();
    b.set_region(exit);
    b.build_return(Some(x), &[]).unwrap();
    b.set_lift_addr(None);
    let mut f = b.build().unwrap();
    canonicalize(&mut f);
    // Bare `Less(diff, 7)` cond (no Xor) -> no branch swap -> dispatch is the
    // If's true-edge consumer.  `x` is a Load and survives canonicalisation.
    let (dispatch_node, _exit_node) = if_edge_consumers(&f);

    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let mut ranges = compute_value_ranges(&f, &doms, &known);

    let iv = ranges.range_of(x, dispatch_node);
    assert_eq!(
        (iv.lo, iv.hi),
        (1, 7),
        "guard on Add(X,-1)  in  [0,6] must bound X  in  [1,7]"
    );
}

// A guard on `Add(X, const)` whose back-propagated interval WRAPS must be
// rejected (-> X stays top), not silently recorded as a straddle-zero interval.
// Guard `(X + 4) < 8` bounds `diff  in  [0, 7]`; shifting back by `-4` gives
// `lo = 0 - 4` (wraps to a huge value) and `hi = 7 - 4 = 3`, so `lo > hi`.
// `add_operand_shifted_interval` must return None and `range_of(X)` stays top.
#[test]
fn guard_on_add_with_wrapping_backprop_stays_top() {
    let ty = ValueType::I32;
    let mut b = RegisterSet::new().build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let entry = b.create_region_all().unwrap();
    let dispatch = b.create_region_all().unwrap();
    let exit = b.create_region_all().unwrap();
    b.set_entry_region_all(entry).unwrap();
    b.set_region(entry);
    let dummy_addr = b.build_int_const(0xDEAD_u64, ValueType::I64).unwrap();
    let x = b.build_load(dummy_addr, rsleigh::VnSpace::RAM, ty).unwrap();
    let four = b.build_int_const(4u64, ty).unwrap();
    let diff = b
        .build_int_binary_operation(x, four, IntBinaryOp::Add, ty)
        .unwrap();
    let eight = b.build_int_const(8u64, ty).unwrap();
    let cond = b
        .build_int_cmp_operation(diff, eight, IntCmpOp::Less, ty)
        .unwrap();
    b.build_if(cond, dispatch, exit).unwrap();
    b.set_region(dispatch);
    b.build_return(Some(x), &[]).unwrap();
    b.set_region(exit);
    b.build_return(Some(x), &[]).unwrap();
    b.set_lift_addr(None);
    let mut f = b.build().unwrap();
    canonicalize(&mut f);
    // Bare `Less(diff, 8)` cond -> no branch swap -> dispatch is the true-edge
    // consumer.  `x` is a Load and survives canonicalisation.
    let (dispatch_node, _exit_node) = if_edge_consumers(&f);

    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let mut ranges = compute_value_ranges(&f, &doms, &known);

    let iv = ranges.range_of(x, dispatch_node);
    let type_mask = ty.bit_mask_u128();
    assert!(
        iv.is_top(type_mask),
        "wrapping back-prop of guard on Add(X,4)  in  [0,7] must leave X top, \
         got [{}, {}]",
        iv.lo,
        iv.hi,
    );
}

// Writing idx into a tracked variable in the entry and reading it back in
// dispatch makes the builder insert a single-input Phi.
#[test]
fn trivial_phi_of_guarded_index_is_bounded() {
    use rsleigh::VnSpace;
    use strider_ir_test_utils::reg_vn;

    let idx_vn = reg_vn(0x10, 4); // 4-byte register -> I32
    let mut b = RegisterSet::new().tracked(idx_vn).build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    let entry = b.create_region_all().unwrap();
    let dispatch = b.create_region_all().unwrap();
    let exit = b.create_region_all().unwrap();

    b.set_entry_region_all(entry).unwrap();

    b.set_region(entry);
    // A load, so idx carries no KnownBits facts.
    let dummy = b.build_int_const(0xCAFEu64, ValueType::I64).unwrap();
    let raw_idx = b.build_load(dummy, VnSpace::RAM, ValueType::I32).unwrap();
    b.write_variable(&idx_vn, raw_idx).unwrap();
    let bound_c = b.build_int_const(8u64, ValueType::I32).unwrap();
    let cond = b
        .build_int_cmp_operation(raw_idx, bound_c, IntCmpOp::Less, ValueType::I32)
        .unwrap();
    b.build_if(cond, dispatch, exit).unwrap();

    b.set_region(dispatch);
    let phi_idx = b.read_variable(&idx_vn).unwrap();
    b.build_return(Some(phi_idx), &[]).unwrap();

    b.set_region(exit);
    b.build_return(Some(raw_idx), &[]).unwrap();

    b.set_lift_addr(None);
    let mut f = b.build().unwrap();
    canonicalize(&mut f);
    // `PhiCollapse` removes the trivial single-input phi, so `idx` is now the
    // underlying `raw_idx` Load (which survives).  The cond is a bare
    // `Less(raw_idx, 8)` (no Xor) -> no swap -> dispatch is the true-edge consumer.
    let (dispatch_node, _exit_node) = if_edge_consumers(&f);

    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let mut ranges = compute_value_ranges(&f, &doms, &known);

    let iv = ranges.range_of(raw_idx, dispatch_node);
    assert_eq!(iv.lo, 0, "trivial phi: lower bound must be 0");
    assert_eq!(iv.hi, 7, "trivial phi: upper bound must be 7");
}

#[test]
fn known_bits_mask_bounds_index_everywhere() {
    let mut b = RegisterSet::new().build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    let entry = b.create_region_all().unwrap();
    let other = b.create_region_all().unwrap();

    b.set_entry_region_all(entry).unwrap();
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

    // A trivial branch to `other`, so the range there is checkable too.
    let one = b.build_boolean_const(true);
    b.build_if(one, other, other).unwrap();

    b.set_region(other);
    let other_ctrl = b.region_cur_ctrl(other);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_lift_addr(None);
    let f = b.build().unwrap();

    let other_node = f.graph().producer(other_ctrl);
    let entry_node = f.entry();

    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let mut ranges = compute_value_ranges(&f, &doms, &known);

    let iv_entry = ranges.range_of(idx, entry_node);
    assert_eq!(iv_entry.hi, 7, "KnownBits bound: hi must be 7 in entry");
    assert_eq!(iv_entry.lo, 0, "KnownBits bound: lo must be 0 in entry");

    let iv_other = ranges.range_of(idx, other_node);
    assert_eq!(iv_other.hi, 7, "KnownBits bound: hi must be 7 in other");
}

/// A scaled index `(arg & 7) << 3` has its low 3 bits known-zero, so KnownBits
/// gives it stride 8 and `count()` = 8 (not the 57-wide raw span).  This is the
/// congruence the table-dispatch cap keys on: a wide-but-strided scaled index is
/// enumerable, an equally-wide dense one is not.
#[test]
fn known_bits_scaled_index_carries_stride() {
    let mut b = RegisterSet::new().build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    let entry = b.create_region_all().unwrap();
    b.set_entry_region_all(entry).unwrap();
    b.set_region(entry);

    let dummy = b.build_int_const(0x1000u64, ValueType::I64).unwrap();
    let arg = b
        .build_load(dummy, rsleigh::VnSpace::RAM, ValueType::I32)
        .unwrap();
    let mask = b.build_int_const(7u64, ValueType::I32).unwrap();
    let idx = b
        .build_int_binary_operation(arg, mask, IntBinaryOp::And, ValueType::I32)
        .unwrap();
    let three = b.build_int_const(3u64, ValueType::I32).unwrap();
    // scaled = idx * 8  =>  values {0, 8, 16, ... 56}, low 3 bits known-zero.
    let scaled = b
        .build_int_binary_operation(idx, three, IntBinaryOp::ShiftLeft, ValueType::I32)
        .unwrap();

    b.build_return(Some(scaled), &[]).unwrap();
    b.set_lift_addr(None);
    let f = b.build().unwrap();

    let entry_node = f.entry();
    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let mut ranges = compute_value_ranges(&f, &doms, &known);

    let iv = ranges.range_of(scaled, entry_node);
    assert_eq!((iv.lo, iv.hi), (0, 56), "scaled index spans [0, 56]");
    assert_eq!(iv.stride, 8, "low 3 known-zero bits ⇒ stride 8");
    assert_eq!(
        iv.count(),
        8,
        "8 distinct entries, not the 57-wide raw span"
    );
}

#[test]
fn unguarded_predecessor_makes_range_top() {
    use rsleigh::VnSpace;
    use strider_ir_test_utils::reg_vn;

    let idx_vn = reg_vn(0x10, 4);
    let mut b = RegisterSet::new().tracked(idx_vn).build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    // Regions: entry -> (guarded_region, unguarded_region) -> dispatch
    let entry = b.create_region_all().unwrap();
    let guarded = b.create_region_all().unwrap();
    let unguarded = b.create_region_all().unwrap();
    let dispatch = b.create_region_all().unwrap();

    b.set_entry_region_all(entry).unwrap();

    b.set_region(entry);
    let dummy = b.build_int_const(0xBEEFu64, ValueType::I64).unwrap();
    let raw_idx = b.build_load(dummy, VnSpace::RAM, ValueType::I32).unwrap();
    b.write_variable(&idx_vn, raw_idx).unwrap();
    let bound_c = b.build_int_const(8u64, ValueType::I32).unwrap();
    let cond = b
        .build_int_cmp_operation(raw_idx, bound_c, IntCmpOp::Less, ValueType::I32)
        .unwrap();
    b.build_if(cond, guarded, unguarded).unwrap();

    b.set_region(guarded);
    // Keep the same SSA write for idx_vn so the phi in dispatch has this value.
    b.write_variable(&idx_vn, raw_idx).unwrap();
    b.build_branch(dispatch).unwrap();

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
    let mut ranges = compute_value_ranges(&f, &doms, &known);

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

// `idx <= 15` lowers to `If(Xor(Less(IntConst(15), idx), IntConst(1)):I1)`.
#[test]
fn lowered_le_guard_bounds_index() {
    let mut b = RegisterSet::new().build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    let entry = b.create_region_all().unwrap();
    let dispatch = b.create_region_all().unwrap();
    let exit = b.create_region_all().unwrap();

    b.set_entry_region_all(entry).unwrap();
    b.set_region(entry);

    let dummy = b.build_int_const(0xF00Du64, ValueType::I64).unwrap();
    let idx = b
        .build_load(dummy, rsleigh::VnSpace::RAM, ValueType::I32)
        .unwrap();

    let n15 = b.build_int_const(15u64, ValueType::I32).unwrap();
    let inner_less = b
        .build_int_cmp_operation(n15, idx, IntCmpOp::Less, ValueType::I32)
        .unwrap();
    // Xor with IntConst(1) at I1 is logical NOT.
    let one_i1 = b.build_int_const(1u64, ValueType::I1).unwrap();
    let cond = b
        .build_int_binary_operation(inner_less, one_i1, IntBinaryOp::Xor, ValueType::I1)
        .unwrap();
    b.build_if(cond, dispatch, exit).unwrap();

    b.set_region(dispatch);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_region(exit);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_lift_addr(None);
    let mut f = b.build().unwrap();
    canonicalize(&mut f);
    // IfCondInversion swaps the branches, so dispatch is now the FALSE edge.
    let (_exit_node, dispatch_node) = if_edge_consumers(&f);

    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let mut ranges = compute_value_ranges(&f, &doms, &known);

    let iv = ranges.range_of(idx, dispatch_node);
    assert_eq!(iv.lo, 0, "lowered <=: lower bound must be 0");
    assert_eq!(
        iv.hi, 15,
        "lowered <=: upper bound must be 15 for idx <= 15"
    );
}

// Xor is commutative, so the guard extractor must recognise the
// constant-first operand order too.
#[test]
fn lowered_le_guard_swapped_xor_operands_still_bounds_index() {
    let mut b = RegisterSet::new().build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    let entry = b.create_region_all().unwrap();
    let dispatch = b.create_region_all().unwrap();
    let exit = b.create_region_all().unwrap();

    b.set_entry_region_all(entry).unwrap();
    b.set_region(entry);

    let dummy = b.build_int_const(0xF00Du64, ValueType::I64).unwrap();
    let idx = b
        .build_load(dummy, rsleigh::VnSpace::RAM, ValueType::I32)
        .unwrap();

    let n15 = b.build_int_const(15u64, ValueType::I32).unwrap();
    let inner_less = b
        .build_int_cmp_operation(n15, idx, IntCmpOp::Less, ValueType::I32)
        .unwrap();
    let one_i1 = b.build_int_const(1u64, ValueType::I1).unwrap();
    // one_i1 is the FIRST operand here.
    let cond = b
        .build_int_binary_operation(one_i1, inner_less, IntBinaryOp::Xor, ValueType::I1)
        .unwrap();
    b.build_if(cond, dispatch, exit).unwrap();

    b.set_region(dispatch);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_region(exit);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_lift_addr(None);
    let mut f = b.build().unwrap();
    canonicalize(&mut f);
    // IfCondInversion swaps the branches, so dispatch is now the FALSE edge.
    let (_exit_node, dispatch_node) = if_edge_consumers(&f);

    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let mut ranges = compute_value_ranges(&f, &doms, &known);

    let iv = ranges.range_of(idx, dispatch_node);
    assert_eq!(iv.lo, 0, "swapped Xor: lower bound must be 0");
    assert_eq!(
        iv.hi, 15,
        "swapped Xor: upper bound must be 15 for idx <= 15 (operand-order-independent)"
    );
}

// With `idx = load & 0xFF` the I32 sign bit is known zero, so `Sless(idx, 8)`
// is trustworthy as an unsigned bound: the guard interval intersected with the
// KnownBits base [0, 255] gives [0, 7].
#[test]
fn sless_guard_with_known_zero_sign_bit_bounds_index() {
    let mut b = RegisterSet::new().build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    let entry = b.create_region_all().unwrap();
    let dispatch = b.create_region_all().unwrap();
    let exit = b.create_region_all().unwrap();

    b.set_entry_region_all(entry).unwrap();
    b.set_region(entry);

    let dummy = b.build_int_const(0xABCDu64, ValueType::I64).unwrap();
    let raw = b
        .build_load(dummy, rsleigh::VnSpace::RAM, ValueType::I32)
        .unwrap();
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
    b.build_return(Some(idx), &[]).unwrap();

    b.set_region(exit);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_lift_addr(None);
    let mut f = b.build().unwrap();
    canonicalize(&mut f);
    // Bare `Sless` cond: no swap, so dispatch is the true-edge consumer.
    let (dispatch_node, _exit_node) = if_edge_consumers(&f);

    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let mut ranges = compute_value_ranges(&f, &doms, &known);

    let iv = ranges.range_of(idx, dispatch_node);
    assert_eq!(iv.lo, 0, "Sless with known-zero sign bit: lower bound 0");
    assert_eq!(
        iv.hi, 7,
        "Sless with known-zero sign bit: idx s< 8 -> [0, 7]"
    );
}

// In the inverted-sense branch `Xor(Less(idx, 8), 1)` the constraint `idx < 8`
// holds on the FALSE successor, while the TRUE successor knows only
// `idx >= 8`, which is lower-only and useless for table sizing.
// `IfCondInversion` normalises this shape away in the production pipeline, but
// a custom pipeline omitting that pass must still get the false-edge bound.
#[test]
fn inverted_less_guard_bounds_index_on_false_edge() {
    let mut b = RegisterSet::new().build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    let entry = b.create_region_all().unwrap();
    let oob = b.create_region_all().unwrap(); // true edge: idx >= 8
    let dispatch = b.create_region_all().unwrap(); // false edge: idx < 8

    b.set_entry_region_all(entry).unwrap();
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
    b.build_return(Some(idx), &[]).unwrap();

    b.set_region(dispatch);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_lift_addr(None);
    let mut f = b.build().unwrap();
    canonicalize(&mut f);
    // IfCondInversion swaps the branches, so dispatch is now the TRUE edge.
    let (dispatch_node, oob_node) = if_edge_consumers(&f);

    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let mut ranges = compute_value_ranges(&f, &doms, &known);

    let type_mask = ValueType::I32.bit_mask_u128();
    assert!(
        ranges.range_of(idx, oob_node).is_top(type_mask),
        "true edge of the inverted guard carries only a lower bound (idx >= 8) -> top"
    );
    let iv = ranges.range_of(idx, dispatch_node);
    assert_eq!(iv.lo, 0, "false edge of inverted guard: lower bound 0");
    assert_eq!(
        iv.hi, 7,
        "false edge of inverted guard: `idx < 8` holds -> upper bound 7"
    );
}

#[test]
fn no_constraint_is_top() {
    let mut b = RegisterSet::new().build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    let entry = b.create_region_all().unwrap();
    b.set_entry_region_all(entry).unwrap();
    b.set_region(entry);

    let dummy = b.build_int_const(0x4242u64, ValueType::I64).unwrap();
    let idx = b
        .build_load(dummy, rsleigh::VnSpace::RAM, ValueType::I32)
        .unwrap();
    b.build_return(Some(idx), &[]).unwrap();

    b.set_lift_addr(None);
    let f = b.build().unwrap();
    let entry_node = f.entry();

    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let mut ranges = compute_value_ranges(&f, &doms, &known);

    let iv = ranges.range_of(idx, entry_node);
    let type_mask = ValueType::I32.bit_mask_u128();
    assert!(
        iv.is_top(type_mask),
        "unconstrained load must produce top, got [{}, {}]",
        iv.lo,
        iv.hi
    );
}

// Without a known-zero sign bit a signed guard cannot be trusted as unsigned.
#[test]
fn sless_guard_without_known_sign_bit_is_top() {
    // A raw load carries no KnownBits, so idx's sign bit is unknown.
    let mut b = RegisterSet::new().build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    let entry = b.create_region_all().unwrap();
    let dispatch = b.create_region_all().unwrap();
    let exit = b.create_region_all().unwrap();

    b.set_entry_region_all(entry).unwrap();
    b.set_region(entry);

    let dummy = b.build_int_const(0xABCDu64, ValueType::I64).unwrap();
    let idx = b
        .build_load(dummy, rsleigh::VnSpace::RAM, ValueType::I32)
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
    let mut ranges = compute_value_ranges(&f, &doms, &known);

    let iv = ranges.range_of(idx, dispatch_node);
    let type_mask = ValueType::I32.bit_mask_u128();
    assert!(
        iv.is_top(type_mask),
        "Sless without known-zero sign bit must yield top, got [{}, {}]",
        iv.lo,
        iv.hi
    );
}

// The guard constrains the TRUE edge only.  The FALSE edge knows just
// `idx >= 8`, which the analysis does not model.
#[test]
fn false_successor_of_guard_is_top() {
    let (f, idx, _dispatch, exit) = build_guarded_dispatch(8, ValueType::I32);

    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let mut ranges = compute_value_ranges(&f, &doms, &known);

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

// A region not dominated by the guard's true successor stays top.  Entry can
// carry only one terminator, so the guard lives in the left arm of a diamond:
//
//   entry       -> If(flag) -> left, right
//   left        -> If(idx < 8) -> dispatch, guarded_exit
//   right       -> returns directly, never dominated by the guard
#[test]
fn sibling_region_not_dominated_is_top() {
    use rsleigh::VnSpace;
    use strider_ir_test_utils::reg_vn;

    let idx_vn = reg_vn(0x10, 4);
    let mut b = RegisterSet::new().tracked(idx_vn).build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    let entry = b.create_region_all().unwrap();
    let left = b.create_region_all().unwrap();
    let right = b.create_region_all().unwrap();
    let dispatch = b.create_region_all().unwrap();
    let guarded_exit = b.create_region_all().unwrap();

    b.set_entry_region_all(entry).unwrap();

    b.set_region(entry);
    let dummy = b.build_int_const(0x1000u64, ValueType::I64).unwrap();
    let raw_idx = b.build_load(dummy, VnSpace::RAM, ValueType::I32).unwrap();
    b.write_variable(&idx_vn, raw_idx).unwrap();
    let flag = b.build_boolean_const(true);
    b.build_if(flag, left, right).unwrap();

    b.set_region(left);
    let bound_c = b.build_int_const(8u64, ValueType::I32).unwrap();
    let cond = b
        .build_int_cmp_operation(raw_idx, bound_c, IntCmpOp::Less, ValueType::I32)
        .unwrap();
    b.build_if(cond, dispatch, guarded_exit).unwrap();

    b.set_region(dispatch);
    b.build_return(Some(raw_idx), &[]).unwrap();

    b.set_region(guarded_exit);
    b.build_return(Some(raw_idx), &[]).unwrap();

    b.set_region(right);
    b.build_return(Some(raw_idx), &[]).unwrap();

    b.set_lift_addr(None);
    let mut f = b.build().unwrap();
    canonicalize(&mut f);

    // Two Ifs survive, so identify the guard one by its IntCmpOp cond.
    let guard_if = f
        .walk()
        .find(|&n| {
            matches!(f.node_kind(n), NodeKind::If)
                && matches!(
                    f.node_kind(f.graph().producer(f.graph().nth_input(n, 1).unwrap())),
                    NodeKind::IntCmpOp(_)
                )
        })
        .expect("guard If");
    let flag_if = f
        .walk()
        .find(|&n| matches!(f.node_kind(n), NodeKind::If) && n != guard_if)
        .expect("flag If");
    let edge_consumer = |if_node: NodeId, slot: usize| -> NodeId {
        let ctrl = f.node_outputs(if_node)[slot];
        f.graph().value_uses(ctrl).next().expect("edge consumer").0
    };
    let dispatch_node = edge_consumer(guard_if, 0); // true edge of the guard
    let right_node = edge_consumer(flag_if, 1); // false edge -> sibling right

    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let mut ranges = compute_value_ranges(&f, &doms, &known);

    let iv_dispatch = ranges.range_of(raw_idx, dispatch_node);
    assert_eq!(
        iv_dispatch.hi, 7,
        "dispatch must have bound 7, got {}",
        iv_dispatch.hi
    );

    let iv_right = ranges.range_of(raw_idx, right_node);
    let type_mask = ValueType::I32.bit_mask_u128();
    assert!(
        iv_right.is_top(type_mask),
        "sibling region must be top, got [{}, {}]",
        iv_right.lo,
        iv_right.hi
    );
}

// A loop-carried `idx_phi = Phi(0, idx_phi + 1)` is top: the back-edge arm is
// an `Add`, an opaque leaf here, with no guard or KnownBits bound, and no
// guard on idx_phi dominates the header.
#[test]
fn cyclic_phi_is_top() {
    use strider_ir_test_utils::reg_vn;

    let idx_vn = reg_vn(0x10, 4); // 4-byte register -> I32
    let mut b = RegisterSet::new().tracked(idx_vn).build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    let entry = b.create_region_all().unwrap();
    let header = b.create_region_all().unwrap();
    let body = b.create_region_all().unwrap();
    let loop_exit = b.create_region_all().unwrap();

    b.set_entry_region_all(entry).unwrap();

    b.set_region(entry);
    let zero = b.build_int_const(0u64, ValueType::I32).unwrap();
    b.write_variable(&idx_vn, zero).unwrap();
    b.build_branch(header).unwrap();

    // Reading the variable here becomes a Phi once the regions are linked.
    b.set_region(header);
    let header_ctrl = b.region_cur_ctrl(header);
    let idx_phi = b.read_variable(&idx_vn).unwrap();
    let bound = b.build_int_const(16u64, ValueType::I32).unwrap();
    let cond = b
        .build_int_cmp_operation(idx_phi, bound, IntCmpOp::Less, ValueType::I32)
        .unwrap();
    b.build_if(cond, body, loop_exit).unwrap();

    b.set_region(body);
    let one = b.build_int_const(1u64, ValueType::I32).unwrap();
    let idx_next = b
        .build_int_binary_operation(idx_phi, one, IntBinaryOp::Add, ValueType::I32)
        .unwrap();
    b.write_variable(&idx_vn, idx_next).unwrap();
    b.build_branch(header).unwrap();

    b.set_region(loop_exit);
    b.build_return(Some(idx_phi), &[]).unwrap();

    b.set_lift_addr(None);
    let f = b.build().unwrap();
    let header_node = f.graph().producer(header_ctrl);

    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let mut ranges = compute_value_ranges(&f, &doms, &known);

    let iv = ranges.range_of(idx_phi, header_node);
    let type_mask = ValueType::I32.bit_mask_u128();
    assert!(
        iv.is_top(type_mask),
        "cyclic phi must yield top, got [{}, {}]",
        iv.lo,
        iv.hi
    );
}

// The resolver only recurses through `Phi`, so a phi-of-phi is the one shape
// that drives it back into itself: two loop variables swapping every
// iteration, `phi_a = Phi(0, phi_b)` and `phi_b = Phi(1, phi_a)`, referencing
// each other with no intervening node.  `range_of` must RETURN: the cycle is
// cut rather than chased forever.
#[test]
fn phi_of_phi_cycle_terminates_top() {
    use strider_ir_test_utils::reg_vn;

    let a_vn = reg_vn(0x10, 4); // 4-byte register -> I32
    let b_vn = reg_vn(0x20, 4); // 4-byte register -> I32
    let mut b = RegisterSet::new()
        .tracked(a_vn)
        .tracked(b_vn)
        .build_fn()
        .unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    let entry = b.create_region_all().unwrap();
    let header = b.create_region_all().unwrap();
    let body = b.create_region_all().unwrap();
    let loop_exit = b.create_region_all().unwrap();

    b.set_entry_region_all(entry).unwrap();

    b.set_region(entry);
    let zero = b.build_int_const(0u64, ValueType::I32).unwrap();
    let one_seed = b.build_int_const(1u64, ValueType::I32).unwrap();
    b.write_variable(&a_vn, zero).unwrap();
    b.write_variable(&b_vn, one_seed).unwrap();
    b.build_branch(header).unwrap();

    // Reading `a` here becomes phi_a once the regions are linked.
    b.set_region(header);
    let header_ctrl = b.region_cur_ctrl(header);
    let phi_a = b.read_variable(&a_vn).unwrap();
    let bound = b.build_int_const(16u64, ValueType::I32).unwrap();
    let cond = b
        .build_int_cmp_operation(phi_a, bound, IntCmpOp::Less, ValueType::I32)
        .unwrap();
    b.build_if(cond, body, loop_exit).unwrap();

    // Swapping a and b makes each variable's back-edge value the OTHER header
    // phi, so the two phis reference each other directly.
    b.set_region(body);
    let old_a = b.read_variable(&a_vn).unwrap();
    let old_b = b.read_variable(&b_vn).unwrap();
    b.write_variable(&a_vn, old_b).unwrap();
    b.write_variable(&b_vn, old_a).unwrap();
    b.build_branch(header).unwrap();

    b.set_region(loop_exit);
    b.build_return(Some(phi_a), &[]).unwrap();

    b.set_lift_addr(None);
    let f = b.build().unwrap();
    let header_node = f.graph().producer(header_ctrl);

    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let mut ranges = compute_value_ranges(&f, &doms, &known);

    let iv = ranges.range_of(phi_a, header_node);
    let type_mask = ValueType::I32.bit_mask_u128();
    assert!(
        iv.is_top(type_mask),
        "phi-of-phi cycle must yield top, got [{}, {}]",
        iv.lo,
        iv.hi
    );
}

// Nested `idx < 16` then `idx < 8` guards must intersect to [0, 7] at the
// inner dispatch.
#[test]
fn nested_guards_intersect_at_inner_region() {
    let mut b = RegisterSet::new().build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    let entry = b.create_region_all().unwrap();
    let outer_guard = b.create_region_all().unwrap();
    let dispatch = b.create_region_all().unwrap();
    let mid_exit = b.create_region_all().unwrap();
    let exit = b.create_region_all().unwrap();

    b.set_entry_region_all(entry).unwrap();

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

    b.set_region(outer_guard);
    let bound8 = b.build_int_const(8u64, ValueType::I32).unwrap();
    let cond8 = b
        .build_int_cmp_operation(idx, bound8, IntCmpOp::Less, ValueType::I32)
        .unwrap();
    b.build_if(cond8, dispatch, mid_exit).unwrap();

    b.set_region(dispatch);
    b.build_return(Some(idx), &[]).unwrap();

    // Bounded by the outer guard only.
    b.set_region(mid_exit);
    b.build_return(Some(idx), &[]).unwrap();

    // Outside both guards.
    b.set_region(exit);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_lift_addr(None);
    let mut f = b.build().unwrap();
    canonicalize(&mut f);

    // Two guard Ifs survive, so the sole-If helper cannot be used; find the
    // inner one by its bound.
    let inner_if = f
        .walk()
        .find(|&n| {
            if !matches!(f.node_kind(n), NodeKind::If) {
                return false;
            }
            let cmp = f.graph().producer(f.graph().nth_input(n, 1).unwrap());
            matches!(f.node_kind(cmp), NodeKind::IntCmpOp(_))
                && f.int_const_u128(f.graph().nth_input(cmp, 1).unwrap()) == Some(8)
        })
        .expect("inner guard If (bound 8)");
    let inner_true_ctrl = f.node_outputs(inner_if)[0];
    let dispatch_node = f
        .graph()
        .value_uses(inner_true_ctrl)
        .next()
        .expect("inner true-edge consumer")
        .0;

    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let mut ranges = compute_value_ranges(&f, &doms, &known);

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

// `v < 0` is impossible unsigned, so the guard can never fire and the analysis
// must not yield the spurious [0, 0] that `saturating_sub(1)` gives for N = 0.
#[test]
fn strict_less_zero_bound_is_top() {
    let (f, idx, dispatch_region, _exit) = build_guarded_dispatch(0, ValueType::I32);

    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let mut ranges = compute_value_ranges(&f, &doms, &known);

    let iv = ranges.range_of(idx, dispatch_region);
    let type_mask = ValueType::I32.bit_mask_u128();
    assert!(
        iv.is_top(type_mask),
        "Less(idx, 0) guard must yield top (impossible guard), got [{}, {}]: \
         saturating_sub(1) must not produce [0,0]",
        iv.lo,
        iv.hi
    );
}

// `saturating_sub(1)` at the boundary: `Less(idx, type_mask)` still narrows,
// just by a single step, so the result is not top.
//
// A bound of `type_mask + 1` is untestable here: `build_int_const` masks to
// the type width and wraps it to 0, which is the impossible-guard case above.
#[test]
fn strict_less_at_type_mask_narrows_by_one() {
    let type_mask_u64 = 0xFFFF_FFFFu64;
    let (f, idx, dispatch_region, _exit) = build_guarded_dispatch(type_mask_u64, ValueType::I32);

    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let mut ranges = compute_value_ranges(&f, &doms, &known);

    let iv = ranges.range_of(idx, dispatch_region);
    let type_mask = ValueType::I32.bit_mask_u128();
    assert_eq!(
        iv.hi,
        type_mask - 1,
        "Less(idx, type_mask) must narrow by 1, got hi={}",
        iv.hi
    );
    assert_eq!(
        iv.upper_exclusive(type_mask),
        Some(type_mask_u64),
        "upper_exclusive must be Some(type_mask)"
    );
    assert!(
        !iv.is_top(type_mask),
        "Less(idx, type_mask) must NOT be top (it narrows by 1)"
    );
}

// The same value guarded `< 8` in one sibling branch and `< 16` in the other
// must get each bound independently at its own dispatch.
#[test]
fn two_sibling_guard_regions_give_independent_bounds() {
    let mut b = RegisterSet::new().build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    let entry = b.create_region_all().unwrap();
    let branch_a = b.create_region_all().unwrap();
    let branch_b = b.create_region_all().unwrap();
    let dispatch_a = b.create_region_all().unwrap();
    let sink_a = b.create_region_all().unwrap();
    let dispatch_b = b.create_region_all().unwrap();
    let sink_b = b.create_region_all().unwrap();

    b.set_entry_region_all(entry).unwrap();

    b.set_region(entry);
    let dummy = b.build_int_const(0xCAFEu64, ValueType::I64).unwrap();
    let idx = b
        .build_load(dummy, rsleigh::VnSpace::RAM, ValueType::I32)
        .unwrap();
    let flag = b.build_boolean_const(true);
    b.build_if(flag, branch_a, branch_b).unwrap();

    b.set_region(branch_a);
    let bound8 = b.build_int_const(8u64, ValueType::I32).unwrap();
    let cond8 = b
        .build_int_cmp_operation(idx, bound8, IntCmpOp::Less, ValueType::I32)
        .unwrap();
    b.build_if(cond8, dispatch_a, sink_a).unwrap();

    b.set_region(dispatch_a);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_region(sink_a);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_region(branch_b);
    let bound16 = b.build_int_const(16u64, ValueType::I32).unwrap();
    let cond16 = b
        .build_int_cmp_operation(idx, bound16, IntCmpOp::Less, ValueType::I32)
        .unwrap();
    b.build_if(cond16, dispatch_b, sink_b).unwrap();

    b.set_region(dispatch_b);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_region(sink_b);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_lift_addr(None);
    let mut f = b.build().unwrap();
    canonicalize(&mut f);

    // Several Ifs survive, so identify each guard by its cond constant.
    let true_edge_consumer_of_guard = |bound: u128| -> NodeId {
        let if_node = f
            .walk()
            .find(|&n| {
                if !matches!(f.node_kind(n), NodeKind::If) {
                    return false;
                }
                let cond = f.graph().nth_input(n, 1).expect("If cond");
                let cmp = f.graph().producer(cond);
                if !matches!(f.node_kind(cmp), NodeKind::IntCmpOp(_)) {
                    return false;
                }
                let rhs = f.graph().nth_input(cmp, 1).expect("cmp rhs");
                f.int_const_u128(rhs) == Some(bound)
            })
            .expect("guard If with matching bound");
        let true_ctrl = f.node_outputs(if_node)[0];
        f.graph()
            .value_uses(true_ctrl)
            .next()
            .expect("true-edge consumer")
            .0
    };
    let da_node = true_edge_consumer_of_guard(8);
    let db_node = true_edge_consumer_of_guard(16);

    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let mut ranges = compute_value_ranges(&f, &doms, &known);

    let iv_a = ranges.range_of(idx, da_node);
    assert_eq!(iv_a.lo, 0, "dispatch_a lo must be 0");
    assert_eq!(iv_a.hi, 7, "dispatch_a hi must be 7 (idx < 8)");

    let iv_b = ranges.range_of(idx, db_node);
    assert_eq!(iv_b.lo, 0, "dispatch_b lo must be 0");
    assert_eq!(iv_b.hi, 15, "dispatch_b hi must be 15 (idx < 16)");
}

// Two arms with distinct finite KnownBits bounds, `& 7` and `& 15`, must
// UNION to the wider [0,15], not intersect to [0,7] and not fall back to top.
#[test]
fn multi_input_phi_unions_two_distinct_finite_arms() {
    use rsleigh::VnSpace;
    use strider_ir_test_utils::reg_vn;

    let idx_vn = reg_vn(0x10, 4); // 4-byte register -> I32
    let mut b = RegisterSet::new().tracked(idx_vn).build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    let entry = b.create_region_all().unwrap();
    let path_a = b.create_region_all().unwrap();
    let path_b = b.create_region_all().unwrap();
    let join = b.create_region_all().unwrap();

    b.set_entry_region_all(entry).unwrap();

    b.set_region(entry);
    let flag = b.build_boolean_const(true);
    b.build_if(flag, path_a, path_b).unwrap();

    b.set_region(path_a);
    let dummy_a = b.build_int_const(0xAAAAu64, ValueType::I64).unwrap();
    let load_a = b.build_load(dummy_a, VnSpace::RAM, ValueType::I32).unwrap();
    let mask7 = b.build_int_const(7u64, ValueType::I32).unwrap();
    let idx_a = b
        .build_int_binary_operation(load_a, mask7, IntBinaryOp::And, ValueType::I32)
        .unwrap();
    b.write_variable(&idx_vn, idx_a).unwrap();
    b.build_branch(join).unwrap();

    // A DISTINCT load, so the join phi is genuinely multi-input and survives
    // PhiCollapse.
    b.set_region(path_b);
    let dummy_b = b.build_int_const(0xBBBBu64, ValueType::I64).unwrap();
    let load_b = b.build_load(dummy_b, VnSpace::RAM, ValueType::I32).unwrap();
    let mask15 = b.build_int_const(15u64, ValueType::I32).unwrap();
    let idx_b = b
        .build_int_binary_operation(load_b, mask15, IntBinaryOp::And, ValueType::I32)
        .unwrap();
    b.write_variable(&idx_vn, idx_b).unwrap();
    b.build_branch(join).unwrap();

    b.set_region(join);
    let phi_idx = b.read_variable(&idx_vn).unwrap();
    b.build_return(Some(phi_idx), &[]).unwrap();

    b.set_lift_addr(None);
    let mut f = b.build().unwrap();
    canonicalize(&mut f);

    let phi_producer = f
        .walk()
        .find(|&n| matches!(f.node_kind(n), NodeKind::Phi) && f.phi_data_inputs(n).count() >= 2)
        .expect("the multi-input join phi");
    let phi_idx = f.node_outputs(phi_producer)[0];
    // The joining Region produces the phi's slot-0 PhiToken input.
    let phi_token = f.graph().nth_input(phi_producer, 0).unwrap();
    let join_region = f.graph().producer(phi_token);

    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let mut ranges = compute_value_ranges(&f, &doms, &known);

    let iv = ranges.range_of(phi_idx, join_region);
    assert_eq!(iv.lo, 0, "union lower bound is 0");
    assert_eq!(
        iv.hi, 15,
        "phi must UNION the two distinct finite arms ([0,7] ∪ [0,15] = [0,15]); \
         it must NOT tighten to [0,7] nor widen to top"
    );
    assert!(
        !iv.is_top(ValueType::I32.bit_mask_u128()),
        "the union of two finite arms must stay finite, not be top"
    );
}

#[test]
fn multi_input_phi_output_guard_bounds_index() {
    use rsleigh::VnSpace;
    use strider_ir_test_utils::reg_vn;

    let idx_vn = reg_vn(0x10, 4); // 4-byte register -> I32
    let mut b = RegisterSet::new().tracked(idx_vn).build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    let entry = b.create_region_all().unwrap();
    let path_a = b.create_region_all().unwrap();
    let path_b = b.create_region_all().unwrap();
    let join = b.create_region_all().unwrap();
    let dispatch = b.create_region_all().unwrap();
    let exit = b.create_region_all().unwrap();

    b.set_entry_region_all(entry).unwrap();

    b.set_region(entry);
    let dummy = b.build_int_const(0xDEADu64, ValueType::I64).unwrap();
    let raw_idx = b.build_load(dummy, VnSpace::RAM, ValueType::I32).unwrap();
    b.write_variable(&idx_vn, raw_idx).unwrap();
    let flag = b.build_boolean_const(true);
    b.build_if(flag, path_a, path_b).unwrap();

    // The two arms must carry different SSA values, or the join phi is trivial
    // and `PhiCollapse` removes it, defeating the test.
    b.set_region(path_a);
    let dummy_a = b.build_int_const(0xAAAAu64, ValueType::I64).unwrap();
    let idx_a = b.build_load(dummy_a, VnSpace::RAM, ValueType::I32).unwrap();
    b.write_variable(&idx_vn, idx_a).unwrap();
    b.build_branch(join).unwrap();

    // A DISTINCT load, giving the join phi two distinct data inputs.
    b.set_region(path_b);
    let dummy_b = b.build_int_const(0xBBBBu64, ValueType::I64).unwrap();
    let idx_b = b.build_load(dummy_b, VnSpace::RAM, ValueType::I32).unwrap();
    b.write_variable(&idx_vn, idx_b).unwrap();
    b.build_branch(join).unwrap();

    b.set_region(join);
    let phi_idx = b.read_variable(&idx_vn).unwrap();
    let bound_c = b.build_int_const(8u64, ValueType::I32).unwrap();
    let cond = b
        .build_int_cmp_operation(phi_idx, bound_c, IntCmpOp::Less, ValueType::I32)
        .unwrap();
    b.build_if(cond, dispatch, exit).unwrap();

    b.set_region(dispatch);
    b.build_return(Some(phi_idx), &[]).unwrap();

    b.set_region(exit);
    b.build_return(Some(phi_idx), &[]).unwrap();

    b.set_lift_addr(None);
    let mut f = b.build().unwrap();
    canonicalize(&mut f);

    // Two Ifs survive, so find the guard by its IntCmpOp cond and read the
    // guarded value straight off it, i.e. exactly the value the guard was
    // recorded against.
    let guard_if = f
        .walk()
        .find(|&n| {
            matches!(f.node_kind(n), NodeKind::If)
                && matches!(
                    f.node_kind(f.graph().producer(f.graph().nth_input(n, 1).unwrap())),
                    NodeKind::IntCmpOp(_)
                )
        })
        .expect("the guard If");
    let cmp = f
        .graph()
        .producer(f.graph().nth_input(guard_if, 1).unwrap());
    let phi_idx = f.graph().nth_input(cmp, 0).unwrap();
    let phi_producer = f.graph().producer(phi_idx);
    let true_ctrl = f.node_outputs(guard_if)[0];
    let dispatch_node = f
        .graph()
        .value_uses(true_ctrl)
        .next()
        .expect("dispatch true-edge consumer")
        .0;

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
    let mut ranges = compute_value_ranges(&f, &doms, &known);

    let iv = ranges.range_of(phi_idx, dispatch_node);
    assert_eq!(iv.lo, 0, "multi-phi output guard: lower bound must be 0");
    assert_eq!(
        iv.hi, 7,
        "multi-phi output guard: a dominating `phi_idx < 8` guard must bound the \
         multi-input phi output to [0, 7]"
    );
}

// The soundness-critical fail-closed case: a guard holding on only ONE
// incoming path to a join must not bound the phi.
//
//   entry  -> If(flag) -> path_a, path_b
//   path_a -> If(idx < 4) -> dispatch, exit_a   [idx in [0,3] on this arm]
//   path_b -> dispatch                          [idx UNCONSTRAINED]
//
// Both arms refer to the same underlying value, so querying every arm in the
// joining region would make `dominates(dispatch, dispatch)` reflexively true
// and wrongly yield [0,3].  path_b leaves idx unconstrained, so it is top.
#[test]
fn join_fails_closed_when_one_predecessor_unguarded() {
    use rsleigh::VnSpace;
    use strider_ir::IntCmpOp;
    use strider_ir_test_utils::reg_vn;

    let idx_vn = reg_vn(0x10, 4); // 4-byte register -> I32
    let mut b = RegisterSet::new().tracked(idx_vn).build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    let entry = b.create_region_all().unwrap();
    let path_a = b.create_region_all().unwrap();
    let path_b = b.create_region_all().unwrap();
    let dispatch = b.create_region_all().unwrap();
    let exit_a = b.create_region_all().unwrap();

    b.set_entry_region_all(entry).unwrap();

    b.set_region(entry);
    let dummy = b.build_int_const(0xDEADu64, ValueType::I64).unwrap();
    let raw_idx = b.build_load(dummy, VnSpace::RAM, ValueType::I32).unwrap();
    b.write_variable(&idx_vn, raw_idx).unwrap();
    // A throwaway comparison, only here to split control.
    let zero = b.build_int_const(0u64, ValueType::I32).unwrap();
    let flag = b
        .build_int_cmp_operation(raw_idx, zero, IntCmpOp::Equal, ValueType::I32)
        .unwrap();
    b.build_if(flag, path_a, path_b).unwrap();

    b.set_region(path_a);
    let idx_a = b.read_variable(&idx_vn).unwrap();
    let four = b.build_int_const(4u64, ValueType::I32).unwrap();
    let cond_a = b
        .build_int_cmp_operation(idx_a, four, IntCmpOp::Less, ValueType::I32)
        .unwrap();
    b.build_if(cond_a, dispatch, exit_a).unwrap();

    b.set_region(path_b);
    b.build_branch(dispatch).unwrap();

    b.set_region(dispatch);
    let dispatch_ctrl = b.region_cur_ctrl(dispatch);
    let phi_idx = b.read_variable(&idx_vn).unwrap();
    b.build_return(Some(phi_idx), &[]).unwrap();

    b.set_region(exit_a);
    b.build_return(Some(raw_idx), &[]).unwrap();

    b.set_lift_addr(None);
    let f = b.build().unwrap();
    let dispatch_node = f.graph().producer(dispatch_ctrl);

    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let mut ranges = compute_value_ranges(&f, &doms, &known);

    let iv = ranges.range_of(phi_idx, dispatch_node);
    let type_mask = ValueType::I32.bit_mask_u128();
    assert!(
        iv.is_top(type_mask),
        "join with one unguarded predecessor must be top (fail-closed), \
         got [{}, {}]; a guard on one path must not bound the phi",
        iv.lo,
        iv.hi
    );
    assert!(
        iv.upper_exclusive(type_mask).is_none(),
        "upper_exclusive must be None when join is top"
    );
}

// `Less(IntConst(8), v)` is `8 < v`, so the FALSE edge carries `v <= 8` while
// the TRUE edge knows only `v >= 9`, which is useless for table sizing.  This
// is the post-canonicalisation `ja` shape the jump-table classifier needs.
#[test]
fn const_lhs_less_guard_bounds_false_edge() {
    let mut b = RegisterSet::new().build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    let entry = b.create_region_all().unwrap();
    let above = b.create_region_all().unwrap(); // true edge: v > 8
    let at_or_below = b.create_region_all().unwrap(); // false edge: v <= 8

    b.set_entry_region_all(entry).unwrap();
    b.set_region(entry);

    let dummy = b.build_int_const(0xF00Du64, ValueType::I64).unwrap();
    let idx = b
        .build_load(dummy, rsleigh::VnSpace::RAM, ValueType::I32)
        .unwrap();
    let n8 = b.build_int_const(8u64, ValueType::I32).unwrap();
    // Const on the LHS.
    let cond = b
        .build_int_cmp_operation(n8, idx, IntCmpOp::Less, ValueType::I32)
        .unwrap();
    b.build_if(cond, above, at_or_below).unwrap();

    b.set_region(above);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_region(at_or_below);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_lift_addr(None);
    let mut f = b.build().unwrap();
    canonicalize(&mut f);
    // Bare `Less` cond: no swap, so `above` is the true-edge consumer.
    let (above_node, below_node) = if_edge_consumers(&f);

    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let mut ranges = compute_value_ranges(&f, &doms, &known);

    let type_mask = ValueType::I32.bit_mask_u128();
    assert!(
        ranges.range_of(idx, above_node).is_top(type_mask),
        "const-LHS Less true edge carries only a lower bound (v >= 9) -> top"
    );
    let iv = ranges.range_of(idx, below_node);
    assert_eq!(iv.lo, 0, "const-LHS Less false edge: lower bound 0");
    assert_eq!(
        iv.hi, 8,
        "const-LHS Less false edge: v <= 8 -> upper bound 8"
    );
}

// The guard's true edge feeds a Region that ALSO has a sibling predecessor.
// The guard does not hold for paths arriving that way, so the soundness gate
// must skip recording it and the merge (plus all it dominates) stays top.
#[test]
fn guard_into_control_merge_is_not_applied() {
    let mut b = RegisterSet::new().build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    let entry = b.create_region_all().unwrap();
    let guarded = b.create_region_all().unwrap(); // entry's true edge
    let other = b.create_region_all().unwrap(); // entry's false edge
    let merge = b.create_region_all().unwrap(); // 2 predecessors

    b.set_entry_region_all(entry).unwrap();
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

    b.set_region(guarded);
    b.build_branch(merge).unwrap();

    // `other` branches in too, making this a 2-pred control merge.
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
    let mut ranges = compute_value_ranges(&f, &doms, &known);

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

// The shape the soundness gate exists to reject.  In
// `guard_into_control_merge_is_not_applied` the true edge passes through a
// single-pred Region first, so it is the dominance filter, not the gate, that
// yields top.  Here the If's true output feeds the merge DIRECTLY:
//
//   entry -> If(idx < 8) -> merge (TRUE), other (FALSE)
//   other -> branch -> merge          [merge's 2nd predecessor, no bound]
//
// So `single_control_consumer(true_ctrl)` is the multi-pred merge itself and
// the gate fires.  A region-keyed model instead records the guard against the
// merge, and `dominates(merge, query)` then wrongly bounds `idx` below it.
#[test]
fn guard_on_edge_into_merge_is_top_below_merge() {
    let mut b = RegisterSet::new().build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    let entry = b.create_region_all().unwrap();
    let other = b.create_region_all().unwrap(); // false edge, branches to merge
    let merge = b.create_region_all().unwrap(); // 2 preds: If true edge + other

    b.set_entry_region_all(entry).unwrap();
    b.set_region(entry);
    let dummy = b.build_int_const(0xF00Du64, ValueType::I64).unwrap();
    let idx = b
        .build_load(dummy, rsleigh::VnSpace::RAM, ValueType::I32)
        .unwrap();
    let n8 = b.build_int_const(8u64, ValueType::I32).unwrap();
    let cond = b
        .build_int_cmp_operation(idx, n8, IntCmpOp::Less, ValueType::I32)
        .unwrap();
    // True edge feeds `merge` DIRECTLY, with no intervening region.
    b.build_if(cond, merge, other).unwrap();

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
    let mut ranges = compute_value_ranges(&f, &doms, &known);

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

// `RegionCollapse` deletes the single-predecessor dispatch Region, leaving the
// If's true output feeding a `Return` directly.  Keying the guard on that
// non-Region consumer is what production jump-table resolution depends on.
#[test]
fn guard_survives_region_collapse_at_nonregion_consumer() {
    use crate::RegionCollapse;

    let mut b = RegisterSet::new().build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    let entry = b.create_region_all().unwrap();
    let dispatch = b.create_region_all().unwrap(); // single-pred, collapses
    let exit = b.create_region_all().unwrap();

    b.set_entry_region_all(entry).unwrap();
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

    b.set_region(dispatch);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_region(exit);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_lift_addr(None);
    let mut f = b.build().unwrap();

    // Capture the true control output BEFORE collapse, to find its consumer
    // afterward.
    let if_node = f
        .graph()
        .all_node_ids()
        .find(|&n| matches!(f.node_kind(n), NodeKind::If))
        .expect("If node");
    let true_ctrl = f.node_outputs(if_node)[0];

    let changed = crate::pipeline::run_one(&RegionCollapse, &mut f, &mut crate::OptCtx::new(None))
        .unwrap()
        .changed();
    assert!(changed, "dispatch Region must collapse");

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
    let mut ranges = compute_value_ranges(&f, &doms, &known);

    let iv = ranges.range_of(idx, consumer);
    assert_eq!(iv.lo, 0, "collapsed-shape guard: lower bound 0");
    assert_eq!(
        iv.hi, 7,
        "collapsed-shape guard: bound survives RegionCollapse -> [0, 7]"
    );
}

/// The CRT meet must not step one stride at a time: `s1 = 1` against a
/// KnownBits stride of `2^32` is `lcm/s1`, about four billion iterations.
#[test]
fn intersect_with_a_huge_stride_is_not_a_stepping_loop() {
    let guard = Interval {
        lo: 3,
        hi: 102,
        stride: 1,
    };
    let known_bits = Interval {
        lo: 0,
        hi: u128::from(u64::MAX),
        stride: 1u128 << 32,
    };
    // Only multiples of 2^32 are in `known_bits`, and none lie in 3..=102.
    let met = guard.intersect(known_bits);
    assert_eq!(met.count(), 0, "incompatible residues meet as empty");
}

/// A stride past `2^127` makes `other.lo % s2 + s2` exceed the `u128` carrier,
/// so the residue difference has to be taken without ever forming that sum.
#[test]
fn intersect_with_a_stride_past_the_carrier_half() {
    let huge = (1u128 << 127) + 1;
    let dense = Interval::top(u128::MAX);
    let strided = Interval {
        lo: 1u128 << 127,
        hi: u128::MAX,
        stride: huge,
    };
    let met = dense.intersect(strided);
    // The only element of `strided` inside the carrier is its own `lo`.
    assert_eq!((met.lo, met.stride), (1u128 << 127, huge));
    assert_eq!(met.count(), 1);
}

/// A meet that DOES have a solution still finds it.
#[test]
fn intersect_finds_the_first_common_residue() {
    let a = Interval {
        lo: 0,
        hi: 100,
        stride: 6,
    };
    let b = Interval {
        lo: 0,
        hi: 100,
        stride: 10,
    };
    let met = a.intersect(b);
    // lcm(6,10) = 30, first common element >= 0 is 0.
    assert_eq!((met.lo, met.stride), (0, 30));

    let c = Interval {
        lo: 4,
        hi: 100,
        stride: 6,
    };
    let d = Interval {
        lo: 0,
        hi: 100,
        stride: 10,
    };
    // 4,10,16,22,28,34,40 ... vs 0,10,20,...: first common is 10.
    let met2 = c.intersect(d);
    assert_eq!((met2.lo, met2.stride), (10, 30));
}

/// `shr` and `udiv` map both endpoints, so an EMPTY interval must stay empty
/// rather than collapsing to `{0,0}` ("the value is exactly zero"), which
/// passes `bounded_index`'s gate and fabricates one switch target out of
/// provably dead code. `mul`/`shl` already preserve emptiness.
#[test]
fn shr_and_udiv_preserve_emptiness() {
    let empty = Interval {
        lo: 1,
        hi: 0,
        stride: 1,
    };
    assert_eq!(empty.count(), 0, "precondition: the interval is empty");
    assert_eq!(empty.shr(1).count(), 0, "shr must not resurrect EMPTY");
    assert_eq!(empty.udiv(2).count(), 0, "udiv must not resurrect EMPTY");
    assert_eq!(
        empty.shl(1, u128::MAX).count(),
        0,
        "shl already preserved it"
    );
    assert_eq!(
        empty.mul(2, u128::MAX).count(),
        0,
        "mul already preserved it"
    );
}

/// `count()` on a full-width top: `hi - lo == u128::MAX`, so the `+ 1` must
/// saturate instead of overflowing.
#[test]
fn count_of_full_width_top_saturates() {
    assert_eq!(Interval::top(u128::MAX).count(), u128::MAX);
}

/// A strided interval spanning the full width is NOT top: it carries the extra
/// fact "multiple of `stride`", which a consumer treating it as top would
/// silently adopt.
#[test]
fn strided_full_span_is_not_top() {
    let mask = 0xFFFF_FFFFu128;
    let strided = Interval {
        lo: 0,
        hi: mask,
        stride: 3,
    };
    assert!(!strided.is_top(mask));
    assert!(Interval::top(mask).is_top(mask));
}

/// Every value the interval denotes, clipped to the carrier.
fn members(iv: Interval, mask: u128) -> Vec<u128> {
    if iv.hi < iv.lo {
        return Vec::new();
    }
    let stride = iv.stride.max(1);
    let mut out = Vec::new();
    let mut x = iv.lo;
    while x <= iv.hi.min(mask) {
        out.push(x);
        match x.checked_add(stride) {
            Some(next) => x = next,
            None => break,
        }
    }
    out
}

fn contains(iv: Interval, y: u128) -> bool {
    iv.lo <= y && y <= iv.hi && (y - iv.lo).is_multiple_of(iv.stride.max(1))
}

/// Every concrete result of `shl` / `shr` / `mul` / `udiv` lands inside the
/// abstract one.  The concrete model saturates shift counts at and past the
/// width rather than wrapping, matching `opbehavior.cc`.
#[test]
fn scale_ops_contain_every_concrete_result() {
    const WIDTH: u32 = 5;
    const MASK: u128 = (1 << WIDTH) - 1;
    let mut fails: Vec<String> = Vec::new();
    let mut record = |op: &str, iv: Interval, by: u128, x: u128, concrete: u128, out: Interval| {
        if !contains(out, concrete) {
            fails.push(format!(
                "{op} iv={iv:?} by={by} x={x} concrete={concrete} out={out:?}"
            ));
        }
    };
    for lo in 0..=MASK {
        for hi in 0..=MASK {
            for stride in 1u128..=8 {
                let iv = Interval { lo, hi, stride };
                let set = members(iv, MASK);
                if set.is_empty() {
                    continue;
                }
                for k in [0u32, 1, 2, 3, 4, 5, 6, 7, 31, 127, 128, 200] {
                    let shl = iv.shl(k, MASK);
                    let shr = iv.shr(k);
                    for &x in &set {
                        let up = if k >= WIDTH { 0 } else { (x << k) & MASK };
                        let down = if k >= WIDTH { 0 } else { x >> k };
                        record("shl", iv, u128::from(k), x, up, shl);
                        record("shr", iv, u128::from(k), x, down, shr);
                    }
                }
                for c in 1u128..=8 {
                    let mul = iv.mul(c, MASK);
                    let udiv = iv.udiv(c);
                    for &x in &set {
                        record("mul", iv, c, x, (x * c) & MASK, mul);
                        record("udiv", iv, c, x, x / c, udiv);
                    }
                }
            }
        }
    }
    assert!(
        fails.is_empty(),
        "scale-op soundness failures: {:#?}",
        &fails[..fails.len().min(20)]
    );
}

#[test]
fn meet_and_join_contain_every_concrete_result() {
    const MASK: u128 = 15;
    let mut ivs = Vec::new();
    for lo in 0..=MASK {
        for hi in 0..=MASK {
            for stride in 1u128..=6 {
                ivs.push(Interval { lo, hi, stride });
            }
        }
    }
    let mut fails: Vec<String> = Vec::new();
    for &a in &ivs {
        let sa = members(a, MASK);
        for &b in &ivs {
            let sb = members(b, MASK);
            let meet = a.intersect(b);
            for &x in &sa {
                if sb.contains(&x) && !contains(meet, x) {
                    fails.push(format!("meet a={a:?} b={b:?} x={x} meet={meet:?}"));
                }
            }
            let join = a.union(b);
            for &x in sa.iter().chain(sb.iter()) {
                if !contains(join, x) {
                    fails.push(format!("join a={a:?} b={b:?} x={x} join={join:?}"));
                }
            }
        }
    }
    assert!(
        fails.is_empty(),
        "meet/join soundness failures: {:#?}",
        &fails[..fails.len().min(20)]
    );
}

/// A non-integer value has no width mask, so its range must be top rather than
/// the singleton `{0}` a zero mask yields.
#[test]
fn range_of_non_integer_value_is_top() {
    let mut b = RegisterSet::new().build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let entry = b.create_region_all().unwrap();
    b.set_entry_region_all(entry).unwrap();

    b.set_region(entry);
    let addr = b.build_int_const(0xDEAD_u64, ValueType::I64).unwrap();
    let raw = b
        .build_load(addr, rsleigh::VnSpace::RAM, ValueType::I64)
        .unwrap();
    let fv = b.build_int_bits_to_float(raw, ValueType::F64).unwrap();
    let bits = b.build_float_bits_to_int(fv, ValueType::I64).unwrap();
    b.build_return(Some(bits), &[]).unwrap();

    b.set_lift_addr(None);
    let f = b.build().unwrap();

    let doms = control_dominators(&f);
    let known = analyze_known_bits(&f).unwrap();
    let mut ranges = compute_value_ranges(&f, &doms, &known);

    let iv = ranges.range_of(fv, f.entry());
    assert!(iv.is_top(u128::MAX), "F64 range must be top, got {iv:?}",);
    assert!(iv.count() > 1, "F64 range must not be a singleton");
}
