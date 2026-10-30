use strider_ir::Function;
use strider_ir::UBig;
use strider_ir::node::{ExtendOp, NodeId, NodeKind, ValueId, ValueType};
use strider_ir::{
    FunctionBuilder, IRBuilderExt, IRViewer, IRWalker, IntBinaryOp, IntCmpOp,
    control_dominator_tree,
};
use strider_ir_test_utils::{RegisterSet, SENTINEL_LIFT_ADDR};

use super::Interval;
use super::RangeMap;

/// `Interval::new` over `u128` literals.
fn new_iv(lo: u128, hi: u128, stride: u128) -> Interval {
    Interval::new(ub(lo), ub(hi), ub(stride))
}

/// [`new_iv`] with a stride of 1.
fn dense_iv(lo: u128, hi: u128) -> Interval {
    new_iv(lo, hi, 1)
}

/// A strided interval, as a reader spells one.
fn strided(lo: u128, hi: u128, stride: u128) -> Interval {
    new_iv(lo, hi, stride)
}

/// Every value at or below `bound`.
fn top_iv(bound: u128) -> Interval {
    Interval::top(&ub(bound))
}

fn empty_iv() -> Interval {
    new_iv(1, 0, 1)
}

/// `(lo, hi, stride)` in the carrier, for an interval a test keeps there.
fn parts(iv: &Interval) -> (u128, u128, u128) {
    (small(iv.lo()), small(iv.hi()), small(iv.stride()))
}

/// Whether `iv` holds the carrier value `x`.
fn has(iv: &Interval, x: u128) -> bool {
    iv.contains(&ub(x))
}

/// A carrier literal as a `UBig`.
fn ub(x: u128) -> UBig {
    UBig::from(x)
}

/// A `UBig` a test keeps inside the carrier, as a literal.
fn small(v: &UBig) -> u128 {
    u128::try_from(v).expect("a test value inside the carrier")
}

/// The stride is at least 1 by construction, and emptiness has one name.
#[test]
fn new_clamps_the_stride_and_empty_counts_nothing() {
    for (lo, hi) in [(0, 0), (3, 9), (5, 2)] {
        assert_eq!(new_iv(lo, hi, 0), new_iv(lo, hi, 1));
    }
    assert!(empty_iv().is_empty());
    assert_eq!(small(&empty_iv().count()), 0);
}

/// The AP-join keeps the coarsest spacing every element of both sides obeys: a
/// constant arm joined with a scaled one must not flatten it to dense, or a
/// 6-entry table reads as 21 slots.
#[test]
fn union_keeps_a_stride_the_join_still_obeys() {
    let scaled = strided(0, 20, 4);
    let joined = dense_iv(0, 0).union(scaled.clone());
    assert_eq!(parts(&joined), (0, 20, 4));
    assert_eq!(small(&joined.count()), 6);
    // An offset start is only congruent modulo the gap between the two.
    let offset = dense_iv(2, 2).union(scaled.clone());
    assert_eq!(parts(&offset), (0, 20, 2));
    // Two genuinely different spacings meet at their gcd.
    let by_six = strided(0, 18, 6);
    assert_eq!(small(scaled.union(by_six).stride()), 2);
}

/// Two single-element sides carry no spacing of their own, so the gap between
/// their starts is the whole answer: `phi(4, 8, 12, 16)` is four indices, not
/// the thirteen a flattened join enumerates.
#[test]
fn union_of_single_elements_keeps_the_gap_as_the_stride() {
    let joined = [4u128, 8, 12, 16]
        .into_iter()
        .map(|k| dense_iv(k, k))
        .reduce(Interval::union)
        .expect("four arms");
    assert_eq!(parts(&joined), (4, 16, 4));
    assert_eq!(small(&joined.count()), 4);
    // One point joined with itself has no gap and no spacing; the stride still
    // has to be a usable divisor.
    let point = dense_iv(7, 7).union(dense_iv(7, 7));
    assert_eq!(parts(&point), (7, 7, 1));
    assert_eq!(small(&point.count()), 1);
}

