use rsleigh::VnSpace;
use strider_ir::Function;
use strider_ir::RegionId;
use strider_ir::node::{ExtendOp, NodeId, NodeKind, ValueId, ValueType};
use strider_ir::{
    FunctionBuilder, IRBuilderExt, IRViewer, IRWalker, IntBinaryOp, IntCmpOp,
    control_dominator_tree,
};
use strider_ir_test_utils::{IrWalkerEx, RegisterSet, reg_vn};

use crate::test_support::{run_passes, steps};
use crate::{PhiCollapse, RegionCollapse};

use super::super::tests::{ext_26_6, srl_sltiu};
use super::Interval;
use super::RangeMap;

/// The stride is at least 1 by construction, and emptiness has one name.
#[test]
fn new_clamps_the_stride_and_empty_counts_nothing() {
    for (lo, hi) in [(0, 0), (3, 9), (5, 2)] {
        assert_eq!(Interval::new(lo, hi, 0), Interval::new(lo, hi, 1));
    }
    assert!(super::interval::EMPTY.is_empty());
    assert_eq!(super::interval::EMPTY.count(), 0);
}

/// The AP-join keeps the coarsest spacing every element of both sides obeys: a
/// constant arm joined with a scaled one must not flatten it to dense, or a
/// 6-entry table reads as 21 slots.
#[test]
fn union_keeps_a_stride_the_join_still_obeys() {
    let scaled = Interval {
        lo: 0,
        hi: 20,
        stride: 4,
    };
    let joined = Interval::dense(0, 0).union(scaled);
    assert_eq!((joined.lo, joined.hi, joined.stride), (0, 20, 4));
    assert_eq!(joined.count(), 6);
    // An offset start is only congruent modulo the gap between the two.
    let offset = Interval::dense(2, 2).union(scaled);
    assert_eq!((offset.lo, offset.hi, offset.stride), (0, 20, 2));
    // Two genuinely different spacings meet at their gcd.
    let by_six = Interval {
        lo: 0,
        hi: 18,
        stride: 6,
    };
    assert_eq!(scaled.union(by_six).stride, 2);
}

/// Two single-element sides carry no spacing of their own, so the gap between
/// their starts is the whole answer: `phi(4, 8, 12, 16)` is four indices, not
/// the thirteen a flattened join enumerates.
#[test]
fn union_of_single_elements_keeps_the_gap_as_the_stride() {
    let joined = [4u128, 8, 12, 16]
        .into_iter()
        .map(|k| Interval::dense(k, k))
        .reduce(Interval::union)
        .expect("four arms");
    assert_eq!((joined.lo, joined.hi, joined.stride), (4, 16, 4));
    assert_eq!(joined.count(), 4);
    // One point joined with itself has no gap and no spacing; the stride still
    // has to be a usable divisor.
    let point = Interval::dense(7, 7).union(Interval::dense(7, 7));
    assert_eq!((point.lo, point.hi, point.stride), (7, 7, 1));
    assert_eq!(point.count(), 1);
}

/// An empty side adds nothing, so the join is the other side exactly: its
/// `lo` and stride kept. Every 8-bit interval, against each spelling of empty.
/// Mutation: drop the empty early-outs from `union`.
#[test]
fn union_with_an_empty_side_is_the_other_side() {
    for empty in [
        super::interval::EMPTY,
        Interval::new(9, 3, 1),
        Interval::new(255, 0, 7),
    ] {
        assert!(empty.is_empty());
        for lo in 0..=255u128 {
            for hi in 0..=255u128 {
                for stride in 1..=255u128 {
                    let x = Interval::new(lo, hi, stride);
                    if x.is_empty() {
                        assert!(empty.union(x).is_empty() && x.union(empty).is_empty());
                        continue;
                    }
                    assert_eq!(empty.union(x), x, "{empty:?} with {x:?}");
                    assert_eq!(x.union(empty), x, "{x:?} with {empty:?}");
                }
            }
        }
    }
}

/// Every element of either side is in the join: all pairs of intervals over
/// `0..8`, empties included.
#[test]
fn union_contains_both_sides() {
    let all: Vec<Interval> = (0..8u128)
        .flat_map(|lo| (0..8u128).flat_map(move |hi| (1..8u128).map(move |s| (lo, hi, s))))
        .map(|(lo, hi, s)| Interval::new(lo, hi, s))
        .collect();
    for &a in &all {
        for &b in &all {
            let u = a.union(b);
            for x in 0..8u128 {
                if a.contains(x) || b.contains(x) {
                    assert!(u.contains(x), "{a:?} | {b:?} = {u:?} misses {x}");
                }
            }
        }
    }
}

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

/// `iv << k` at `mask`, through the scaling the analysis applies.
fn shl(iv: Interval, k: u32, mask: u128) -> Interval {
    super::interval::ScaleOp::shl(k).unwrap().apply(iv, mask)
}

/// `iv >> k`, through the scaling the analysis applies.
fn shr(iv: Interval, k: u32) -> Interval {
    super::interval::ScaleOp::shr(k)
        .unwrap()
        .apply(iv, u128::MAX)
}

/// `value`'s interval at `region`, from a fresh analysis of `f`.
fn range_at(f: &Function, value: ValueId, region: NodeId) -> Interval {
    RangeMap::new(f).range_of(value, region)
}

/// Brings a hand-built fixture to the converged IR shape the range analysis
/// assumes: no single-input phis, no single-predecessor regions, no
/// `If(Xor(C,1))` conds (rewritten to `If(C)` with the branches swapped).
fn canonicalize(f: &mut strider_ir::Function) {
    run_passes(f, &[&crate::IfCondInversion, &PhiCollapse, &RegionCollapse]).unwrap();
}

/// An `I32` load from `addr`, carrying no KnownBits facts at all.
fn opaque(b: &mut FunctionBuilder, addr: u64) -> ValueId {
    let addr = b.build_int_const(addr, ValueType::int(64)).unwrap();
    b.build_load(addr, VnSpace::RAM, ValueType::int(32))
        .unwrap()
}

/// `if (x < k) goto t else e`, `x` an `I32`.
fn guard_if(b: &mut FunctionBuilder, x: ValueId, k: u64, t: RegionId, e: RegionId) {
    let cond = cmp_c(b, x, IntCmpOp::Less, k);
    b.build_if(cond, t, e).unwrap();
}

/// `iv` is the full `I32` range.
#[track_caller]
fn assert_top32(iv: Interval, what: &str) {
    assert!(iv.is_top(0xFFFF_FFFF), "{what}, got {iv:?}");
}

/// The `If` on an integer comparison, where a fixture also branches on a
/// constant flag.
fn compare_if(f: &Function) -> NodeId {
    f.walk()
        .find(|&n| {
            matches!(f.node_kind(n), NodeKind::If)
                && matches!(
                    f.kind_of_value(f.graph().nth_input(n, 1).unwrap()),
                    NodeKind::IntCmpOp(_)
                )
        })
        .expect("an If on a comparison")
}

/// The consumer of edge `slot` of `if_node`.
fn edge_consumer(f: &Function, if_node: NodeId, slot: usize) -> NodeId {
    let ctrl = f.node_outputs(if_node)[slot];
    f.graph().value_uses(ctrl).next().expect("edge consumer").0
}

/// The join `Phi` with at least `arms` data inputs, and its `Region`.
fn join_phi(f: &Function, arms: usize) -> (ValueId, NodeId) {
    let phi = f
        .walk()
        .find(|&n| matches!(f.node_kind(n), NodeKind::Phi) && f.phi_data_inputs(n).count() >= arms)
        .expect("the multi-input join phi");
    // The joining Region produces the phi's slot-0 PhiToken input.
    let token = f.graph().nth_input(phi, 0).unwrap();
    (f.node_outputs(phi)[0], f.graph().producer(token))
}

/// `entry -> If(true) -> a, b -> join`, arm `i` writing `arm(b, i)` to the
/// tracked `I32` register it is given; the builder is left in `join`.
fn two_arm_join(
    arm: impl Fn(&mut FunctionBuilder, usize) -> ValueId,
) -> (FunctionBuilder, rsleigh::Vn) {
    let idx_vn = reg_vn(0x10, 4);
    let mut b = RegisterSet::new().tracked(idx_vn).build_fn().unwrap();
    let [entry, path_a, path_b, join] = std::array::from_fn(|_| b.create_region_all().unwrap());
    b.set_entry_region_all(entry).unwrap();
    b.set_region(entry);
    let flag = b.build_boolean_const(true);
    b.build_if(flag, path_a, path_b).unwrap();
    for (i, region) in [path_a, path_b].into_iter().enumerate() {
        b.set_region(region);
        let v = arm(&mut b, i);
        b.write_variable(&idx_vn, v).unwrap();
        b.build_branch(join).unwrap();
    }
    b.set_region(join);
    (b, idx_vn)
}

/// `(true_edge_consumer, false_edge_consumer)` of the sole `If`: the
/// post-collapse query points where each edge's guard holds.  `IfCondInversion`
/// swaps the branches of an `Xor(C,1)` cond, so the original "true" dispatch
/// becomes the false edge in that case.
fn if_edge_consumers(f: &strider_ir::Function) -> (NodeId, NodeId) {
    let if_node = f.sole_kind(|k| matches!(k, NodeKind::If));
    (edge_consumer(f, if_node, 0), edge_consumer(f, if_node, 1))
}
/// [`guarded`]'s function, its guarded load and the two `If` edges.
struct Guarded {
    f: Function,
    /// The `ty` load the condition reads, with no KnownBits facts at all.
    idx: ValueId,
    /// What the `then` arm returns.
    ret: ValueId,
    /// The consumer of the `If`'s true edge after `canonicalize`.
    true_node: NodeId,
    /// The consumer of its false edge.
    false_node: NodeId,
}

/// ```text
///   entry: idx = Load(ty); If(cond(idx)) -> then, else
///   then:  Return(ret(idx))
///   else:  Return(idx)
/// ```
/// The function is canonicalised: `IfCondInversion` rewrites `If(Xor(C, 1))`
/// to `If(C)` with the arms swapped, so for such a cond `then` is the false
/// edge.
fn guarded(
    ty: ValueType,
    cond: impl FnOnce(&mut FunctionBuilder, ValueId) -> ValueId,
    ret: impl FnOnce(&mut FunctionBuilder, ValueId) -> ValueId,
) -> Guarded {
    let mut b = RegisterSet::new().build_fn().unwrap();
    let entry = b.create_region_all().unwrap();
    let then_arm = b.create_region_all().unwrap();
    let else_arm = b.create_region_all().unwrap();
    b.set_entry_region_all(entry).unwrap();

    b.set_region(entry);
    let addr = b.build_int_const(0xDEAD_u64, ValueType::int(64)).unwrap();
    let idx = b.build_load(addr, VnSpace::RAM, ty).unwrap();
    let c = cond(&mut b, idx);
    b.build_if(c, then_arm, else_arm).unwrap();

    b.set_region(then_arm);
    let value = ret(&mut b, idx);
    b.build_return(Some(value), &[]).unwrap();
    b.set_region(else_arm);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_lift_addr(None);
    let mut f = b.build().unwrap();
    canonicalize(&mut f);
    let (true_node, false_node) = if_edge_consumers(&f);
    Guarded {
        f,
        idx,
        ret: value,
        true_node,
        false_node,
    }
}

/// `x <op> c` at `I32`.
fn cmp_c(b: &mut FunctionBuilder, x: ValueId, op: IntCmpOp, c: u64) -> ValueId {
    let c = b.build_int_const(c, ValueType::int(32)).unwrap();
    b.build_int_cmp_operation(x, c, op, ValueType::int(32))
        .unwrap()
}

/// `c < x` at `I32`.
fn c_less(b: &mut FunctionBuilder, c: u64, x: ValueId) -> ValueId {
    let c = b.build_int_const(c, ValueType::int(32)).unwrap();
    b.build_int_cmp_operation(c, x, IntCmpOp::Less, ValueType::int(32))
        .unwrap()
}