/// An empty side adds nothing, so the join is the other side exactly: its
/// `lo` and stride kept. Every 8-bit interval, against each spelling of empty.
/// Mutation: drop the empty early-outs from `union`.
#[test]
fn union_with_an_empty_side_is_the_other_side() {
    for empty in [empty_iv(), new_iv(9, 3, 1), new_iv(255, 0, 7)] {
        assert!(empty.is_empty());
        for lo in 0..=255u128 {
            for hi in 0..=255u128 {
                for stride in 1..=255u128 {
                    let x = new_iv(lo, hi, stride);
                    if x.is_empty() {
                        assert!(
                            empty.clone().union(x.clone()).is_empty()
                                && x.clone().union(empty.clone()).is_empty()
                        );
                        continue;
                    }
                    assert_eq!(empty.clone().union(x.clone()), x, "{empty:?} with {x:?}");
                    assert_eq!(x.clone().union(empty.clone()), x, "{x:?} with {empty:?}");
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
        .map(|(lo, hi, s)| new_iv(lo, hi, s))
        .collect();
    for a in &all {
        for b in &all {
            let u = a.clone().union(b.clone());
            for x in 0..8u128 {
                if has(a, x) || has(b, x) {
                    assert!(has(&u, x), "{a:?} | {b:?} = {u:?} misses {x}");
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
    let dense = dense_iv(5, 40);
    let mul8 = strided(0, 64, 8);
    let m = dense.intersect(mul8);
    assert_eq!(parts(&m), (8, 40, 8));
    assert_eq!(small(&m.count()), 5);
    // Equal strides with opposite phases have no common element.
    let odd = strided(1, 100, 2);
    let ev = strided(0, 100, 2);
    assert_eq!(small(&odd.intersect(ev).count()), 0);
    // Compatible strides combine to lcm with the shared phase.
    let a = strided(0, 200, 4);
    let b = strided(0, 200, 6);
    let m = a.intersect(b);
    assert_eq!((small(m.lo()), small(m.stride())), (0, 12));
}

/// One past `iv`'s top element, or `None` when `iv` spans the whole width.
fn upper_exclusive(iv: &Interval, width_mask: u128) -> Option<u64> {
    if small(iv.hi()) >= width_mask {
        None
    } else {
        u64::try_from(small(iv.hi()) + 1).ok()
    }
}

/// `value`'s interval at `region`, from a fresh analysis of `f`.
fn range_at(f: &Function, value: ValueId, region: NodeId) -> Interval {
    RangeMap::new(f).unwrap().range_of(value, region)
}

/// Brings a hand-built fixture to the converged IR shape the range analysis
/// assumes: no single-input phis, no single-predecessor regions, no
/// `If(Xor(C,1))` conds (rewritten to `If(C)` with the branches swapped).
fn canonicalize(f: &mut strider_ir::Function) {
    let mut p = crate::OptimizerPipeline::new();
    p.add(crate::IfCondInversion::new());
    p.add(crate::PhiCollapse);
    p.add(crate::RegionCollapse);
    p.run(f, &crate::OptCtx::new(None)).unwrap();
}

/// `(true_edge_consumer, false_edge_consumer)` of the sole `If`: the
/// post-collapse query points where each edge's guard holds.  `IfCondInversion`
/// swaps the branches of an `Xor(C,1)` cond, so the original "true" dispatch
/// becomes the false edge in that case.
fn if_edge_consumers(f: &strider_ir::Function) -> (NodeId, NodeId) {
    let if_node = f
        .walk_kind(|k| matches!(k, NodeKind::If))
        .next()
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
    let idx = b.build_load(addr, rsleigh::VnSpace::RAM, ty).unwrap();
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
                Bounds(lo, hi) => {
                    assert_eq!(
                        (small(iv.lo()), small(iv.hi())),
                        (lo, hi),
                        "{name}: {edge} edge"
                    );
                }
                Top => assert!(
                    iv.is_top(&ub(mask)) && upper_exclusive(&iv, mask).is_none(),
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
    assert_eq!(parts(&iv), (0, 3 << 30, 1 << 30));
    assert!(at(4).is_top(&ub(0xffff_ffff)), "{:?}", at(4));
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
    assert_eq!(parts(&iv), (0, 12, 4));
    assert_eq!(small(&iv.count()), 4);
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
    assert_eq!(parts(&iv), (4, 12, 4));
    let iv = range(0xffff_fff8);
    assert!(
        small(iv.lo()) == 0 && small(iv.hi()) >= 8 && small(iv.stride()) == 8,
        "a mask clearing bit 2 is not the guarded value, got {iv:?}"
    );
}

/// Same through a non-power-of-two `idx * c`: `idx < 4` gives `idx*3  in
/// {0,3,6,9}`.
#[test]
fn guard_propagates_through_mul_const() {
    let iv = scaled_range(4, IntBinaryOp::Mul, 3);
    assert_eq!(parts(&iv), (0, 9, 3));
    assert_eq!(small(&iv.count()), 4);
}

/// The floor shapes reduce the range: `idx < 8` gives `idx >> 1  in  [0,3]`.
#[test]
fn guard_propagates_through_shift_right() {
    let iv = scaled_range(8, IntBinaryOp::ShiftRight, 1);
    assert_eq!((small(iv.lo()), small(iv.hi())), (0, 3));
}

/// `idx < 8` gives `idx / 2  in  [0,3]`. A scale's operand guard gates the
/// propagation and then bounds the operand, which is one scan, not two: each is
/// a linear walk of the operand's guard list with a dominance test per entry.
#[test]
fn scaled_range_scans_the_operand_guards_once() {
    let g = guarded_scaled(8, IntBinaryOp::Div, 2);
    let mut ranges = RangeMap::new(&g.f).unwrap();
    super::GUARD_SCANS.with(|c| c.set(0));
    let iv = ranges.range_of(g.ret, g.true_node);
    assert_eq!((small(iv.lo()), small(iv.hi())), (0, 3));
    assert_eq!(
        super::GUARD_SCANS.with(std::cell::Cell::get),
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

    let ty = ValueType::int(32);
    b.set_region(entry);
    let va = b.build_int_const(0xDEAD_u64, ValueType::int(64)).unwrap();
    let v = b.build_load(va, rsleigh::VnSpace::RAM, ty).unwrap();
    let other = if second_arm_bounds_v {
        v
    } else {
        let wa = b.build_int_const(0xBEEF_u64, ValueType::int(64)).unwrap();
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
    let iv = range_at(&f, v, merge);
    assert_eq!(
        (small(iv.lo()), small(iv.hi())),
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
    assert!(
        iv.is_top(&ub(ValueType::int(32).int_mask_u128().unwrap())),
        "v must stay unbounded: one merge arm does not bound it, got [{},{}]",
        small(iv.lo()),
        small(iv.hi()),
    );
}

/// `shl`/`mul` must WIDEN to top when the scaled top leaves the bound, and
/// must NOT when a wider bound still holds it: the same product is top at
/// `u128::MAX` and exact at an `i256`.
#[test]
fn shl_and_mul_widen_to_top_only_past_the_bound() {
    let hi = 1u128 << 127;
    assert!(
        dense_iv(0, hi)
            .shl(1, &ub(u128::MAX))
            .is_top(&ub(u128::MAX)),
        "2^127 << 1 leaves u128 -> must be top, not a wrapped [0,0]",
    );
    assert!(
        dense_iv(0, hi)
            .mul(&ub(4), &ub(u128::MAX))
            .is_top(&ub(u128::MAX)),
        "2^127 * 4 leaves u128 -> must be top",
    );
    // The same two products at an i256 bound are exact, where the old carrier
    // could only answer top.
    let wide = strider_ir::eval::all_ones(ValueType::int(256));
    assert_eq!(
        dense_iv(0, hi).shl(1, &wide),
        Interval::new(UBig::ZERO, UBig::ONE << 128usize, UBig::from(2u32)),
    );
    assert_eq!(
        dense_iv(0, hi).mul(&ub(4), &wide),
        Interval::new(UBig::ZERO, UBig::ONE << 129usize, UBig::from(4u32)),
    );
    // The common in-type case is unaffected by routing shl through mul.
    let iv = dense_iv(0, 3).shl(2, &ub(0xFFFF_FFFF));
    assert_eq!(parts(&iv), (0, 12, 4));
}

/// `mul` bounds `hi` against the type, and the stride with it: scaling a
/// single point must not carry the factor into a stride no value of the type
/// can span, which a chain of scalings would grow without limit.
#[test]
fn mul_holds_the_stride_inside_the_span() {
    let wide = strider_ir::eval::all_ones(ValueType::int(512));
    // {0} scaled by 2^400 is still {0}: `hi == lo`, so the stride is 1.
    let point = dense_iv(0, 0).shl(400, &wide);
    assert_eq!(point, Interval::new(UBig::ZERO, UBig::ZERO, UBig::ONE));
    // A stride already past its own span denotes one point; scaling keeps it
    // one point rather than a 2^400 stride over a widened span.
    let one_point = new_iv(0, 1, 8).shl(400, &wide);
    assert_eq!(one_point.count(), UBig::ONE);
    assert!(one_point.contains(&UBig::ZERO));
    assert!(*one_point.stride() <= wide, "a stride inside the type");
}

/// Floored `shr`/`udiv` recover `stride/divisor` when the divisor divides the
/// stride (exact), and widen to 1 otherwise (a non-dividing divisor breaks the
/// progression, so the floored image is not an AP and must over-approximate).
#[test]
fn shr_udiv_recover_stride_only_on_clean_division() {
    // c | stride: x/c walks exactly stride/c.
    let clean = strided(0, 12, 4);
    let d = clean.clone().udiv(&ub(2));
    assert_eq!(parts(&d), (0, 6, 2), "4 / 2 -> stride 2");
    let s = clean.shr(1);
    assert_eq!(parts(&s), (0, 6, 2), "4 >> 1 -> stride 2");

    // c does NOT divide stride: {0,5,10,15}/2 = {0,2,5,7}, which is not an AP;
    // widening to stride 1 keeps it sound (stride/c = 2 would drop 5 and 7).
    let ragged = strided(0, 15, 5);
    let r = ragged.clone().udiv(&ub(2));
    assert_eq!(parts(&r), (0, 7, 1), "non-dividing stride widens");
    for real in [0u128, 2, 5, 7] {
        assert!(
            small(r.lo()) <= real
                && real <= small(r.hi())
                && (real - small(r.lo())).is_multiple_of(small(r.stride())),
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
            let src = new_iv(lo, lo + stride * 6, stride);
            let elems: Vec<u128> = (0..=6).map(|i| lo + i * stride).collect();
            for c in 1u128..=8 {
                let r = src.clone().udiv(&ub(c));
                for &x in &elems {
                    let v = x / c;
                    assert!(
                        small(r.lo()) <= v
                            && v <= small(r.hi())
                            && (v - small(r.lo())).is_multiple_of(small(r.stride())),
                        "udiv lied: lo={lo} stride={stride} c={c} x={x} -> {v} outside {r:?}",
                    );
                }
                if c.is_power_of_two() {
                    let k = c.trailing_zeros();
                    let r = src.clone().shr(k);
                    for &x in &elems {
                        let v = x >> k;
                        assert!(
                            small(r.lo()) <= v
                                && v <= small(r.hi())
                                && (v - small(r.lo())).is_multiple_of(small(r.stride())),
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
    let dummy = b.build_int_const(0xCAFEu64, ValueType::int(64)).unwrap();
    let raw_idx = b
        .build_load(dummy, VnSpace::RAM, ValueType::int(32))
        .unwrap();
    b.write_variable(&idx_vn, raw_idx).unwrap();
    let bound_c = b.build_int_const(8u64, ValueType::int(32)).unwrap();
    let cond = b
        .build_int_cmp_operation(raw_idx, bound_c, IntCmpOp::Less, ValueType::int(32))
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

    let iv = range_at(&f, raw_idx, dispatch_node);
    assert_eq!(small(iv.lo()), 0, "trivial phi: lower bound must be 0");
    assert_eq!(small(iv.hi()), 7, "trivial phi: upper bound must be 7");
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
    let dummy = b.build_int_const(0x1000u64, ValueType::int(64)).unwrap();
    let arg = b
        .build_load(dummy, rsleigh::VnSpace::RAM, ValueType::int(32))
        .unwrap();
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

    let mut ranges = RangeMap::new(&f).unwrap();

    let iv_entry = ranges.range_of(idx, entry_node);
    assert_eq!(
        small(iv_entry.hi()),
        7,
        "KnownBits bound: hi must be 7 in entry"
    );
    assert_eq!(
        small(iv_entry.lo()),
        0,
        "KnownBits bound: lo must be 0 in entry"
    );

    let iv_other = ranges.range_of(idx, other_node);
    assert_eq!(
        small(iv_other.hi()),
        7,
        "KnownBits bound: hi must be 7 in other"
    );
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

    let dummy = b.build_int_const(0x1000u64, ValueType::int(64)).unwrap();
    let arg = b
        .build_load(dummy, rsleigh::VnSpace::RAM, ValueType::int(32))
        .unwrap();
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
    assert_eq!(
        (small(iv.lo()), small(iv.hi())),
        (0, 56),
        "scaled index spans [0, 56]"
    );
    assert_eq!(small(iv.stride()), 8, "low 3 known-zero bits ⇒ stride 8");
    assert_eq!(
        small(&iv.count()),
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
    let dummy = b.build_int_const(0xBEEFu64, ValueType::int(64)).unwrap();
    let raw_idx = b
        .build_load(dummy, VnSpace::RAM, ValueType::int(32))
        .unwrap();
    b.write_variable(&idx_vn, raw_idx).unwrap();
    let bound_c = b.build_int_const(8u64, ValueType::int(32)).unwrap();
    let cond = b
        .build_int_cmp_operation(raw_idx, bound_c, IntCmpOp::Less, ValueType::int(32))
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

    let iv = range_at(&f, phi_idx, dispatch_node);
    let type_mask = ValueType::int(32).int_mask_u128().unwrap();
    assert!(
        iv.is_top(&ub(type_mask)),
        "phi with one unguarded predecessor must produce top, got [{}, {}]",
        small(iv.lo()),
        small(iv.hi())
    );
    assert!(
        upper_exclusive(&iv, type_mask).is_none(),
        "upper_exclusive must be None for top"
    );
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
    assert_eq!(
        small(iv.lo()),
        0,
        "Sless with known-zero sign bit: lower bound 0"
    );
    assert_eq!(
        small(iv.hi()),
        7,
        "Sless with known-zero sign bit: idx s< 8 -> [0, 7]"
    );
}

#[test]
fn no_constraint_is_top() {
    let mut b = RegisterSet::new().build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    let entry = b.create_region_all().unwrap();
    b.set_entry_region_all(entry).unwrap();
    b.set_region(entry);

    let dummy = b.build_int_const(0x4242u64, ValueType::int(64)).unwrap();
    let idx = b
        .build_load(dummy, rsleigh::VnSpace::RAM, ValueType::int(32))
        .unwrap();
    b.build_return(Some(idx), &[]).unwrap();

    b.set_lift_addr(None);
    let f = b.build().unwrap();
    let entry_node = f.entry();

    let iv = range_at(&f, idx, entry_node);
    let type_mask = ValueType::int(32).int_mask_u128().unwrap();
    assert!(
        iv.is_top(&ub(type_mask)),
        "unconstrained load must produce top, got [{}, {}]",
        small(iv.lo()),
        small(iv.hi())
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
    let dummy = b.build_int_const(0x1000u64, ValueType::int(64)).unwrap();
    let raw_idx = b
        .build_load(dummy, VnSpace::RAM, ValueType::int(32))
        .unwrap();
    b.write_variable(&idx_vn, raw_idx).unwrap();
    let flag = b.build_boolean_const(true);
    b.build_if(flag, left, right).unwrap();

    b.set_region(left);
    let bound_c = b.build_int_const(8u64, ValueType::int(32)).unwrap();
    let cond = b
        .build_int_cmp_operation(raw_idx, bound_c, IntCmpOp::Less, ValueType::int(32))
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

    let mut ranges = RangeMap::new(&f).unwrap();

    let iv_dispatch = ranges.range_of(raw_idx, dispatch_node);
    assert_eq!(
        small(iv_dispatch.hi()),
        7,
        "dispatch must have bound 7, got {}",
        small(iv_dispatch.hi())
    );

    let iv_right = ranges.range_of(raw_idx, right_node);
    let type_mask = ValueType::int(32).int_mask_u128().unwrap();
    assert!(
        iv_right.is_top(&ub(type_mask)),
        "sibling region must be top, got [{}, {}]",
        small(iv_right.lo()),
        small(iv_right.hi())
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
    let zero = b.build_int_const(0u64, ValueType::int(32)).unwrap();
    b.write_variable(&idx_vn, zero).unwrap();
    b.build_branch(header).unwrap();

    // Reading the variable here becomes a Phi once the regions are linked.
    b.set_region(header);
    let header_ctrl = b.region_cur_ctrl(header);
    let idx_phi = b.read_variable(&idx_vn).unwrap();
    let bound = b.build_int_const(16u64, ValueType::int(32)).unwrap();
    let cond = b
        .build_int_cmp_operation(idx_phi, bound, IntCmpOp::Less, ValueType::int(32))
        .unwrap();
    b.build_if(cond, body, loop_exit).unwrap();

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
    let type_mask = ValueType::int(32).int_mask_u128().unwrap();
    assert!(
        iv.is_top(&ub(type_mask)),
        "cyclic phi must yield top, got [{}, {}]",
        small(iv.lo()),
        small(iv.hi())
    );
}

// A loop-carried masked index: `idx_phi = Phi(0, raw & 7)` where the back edge
// is the taken side of `if (raw & 7 < 6)`.  The mask alone admits 0..7 and no
// guard node dominates the header, so the bound lives on the back edge, where
// it holds for the arm arriving on it.
#[test]
fn back_edge_guard_bounds_a_masked_loop_index() {
    use rsleigh::VnSpace;
    use strider_ir_test_utils::reg_vn;

    let idx_vn = reg_vn(0x10, 4);
    let mut b = RegisterSet::new().tracked(idx_vn).build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

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
    let addr = b.build_int_const(0xDEADu64, ValueType::int(64)).unwrap();
    let raw = b
        .build_load(addr, VnSpace::RAM, ValueType::int(32))
        .unwrap();
    let seven = b.build_int_const(7u64, ValueType::int(32)).unwrap();
    let masked = b
        .build_int_binary_operation(raw, seven, IntBinaryOp::And, ValueType::int(32))
        .unwrap();
    b.write_variable(&idx_vn, masked).unwrap();
    let six = b.build_int_const(6u64, ValueType::int(32)).unwrap();
    let cond = b
        .build_int_cmp_operation(masked, six, IntCmpOp::Less, ValueType::int(32))
        .unwrap();
    b.build_if(cond, header, loop_exit).unwrap();

    b.set_region(loop_exit);
    b.build_return(Some(idx_phi), &[]).unwrap();

    b.set_lift_addr(None);
    let f = b.build().unwrap();
    let header_node = f.graph().producer(header_ctrl);

    let iv = range_at(&f, idx_phi, header_node);
    assert_eq!(
        (small(iv.lo()), small(iv.hi())),
        (0, 5),
        "0 on entry, at most 5 on the back edge"
    );
}

/// `a, b = 0, 1; while (a < 16) a, b = b, a`: each header phi's back-edge
/// arm is the OTHER header phi.  The `a` phi and its header.
fn swap_loop() -> (Function, ValueId, NodeId) {
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
    let zero = b.build_int_const(0u64, ValueType::int(32)).unwrap();
    let one_seed = b.build_int_const(1u64, ValueType::int(32)).unwrap();
    b.write_variable(&a_vn, zero).unwrap();
    b.write_variable(&b_vn, one_seed).unwrap();
    b.build_branch(header).unwrap();

    // Reading `a` here becomes phi_a once the regions are linked.
    b.set_region(header);
    let header_ctrl = b.region_cur_ctrl(header);
    let phi_a = b.read_variable(&a_vn).unwrap();
    let bound = b.build_int_const(16u64, ValueType::int(32)).unwrap();
    let cond = b
        .build_int_cmp_operation(phi_a, bound, IntCmpOp::Less, ValueType::int(32))
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
    let type_mask = ValueType::int(32).int_mask_u128().unwrap();
    assert!(
        iv.is_top(&ub(type_mask)),
        "phi-of-phi cycle must yield top, got [{}, {}]",
        small(iv.lo()),
        small(iv.hi())
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
    let dummy = b.build_int_const(0xF00Du64, ValueType::int(64)).unwrap();
    let idx = b
        .build_load(dummy, rsleigh::VnSpace::RAM, ValueType::int(32))
        .unwrap();
    let bound16 = b.build_int_const(16u64, ValueType::int(32)).unwrap();
    let cond16 = b
        .build_int_cmp_operation(idx, bound16, IntCmpOp::Less, ValueType::int(32))
        .unwrap();
    b.build_if(cond16, outer_guard, exit).unwrap();

    b.set_region(outer_guard);
    let bound8 = b.build_int_const(8u64, ValueType::int(32)).unwrap();
    let cond8 = b
        .build_int_cmp_operation(idx, bound8, IntCmpOp::Less, ValueType::int(32))
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

    let iv = range_at(&f, idx, dispatch_node);
    assert_eq!(
        small(iv.lo()),
        0,
        "nested guards: lower bound must be 0, got {}",
        small(iv.lo())
    );
    assert_eq!(
        small(iv.hi()),
        7,
        "nested guards: inner dispatch must see [0,7] from intersection, got {}",
        small(iv.hi())
    );
    assert_eq!(
        upper_exclusive(&iv, ValueType::int(32).int_mask_u128().unwrap()),
        Some(8),
        "upper_exclusive must be Some(8)"
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
    let dummy = b.build_int_const(0xCAFEu64, ValueType::int(64)).unwrap();
    let idx = b
        .build_load(dummy, rsleigh::VnSpace::RAM, ValueType::int(32))
        .unwrap();
    let flag = b.build_boolean_const(true);
    b.build_if(flag, branch_a, branch_b).unwrap();

    b.set_region(branch_a);
    let bound8 = b.build_int_const(8u64, ValueType::int(32)).unwrap();
    let cond8 = b
        .build_int_cmp_operation(idx, bound8, IntCmpOp::Less, ValueType::int(32))
        .unwrap();
    b.build_if(cond8, dispatch_a, sink_a).unwrap();

    b.set_region(dispatch_a);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_region(sink_a);
    b.build_return(Some(idx), &[]).unwrap();

    b.set_region(branch_b);
    let bound16 = b.build_int_const(16u64, ValueType::int(32)).unwrap();
    let cond16 = b
        .build_int_cmp_operation(idx, bound16, IntCmpOp::Less, ValueType::int(32))
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

    let mut ranges = RangeMap::new(&f).unwrap();

    let iv_a = ranges.range_of(idx, da_node);
    assert_eq!(small(iv_a.lo()), 0, "dispatch_a lo must be 0");
    assert_eq!(small(iv_a.hi()), 7, "dispatch_a hi must be 7 (idx < 8)");

    let iv_b = ranges.range_of(idx, db_node);
    assert_eq!(small(iv_b.lo()), 0, "dispatch_b lo must be 0");
    assert_eq!(small(iv_b.hi()), 15, "dispatch_b hi must be 15 (idx < 16)");
}

/// The join of four constant arms is an arithmetic progression, not the dense
/// span: `table.rs` enumerates `(lo..=hi).step_by(stride)`, so a flattened join
/// offers sixteen indices where the phi holds four, and one surplus index that
/// fails to fold defers the whole site.  The arms share no low zero bits, so
/// the spacing can only come from the join.
#[test]
fn multi_input_phi_of_constants_keeps_the_arm_spacing() {
    use strider_ir_test_utils::reg_vn;

    let idx_vn = reg_vn(0x10, 4);
    let mut b = RegisterSet::new().tracked(idx_vn).build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

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

    let phi_producer = f
        .walk()
        .find(|&n| matches!(f.node_kind(n), NodeKind::Phi) && f.phi_data_inputs(n).count() >= 4)
        .expect("the four-arm join phi");
    let phi_idx = f.node_outputs(phi_producer)[0];
    let phi_token = f.graph().nth_input(phi_producer, 0).unwrap();
    let join_region = f.graph().producer(phi_token);

    let iv = range_at(&f, phi_idx, join_region);
    assert_eq!(
        parts(&iv),
        (3, 18, 5),
        "phi(3, 8, 13, 18) is a stride-5 progression"
    );
    assert_eq!(small(&iv.count()), 4, "four arms, four candidate indices");
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
    let dummy_a = b.build_int_const(0xAAAAu64, ValueType::int(64)).unwrap();
    let load_a = b
        .build_load(dummy_a, VnSpace::RAM, ValueType::int(32))
        .unwrap();
    let mask7 = b.build_int_const(7u64, ValueType::int(32)).unwrap();
    let idx_a = b
        .build_int_binary_operation(load_a, mask7, IntBinaryOp::And, ValueType::int(32))
        .unwrap();
    b.write_variable(&idx_vn, idx_a).unwrap();
    b.build_branch(join).unwrap();

    // A DISTINCT load, so the join phi is genuinely multi-input and survives
    // PhiCollapse.
    b.set_region(path_b);
    let dummy_b = b.build_int_const(0xBBBBu64, ValueType::int(64)).unwrap();
    let load_b = b
        .build_load(dummy_b, VnSpace::RAM, ValueType::int(32))
        .unwrap();
    let mask15 = b.build_int_const(15u64, ValueType::int(32)).unwrap();
    let idx_b = b
        .build_int_binary_operation(load_b, mask15, IntBinaryOp::And, ValueType::int(32))
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

    let iv = range_at(&f, phi_idx, join_region);
    assert_eq!(small(iv.lo()), 0, "union lower bound is 0");
    assert_eq!(
        small(iv.hi()),
        15,
        "phi must UNION the two distinct finite arms ([0,7] ∪ [0,15] = [0,15]); \
         it must NOT tighten to [0,7] nor widen to top"
    );
    assert!(
        !iv.is_top(&ub(ValueType::int(32).int_mask_u128().unwrap())),
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
    let dummy = b.build_int_const(0xDEADu64, ValueType::int(64)).unwrap();
    let raw_idx = b
        .build_load(dummy, VnSpace::RAM, ValueType::int(32))
        .unwrap();
    b.write_variable(&idx_vn, raw_idx).unwrap();
    let flag = b.build_boolean_const(true);
    b.build_if(flag, path_a, path_b).unwrap();

    // The two arms must carry different SSA values, or the join phi is trivial
    // and `PhiCollapse` removes it, defeating the test.
    b.set_region(path_a);
    let dummy_a = b.build_int_const(0xAAAAu64, ValueType::int(64)).unwrap();
    let idx_a = b
        .build_load(dummy_a, VnSpace::RAM, ValueType::int(32))
        .unwrap();
    b.write_variable(&idx_vn, idx_a).unwrap();
    b.build_branch(join).unwrap();

    // A DISTINCT load, giving the join phi two distinct data inputs.
    b.set_region(path_b);
    let dummy_b = b.build_int_const(0xBBBBu64, ValueType::int(64)).unwrap();
    let idx_b = b
        .build_load(dummy_b, VnSpace::RAM, ValueType::int(32))
        .unwrap();
    b.write_variable(&idx_vn, idx_b).unwrap();
    b.build_branch(join).unwrap();

    b.set_region(join);
    let phi_idx = b.read_variable(&idx_vn).unwrap();
    let bound_c = b.build_int_const(8u64, ValueType::int(32)).unwrap();
    let cond = b
        .build_int_cmp_operation(phi_idx, bound_c, IntCmpOp::Less, ValueType::int(32))
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

    let iv = range_at(&f, phi_idx, dispatch_node);
    assert_eq!(
        small(iv.lo()),
        0,
        "multi-phi output guard: lower bound must be 0"
    );
    assert_eq!(
        small(iv.hi()),
        7,
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
    let dummy = b.build_int_const(0xDEADu64, ValueType::int(64)).unwrap();
    let raw_idx = b
        .build_load(dummy, VnSpace::RAM, ValueType::int(32))
        .unwrap();
    b.write_variable(&idx_vn, raw_idx).unwrap();
    // A throwaway comparison, only here to split control.
    let zero = b.build_int_const(0u64, ValueType::int(32)).unwrap();
    let flag = b
        .build_int_cmp_operation(raw_idx, zero, IntCmpOp::Equal, ValueType::int(32))
        .unwrap();
    b.build_if(flag, path_a, path_b).unwrap();

    b.set_region(path_a);
    let idx_a = b.read_variable(&idx_vn).unwrap();
    let four = b.build_int_const(4u64, ValueType::int(32)).unwrap();
    let cond_a = b
        .build_int_cmp_operation(idx_a, four, IntCmpOp::Less, ValueType::int(32))
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

    let iv = range_at(&f, phi_idx, dispatch_node);
    let type_mask = ValueType::int(32).int_mask_u128().unwrap();
    assert!(
        iv.is_top(&ub(type_mask)),
        "join with one unguarded predecessor must be top (fail-closed), \
         got [{}, {}]; a guard on one path must not bound the phi",
        small(iv.lo()),
        small(iv.hi())
    );
    assert!(
        upper_exclusive(&iv, type_mask).is_none(),
        "upper_exclusive must be None when join is top"
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
    let dummy = b.build_int_const(0xF00Du64, ValueType::int(64)).unwrap();
    let idx = b
        .build_load(dummy, rsleigh::VnSpace::RAM, ValueType::int(32))
        .unwrap();
    let n8 = b.build_int_const(8u64, ValueType::int(32)).unwrap();
    let cond = b
        .build_int_cmp_operation(idx, n8, IntCmpOp::Less, ValueType::int(32))
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

    let iv = range_at(&f, idx, merge_node);
    let type_mask = ValueType::int(32).int_mask_u128().unwrap();
    assert!(
        iv.is_top(&ub(type_mask)),
        "guard whose edge feeds a control merge must not be applied (soundness gate), \
         got [{}, {}]",
        small(iv.lo()),
        small(iv.hi())
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
    let dummy = b.build_int_const(0xF00Du64, ValueType::int(64)).unwrap();
    let idx = b
        .build_load(dummy, rsleigh::VnSpace::RAM, ValueType::int(32))
        .unwrap();
    let n8 = b.build_int_const(8u64, ValueType::int(32)).unwrap();
    let cond = b
        .build_int_cmp_operation(idx, n8, IntCmpOp::Less, ValueType::int(32))
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

    let iv = range_at(&f, idx, merge_node);
    let type_mask = ValueType::int(32).int_mask_u128().unwrap();
    assert!(
        iv.is_top(&ub(type_mask)),
        "guard on an If edge feeding a control merge directly must be top at \
         the merge (the soundness gate must skip it), got [{}, {}]",
        small(iv.lo()),
        small(iv.hi())
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
    let dummy = b.build_int_const(0xF00Du64, ValueType::int(64)).unwrap();
    let idx = b
        .build_load(dummy, rsleigh::VnSpace::RAM, ValueType::int(32))
        .unwrap();
    let n8 = b.build_int_const(8u64, ValueType::int(32)).unwrap();
    let cond = b
        .build_int_cmp_operation(idx, n8, IntCmpOp::Less, ValueType::int(32))
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

    let changed = crate::pipeline::run_one(&RegionCollapse, &mut f, &crate::OptCtx::new(None))
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

    let iv = range_at(&f, idx, consumer);
    assert_eq!(small(iv.lo()), 0, "collapsed-shape guard: lower bound 0");
    assert_eq!(
        small(iv.hi()),
        7,
        "collapsed-shape guard: bound survives RegionCollapse -> [0, 7]"
    );
}

/// The CRT meet must not step one stride at a time: `s1 = 1` against a
/// KnownBits stride of `2^32` is `lcm/s1`, about four billion iterations.
#[test]
fn intersect_with_a_huge_stride_is_not_a_stepping_loop() {
    let guard = strided(3, 102, 1);
    let known_bits = strided(0, u128::from(u64::MAX), 1u128 << 32);
    // Only multiples of 2^32 are in `known_bits`, and none lie in 3..=102.
    let met = guard.intersect(known_bits);
    assert_eq!(
        small(&met.count()),
        0,
        "incompatible residues meet as empty"
    );
}

/// A stride past `2^127` makes `small(other.lo()) % s2 + s2` exceed the `u128` carrier,
/// so the residue difference has to be taken without ever forming that sum.
#[test]
fn intersect_with_a_stride_past_the_carrier_half() {
    let huge = (1u128 << 127) + 1;
    let dense = top_iv(u128::MAX);
    let strided = strided(1u128 << 127, u128::MAX, huge);
    let met = dense.intersect(strided);
    // The only element of `strided` inside the carrier is its own `lo`.
    assert_eq!((small(met.lo()), small(met.stride())), (1u128 << 127, huge));
    assert_eq!(small(&met.count()), 1);
}

/// A meet that DOES have a solution still finds it.
#[test]
fn intersect_finds_the_first_common_residue() {
    let a = strided(0, 100, 6);
    let b = strided(0, 100, 10);
    let met = a.intersect(b);
    // lcm(6,10) = 30, first common element >= 0 is 0.
    assert_eq!((small(met.lo()), small(met.stride())), (0, 30));

    let c = strided(4, 100, 6);
    let d = strided(0, 100, 10);
    // 4,10,16,22,28,34,40 ... vs 0,10,20,...: first common is 10.
    let met2 = c.intersect(d);
    assert_eq!((small(met2.lo()), small(met2.stride())), (10, 30));
}

/// `shr` and `udiv` map both endpoints, so an EMPTY interval must stay empty
/// rather than collapsing to `{0,0}` ("the value is exactly zero"), which
/// passes `bounded_index`'s gate and fabricates one switch target out of
/// provably dead code. `mul`/`shl` already preserve emptiness.
#[test]
fn shr_and_udiv_preserve_emptiness() {
    let empty = empty_iv();
    for k in [0, 1, 7, 127] {
        assert!(empty.clone().shr(k).is_empty(), "shr {k}");
    }
    for c in [0u128, 1, 3, 1 << 40] {
        assert!(empty.clone().udiv(&ub(c)).is_empty(), "udiv {c}");
    }
    assert!(
        empty.clone().shl(1, &ub(u128::MAX)).is_empty(),
        "shl already preserved it"
    );
    assert!(
        empty.mul(&ub(2), &ub(u128::MAX)).is_empty(),
        "mul already preserved it"
    );
}

/// `count()` on a full-width top is `2^128`, exactly: the count is one more
/// than the span, which the unbounded carrier holds rather than saturating.
#[test]
fn count_of_full_width_top_is_exact() {
    assert_eq!(top_iv(u128::MAX).count(), UBig::ONE << 128usize);
    assert_eq!(
        Interval::top(&strider_ir::eval::all_ones(ValueType::int(256))).count(),
        UBig::ONE << 256usize
    );
}

/// A strided interval spanning the full width is NOT top: it carries the extra
/// fact "multiple of `stride`", which a consumer treating it as top would
/// silently adopt.
#[test]
fn strided_full_span_is_not_top() {
    let mask = 0xFFFF_FFFFu128;
    let strided = strided(0, mask, 3);
    assert!(!strided.is_top(&ub(mask)));
    assert!(top_iv(mask).is_top(&ub(mask)));
}

/// Every value the interval denotes, clipped to the carrier.
fn members(iv: &Interval, mask: u128) -> Vec<u128> {
    if small(iv.hi()) < small(iv.lo()) {
        return Vec::new();
    }
    let stride = small(iv.stride()).max(1);
    let mut out = Vec::new();
    let mut x = small(iv.lo());
    while x <= small(iv.hi()).min(mask) {
        out.push(x);
        match x.checked_add(stride) {
            Some(next) => x = next,
            None => break,
        }
    }
    out
}

fn contains(iv: &Interval, y: u128) -> bool {
    small(iv.lo()) <= y
        && y <= small(iv.hi())
        && (y - small(iv.lo())).is_multiple_of(small(iv.stride()).max(1))
}

/// Every concrete result of `shl` / `shr` / `mul` / `udiv` lands inside the
/// abstract one.  The concrete model saturates shift counts at and past the
/// width rather than wrapping, matching `opbehavior.cc`.
#[test]
fn scale_ops_contain_every_concrete_result() {
    const WIDTH: u32 = 5;
    const MASK: u128 = (1 << WIDTH) - 1;
    let mut fails: Vec<String> = Vec::new();
    let mut record =
        |op: &str, iv: &Interval, by: u128, x: u128, concrete: u128, out: &Interval| {
            if !contains(out, concrete) {
                fails.push(format!(
                    "{op} iv={iv:?} by={by} x={x} concrete={concrete} out={out:?}"
                ));
            }
        };
    for lo in 0..=MASK {
        for hi in 0..=MASK {
            for stride in 1u128..=8 {
                let iv = new_iv(lo, hi, stride);
                let set = members(&iv, MASK);
                if set.is_empty() {
                    continue;
                }
                for k in [0u32, 1, 2, 3, 4, 5, 6, 7, 31, 127, 128, 200] {
                    let shl = iv.clone().shl(k, &ub(MASK));
                    let shr = iv.clone().shr(k);
                    for &x in &set {
                        let up = if k >= WIDTH { 0 } else { (x << k) & MASK };
                        let down = if k >= WIDTH { 0 } else { x >> k };
                        record("shl", &iv, u128::from(k), x, up, &shl);
                        record("shr", &iv, u128::from(k), x, down, &shr);
                    }
                }
                for c in 1u128..=8 {
                    let mul = iv.clone().mul(&ub(c), &ub(MASK));
                    let udiv = iv.clone().udiv(&ub(c));
                    for &x in &set {
                        record("mul", &iv, c, x, (x * c) & MASK, &mul);
                        record("udiv", &iv, c, x, x / c, &udiv);
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
                ivs.push(new_iv(lo, hi, stride));
            }
        }
    }
    let mut fails: Vec<String> = Vec::new();
    for a in &ivs {
        let sa = members(a, MASK);
        for b in &ivs {
            let sb = members(b, MASK);
            let meet = a.clone().intersect(b.clone());
            for &x in &sa {
                if sb.contains(&x) && !contains(&meet, x) {
                    fails.push(format!("meet a={a:?} b={b:?} x={x} meet={meet:?}"));
                }
            }
            let join = a.clone().union(b.clone());
            for &x in sa.iter().chain(sb.iter()) {
                if !contains(&join, x) {
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
    let addr = b.build_int_const(0xDEAD_u64, ValueType::int(64)).unwrap();
    let raw = b
        .build_load(addr, rsleigh::VnSpace::RAM, ValueType::int(64))
        .unwrap();
    let fv = b
        .build_int_bits_to_float(raw, ValueType::float(64))
        .unwrap();
    let bits = b.build_float_bits_to_int(fv, ValueType::int(64)).unwrap();
    b.build_return(Some(bits), &[]).unwrap();

    b.set_lift_addr(None);
    let f = b.build().unwrap();

    let iv = range_at(&f, fv, f.entry());
    assert!(
        iv.is_top(&super::interval::unbounded()),
        "F64 range must be top, got {iv:?}",
    );
    assert!(iv.count() > UBig::ONE, "F64 range must not be a singleton");
}

/// `k` `If`s in a chain, each bounding the same value on its true edge, which
/// leaves for a leaf `Return`; the false edge continues the chain.  Returns
/// `(function, value, first_leaf)`, the leaf two dominator steps from the
/// entry however large `k` is.
fn build_guard_fan(k: usize) -> (strider_ir::Function, ValueId, NodeId) {
    let mut b = RegisterSet::new().build_fn().unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let entry = b.create_region_all().unwrap();
    let leaves: Vec<_> = (0..k).map(|_| b.create_region_all().unwrap()).collect();
    let rest: Vec<_> = (0..k).map(|_| b.create_region_all().unwrap()).collect();
    b.set_entry_region_all(entry).unwrap();

    b.set_region(entry);
    let dummy_addr = b.build_int_const(0xDEAD_u64, ValueType::int(64)).unwrap();
    let idx = b
        .build_load(dummy_addr, rsleigh::VnSpace::RAM, ValueType::int(32))
        .unwrap();
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

    let first_if = f
        .walk_kind(|k| matches!(k, NodeKind::If))
        .next()
        .expect("an If node");
    let true_edge = f.node_outputs(first_if)[0];
    let leaf = f
        .graph()
        .value_uses(true_edge)
        .next()
        .expect("the true edge has a consumer")
        .0;
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
        let mut ranges = RangeMap::new(&f).unwrap();
        assert_eq!(
            ranges.guards[&idx].len(),
            k,
            "each If's true edge bounds the value"
        );
        super::GUARD_PROBES.with(|c| c.set(0));
        let iv = ranges.range_of(idx, leaf);
        assert_eq!(
            (small(iv.lo()), small(iv.hi())),
            (0, 999),
            "the outermost guard is idx < 1000"
        );
        probes.push(super::GUARD_PROBES.with(std::cell::Cell::get));
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
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
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
            .build_load(addr, rsleigh::VnSpace::RAM, ValueType::int(32))
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
    let ranges = RangeMap::new(&f).unwrap();
    let sites = values
        .iter()
        .map(|v| {
            let node = *ranges.guards[v]
                .keys()
                .next()
                .expect("each site guards its value");
            (*v, node)
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
        let mut ranges = RangeMap::new(&f).unwrap();
        super::GUARD_PROBES.with(|c| c.set(0));
        for &(value, node) in &sites {
            let iv = ranges.range_of(value, node);
            assert_eq!(
                (small(iv.lo()), small(iv.hi())),
                (0, 7),
                "each site's own guard holds"
            );
        }
        super::GUARD_PROBES.with(std::cell::Cell::get)
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
    assert_eq!(parts(&iv), (0, 80, 4));
    assert_eq!(small(&iv.count()), 21);
}

/// `x & c <= x`, so the same guard survives a mask wider than it.
#[test]
fn guard_carries_up_through_a_mask() {
    let iv = cast_ladder_range(21, Some(0x17f));
    assert_eq!(small(iv.lo()), 0);
    assert_eq!(small(iv.hi()), 80);
    assert!(small(&iv.count()) <= 21, "count {}", small(&iv.count()));
}

/// MIPS64 `ext i,w,26,6; andi i,i,0xff` on the sign-extended word `w`:
/// `((sext64(w) << 32) >> 58) & 0xff`.
fn ext_26_6(b: &mut FunctionBuilder, w: ValueId) -> ValueId {
    let i64t = ValueType::int(64);
    let wide = b.extend_if_needed(w, i64t, ExtendOp::SignExtend).unwrap();
    let mut op = |x: ValueId, c: u64, op: IntBinaryOp| {
        let c = b.build_int_const(c, i64t).unwrap();
        b.build_int_binary_operation(x, c, op, i64t).unwrap()
    };
    let up = op(wide, 32, IntBinaryOp::ShiftLeft);
    let field = op(up, 58, IntBinaryOp::ShiftRight);
    op(field, 0xff, IntBinaryOp::And)
}

/// MIPS64 `srl t,w,lo; sltiu t,t,bound` on the `I32` word `w`:
/// `sext64(w >> lo) < bound`.
fn srl_sltiu(b: &mut FunctionBuilder, w: ValueId, lo: u64, bound: u64) -> ValueId {
    let (i32t, i64t) = (ValueType::int(32), ValueType::int(64));
    let k = b.build_int_const(lo, i32t).unwrap();
    let field = b
        .build_int_binary_operation(w, k, IntBinaryOp::ShiftRight, i32t)
        .unwrap();
    let wide = b
        .extend_if_needed(field, i64t, ExtendOp::SignExtend)
        .unwrap();
    let n = b.build_int_const(bound, i64t).unwrap();
    b.build_int_cmp_operation(wide, n, IntCmpOp::Less, i64t)
        .unwrap()
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
    assert_eq!((small(iv.lo()), small(iv.hi())), (0, 61));
}

/// Bits 27.. of `w` are not bits 26.. of it, and `sext64(w) >> 26` carries
/// sign copies above the six bits: neither is the guarded value, so each keeps
/// its own KnownBits bound.
#[test]
fn a_guard_on_other_bits_does_not_bound_the_index() {
    let iv = bit_field_guard_range(27, 30, ext_26_6);
    assert_eq!((small(iv.lo()), small(iv.hi())), (0, 63));
    let sext_shr_26 = |b: &mut FunctionBuilder, w: ValueId| {
        let i64t = ValueType::int(64);
        let wide = b.extend_if_needed(w, i64t, ExtendOp::SignExtend).unwrap();
        let k = b.build_int_const(26u64, i64t).unwrap();
        b.build_int_binary_operation(wide, k, IntBinaryOp::ShiftRight, i64t)
            .unwrap()
    };
    let iv = bit_field_guard_range(26, 62, sext_shr_26);
    assert_eq!((small(iv.lo()), small(iv.hi())), (0, (1 << 38) - 1));
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
    assert_eq!((small(iv.lo()), small(iv.hi())), (0, 1));
    let g = guarded(
        ValueType::int(32),
        |b, w| srl_sltiu(b, w, 26, 8),
        |b, w| field_of(b, w, 28, 3),
    );
    let iv = range_at(&g.f, g.ret, g.true_node);
    assert_eq!((small(iv.lo()), small(iv.hi())), (0, 1));
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
    let var = strider_ir_test_utils::reg_vn(0x1000, 4);
    let cvar = strider_ir_test_utils::reg_vn(0x2000, 4);
    let mut b = RegisterSet::new()
        .tracked(var)
        .tracked(cvar)
        .arg(cvar)
        .build_fn()
        .unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
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
    let ret = f
        .walk_kind(|k| matches!(k, NodeKind::Return))
        .next()
        .expect("a Return node");
    let returned = f.node_inputs(ret)[2];
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
        small(iv.lo()) == 0 && small(iv.hi()) >= 6 && small(iv.stride()) == 1,
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
        let mut map = RangeMap::new(f).unwrap();
        map.range_of(*value, *region);
        let kept: Vec<((ValueId, NodeId), Interval)> = map
            .memo
            .iter()
            .filter_map(|(&key, slot)| match slot {
                super::MemoSlot::Done(iv) => Some((key, iv.clone())),
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
    assert_eq!(parts(&iv), (0, 6, 1));
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
            by_node.insert(n, dense_iv(lo, hi));
            tally.off_tree += usize::from(!on_tree);
        }
    }
    let forest = super::GuardForest::new(&doms, &by_node);
    tally.most_entries = tally.most_entries.max(forest.entries.len());
    for (q, &qn) in nodes.iter().enumerate() {
        assert_eq!(doms.span(qn).is_some(), dom[entry as usize][q], "{qn:?}");
        let Some(span) = doms.span(qn) else {
            continue;
        };
        let dominating: Vec<Interval> = by_node
            .iter()
            .filter(|&(g, _)| dom[index[g] as usize][q])
            .map(|(_, iv)| iv.clone())
            .collect();
        let want = dominating.iter().cloned().reduce(Interval::intersect);
        let got = forest.meet_dominating(span);
        assert_eq!(
            got.map(|iv| (small(iv.lo()), small(iv.hi()))),
            want.as_ref().map(|iv| (small(iv.lo()), small(iv.hi()))),
            "at {qn:?}"
        );
        tally.queries += 1;
        tally.by_met[dominating.len().min(3)] += 1;
        tally.most_met = tally.most_met.max(dominating.len());
        tally.deep_nonempty += usize::from(
            dominating.len() >= 3 && want.as_ref().is_some_and(|iv| iv.count() > UBig::ZERO),
        );
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
    use strider_ir_test_utils::reg_vn;

    let s_vn = reg_vn(0x10, 4);
    let arg_vn = reg_vn(0x20, 4);
    let mut b = RegisterSet::new()
        .tracked(s_vn)
        .tracked(arg_vn)
        .arg(arg_vn)
        .build_fn()
        .unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
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
    let s = return_operand(&f);
    let token = f.graph().nth_input(f.producer(s), 0).unwrap();
    let at = f.graph().producer(token);
    (f, s, at)
}

/// The value the function's sole `Return` returns.
fn return_operand(f: &Function) -> ValueId {
    let ret = f
        .walk()
        .find(|&n| matches!(f.node_kind(n), NodeKind::Return))
        .expect("a return");
    f.value_inputs(ret).last().expect("a returned value")
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
    assert_eq!(parts(&iv), (0, 5, 1), "the range is the hull");
    assert_eq!(
        super::phi_constants(&f, s, 3),
        Some(vec![ub(0), ub(3), ub(5)])
    );
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
    assert_eq!(
        super::phi_constants(&f, phi_a, 16),
        Some(vec![ub(0), ub(1)])
    );
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
    use strider_ir_test_utils::reg_vn;

    let x_vn = reg_vn(0x10, 4);
    let mut b = RegisterSet::new()
        .tracked(x_vn)
        .arg(x_vn)
        .build_fn()
        .unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
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
    let ranges = RangeMap::new(&f).unwrap();
    let ruled: Vec<u128> = ranges.ruled_out(x, dispatch).iter().map(small).collect();
    assert_eq!(ruled, [0], "{}", f.to_text(false));
}

/// `0 == x` rules 0 out as `x == 0` does.
/// Mutation: take the comparison's second operand as the constant always.
#[test]
fn an_equality_guard_with_the_constant_first_rules_it_out() {
    let (f, x, dispatch) = not_zero_then_dispatch(false, true);
    let ranges = RangeMap::new(&f).unwrap();
    let ruled: Vec<u128> = ranges.ruled_out(x, dispatch).iter().map(small).collect();
    assert_eq!(ruled, [0], "{}", f.to_text(false));
}

/// Where the `x == 0` path joins the other first, `x` may be 0 again.
/// Mutation: accept a merge as the false edge's sink.
#[test]
fn an_equality_guard_rules_nothing_out_past_a_merge() {
    let (f, x, dispatch) = not_zero_then_dispatch(true, false);
    let ranges = RangeMap::new(&f).unwrap();
    assert!(
        ranges.ruled_out(x, dispatch).is_empty(),
        "{}",
        f.to_text(false)
    );
}

/// `a, b, c = 0, 1, 1; while (a < 16) a, b, c = b, c, a`: three header phis,
/// each the next one's back-edge arm, holding two constants.  The `a` phi.
fn rotate_loop() -> (Function, ValueId) {
    use strider_ir_test_utils::reg_vn;

    let vns = [reg_vn(0x10, 4), reg_vn(0x20, 4), reg_vn(0x30, 4)];
    let mut b = RegisterSet::new()
        .tracked(vns[0])
        .tracked(vns[1])
        .tracked(vns[2])
        .build_fn()
        .unwrap();
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
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
    let a = return_operand(&f);
    (f, a)
}

/// Three phis holding two constants: a cap of two is past the phi count,
/// three is not.  Mutation: drop the phi count from the cap.
#[test]
fn phi_constants_give_up_past_the_cap_on_phis() {
    let (f, a) = rotate_loop();
    assert_eq!(
        super::phi_constants(&f, a, 3),
        Some(vec![ub(0), ub(1)]),
        "{}",
        f.to_text(false)
    );
    assert_eq!(super::phi_constants(&f, a, 2), None);
}

/// Every integer width is its own bound, past 128 bits too. Only the two
/// facts that name no integer width -- a float, and (through `value_type_opt`)
/// an edge carrying no type -- fall back to the widest type's all-ones.
#[test]
fn a_type_bound_is_the_types_own_all_ones() {
    let unbounded = super::interval::unbounded();
    assert_eq!(
        super::interval::type_bound(None),
        unbounded,
        "no typed edge"
    );
    assert_eq!(
        super::interval::type_bound(Some(ValueType::float(64))),
        unbounded,
        "a float has no integer all-ones"
    );
    let wide = ValueType::int(256);
    assert_eq!(
        super::interval::type_bound(Some(wide)),
        strider_ir::eval::all_ones(wide),
        "past the carrier the width is still its own bound"
    );
    assert_ne!(
        super::interval::type_bound(Some(wide)),
        unbounded,
        "a tracked wide width is not the widest type's bound"
    );
    assert_eq!(
        super::interval::type_bound(Some(ValueType::int(32))),
        ub(0xFFFF_FFFF),
        "a narrow width is its own bound"
    );
}

/// A whole function whose only value is `wide`, at a width past the carrier.
fn over_carrier_fn() -> (Function, ValueId, NodeId) {
    let vn = strider_ir_test_utils::reg_vn(0x2000, 32);
    let (f, wide) = strider_ir_test_utils::make_fn_with_var(vn, |_, x| Ok(x)).unwrap();
    let entry = f.entry();
    (f, wide, entry)
}

/// `range_of` answers at the value's OWN width past 128 bits, not at the
/// widest type: `top(all_ones(i256))` is a claim about the value.
#[test]
fn a_range_query_past_the_carrier_is_bounded_by_its_own_width() {
    let (f, wide, region) = over_carrier_fn();
    let ty = ValueType::int(256);
    assert_eq!(f.value_type_opt(wide), Some(ty));
    let iv = range_at(&f, wide, region);
    assert_eq!(iv, Interval::top(&strider_ir::eval::all_ones(ty)));
    assert!(
        !iv.is_top(&super::interval::unbounded()),
        "an i256 top is not the widest type's top"
    );
    assert_eq!(
        super::interval::value_bound(&f, wide),
        strider_ir::eval::all_ones(ty)
    );
}

/// A guard and a scaling past the `u128` carrier track end to end: the guard
/// `x < 1000` at `I256` and the `x << 200` under it answer an exact interval,
/// where a 128-bit carrier could only answer top.
#[test]
fn a_wide_guard_and_scale_track_past_the_carrier() {
    let wide = ValueType::int(256);
    let g = guarded(
        wide,
        |b, x| {
            let c = b.build_int_const(1000u64, wide).unwrap();
            b.build_int_cmp_operation(x, c, IntCmpOp::Less, wide)
                .unwrap()
        },
        |b, x| {
            let k = b.build_int_const(200u64, wide).unwrap();
            b.build_int_binary_operation(x, k, IntBinaryOp::ShiftLeft, wide)
                .unwrap()
        },
    );
    assert_eq!(
        range_at(&g.f, g.idx, g.true_node),
        Interval::dense(UBig::ZERO, UBig::from(999u32)),
        "the guard bounds the i256 index"
    );
    let scaled = range_at(&g.f, g.ret, g.true_node);
    assert_eq!(
        scaled,
        Interval::new(
            UBig::ZERO,
            UBig::from(999u32) << 200usize,
            UBig::ONE << 200usize,
        ),
        "the scaling carries the bound past 128 bits"
    );
    assert!(!scaled.is_top(&strider_ir::eval::all_ones(wide)));
}

/// The sign-bit question reads the value's own width: a `u128` of known zeros
/// names no bit of an `i256`, while the bit at 255 is its sign bit.
#[test]
fn a_wide_sign_bit_is_read_at_its_own_width() {
    let (f, wide, _) = over_carrier_fn();
    let mut known = crate::analyze_known_bits(&f).unwrap();
    assert!(!super::is_sign_bit_known_zero(&f, wide, &known), "no facts");
    known[wide] = crate::KnownBitsFacts {
        ones: UBig::ZERO,
        zeros: UBig::from(u128::MAX),
    };
    assert!(
        !super::is_sign_bit_known_zero(&f, wide, &known),
        "a u128 of zeros names no bit of an i256"
    );
    known[wide] = crate::KnownBitsFacts {
        ones: UBig::ZERO,
        zeros: UBig::ONE << 255usize,
    };
    assert!(
        super::is_sign_bit_known_zero(&f, wide, &known),
        "bit 255 is the i256 sign bit"
    );
}

/// `UBig::trailing_ones` indexes past its own word buffer on an all-ones value
/// of 192 bits or more, which a whole `i256`'s known-zero set is.
#[test]
fn trailing_ones_counts_an_all_ones_wide_value() {
    for bits in [1usize, 64, 128, 192, 256, 512] {
        let all = (UBig::ONE << bits) - UBig::ONE;
        assert_eq!(super::interval::trailing_ones(&all), bits, "{bits} bits");
    }
    assert_eq!(super::interval::trailing_ones(&UBig::ZERO), 0);
    assert_eq!(super::interval::trailing_ones(&UBig::from(0b1011u32)), 2);
}