/// `x + c` at `I32`.
fn add_c(b: &mut FunctionBuilder, x: ValueId, c: u128) -> ValueId {
    let c = b.build_int_const(c, ValueType::int(32)).unwrap();
    b.build_int_binary_operation(x, c, IntBinaryOp::Add, ValueType::int(32))
        .unwrap()
}

/// `Xor(cond, 1):I1`, the lowered logical NOT; `Xor(1, cond)` if `one_first`.
fn not_i1(b: &mut FunctionBuilder, cond: ValueId, one_first: bool) -> ValueId {
    let one = b.build_int_const(1u64, ValueType::I1).unwrap();
    let (l, r) = if one_first { (one, cond) } else { (cond, one) };
    b.build_int_binary_operation(l, r, IntBinaryOp::Xor, ValueType::I1)
        .unwrap()
}

/// A guard condition over `idx`.
type Cond = fn(&mut FunctionBuilder, ValueId) -> ValueId;

/// What a guard row expects of `idx` on one edge.
#[derive(Clone, Copy, Debug)]
enum Expect {
    /// Exactly `lo..=hi`.
    Bounds(u128, u128),
    /// The full `I32` range.
    Top,
    /// Not checked.
    Any,
}

/// Each guard shape's range for `idx` on its `If`'s true and false edges after
/// `canonicalize`, which rewrites an `Xor(C, 1)` cond to `If(C)`: the lowered
/// `idx <= 15` bounds the false edge of `15 < idx`.
#[test]
fn guard_extraction_bounds_each_edge() {
    use Expect::{Any, Bounds, Top};
    use IntCmpOp::{Less, Sless};
    let rows: &[(&str, Cond, Expect, Expect)] = &[
        (
            "idx < 8",
            |b, idx| cmp_c(b, idx, Less, 8),
            Bounds(0, 7),
            Top,
        ),
        // Impossible unsigned, so not the `[0, 0]` `saturating_sub(1)` gives.
        ("idx < 0", |b, idx| cmp_c(b, idx, Less, 0), Top, Any),
        // The largest bound: `build_int_const` wraps `type_mask + 1` to 0.
        (
            "idx < type_mask",
            |b, idx| cmp_c(b, idx, Less, 0xFFFF_FFFF),
            Bounds(0, 0xFFFF_FFFE),
            Any,
        ),
        // `idx <= 15` as lowered, in both Xor operand orders: after
        // `IfCondInversion` both are `15 < idx`, so these pin the inversion.
        (
            "Xor(15 < idx, 1)",
            |b, idx| {
                let gt = c_less(b, 15, idx);
                not_i1(b, gt, false)
            },
            Any,
            Bounds(0, 15),
        ),
        (
            "Xor(1, 15 < idx)",
            |b, idx| {
                let gt = c_less(b, 15, idx);
                not_i1(b, gt, true)
            },
            Any,
            Bounds(0, 15),
        ),
        // `idx >= 8`, which `IfCondInversion` rewrites to `idx < 8` with the
        // arms swapped.
        (
            "Xor(idx < 8, 1)",
            |b, idx| {
                let lt = cmp_c(b, idx, Less, 8);
                not_i1(b, lt, false)
            },
            Bounds(0, 7),
            Top,
        ),
        // The `ja` shape: the true edge knows only `idx >= 9`.
        ("8 < idx", |b, idx| c_less(b, 8, idx), Top, Bounds(0, 8)),
        // No known-zero sign bit, so no unsigned bound.
        ("idx s< 8", |b, idx| cmp_c(b, idx, Sless, 8), Top, Any),
        // The masked-Thumb shape: the bound carries back through the add.
        (
            "idx + -1 < 7",
            |b, idx| {
                let diff = add_c(b, idx, 0u128.wrapping_sub(1));
                cmp_c(b, diff, Less, 7)
            },
            Bounds(1, 7),
            Any,
        ),
        // Shifting `[0, 7]` back by 4 wraps, so it bounds nothing.
        (
            "idx + 4 < 8",
            |b, idx| {
                let diff = add_c(b, idx, 4);
                cmp_c(b, diff, Less, 8)
            },
            Top,
            Any,
        ),
    ];
    let mask = ValueType::int(32).int_mask_u128().unwrap();
    for &(name, cond, on_true, on_false) in rows {
        let g = guarded(ValueType::int(32), cond, |_, idx| idx);
        for (edge, node, want) in [
            ("true", g.true_node, on_true),
            ("false", g.false_node, on_false),
        ] {
            let iv = range_at(&g.f, g.idx, node);
            match want {
                Bounds(lo, hi) => assert_eq!((iv.lo, iv.hi), (lo, hi), "{name}: {edge} edge"),
                Top => assert!(
                    iv.is_top(mask),
                    "{name}: {edge} edge must be top, got {iv:?}"
                ),
                Any => {}
            }
        }
    }
}

/// `rotl(x, r)` at `I32`, `(x << r) | (x >> (32 - r))`, as PowerPC `rotlwi`
/// lifts it.
fn rotl(b: &mut FunctionBuilder, x: ValueId, r: u64) -> ValueId {
    let ty = ValueType::int(32);
    let up = b.build_int_const(r, ty).unwrap();
    let down = b.build_int_const(32 - r, ty).unwrap();
    let hi = b
        .build_int_binary_operation(x, up, IntBinaryOp::ShiftLeft, ty)
        .unwrap();
    let lo = b
        .build_int_binary_operation(x, down, IntBinaryOp::ShiftRight, ty)
        .unwrap();
    b.build_int_binary_operation(hi, lo, IntBinaryOp::Or, ty)
        .unwrap()
}

/// A guard holding `rotl(idx, r)` below `2^r` bounds `idx` to that value
/// shifted up by `32 - r`.  At `2^r` the rotated value's bit `r` is `idx`'s
/// bit 0, so `idx` is no interval and stays unbounded.
/// Mutation: drop the `hi < 2^r` test in `rotate_operand_interval`.
#[test]
fn a_rotate_guard_bounds_its_operand_only_below_the_rotation() {
    let at = |c: u64| {
        let g = guarded(
            ValueType::int(32),
            |b, idx| {
                let y = rotl(b, idx, 2);
                c_less(b, c, y)
            },
            |_, idx| idx,
        );
        range_at(&g.f, g.idx, g.false_node)
    };
    let iv = at(3);
    assert_eq!((iv.lo, iv.hi, iv.stride), (0, 3 << 30, 1 << 30));
    assert!(at(4).is_top(0xffff_ffff), "{:?}", at(4));
}

/// [`guarded`] on `idx < bound` at `I32` whose `then` arm returns `idx <op> c`,
/// the scaled value a table index feeds into its address arithmetic.
fn guarded_scaled(bound: u64, op: IntBinaryOp, c: u64) -> Guarded {
    let ty = ValueType::int(32);
    guarded(
        ty,
        |b, idx| cmp_c(b, idx, IntCmpOp::Less, bound),
        |b, idx| {
            let c = b.build_int_const(c, ty).unwrap();
            b.build_int_binary_operation(idx, c, op, ty).unwrap()
        },
    )
}

fn scaled_range(bound: u64, op: IntBinaryOp, c: u64) -> Interval {
    let g = guarded_scaled(bound, op, c);
    range_at(&g.f, g.ret, g.true_node)
}

/// The guard must forward-propagate through `idx << k`, the value a table index
/// feeds into `table + idx*stride`: `idx < 4` gives `idx << 2  in  {0,4,8,12}`.
#[test]
fn guard_propagates_through_shift_left() {
    let iv = scaled_range(4, IntBinaryOp::ShiftLeft, 2);
    assert_eq!((iv.lo, iv.hi, iv.stride), (0, 12, 4));
    assert_eq!(iv.count(), 4);
}

/// PowerPC `slwi` lifts to `And(idx << 2, ~3)`. The mask clears only bits the
/// shift already zeroed, so the guard on `idx` bounds it exactly as it bounds
/// the bare shift; a mask that clears a bit the value can hold is not the same
/// value.  `idx` is in `[1, 3]`, a low end only the shift's own range keeps:
/// `{4, 8, 12}` through the identity, `{0, 8}` without it.
#[test]
fn guard_propagates_through_a_mask_of_known_zero_bits() {
    let range = |mask: u64| {
        let ty = ValueType::int(32);
        let g = guarded(
            ty,
            |b, idx| {
                let less_one = add_c(b, idx, 0xffff_ffff);
                cmp_c(b, less_one, IntCmpOp::Less, 3)
            },
            |b, idx| {
                let two = b.build_int_const(2u64, ty).unwrap();
                let shifted = b
                    .build_int_binary_operation(idx, two, IntBinaryOp::ShiftLeft, ty)
                    .unwrap();
                let mask_c = b.build_int_const(mask, ty).unwrap();
                b.build_int_binary_operation(shifted, mask_c, IntBinaryOp::And, ty)
                    .unwrap()
            },
        );
        range_at(&g.f, g.ret, g.true_node)
    };
    let iv = range(0xffff_fffc);
    assert_eq!((iv.lo, iv.hi, iv.stride), (4, 12, 4));
    let iv = range(0xffff_fff8);
    assert!(
        iv.lo == 0 && iv.hi >= 8 && iv.stride == 8,
        "a mask clearing bit 2 is not the guarded value, got {iv:?}"
    );
}

/// Same through a non-power-of-two `idx * c`: `idx < 4` gives `idx*3  in
/// {0,3,6,9}`.
#[test]
fn guard_propagates_through_mul_const() {
    let iv = scaled_range(4, IntBinaryOp::Mul, 3);
    assert_eq!((iv.lo, iv.hi, iv.stride), (0, 9, 3));
    assert_eq!(iv.count(), 4);
}

/// The floor shapes reduce the range: `idx < 8` gives `idx >> 1  in  [0,3]`.
#[test]
fn guard_propagates_through_shift_right() {
    let iv = scaled_range(8, IntBinaryOp::ShiftRight, 1);
    assert_eq!((iv.lo, iv.hi), (0, 3));
}

/// A divide by zero traps rather than dividing by one, and a shift is a
/// scaling of its left operand only: neither carries `idx < 8` forward.
#[test]
fn a_guard_skips_a_divide_by_zero_and_a_shift_of_a_constant() {
    assert!(scaled_range(8, IntBinaryOp::Div, 0).is_top(0xffff_ffff));
    let ty = ValueType::int(32);
    let g = guarded(
        ty,
        |b, idx| cmp_c(b, idx, IntCmpOp::Less, 8),
        |b, idx| {
            let four = b.build_int_const(4u64, ty).unwrap();
            b.build_int_binary_operation(four, idx, IntBinaryOp::ShiftLeft, ty)
                .unwrap()
        },
    );
    let iv = range_at(&g.f, g.ret, g.true_node);
    assert!(iv.is_top(0xffff_ffff), "{iv:?}");
}

/// `idx < 8` gives `idx / 2  in  [0,3]`. A scale's operand guard gates the
/// propagation and then bounds the operand, which is one scan, not two: each is
/// a linear walk of the operand's guard list with a dominance test per entry.
#[test]
fn scaled_range_scans_the_operand_guards_once() {
    let g = guarded_scaled(8, IntBinaryOp::Div, 2);
    let mut ranges = RangeMap::new(&g.f);
    let scans = steps(&[&super::GUARD_SCANS], || {
        let iv = ranges.range_of(g.ret, g.true_node);
        assert_eq!((iv.lo, iv.hi), (0, 3));
        Ok(())
    })
    .unwrap();
    assert_eq!(
        scans, 2,
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
    let entry = b.create_region_all().unwrap();
    let mid = b.create_region_all().unwrap();
    let merge = b.create_region_all().unwrap();
    let exit = b.create_region_all().unwrap();
    b.set_entry_region_all(entry).unwrap();

    let ty = ValueType::int(32);
    b.set_region(entry);
    let va = b.build_int_const(0xDEAD_u64, ValueType::int(64)).unwrap();
    let v = b.build_load(va, VnSpace::RAM, ty).unwrap();
    let other = if second_arm_bounds_v {
        v
    } else {
        let wa = b.build_int_const(0xBEEF_u64, ValueType::int(64)).unwrap();
        b.build_load(wa, VnSpace::RAM, ty).unwrap()
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
    let iv = range_at(&f, v, merge);
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
    let iv = range_at(&f, v, merge);
    assert_top32(iv, "v must stay unbounded: one merge arm does not bound it");
}

/// `shl`/`mul` must WIDEN to top when the scaled top overflows the carrier:
/// `checked_shl` alone only rejects `k >= 128` and otherwise wraps, which would
/// under-approximate (unsound) for a `u128::MAX` width mask.
#[test]
fn shl_and_mul_widen_to_top_on_carrier_overflow() {
    let hi = 1u128 << 127;
    assert!(
        shl(Interval::dense(0, hi), 1, u128::MAX).is_top(u128::MAX),
        "2^127 << 1 overflows u128 -> must be top, not a wrapped [0,0]",
    );
    assert!(
        Interval::dense(0, hi).mul(4, u128::MAX).is_top(u128::MAX),
        "2^127 * 4 overflows u128 -> must be top",
    );
    // The common in-type case is unaffected by routing shl through mul.
    let iv = shl(Interval::dense(0, 3), 2, 0xFFFF_FFFF);
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
    let s = shr(clean, 1);
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
            r.contains(real),
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
                        r.contains(v),
                        "udiv lied: lo={lo} stride={stride} c={c} x={x} -> {v} outside {r:?}",
                    );
                }
                if c.is_power_of_two() {
                    let k = c.trailing_zeros();
                    let r = shr(src, k);
                    for &x in &elems {
                        let v = x >> k;
                        assert!(
                            r.contains(v),
                            "shr lied: lo={lo} stride={stride} k={k} x={x} -> {v} outside {r:?}",
                        );
                    }
                }
            }
        }
    }
}

// Writing idx into a tracked variable in the entry and reading it back in
// dispatch makes the builder insert a single-input Phi.
#[test]
fn trivial_phi_of_guarded_index_is_bounded() {
    let idx_vn = reg_vn(0x10, 4); // 4-byte register -> I32
    let mut b = RegisterSet::new().tracked(idx_vn).build_fn().unwrap();
    let entry = b.create_region_all().unwrap();
    let dispatch = b.create_region_all().unwrap();
    let exit = b.create_region_all().unwrap();

    b.set_entry_region_all(entry).unwrap();

    b.set_region(entry);
    // A load, so idx carries no KnownBits facts.
    let raw_idx = opaque(&mut b, 0xCAFE);
    b.write_variable(&idx_vn, raw_idx).unwrap();
    guard_if(&mut b, raw_idx, 8u64, dispatch, exit);

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

    let iv = range_at(&f, raw_idx, dispatch_node);
    assert_eq!(iv.lo, 0, "trivial phi: lower bound must be 0");
    assert_eq!(iv.hi, 7, "trivial phi: upper bound must be 7");
}

#[test]
fn known_bits_mask_bounds_index_everywhere() {
    let mut b = RegisterSet::new().build_fn().unwrap();
    let entry = b.create_region_all().unwrap();
    let other = b.create_region_all().unwrap();

    b.set_entry_region_all(entry).unwrap();
    b.set_region(entry);

    // arg = unconstrained value (load from arbitrary address).
    let arg = opaque(&mut b, 0x1000);
    let mask = b.build_int_const(7u64, ValueType::int(32)).unwrap();
    let idx = b
        .build_int_binary_operation(arg, mask, IntBinaryOp::And, ValueType::int(32))
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

    let mut ranges = RangeMap::new(&f);

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
    let entry = b.create_region_all().unwrap();
    b.set_entry_region_all(entry).unwrap();
    b.set_region(entry);

    let arg = opaque(&mut b, 0x1000);
    let mask = b.build_int_const(7u64, ValueType::int(32)).unwrap();
    let idx = b
        .build_int_binary_operation(arg, mask, IntBinaryOp::And, ValueType::int(32))
        .unwrap();
    let three = b.build_int_const(3u64, ValueType::int(32)).unwrap();
    // scaled = idx * 8  =>  values {0, 8, 16, ... 56}, low 3 bits known-zero.
    let scaled = b
        .build_int_binary_operation(idx, three, IntBinaryOp::ShiftLeft, ValueType::int(32))
        .unwrap();

    b.build_return(Some(scaled), &[]).unwrap();
    b.set_lift_addr(None);
    let f = b.build().unwrap();

    let entry_node = f.entry();

    let iv = range_at(&f, scaled, entry_node);
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
    let idx_vn = reg_vn(0x10, 4);
    let mut b = RegisterSet::new().tracked(idx_vn).build_fn().unwrap();
    // Regions: entry -> (guarded_region, unguarded_region) -> dispatch
    let entry = b.create_region_all().unwrap();
    let guarded = b.create_region_all().unwrap();
    let unguarded = b.create_region_all().unwrap();
    let dispatch = b.create_region_all().unwrap();

    b.set_entry_region_all(entry).unwrap();

    b.set_region(entry);
    let raw_idx = opaque(&mut b, 0xBEEF);
    b.write_variable(&idx_vn, raw_idx).unwrap();
    guard_if(&mut b, raw_idx, 8u64, guarded, unguarded);

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

    let iv = range_at(&f, phi_idx, dispatch_node);
    assert_top32(iv, "phi with one unguarded predecessor must produce top");
}

// With `v = idx & 0xFF` the I32 sign bit is known zero, so `Sless(v, 8)` is
// trustworthy as an unsigned bound: the guard interval intersected with the
// KnownBits base [0, 255] gives [0, 7].
#[test]
fn sless_guard_with_known_zero_sign_bit_bounds_index() {
    fn low_byte(b: &mut FunctionBuilder, idx: ValueId) -> ValueId {
        let mask = b.build_int_const(0xFFu64, ValueType::int(32)).unwrap();
        b.build_int_binary_operation(idx, mask, IntBinaryOp::And, ValueType::int(32))
            .unwrap()
    }
    let g = guarded(
        ValueType::int(32),
        |b, idx| {
            let v = low_byte(b, idx);
            cmp_c(b, v, IntCmpOp::Sless, 8)
        },
        low_byte,
    );
    // `ret` is the `And` the cond compares: pure nodes hash-cons.
    let iv = range_at(&g.f, g.ret, g.true_node);
    assert_eq!(iv.lo, 0, "Sless with known-zero sign bit: lower bound 0");
    assert_eq!(
        iv.hi, 7,
        "Sless with known-zero sign bit: idx s< 8 -> [0, 7]"
    );
}

#[test]
fn no_constraint_is_top() {
    let mut b = RegisterSet::new().build_fn().unwrap();
    let entry = b.create_region_all().unwrap();
    b.set_entry_region_all(entry).unwrap();
    b.set_region(entry);

    let idx = opaque(&mut b, 0x4242);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_lift_addr(None);
    let f = b.build().unwrap();
    let entry_node = f.entry();

    let iv = range_at(&f, idx, entry_node);
    assert_top32(iv, "unconstrained load must produce top");
}

// A region not dominated by the guard's true successor stays top.  Entry can
// carry only one terminator, so the guard lives in the left arm of a diamond:
//
//   entry       -> If(flag) -> left, right
//   left        -> If(idx < 8) -> dispatch, guarded_exit
//   right       -> returns directly, never dominated by the guard
#[test]
fn sibling_region_not_dominated_is_top() {
    let idx_vn = reg_vn(0x10, 4);
    let mut b = RegisterSet::new().tracked(idx_vn).build_fn().unwrap();
    let entry = b.create_region_all().unwrap();
    let left = b.create_region_all().unwrap();
    let right = b.create_region_all().unwrap();
    let dispatch = b.create_region_all().unwrap();
    let guarded_exit = b.create_region_all().unwrap();

    b.set_entry_region_all(entry).unwrap();

    b.set_region(entry);
    let raw_idx = opaque(&mut b, 0x1000);
    b.write_variable(&idx_vn, raw_idx).unwrap();
    let flag = b.build_boolean_const(true);
    b.build_if(flag, left, right).unwrap();

    b.set_region(left);
    guard_if(&mut b, raw_idx, 8u64, dispatch, guarded_exit);

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
    let guard_if = compare_if(&f);
    let flag_if = f
        .walk()
        .find(|&n| matches!(f.node_kind(n), NodeKind::If) && n != guard_if)
        .expect("flag If");
    let dispatch_node = edge_consumer(&f, guard_if, 0); // true edge of the guard
    let right_node = edge_consumer(&f, flag_if, 1); // false edge -> sibling right

    let mut ranges = RangeMap::new(&f);

    let iv_dispatch = ranges.range_of(raw_idx, dispatch_node);
    assert_eq!(
        iv_dispatch.hi, 7,
        "dispatch must have bound 7, got {}",
        iv_dispatch.hi
    );

    let iv_right = ranges.range_of(raw_idx, right_node);
    assert_top32(iv_right, "sibling region must be top");
}

// A loop-carried `idx_phi = Phi(0, idx_phi + 1)` is top: the back-edge arm is
// an `Add`, an opaque leaf here, with no guard or KnownBits bound, and no
// guard on idx_phi dominates the header.
#[test]
fn cyclic_phi_is_top() {
    let idx_vn = reg_vn(0x10, 4); // 4-byte register -> I32
    let mut b = RegisterSet::new().tracked(idx_vn).build_fn().unwrap();
    let entry = b.create_region_all().unwrap();
    let header = b.create_region_all().unwrap();
    let body = b.create_region_all().unwrap();
    let loop_exit = b.create_region_all().unwrap();

    b.set_entry_region_all(entry).unwrap();

    b.set_region(entry);
    let zero = b.build_int_const(0u64, ValueType::int(32)).unwrap();
    b.write_variable(&idx_vn, zero).unwrap();
    b.build_branch(header).unwrap();

    // Reading the variable here becomes a Phi once the regions are linked.
    b.set_region(header);
    let header_ctrl = b.region_cur_ctrl(header);
    let idx_phi = b.read_variable(&idx_vn).unwrap();
    guard_if(&mut b, idx_phi, 16u64, body, loop_exit);

    b.set_region(body);
    let one = b.build_int_const(1u64, ValueType::int(32)).unwrap();
    let idx_next = b
        .build_int_binary_operation(idx_phi, one, IntBinaryOp::Add, ValueType::int(32))
        .unwrap();
    b.write_variable(&idx_vn, idx_next).unwrap();
    b.build_branch(header).unwrap();

    b.set_region(loop_exit);
    b.build_return(Some(idx_phi), &[]).unwrap();

    b.set_lift_addr(None);
    let f = b.build().unwrap();
    let header_node = f.graph().producer(header_ctrl);

    let iv = range_at(&f, idx_phi, header_node);
    assert_top32(iv, "cyclic phi must yield top");
}

// A loop-carried masked index: `idx_phi = Phi(0, raw & 7)` where the back edge
// is the taken side of `if (raw & 7 < 6)`.  The mask alone admits 0..7 and no
// guard node dominates the header, so the bound lives on the back edge, where
// it holds for the arm arriving on it.
#[test]
fn back_edge_guard_bounds_a_masked_loop_index() {
    let idx_vn = reg_vn(0x10, 4);
    let mut b = RegisterSet::new().tracked(idx_vn).build_fn().unwrap();
    let entry = b.create_region_all().unwrap();
    let header = b.create_region_all().unwrap();
    let body = b.create_region_all().unwrap();
    let loop_exit = b.create_region_all().unwrap();

    b.set_entry_region_all(entry).unwrap();

    b.set_region(entry);
    let zero = b.build_int_const(0u64, ValueType::int(32)).unwrap();
    b.write_variable(&idx_vn, zero).unwrap();
    b.build_branch(header).unwrap();

    b.set_region(header);
    let header_ctrl = b.region_cur_ctrl(header);
    let idx_phi = b.read_variable(&idx_vn).unwrap();
    b.build_branch(body).unwrap();

    b.set_region(body);
    let raw = opaque(&mut b, 0xDEAD);
    let seven = b.build_int_const(7u64, ValueType::int(32)).unwrap();
    let masked = b
        .build_int_binary_operation(raw, seven, IntBinaryOp::And, ValueType::int(32))
        .unwrap();
    b.write_variable(&idx_vn, masked).unwrap();
    guard_if(&mut b, masked, 6u64, header, loop_exit);

    b.set_region(loop_exit);
    b.build_return(Some(idx_phi), &[]).unwrap();

    b.set_lift_addr(None);
    let f = b.build().unwrap();
    let header_node = f.graph().producer(header_ctrl);

    let iv = range_at(&f, idx_phi, header_node);
    assert_eq!(
        (iv.lo, iv.hi),
        (0, 5),
        "0 on entry, at most 5 on the back edge"
    );
}

/// `a, b = 0, 1; while (a < 16) a, b = b, a`: each header phi's back-edge
/// arm is the OTHER header phi.  The `a` phi and its header.
fn swap_loop() -> (Function, ValueId, NodeId) {
    let a_vn = reg_vn(0x10, 4); // 4-byte register -> I32
    let b_vn = reg_vn(0x20, 4); // 4-byte register -> I32
    let mut b = RegisterSet::new()
        .tracked(a_vn)
        .tracked(b_vn)
        .build_fn()
        .unwrap();
    let entry = b.create_region_all().unwrap();
    let header = b.create_region_all().unwrap();
    let body = b.create_region_all().unwrap();
    let loop_exit = b.create_region_all().unwrap();

    b.set_entry_region_all(entry).unwrap();

    b.set_region(entry);
    let zero = b.build_int_const(0u64, ValueType::int(32)).unwrap();
    let one_seed = b.build_int_const(1u64, ValueType::int(32)).unwrap();
    b.write_variable(&a_vn, zero).unwrap();
    b.write_variable(&b_vn, one_seed).unwrap();
    b.build_branch(header).unwrap();

    // Reading `a` here becomes phi_a once the regions are linked.
    b.set_region(header);
    let header_ctrl = b.region_cur_ctrl(header);
    let phi_a = b.read_variable(&a_vn).unwrap();
    guard_if(&mut b, phi_a, 16u64, body, loop_exit);

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

    (f, phi_a, header_node)
}

// The resolver only recurses through `Phi`, so a phi-of-phi is the one shape
// that drives it back into itself: two loop variables swapping every
// iteration, `phi_a = Phi(0, phi_b)` and `phi_b = Phi(1, phi_a)`, referencing
// each other with no intervening node.  `range_of` must RETURN: the cycle is
// cut rather than chased forever.
#[test]
fn phi_of_phi_cycle_terminates_top() {
    let (f, phi_a, header_node) = swap_loop();
    let iv = range_at(&f, phi_a, header_node);
    assert_top32(iv, "phi-of-phi cycle must yield top");
}

// Nested `idx < 16` then `idx < 8` guards must intersect to [0, 7] at the
// inner dispatch.
#[test]
fn nested_guards_intersect_at_inner_region() {
    let mut b = RegisterSet::new().build_fn().unwrap();
    let entry = b.create_region_all().unwrap();
    let outer_guard = b.create_region_all().unwrap();
    let dispatch = b.create_region_all().unwrap();
    let mid_exit = b.create_region_all().unwrap();
    let exit = b.create_region_all().unwrap();

    b.set_entry_region_all(entry).unwrap();

    b.set_region(entry);
    let idx = opaque(&mut b, 0xF00D);
    guard_if(&mut b, idx, 16u64, outer_guard, exit);

    b.set_region(outer_guard);
    guard_if(&mut b, idx, 8u64, dispatch, mid_exit);

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
    let dispatch_node = edge_consumer(&f, inner_if, 0);

    let iv = range_at(&f, idx, dispatch_node);
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
}

/// A guard dominating both arms of a phi bounds each arm alike, whichever
/// edge of an unrelated inner `If` the arm arrives on.
#[test]
fn a_dominating_guard_bounds_a_phi_arm_on_either_inner_edge() {
    for x_first in [true, false] {
        let idx_vn = reg_vn(0x10, 4);
        let mut b = RegisterSet::new().tracked(idx_vn).build_fn().unwrap();
        let [entry, inner, path_a, path_b, join, exit] =
            std::array::from_fn(|_| b.create_region_all().unwrap());
        b.set_entry_region_all(entry).unwrap();

        b.set_region(entry);
        let x = opaque(&mut b, 0xAAAA);
        guard_if(&mut b, x, 5, inner, exit);

        b.set_region(inner);
        let y = opaque(&mut b, 0xBBBB);
        let cond = cmp_c(&mut b, y, IntCmpOp::Equal, 9);
        b.build_if(cond, path_a, path_b).unwrap();

        for (region, is_x) in [(path_a, x_first), (path_b, !x_first)] {
            b.set_region(region);
            let v = if is_x {
                x
            } else {
                b.build_int_const(3u64, ValueType::int(32)).unwrap()
            };
            b.write_variable(&idx_vn, v).unwrap();
            b.build_branch(join).unwrap();
        }

        b.set_region(join);
        let phi = b.read_variable(&idx_vn).unwrap();
        b.build_return(Some(phi), &[]).unwrap();
        b.set_region(exit);
        b.build_return(Some(x), &[]).unwrap();

        b.set_lift_addr(None);
        let mut f = b.build().unwrap();
        canonicalize(&mut f);
        let (phi, join_node) = join_phi(&f, 2);
        let iv = range_at(&f, phi, join_node);
        assert_eq!((iv.lo, iv.hi), (0, 4), "x first: {x_first}, got {iv:?}");
    }
}

// The same value guarded `< 8` in one sibling branch and `< 16` in the other
// must get each bound independently at its own dispatch.
#[test]
fn two_sibling_guard_regions_give_independent_bounds() {
    let mut b = RegisterSet::new().build_fn().unwrap();
    let entry = b.create_region_all().unwrap();
    let branch_a = b.create_region_all().unwrap();
    let branch_b = b.create_region_all().unwrap();
    let dispatch_a = b.create_region_all().unwrap();
    let sink_a = b.create_region_all().unwrap();
    let dispatch_b = b.create_region_all().unwrap();
    let sink_b = b.create_region_all().unwrap();

    b.set_entry_region_all(entry).unwrap();

    b.set_region(entry);
    let idx = opaque(&mut b, 0xCAFE);
    let flag = b.build_boolean_const(true);
    b.build_if(flag, branch_a, branch_b).unwrap();

    b.set_region(branch_a);
    guard_if(&mut b, idx, 8u64, dispatch_a, sink_a);

    b.set_region(dispatch_a);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_region(sink_a);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_region(branch_b);
    guard_if(&mut b, idx, 16u64, dispatch_b, sink_b);

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
        edge_consumer(&f, if_node, 0)
    };
    let da_node = true_edge_consumer_of_guard(8);
    let db_node = true_edge_consumer_of_guard(16);

    let mut ranges = RangeMap::new(&f);

    let iv_a = ranges.range_of(idx, da_node);
    assert_eq!(iv_a.lo, 0, "dispatch_a lo must be 0");
    assert_eq!(iv_a.hi, 7, "dispatch_a hi must be 7 (idx < 8)");

    let iv_b = ranges.range_of(idx, db_node);
    assert_eq!(iv_b.lo, 0, "dispatch_b lo must be 0");
    assert_eq!(iv_b.hi, 15, "dispatch_b hi must be 15 (idx < 16)");
}

/// The join of four constant arms is an arithmetic progression, not the dense
/// span: `table.rs` enumerates `(lo..=hi).step_by(stride)`, so a flattened join
/// offers sixteen indices where the phi holds four, and one surplus index that
/// fails to fold defers the whole site.  The arms share no low zero bits, so
/// the spacing can only come from the join.
#[test]
fn multi_input_phi_of_constants_keeps_the_arm_spacing() {
    let idx_vn = reg_vn(0x10, 4);
    let mut b = RegisterSet::new().tracked(idx_vn).build_fn().unwrap();
    let entry = b.create_region_all().unwrap();
    let outer_a = b.create_region_all().unwrap();
    let outer_b = b.create_region_all().unwrap();
    let join = b.create_region_all().unwrap();
    let arms: Vec<_> = (0..4).map(|_| b.create_region_all().unwrap()).collect();
    b.set_entry_region_all(entry).unwrap();

    b.set_region(entry);
    let flag = b.build_boolean_const(true);
    b.build_if(flag, outer_a, outer_b).unwrap();
    for (region, [t, f]) in [(outer_a, [arms[0], arms[1]]), (outer_b, [arms[2], arms[3]])] {
        b.set_region(region);
        let flag = b.build_boolean_const(true);
        b.build_if(flag, t, f).unwrap();
    }
    for (i, &arm) in arms.iter().enumerate() {
        b.set_region(arm);
        let k = b
            .build_int_const(3 + 5 * i as u64, ValueType::int(32))
            .unwrap();
        b.write_variable(&idx_vn, k).unwrap();
        b.build_branch(join).unwrap();
    }

    b.set_region(join);
    let phi_idx = b.read_variable(&idx_vn).unwrap();
    b.build_return(Some(phi_idx), &[]).unwrap();
    b.set_lift_addr(None);
    let mut f = b.build().unwrap();
    canonicalize(&mut f);

    let (phi_idx, join_region) = join_phi(&f, 4);

    let iv = range_at(&f, phi_idx, join_region);
    assert_eq!(
        (iv.lo, iv.hi, iv.stride),
        (3, 18, 5),
        "phi(3, 8, 13, 18) is a stride-5 progression"
    );
    assert_eq!(iv.count(), 4, "four arms, four candidate indices");
}

// Two arms with distinct finite KnownBits bounds, `& 7` and `& 15`, must
// UNION to the wider [0,15], not intersect to [0,7] and not fall back to top.
// The arms read DISTINCT loads, so the join phi is genuinely multi-input and
// survives PhiCollapse.
#[test]
fn multi_input_phi_unions_two_distinct_finite_arms() {
    let (mut b, idx_vn) = two_arm_join(|b, i| {
        let load = opaque(b, [0xAAAA, 0xBBBB][i]);
        b.build_const_binop([7u64, 15][i], load, IntBinaryOp::And, ValueType::int(32))
            .unwrap()
    });
    let phi_idx = b.read_variable(&idx_vn).unwrap();
    b.build_return(Some(phi_idx), &[]).unwrap();
    b.set_lift_addr(None);
    let mut f = b.build().unwrap();
    canonicalize(&mut f);

    let (phi_idx, join_region) = join_phi(&f, 2);
    let iv = range_at(&f, phi_idx, join_region);
    assert_eq!(iv.lo, 0, "union lower bound is 0");
    assert_eq!(
        iv.hi, 15,
        "phi must UNION the two distinct finite arms ([0,7] ∪ [0,15] = [0,15]); \
         it must NOT tighten to [0,7] nor widen to top"
    );
}
#[test]
fn multi_input_phi_output_guard_bounds_index() {
    // The two arms must carry different SSA values, or the join phi is trivial
    // and `PhiCollapse` removes it, defeating the test.
    let (mut b, idx_vn) = two_arm_join(|b, i| opaque(b, [0xAAAA, 0xBBBB][i]));
    let [dispatch, exit] = std::array::from_fn(|_| b.create_region_all().unwrap());
    let phi_idx = b.read_variable(&idx_vn).unwrap();
    guard_if(&mut b, phi_idx, 8, dispatch, exit);
    for region in [dispatch, exit] {
        b.set_region(region);
        b.build_return(Some(phi_idx), &[]).unwrap();
    }
    b.set_lift_addr(None);
    let mut f = b.build().unwrap();
    canonicalize(&mut f);

    // Two Ifs survive, so find the guard by its IntCmpOp cond and read the
    // guarded value straight off it, i.e. exactly the value the guard was
    // recorded against.
    let guard_if = compare_if(&f);
    let cmp = f
        .graph()
        .producer(f.graph().nth_input(guard_if, 1).unwrap());
    let phi_idx = f.graph().nth_input(cmp, 0).unwrap();
    let phi_producer = f.graph().producer(phi_idx);
    let dispatch_node = edge_consumer(&f, guard_if, 0);

    assert!(
        matches!(f.node_kind(phi_producer), NodeKind::Phi),
        "guarded value must be a Phi"
    );
    assert!(
        f.phi_data_inputs(phi_producer).count() >= 2,
        "the join phi must have multiple data inputs for this test to exercise the bug"
    );

    let iv = range_at(&f, phi_idx, dispatch_node);
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
    let idx_vn = reg_vn(0x10, 4); // 4-byte register -> I32
    let mut b = RegisterSet::new().tracked(idx_vn).build_fn().unwrap();
    let entry = b.create_region_all().unwrap();
    let path_a = b.create_region_all().unwrap();
    let path_b = b.create_region_all().unwrap();
    let dispatch = b.create_region_all().unwrap();
    let exit_a = b.create_region_all().unwrap();

    b.set_entry_region_all(entry).unwrap();

    b.set_region(entry);
    let raw_idx = opaque(&mut b, 0xDEAD);
    b.write_variable(&idx_vn, raw_idx).unwrap();
    // A throwaway comparison, only here to split control.
    let zero = b.build_int_const(0u64, ValueType::int(32)).unwrap();
    let flag = b
        .build_int_cmp_operation(raw_idx, zero, IntCmpOp::Equal, ValueType::int(32))
        .unwrap();
    b.build_if(flag, path_a, path_b).unwrap();

    b.set_region(path_a);
    let idx_a = b.read_variable(&idx_vn).unwrap();
    guard_if(&mut b, idx_a, 4u64, dispatch, exit_a);

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

    let iv = range_at(&f, phi_idx, dispatch_node);
    assert_top32(
        iv,
        "join with one unguarded predecessor must be top (fail-closed); a guard \
         on one path must not bound the phi",
    );
}

// The guard's true edge feeds a Region that ALSO has a sibling predecessor.
// The guard does not hold for paths arriving that way, so the soundness gate
// must skip recording it and the merge (plus all it dominates) stays top.
#[test]
fn guard_into_control_merge_is_not_applied() {
    let mut b = RegisterSet::new().build_fn().unwrap();
    let entry = b.create_region_all().unwrap();
    let guarded = b.create_region_all().unwrap(); // entry's true edge
    let other = b.create_region_all().unwrap(); // entry's false edge
    let merge = b.create_region_all().unwrap(); // 2 predecessors

    b.set_entry_region_all(entry).unwrap();
    b.set_region(entry);
    let idx = opaque(&mut b, 0xF00D);
    // True edge: guarded (idx < 8 holds); false edge: other (unconstrained).
    guard_if(&mut b, idx, 8, guarded, other);

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

    let iv = range_at(&f, idx, merge_node);
    assert_top32(
        iv,
        "guard whose edge feeds a control merge must not be applied (soundness gate)",
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
    let entry = b.create_region_all().unwrap();
    let other = b.create_region_all().unwrap(); // false edge, branches to merge
    let merge = b.create_region_all().unwrap(); // 2 preds: If true edge + other

    b.set_entry_region_all(entry).unwrap();
    b.set_region(entry);
    let idx = opaque(&mut b, 0xF00D);
    // True edge feeds `merge` DIRECTLY, with no intervening region.
    guard_if(&mut b, idx, 8, merge, other);

    b.set_region(other);
    b.build_branch(merge).unwrap();

    b.set_region(merge);
    let merge_ctrl = b.region_cur_ctrl(merge);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_lift_addr(None);
    let f = b.build().unwrap();
    let merge_node = f.graph().producer(merge_ctrl);

    let iv = range_at(&f, idx, merge_node);
    assert_top32(
        iv,
        "guard on an If edge feeding a control merge directly must be top at \
         the merge (the soundness gate must skip it)",
    );
}

// `RegionCollapse` deletes the single-predecessor dispatch Region, leaving the
// If's true output feeding a `Return` directly.  Keying the guard on that
// non-Region consumer is what production jump-table resolution depends on.
#[test]
fn guard_survives_region_collapse_at_nonregion_consumer() {
    let mut b = RegisterSet::new().build_fn().unwrap();
    let entry = b.create_region_all().unwrap();
    let dispatch = b.create_region_all().unwrap(); // single-pred, collapses
    let exit = b.create_region_all().unwrap();

    b.set_entry_region_all(entry).unwrap();
    b.set_region(entry);
    let idx = opaque(&mut b, 0xF00D);
    guard_if(&mut b, idx, 8u64, dispatch, exit);

    b.set_region(dispatch);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_region(exit);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_lift_addr(None);
    let mut f = b.build().unwrap();

    // Capture the true control output BEFORE collapse, to find its consumer
    // afterward.
    let if_node = f.sole_kind(|k| matches!(k, NodeKind::If));
    let true_ctrl = f.node_outputs(if_node)[0];

    let changed = crate::test_support::run_pass(&RegionCollapse, &mut f).unwrap();
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

    let iv = range_at(&f, idx, consumer);
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
    let empty = super::interval::EMPTY;
    for k in [0, 1, 7, 127] {
        assert!(shr(empty, k).is_empty(), "shr {k}");
    }
    for c in [0, 1, 3, 1 << 40] {
        assert!(empty.udiv(c).is_empty(), "udiv {c}");
    }
    assert!(
        shl(empty, 1, u128::MAX).is_empty(),
        "shl already preserved it"
    );
    assert!(
        empty.mul(2, u128::MAX).is_empty(),
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
    (0..=mask).filter(|&x| iv.contains(x)).collect()
}

/// A shift count past the `u128` carrier scales nothing: the shifted value
/// falls back to its own facts rather than a mapped operand bound.
#[test]
fn a_shift_past_the_carrier_is_no_scaling() {
    use super::interval::ScaleOp;
    assert!(ScaleOp::shl(127).is_some() && ScaleOp::shr(127).is_some());
    assert!(ScaleOp::shl(128).is_none() && ScaleOp::shr(128).is_none());
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
        if !out.contains(concrete) {
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
                for k in [0u32, 1, 2, 3, 4, 5, 6, 7, 31, 127] {
                    let shl = shl(iv, k, MASK);
                    let shr = shr(iv, k);
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
                if sb.contains(&x) && !meet.contains(x) {
                    fails.push(format!("meet a={a:?} b={b:?} x={x} meet={meet:?}"));
                }
            }
            let join = a.union(b);
            for &x in sa.iter().chain(sb.iter()) {
                if !join.contains(x) {
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
    let entry = b.create_region_all().unwrap();
    b.set_entry_region_all(entry).unwrap();

    b.set_region(entry);
    let addr = b.build_int_const(0xDEAD_u64, ValueType::int(64)).unwrap();
    let raw = b
        .build_load(addr, VnSpace::RAM, ValueType::int(64))
        .unwrap();
    let fv = b
        .build_int_bits_to_float(raw, ValueType::float(64))
        .unwrap();
    let bits = b.build_float_bits_to_int(fv, ValueType::int(64)).unwrap();
    b.build_return(Some(bits), &[]).unwrap();

    b.set_lift_addr(None);
    let f = b.build().unwrap();

    let iv = range_at(&f, fv, f.entry());
    assert!(iv.is_top(u128::MAX), "F64 range must be top, got {iv:?}",);
    assert!(iv.count() > 1, "F64 range must not be a singleton");
}

/// `k` `If`s in a chain, each bounding the same value on its true edge, which
/// leaves for a leaf `Return`; the false edge continues the chain.  Returns
/// `(function, value, first_leaf)`, the leaf two dominator steps from the
/// entry however large `k` is.
fn build_guard_fan(k: usize) -> (strider_ir::Function, ValueId, NodeId) {
    let mut b = RegisterSet::new().build_fn().unwrap();
    let entry = b.create_region_all().unwrap();
    let leaves: Vec<_> = (0..k).map(|_| b.create_region_all().unwrap()).collect();
    let rest: Vec<_> = (0..k).map(|_| b.create_region_all().unwrap()).collect();
    b.set_entry_region_all(entry).unwrap();

    b.set_region(entry);
    let idx = opaque(&mut b, 0xDEAD);
    let mut cur = entry;
    for i in 0..k {
        b.set_region(cur);
        let bound = b
            .build_int_const(1000u64 - i as u64, ValueType::int(32))
            .unwrap();
        let cond = b
            .build_int_cmp_operation(idx, bound, IntCmpOp::Less, ValueType::int(32))
            .unwrap();
        b.build_if(cond, leaves[i], rest[i]).unwrap();
        b.set_region(leaves[i]);
        b.build_return(Some(idx), &[]).unwrap();
        cur = rest[i];
    }
    b.set_region(cur);
    b.build_return(Some(idx), &[]).unwrap();
    b.set_lift_addr(None);
    let mut f = b.build().unwrap();
    canonicalize(&mut f);

    let first_if = f.first_kind(|k| matches!(k, NodeKind::If));
    let leaf = edge_consumer(&f, first_if, 0);
    (f, idx, leaf)
}

/// A guard lookup costs the guards dominating the query point, not the value's
/// guard count: `k` guards on one value must not make a query at one leaf `k`
/// times more expensive.  Only the first `If`'s guard dominates that leaf.
#[test]
fn a_guard_lookup_does_not_scale_with_guard_count() {
    let mut probes = Vec::new();
    for k in [4usize, 32] {
        let (f, idx, leaf) = build_guard_fan(k);
        let mut ranges = RangeMap::new(&f);
        assert_eq!(
            ranges.guard_forests[&idx].entries.len(),
            k,
            "each If's true edge bounds the value"
        );
        let n = steps(&[&super::GUARD_PROBES], || {
            let iv = ranges.range_of(idx, leaf);
            assert_eq!(
                (iv.lo, iv.hi),
                (0, 999),
                "the outermost guard is idx < 1000"
            );
            Ok(())
        })
        .unwrap();
        probes.push(n);
    }
    assert_eq!(
        probes[0], probes[1],
        "8x the guards, one dominating the leaf: {probes:?}"
    );
}

/// `n` guarded sites in sequence, each `If` bounding its own value on the
/// edge into the next: the `i`th guard sits `i` dominator steps deep.
/// Returns the function and each `(value, guard_node)`.
fn build_guard_sequence(n: usize) -> (strider_ir::Function, Vec<(ValueId, NodeId)>) {
    let mut b = RegisterSet::new().build_fn().unwrap();
    let sites: Vec<_> = (0..=n).map(|_| b.create_region_all().unwrap()).collect();
    let exits: Vec<_> = (0..n).map(|_| b.create_region_all().unwrap()).collect();
    b.set_entry_region_all(sites[0]).unwrap();
    let mut values = Vec::new();
    for i in 0..n {
        b.set_region(sites[i]);
        let addr = b
            .build_int_const(0x1000 + i as u64, ValueType::int(64))
            .unwrap();
        let idx = b
            .build_load(addr, VnSpace::RAM, ValueType::int(32))
            .unwrap();
        let bound = b.build_int_const(8u64, ValueType::int(32)).unwrap();
        let cond = b
            .build_int_cmp_operation(idx, bound, IntCmpOp::Less, ValueType::int(32))
            .unwrap();
        b.build_if(cond, sites[i + 1], exits[i]).unwrap();
        b.set_region(exits[i]);
        b.build_return(Some(idx), &[]).unwrap();
        values.push(idx);
    }
    b.set_region(sites[n]);
    let zero = b.build_int_const(0u64, ValueType::int(32)).unwrap();
    b.build_return(Some(zero), &[]).unwrap();
    b.set_lift_addr(None);
    let mut f = b.build().unwrap();
    canonicalize(&mut f);
    // Each value's guard holds at the consumer of its compare's `If` true edge.
    let sites = values
        .iter()
        .map(|&v| {
            let (cmp, _) = f
                .value_uses(v)
                .find(|&(n, _)| matches!(f.node_kind(n), NodeKind::IntCmpOp(_)))
                .expect("the compare reads v");
            let cond = f.first_value_output_of(cmp).expect("the compare's output");
            let (if_node, _) = f.value_uses(cond).next().expect("the If reads it");
            let true_edge = f.node_outputs(if_node)[NodeKind::IF_TRUE_OUT_SLOT];
            let (node, _) = f.value_uses(true_edge).next().expect("a consumer");
            (v, node)
        })
        .collect();
    (f, sites)
}

/// A lookup must not walk from the query point to the entry: over `n`
/// sequential sites that would be quadratic.
#[test]
fn guard_lookups_over_sequential_sites_cost_linear_total() {
    fn probes(n: usize) -> u64 {
        let (f, sites) = build_guard_sequence(n);
        let mut ranges = RangeMap::new(&f);
        steps(&[&super::GUARD_PROBES], || {
            for &(value, node) in &sites {
                let iv = ranges.range_of(value, node);
                assert_eq!((iv.lo, iv.hi), (0, 7), "each site's own guard holds");
            }
            Ok(())
        })
        .unwrap()
    }
    let small = probes(40);
    let large = probes(320);
    // Linear would be 8x; quadratic 64x.
    assert!(
        large < small * 16,
        "8x the sites probed {:.1}x ({small} -> {large})",
        large as f64 / small as f64,
    );
}

/// The x86 shape a `cmp $N,%al` / `movzbl %al,%eax` dispatch builds: the guard
/// lands on the `I8` truncation, and the index the addressing mode scales sits
/// three casts above it.  Without the ladder walk the index falls back to the
/// `I8` KnownBits bound and reads the whole 256-entry domain.
///
/// Returns the scaled value's range on the guard's true edge.
fn cast_ladder_range(bound: u64, and_mask: Option<u64>) -> Interval {
    // `al`, the value the compare names; built in both arms, it hash-conses.
    fn al_of(b: &mut FunctionBuilder, wide: ValueId) -> ValueId {
        b.truncate_if_needed(wide, ValueType::int(8)).unwrap()
    }
    let g = guarded(
        ValueType::int(64),
        |b, wide| {
            let al = al_of(b, wide);
            let bound_c = b.build_int_const(bound, ValueType::int(8)).unwrap();
            b.build_int_cmp_operation(al, bound_c, IntCmpOp::Less, ValueType::int(8))
                .unwrap()
        },
        |b, wide| {
            // zext -> trunc -> zext, then optionally a mask, then the address scale.
            let al = al_of(b, wide);
            let z1 = b
                .extend_if_needed(al, ValueType::int(64), ExtendOp::ZeroExtend)
                .unwrap();
            let t1 = b.truncate_if_needed(z1, ValueType::int(32)).unwrap();
            let z2 = b
                .extend_if_needed(t1, ValueType::int(64), ExtendOp::ZeroExtend)
                .unwrap();
            let idx = match and_mask {
                None => z2,
                Some(m) => {
                    let mc = b.build_int_const(m, ValueType::int(64)).unwrap();
                    b.build_int_binary_operation(z2, mc, IntBinaryOp::And, ValueType::int(64))
                        .unwrap()
                }
            };
            let four = b.build_int_const(4_u64, ValueType::int(64)).unwrap();
            b.build_int_binary_operation(idx, four, IntBinaryOp::Mul, ValueType::int(64))
                .unwrap()
        },
    );
    range_at(&g.f, g.ret, g.true_node)
}

/// `al < 21` bounds `zext(trunc(zext(al))) * 4` to 21 entries, not the 256 the
/// `I8` width alone allows.
#[test]
fn guard_carries_up_a_zext_trunc_ladder() {
    let iv = cast_ladder_range(21, None);
    assert_eq!((iv.lo, iv.hi, iv.stride), (0, 80, 4));
    assert_eq!(iv.count(), 21);
}

/// `x & c <= x`, so the same guard survives a mask wider than it.
#[test]
fn guard_carries_up_through_a_mask() {
    let iv = cast_ladder_range(21, Some(0x17f));
    assert_eq!(iv.lo, 0);
    assert_eq!(iv.hi, 80);
    assert!(iv.count() <= 21, "count {}", iv.count());
}

/// `index(w)`'s range under `srl_sltiu(w, lo, bound)`, on its true edge.
fn bit_field_guard_range(
    lo: u64,
    bound: u64,
    index: fn(&mut FunctionBuilder, ValueId) -> ValueId,
) -> Interval {
    let g = guarded(ValueType::int(32), |b, w| srl_sltiu(b, w, lo, bound), index);
    range_at(&g.f, g.ret, g.true_node)
}

/// MIPS64 guards a 62-entry table with `w >> 26 < 62` but indexes it with an
/// `ext` of the same six bits, a different expression of one value.  The
/// field's own KnownBits bound is 64 values, two past the table.
#[test]
fn a_guard_on_another_extract_of_the_same_bits_bounds_the_index() {
    let iv = bit_field_guard_range(26, 62, ext_26_6);
    assert_eq!((iv.lo, iv.hi), (0, 61));
}

/// Bits 27.. of `w` are not bits 26.. of it, and `sext64(w) >> 26` carries
/// sign copies above the six bits: neither is the guarded value, so each keeps
/// its own KnownBits bound.
#[test]
fn a_guard_on_other_bits_does_not_bound_the_index() {
    let iv = bit_field_guard_range(27, 30, ext_26_6);
    assert_eq!((iv.lo, iv.hi), (0, 63));
    let sext_shr_26 = |b: &mut FunctionBuilder, w: ValueId| {
        let i64t = ValueType::int(64);
        let wide = b.extend_if_needed(w, i64t, ExtendOp::SignExtend).unwrap();
        let k = b.build_int_const(26u64, i64t).unwrap();
        b.build_int_binary_operation(wide, k, IntBinaryOp::ShiftRight, i64t)
            .unwrap()
    };
    let iv = bit_field_guard_range(26, 62, sext_shr_26);
    assert_eq!((iv.lo, iv.hi), (0, (1 << 38) - 1));
}

/// `(w >> lo) & mask` at `I32`.
fn field_of(b: &mut FunctionBuilder, w: ValueId, lo: u64, mask: u64) -> ValueId {
    let i32t = ValueType::int(32);
    let k = b.build_int_const(lo, i32t).unwrap();
    let up = b
        .build_int_binary_operation(w, k, IntBinaryOp::ShiftRight, i32t)
        .unwrap();
    let m = b.build_int_const(mask, i32t).unwrap();
    b.build_int_binary_operation(up, m, IntBinaryOp::And, i32t)
        .unwrap()
}

/// A guard on a run of bits bounds a field inside it by the guard's bound
/// shifted down to the field: `w < 20` puts `(w >> 4) & 7` at most 1, and
/// `w >> 26 < 8` puts `(w >> 28) & 3` at most `7 >> 2`.
#[test]
fn a_guard_on_enclosing_bits_bounds_the_field_inside() {
    let g = guarded(
        ValueType::int(32),
        |b, w| cmp_c(b, w, IntCmpOp::Less, 20),
        |b, w| field_of(b, w, 4, 7),
    );
    let iv = range_at(&g.f, g.ret, g.true_node);
    assert_eq!((iv.lo, iv.hi), (0, 1));
    let g = guarded(
        ValueType::int(32),
        |b, w| srl_sltiu(b, w, 26, 8),
        |b, w| field_of(b, w, 28, 3),
    );
    let iv = range_at(&g.f, g.ret, g.true_node);
    assert_eq!((iv.lo, iv.hi), (0, 1));
}

/// The range at the `Return` of the value [`deep_phi_chain`] returns.
fn deep_phi_chain_range(n: usize) -> Interval {
    let (f, returned, ret) = deep_phi_chain(n);
    range_at(&f, returned, ret)
}

/// `x = 0` then `n` sequential `if (c < i) { x = i % 7; }` triangles, then
/// `Return(x)`:
/// `PhiCollapse` cannot remove the merge phis, so the returned value is a phi
/// `n` levels deep.  The returned value and the `Return`.
fn deep_phi_chain(n: usize) -> (Function, ValueId, NodeId) {
    let var = reg_vn(0x1000, 4);
    let cvar = reg_vn(0x2000, 4);
    let mut b = RegisterSet::new()
        .tracked(var)
        .tracked(cvar)
        .arg(cvar)
        .build_fn()
        .unwrap();
    let entry = b.create_region_all().unwrap();
    b.set_entry_region_all(entry).unwrap();
    b.set_region(entry);
    let zero = b.build_int_const(0_u64, ValueType::int(32)).unwrap();
    b.write_variable(&var, zero).unwrap();
    for i in 0..n {
        let assign = b.create_region_all().unwrap();
        let merge = b.create_region_all().unwrap();
        let c = b.read_variable(&cvar).unwrap();
        let k = b.build_int_const(i as u64 + 1, ValueType::int(32)).unwrap();
        let cond = b
            .build_int_cmp_operation(c, k, IntCmpOp::Less, ValueType::int(32))
            .unwrap();
        b.build_if(cond, assign, merge).unwrap();
        b.set_region(assign);
        let kv = b
            .build_int_const((i % 7) as u64, ValueType::int(32))
            .unwrap();
        b.write_variable(&var, kv).unwrap();
        b.build_branch(merge).unwrap();
        b.set_region(merge);
    }
    let x = b.read_variable(&var).unwrap();
    b.build_return(None, &[x]).unwrap();
    b.set_lift_addr(None);
    let mut f = b.build().unwrap();
    canonicalize(&mut f);
    let ret = f.sole_kind(|k| matches!(k, NodeKind::Return));
    let returned = f.returned();
    (f, returned, ret)
}

/// A phi chain past `MAX_RANGE_DEPTH` widens to a sound answer rather than
/// recursing once per level: 4000 levels overflow a 2 MiB stack unbudgeted.
#[test]
fn phi_chain_deeper_than_the_budget_stays_sound() {
    let worker = std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(|| deep_phi_chain_range(4000))
        .expect("spawns");
    let iv = worker.join().expect("range_of returns");
    // Every arm is `i % 7`.
    assert!(
        iv.lo == 0 && iv.hi >= 6 && iv.stride == 1,
        "unsound range for a capped chain: {iv:?}"
    );
}

/// A memo entry that outlives the query that wrote it answers what a fresh
/// analysis of the same key does.
///
/// Every frame above a depth or cycle cut folds the cut's `top` into its own
/// answer, under a key that does not record the cut; keeping those would hand
/// the degraded answer to the next query, so which jump tables resolve would
/// depend on the order the sites were visited. A query that cuts therefore
/// keeps nothing. The 8-level chain is the control: it cuts nothing, so every
/// answer stays and the comparison is not vacuous.
///
/// The cycle is `a = phi(0, b)`, `b = phi(1, a)` converged, where resolving
/// either re-enters the other.
///
/// Mutations: keep the memo past a cut; stop marking either kind of cut.
#[test]
fn a_memo_entry_outliving_its_query_is_not_degraded() {
    let mut cases: Vec<(String, Function, ValueId, NodeId)> = [8usize, 300]
        .into_iter()
        .map(|n| {
            let (f, returned, ret) = deep_phi_chain(n);
            (format!("a {n}-level chain"), f, returned, ret)
        })
        .collect();
    // Single-input phis in the body region otherwise answer the swap before
    // the cycle closes.
    let (mut cyclic, phi_a, header) = swap_loop();
    canonicalize(&mut cyclic);
    cases.push(("a converged swap loop".to_owned(), cyclic, phi_a, header));

    for (case, f, value, region) in &cases {
        let mut map = RangeMap::new(f);
        map.range_of(*value, *region);
        let kept: Vec<((ValueId, NodeId), Interval)> = map
            .memo
            .iter()
            .filter_map(|(&key, slot)| match slot {
                super::MemoSlot::Done(iv) => Some((key, *iv)),
                super::MemoSlot::InProgress => None,
            })
            .collect();
        for ((value, region), kept) in &kept {
            assert_eq!(
                *kept,
                range_at(f, *value, *region),
                "{case}: the kept answer for {value:?} at {region:?} is not the fresh one"
            );
        }
        assert_eq!(
            kept.is_empty(),
            case != "a 8-level chain",
            "{case}: a query that cuts nothing keeps its answers, one that cuts keeps none"
        );
    }
}

/// A chain within the budget still resolves exactly: the cap must not cost
/// precision on anything a real function reaches.
#[test]
fn phi_chain_within_the_budget_resolves_exactly() {
    let iv = deep_phi_chain_range(8);
    assert_eq!((iv.lo, iv.hi, iv.stride), (0, 6, 1));
}

/// What [`check_guard_forest`] exercised.
#[derive(Default, Debug)]
struct GuardTally {
    functions: usize,
    queries: usize,
    /// Queries by how many guards dominate them: none, one, two, three or
    /// more.
    by_met: [usize; 4],
    /// Queries past a guard that starts earlier in preorder and does not
    /// dominate them, which the descent must skip.
    skipped_over: usize,
    /// Guards dropped for sitting off the tree.
    off_tree: usize,
    most_entries: usize,
    /// Queries with a non-empty answer met from three guards or more.
    deep_nonempty: usize,
    most_met: usize,
}

/// Puts random dense guards on `f`'s nodes and checks every query against
/// the guards whose nodes dominate it. The control edges come from
/// `cfg_outputs` and `value_uses`, as in production; dominance over them is
/// by definition rather than from the tree.
///
/// Every guard spans `2^20..2^21`, so no meet is empty, and two guards almost
/// never share a bound, so a missing or extra guard shows whenever it holds
/// the highest `lo` or the lowest `hi`.
fn check_guard_forest(f: &Function, rng: &mut crate::test_support::Rng, tally: &mut GuardTally) {
    use strider_ir_test_utils::graph_oracle;
    tally.functions += 1;
    let doms = control_dominator_tree(f);
    let nodes: Vec<NodeId> = f.graph().all_node_ids().collect();
    let index: rustc_hash::FxHashMap<NodeId, u32> = nodes
        .iter()
        .enumerate()
        .map(|(i, &n)| (n, i as u32))
        .collect();
    let succs: Vec<Vec<u32>> = nodes
        .iter()
        .map(|&n| {
            strider_ir::walk::cfg_outputs(f.graph(), n)
                .flat_map(|out| f.graph().value_uses(out))
                .map(|(user, _)| index[&user])
                .collect()
        })
        .collect();
    let entry = index[&f.entry()];
    let dom = graph_oracle::dom_matrix(&succs, entry);
    let mut by_node: rustc_hash::FxHashMap<NodeId, Interval> = rustc_hash::FxHashMap::default();
    for &n in &nodes {
        let on_tree = doms.span(n).is_some();
        if rng.below(if on_tree { 2 } else { 3 }) == 0 {
            let lo = rng.below(1 << 20) as u128;
            let hi = (1 << 21) + rng.below(1 << 20) as u128;
            by_node.insert(n, Interval::dense(lo, hi));
            tally.off_tree += usize::from(!on_tree);
        }
    }
    let forest = super::DomFacts::new(
        &doms,
        by_node.iter().map(|(&n, &iv)| (n, iv)),
        Interval::intersect,
    );
    tally.most_entries = tally.most_entries.max(forest.entries.len());
    for (q, &qn) in nodes.iter().enumerate() {
        assert_eq!(doms.span(qn).is_some(), dom[entry as usize][q], "{qn:?}");
        let Some(span) = doms.span(qn) else {
            continue;
        };
        let dominating: Vec<Interval> = by_node
            .iter()
            .filter(|&(g, _)| dom[index[g] as usize][q])
            .map(|(_, &iv)| iv)
            .collect();
        let want = dominating.iter().copied().reduce(Interval::intersect);
        let got = forest.meet_dominating(span);
        assert_eq!(
            got.map(|iv| (iv.lo, iv.hi)),
            want.map(|iv| (iv.lo, iv.hi)),
            "at {qn:?}"
        );
        tally.queries += 1;
        tally.by_met[dominating.len().min(3)] += 1;
        tally.most_met = tally.most_met.max(dominating.len());
        tally.deep_nonempty +=
            usize::from(dominating.len() >= 3 && want.is_some_and(|iv| iv.count() > 0));
        tally.skipped_over += usize::from(by_node.keys().any(|&g| {
            doms.span(g)
                .is_some_and(|(pre, _)| pre < span.0 && !dom[index[&g] as usize][q])
        }));
    }
}

/// Each query meets exactly the guards whose nodes dominate it: over random
/// control flow, and over deep guard sequences whose forests need every
/// level of the sparse table.
#[test]
fn guard_forest_meets_exactly_the_dominating_guards() {
    let seeds = if cfg!(debug_assertions) {
        1_000
    } else {
        20_000
    };
    let mut random = GuardTally::default();
    for seed in 0..seeds {
        let Some(f) = crate::differential::build_random_cfg(seed) else {
            continue;
        };
        let mut rng = crate::test_support::Rng(seed | 1);
        check_guard_forest(&f, &mut rng, &mut random);
    }
    let sequences: &[usize] = if cfg!(debug_assertions) {
        &[1, 5, 31, 64]
    } else {
        &[1, 5, 31, 64, 127, 300]
    };
    let mut deep = GuardTally::default();
    for &n in sequences {
        let (f, _) = build_guard_sequence(n);
        for draw in 0..4 {
            let mut rng = crate::test_support::Rng((n as u64) << 8 | draw | 1);
            check_guard_forest(&f, &mut rng, &mut deep);
        }
    }
    let floor = seeds as usize / 10;
    assert!(
        random.functions >= floor
            && random.skipped_over >= floor
            && random.off_tree >= floor
            && random.by_met.iter().all(|&c| c >= floor)
            && random.deep_nonempty >= floor,
        "floor {floor}: {random:?}"
    );
    assert!(
        deep.most_entries >= 64 && deep.most_met >= 32 && deep.deep_nonempty >= 4 * 64,
        "{deep:?}"
    );
}

/// `s = x ? 0 : (y ? 3 : last)` at the join, `last` the constant 5 or, with
/// `None`, an argument: a phi of `0` and an inner phi of `3` and `last`.  The
/// returned `s` and the region joining it.
fn three_way_phi(last: Option<u64>) -> (Function, ValueId, NodeId) {
    let s_vn = reg_vn(0x10, 4);
    let arg_vn = reg_vn(0x20, 4);
    let mut b = RegisterSet::new()
        .tracked(s_vn)
        .tracked(arg_vn)
        .arg(arg_vn)
        .build_fn()
        .unwrap();
    let [entry, a, mid, arm_b, arm_c, inner, join] =
        std::array::from_fn(|_| b.create_region_all().unwrap());
    b.set_entry_region_all(entry).unwrap();
    let i32t = ValueType::int(32);

    b.set_region(entry);
    let arg = b.read_variable(&arg_vn).unwrap();
    let x = cmp_c(&mut b, arg, IntCmpOp::Equal, 1);
    b.build_if(x, a, mid).unwrap();
    b.set_region(mid);
    let y = cmp_c(&mut b, arg, IntCmpOp::Equal, 2);
    b.build_if(y, arm_b, arm_c).unwrap();
    for (region, k, next) in [
        (a, Some(0), join),
        (arm_b, Some(3), inner),
        (arm_c, last, inner),
    ] {
        b.set_region(region);
        let v = match k {
            Some(k) => b.build_int_const(k, i32t).unwrap(),
            None => b.read_variable(&arg_vn).unwrap(),
        };
        b.write_variable(&s_vn, v).unwrap();
        b.build_branch(next).unwrap();
    }
    b.set_region(inner);
    b.build_branch(join).unwrap();
    b.set_region(join);
    let s = b.read_variable(&s_vn).unwrap();
    b.build_return(Some(s), &[]).unwrap();
    b.set_lift_addr(None);
    let mut f = b.build().unwrap();
    canonicalize(&mut f);
    let s = f.returned();
    let token = f.graph().nth_input(f.producer(s), 0).unwrap();
    let at = f.graph().producer(token);
    (f, s, at)
}

/// A phi of `0` and a phi of `3` and `5` holds those three values, where its
/// range is the hull `0..=5`.  Mutation: return the hull's members.
#[test]
fn phi_constants_are_the_arms_not_their_range() {
    let (f, s, at) = three_way_phi(Some(5));
    assert!(
        matches!(f.kind_of_value(s), NodeKind::Phi),
        "{}",
        f.to_text(false)
    );
    let iv = range_at(&f, s, at);
    assert_eq!(
        (iv.lo, iv.hi, iv.stride),
        (0, 5, 1),
        "the range is the hull"
    );
    assert_eq!(super::phi_constants(&f, s, 3), Some(vec![0, 3, 5]));
}

/// Over `cap` values there is no set.  Mutation: compare against `cap + 1`.
#[test]
fn phi_constants_give_up_past_the_cap() {
    let (f, s, _) = three_way_phi(Some(5));
    assert_eq!(super::phi_constants(&f, s, 2), None);
}

/// An arm that is neither a constant nor such a phi leaves no set.
/// Mutation: skip a non-constant arm instead of giving up.
#[test]
fn phi_constants_need_every_arm_constant() {
    let (f, s, _) = three_way_phi(None);
    assert!(
        matches!(f.kind_of_value(s), NodeKind::Phi),
        "{}",
        f.to_text(false)
    );
    assert_eq!(super::phi_constants(&f, s, 16), None);
}

/// Two loop phis that are each other's back-edge arm hold only what entered
/// them, `0` and `1`, and the cycle between them terminates.  Mutation: give
/// up on a phi seen before instead of skipping it.
#[test]
fn phi_constants_close_over_a_cycle_of_phis() {
    let (f, phi_a, _) = swap_loop();
    assert_eq!(super::phi_constants(&f, phi_a, 16), Some(vec![0, 1]));
}

/// A value that is not a phi has no set here, a constant included.
#[test]
fn phi_constants_are_only_of_a_phi() {
    let (f, s, _) = three_way_phi(Some(5));
    let ret = f.producer(s);
    let arm = f.phi_data_inputs(ret).next().unwrap();
    assert_eq!(super::phi_constants(&f, arm, 16), None);
}

/// `if (x == 0) ... ; dispatch` over the argument `x`, the `x != 0` path
/// either leading straight to the dispatch `If(x < 5)` or, with `merge`,
/// joining the `x == 0` path first; `const_first` spells the test `0 == x`.
/// `x` and the dispatch `If`.
fn not_zero_then_dispatch(merge: bool, const_first: bool) -> (Function, ValueId, NodeId) {
    let x_vn = reg_vn(0x10, 4);
    let mut b = RegisterSet::new()
        .tracked(x_vn)
        .arg(x_vn)
        .build_fn()
        .unwrap();
    let [entry, zero, rest, c, d] = std::array::from_fn(|_| b.create_region_all().unwrap());
    b.set_entry_region_all(entry).unwrap();
    b.set_region(entry);
    let x = b.read_variable(&x_vn).unwrap();
    let is_zero = if const_first {
        let c = b.build_int_const(0u64, ValueType::int(32)).unwrap();
        b.build_int_cmp_operation(c, x, IntCmpOp::Equal, ValueType::int(32))
            .unwrap()
    } else {
        cmp_c(&mut b, x, IntCmpOp::Equal, 0)
    };
    b.build_if(is_zero, zero, rest).unwrap();
    b.set_region(zero);
    if merge {
        b.build_branch(rest).unwrap();
    } else {
        b.build_return(None, &[]).unwrap();
    }
    b.set_region(rest);
    let x_rest = b.read_variable(&x_vn).unwrap();
    let small = cmp_c(&mut b, x_rest, IntCmpOp::Less, 5);
    b.build_if(small, c, d).unwrap();
    for region in [c, d] {
        b.set_region(region);
        b.build_return(None, &[]).unwrap();
    }
    b.set_lift_addr(None);
    let mut f = b.build().unwrap();
    canonicalize(&mut f);
    let dispatch = f
        .walk_kind(|k| matches!(k, NodeKind::If))
        .find(|&n| {
            let cond = f.if_cond(n);
            matches!(f.kind_of_value(cond), NodeKind::IntCmpOp(IntCmpOp::Less))
        })
        .expect("the dispatch If");
    let x = f
        .walk_kind(|k| matches!(k, NodeKind::InitialVar(_)))
        .flat_map(|n| f.node_outputs(n).to_vec())
        .find(|&v| f.value_type_opt(v) == Some(ValueType::int(32)))
        .expect("x");
    (f, x, dispatch)
}

/// Past the false edge of `x == 0`, `x` is not 0, and only 0 is ruled out.
/// Mutation: test the true edge instead of the false one.
#[test]
fn an_equality_guard_rules_its_constant_out_past_its_false_edge() {
    let (f, x, dispatch) = not_zero_then_dispatch(false, false);
    let ranges = RangeMap::new(&f);
    let ruled: Vec<u128> = (0..4)
        .filter(|&k| ranges.excludes(x, k, dispatch))
        .collect();
    assert_eq!(ruled, [0], "{}", f.to_text(false));
}

/// `0 == x` rules 0 out as `x == 0` does.
/// Mutation: take the comparison's second operand as the constant always.
#[test]
fn an_equality_guard_with_the_constant_first_rules_it_out() {
    let (f, x, dispatch) = not_zero_then_dispatch(false, true);
    let ranges = RangeMap::new(&f);
    let ruled: Vec<u128> = (0..4)
        .filter(|&k| ranges.excludes(x, k, dispatch))
        .collect();
    assert_eq!(ruled, [0], "{}", f.to_text(false));
}

/// Where the `x == 0` path joins the other first, `x` may be 0 again.
/// Mutation: accept a merge as the false edge's sink.
#[test]
fn an_equality_guard_rules_nothing_out_past_a_merge() {
    let (f, x, dispatch) = not_zero_then_dispatch(true, false);
    let ranges = RangeMap::new(&f);
    assert!(!ranges.excludes(x, 0, dispatch), "{}", f.to_text(false));
}

/// `a, b, c = 0, 1, 1; while (a < 16) a, b, c = b, c, a`: three header phis,
/// each the next one's back-edge arm, holding two constants.  The `a` phi.
fn rotate_loop() -> (Function, ValueId) {
    let vns = [reg_vn(0x10, 4), reg_vn(0x20, 4), reg_vn(0x30, 4)];
    let mut b = RegisterSet::new()
        .tracked(vns[0])
        .tracked(vns[1])
        .tracked(vns[2])
        .build_fn()
        .unwrap();
    let [entry, header, body, exit] = std::array::from_fn(|_| b.create_region_all().unwrap());
    b.set_entry_region_all(entry).unwrap();
    b.set_region(entry);
    for (vn, k) in vns.iter().zip([0u64, 1, 1]) {
        let k = b.build_int_const(k, ValueType::int(32)).unwrap();
        b.write_variable(vn, k).unwrap();
    }
    b.build_branch(header).unwrap();
    b.set_region(header);
    let a = b.read_variable(&vns[0]).unwrap();
    let cond = c_less(&mut b, 16, a);
    b.build_if(cond, exit, body).unwrap();
    b.set_region(body);
    let old: Vec<ValueId> = vns.iter().map(|vn| b.read_variable(vn).unwrap()).collect();
    for (i, vn) in vns.iter().enumerate() {
        b.write_variable(vn, old[(i + 1) % 3]).unwrap();
    }
    b.build_branch(header).unwrap();
    b.set_region(exit);
    b.build_return(Some(a), &[]).unwrap();
    b.set_lift_addr(None);
    let mut f = b.build().unwrap();
    canonicalize(&mut f);
    let a = f.returned();
    (f, a)
}

/// Three phis holding two constants: a cap of two is past the phi count,
/// three is not.  Mutation: drop the phi count from the cap.
#[test]
fn phi_constants_give_up_past_the_cap_on_phis() {
    let (f, a) = rotate_loop();
    assert_eq!(
        super::phi_constants(&f, a, 3),
        Some(vec![0, 1]),
        "{}",
        f.to_text(false)
    );
    assert_eq!(super::phi_constants(&f, a, 2), None);
}

/// `ValueType::int_mask_u128` answers `None` for three unrelated facts -- a
/// float, a width past the `u128` carrier, and (through `value_type_opt`) an
/// edge carrying no type -- and each consumer degrades its own way. The tests
/// below pin the fallback this module picks, at the point that picks it.
#[test]
fn an_untracked_width_reads_as_lattice_top() {
    assert_eq!(
        super::interval::type_mask_or_top(None),
        u128::MAX,
        "no typed edge"
    );
    assert_eq!(
        super::interval::type_mask_or_top(Some(ValueType::float(64))),
        u128::MAX,
        "a float has no integer modulus"
    );
    assert_eq!(
        super::interval::type_mask_or_top(Some(ValueType::int(256))),
        u128::MAX,
        "past the carrier no u128 is the mask"
    );
    assert_eq!(
        super::interval::type_mask_or_top(Some(ValueType::int(32))),
        0xFFFF_FFFF,
        "a tracked width is its own mask"
    );
}

/// A whole function whose only value is `wide`, at a width past the carrier.
fn over_carrier_fn() -> (Function, ValueId, NodeId) {
    let vn = reg_vn(0x2000, 32);
    let (f, wide) = strider_ir_test_utils::make_fn_with_var(vn, |_, x| Ok(x)).unwrap();
    let entry = f.entry();
    (f, wide, entry)
}

/// `range_of` refuses the whole query rather than returning a bound no `u128`
/// can express: `top(u128::MAX)` is not a claim about the value.
#[test]
fn a_range_query_past_the_carrier_answers_unconstrained_top() {
    let (f, wide, region) = over_carrier_fn();
    assert_eq!(f.value_type_opt(wide), Some(ValueType::int(256)));
    assert_eq!(range_at(&f, wide, region), Interval::top(u128::MAX));
    assert_eq!(super::width_mask(&f, wide), u128::MAX);
}

/// `x < 5` signed, `x` a register of `bytes`, read on its true edge, with
/// the KnownBits facts claiming every bit of `x` zero when `claim` is set.
fn sless_five_guard(bytes: u32, claim: bool) -> Option<Interval> {
    let vn = reg_vn(0x2000, bytes);
    let ty = ValueType::int(8 * bytes as u16);
    let cmp = std::cell::Cell::new(None);
    let (f, x) = strider_ir_test_utils::make_fn_with_var(vn, |b, x| {
        let k = b.build_int_const(5u64, ty)?;
        cmp.set(Some(b.build_int_cmp_operation(
            x,
            k,
            IntCmpOp::Sless,
            ty,
        )?));
        b.extend_if_needed(cmp.get().unwrap(), ty, strider_ir::ExtendOp::ZeroExtend)
    })
    .unwrap();
    let cmp = cmp.get().unwrap();
    let mut known = crate::opt::known_bits::analyze(&f);
    if claim {
        known[x] = crate::opt::known_bits::KnownBitsFacts {
            ones: 0,
            zeros: u128::MAX,
        };
    }
    super::guard_from_compare(&f, cmp, true, &known).map(|(_, iv)| iv)
}

/// A signed compare bounds the value unsigned only once its sign bit is
/// proven zero, and at a width past the carrier no fact set names that bit,
/// even one that claims it.
/// Mutation: drop the sign-bit gate.
#[test]
fn an_unproven_sign_bit_bounds_nothing() {
    assert_eq!(sless_five_guard(4, false), None, "no facts");
    assert_eq!(
        sless_five_guard(4, true),
        Some(Interval::dense(0, 4)),
        "the sign bit proven zero"
    );
    assert_eq!(
        sless_five_guard(32, true),
        None,
        "a u128 of zeros names no bit of an i256"
    );
}

/// The meet is the same either way round, holds every common element, and
/// is exactly them wherever the residue search runs its full cycle.
#[test]
fn intersect_is_exact_and_order_free_on_small_progressions() {
    let all: Vec<Interval> = (0..6u128)
        .flat_map(|lo| (lo..12).flat_map(move |hi| (1..5).map(move |s| Interval::new(lo, hi, s))))
        .collect();
    for &a in &all {
        for &b in &all {
            let m = a.intersect(b);
            assert_eq!(m, b.intersect(a), "{a:?} & {b:?}");
            let common: Vec<u128> = (0..16)
                .filter(|&x| a.contains(x) && b.contains(x))
                .collect();
            let got: Vec<u128> = (0..16).filter(|&x| m.contains(x)).collect();
            assert_eq!(got, common, "{a:?} & {b:?} = {m:?}");
        }
    }
}

/// A residue search past its cap keeps the wider stride's progression rather
/// than flattening to a dense span: strides 97 and 101 first agree 90 steps
/// of 101 in, at 9118.
/// Mutation: fall back to the dense span at the cap, or step by the first
/// operand's stride.
#[test]
fn a_capped_meet_keeps_the_wider_stride() {
    let narrow = Interval::new(0, 1 << 20, 97);
    let wide = Interval::new(28, 1 << 20, 101);
    let m = narrow.intersect(wide);
    assert_eq!(m, Interval::new(28, 1 << 20, 101));
    assert_eq!(m, wide.intersect(narrow));
    assert!(m.contains(9118));
}

/// `p = phi(arm(b, 0xA000), arm(b, 0xA010))` at `I64`, then
/// `if (6 < trunc32(p)) exit else dispatch`. Returns the function, `p` and
/// the dispatch edge's consumer.
fn phi_under_a_truncated_guard(
    arm: impl Fn(&mut FunctionBuilder, u64) -> ValueId,
) -> (Function, ValueId, NodeId) {
    let idx_vn = reg_vn(0x10, 8);
    let mut b = RegisterSet::new().tracked(idx_vn).build_fn().unwrap();
    let [entry, path_a, path_b, join, dispatch, exit] =
        std::array::from_fn(|_| b.create_region_all().unwrap());
    b.set_entry_region_all(entry).unwrap();
    b.set_region(entry);
    let sel = opaque(&mut b, 0xF000);
    guard_if(&mut b, sel, 1, path_a, path_b);
    for (region, addr) in [(path_a, 0xA000), (path_b, 0xA010)] {
        b.set_region(region);
        let v = arm(&mut b, addr);
        b.write_variable(&idx_vn, v).unwrap();
        b.build_branch(join).unwrap();
    }
    b.set_region(join);
    let p = b.read_variable(&idx_vn).unwrap();
    let t = b.build_single_output_pure(NodeKind::Truncate, [p], ValueType::int(32));
    let over = c_less(&mut b, 6, t);
    b.build_if(over, exit, dispatch).unwrap();
    for region in [dispatch, exit] {
        b.set_region(region);
        b.build_return(Some(p), &[]).unwrap();
    }
    b.set_lift_addr(None);
    let mut f = b.build().unwrap();
    canonicalize(&mut f);
    let guard = f
        .walk()
        .find(|&n| {
            matches!(f.node_kind(n), NodeKind::If)
                && f.producer_inputs_exact::<2>(f.graph().nth_input(n, 1).unwrap())
                    .is_ok_and(|[_, t]| matches!(f.kind_of_value(t), NodeKind::Truncate))
        })
        .expect("the If on the truncation");
    let (p, _) = join_phi(&f, 2);
    let dispatch = edge_consumer(&f, guard, 1);
    (f, p, dispatch)
}

/// A phi whose arms fit 32 bits takes a guard on its truncation to 32 bits.
#[test]
fn a_phi_within_a_guarded_truncation_takes_its_guard() {
    let (f, p, dispatch) = phi_under_a_truncated_guard(|b, addr| {
        let x = opaque(b, addr);
        b.build_single_output_pure(
            NodeKind::Extend(ExtendOp::ZeroExtend),
            [x],
            ValueType::int(64),
        )
    });
    let iv = range_at(&f, p, dispatch);
    assert_eq!((iv.lo, iv.hi), (0, 6), "{iv:?}");
}

/// A phi with an arm past 32 bits keeps its high bits: `trunc32(p) <= 6`
/// does not bound it.
#[test]
fn a_phi_past_a_guarded_truncation_ignores_its_guard() {
    let (f, p, dispatch) = phi_under_a_truncated_guard(|b, addr| {
        let a = b.build_int_const(addr, ValueType::int(64)).unwrap();
        b.build_load(a, VnSpace::RAM, ValueType::int(64)).unwrap()
    });
    let iv = range_at(&f, p, dispatch);
    assert!(iv.is_top(u128::from(u64::MAX)), "{iv:?}");
}
