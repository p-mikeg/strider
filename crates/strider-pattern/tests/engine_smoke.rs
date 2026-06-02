//! Integration smoke tests for the bipartite match engine, driven
//! directly through [`MatcherBuilder`] (no typed structs / templates —
//! those land in later changes).

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

use strider_ir::node::ValueType;
use strider_ir::{IntBinaryOp, node::NodeKind};
use strider_ir_test_utils::make_empty_fn;
use strider_pattern::pattern::KindSpec;
use strider_pattern::{Matcher, builder::MatcherBuilder};

#[test]
fn matches_add_const_via_builder() {
    let f = make_empty_fn(|b| {
        let x = b.build_int_const(5u64, ValueType::I64)?;
        let k = b.build_int_const(1u64, ValueType::I64)?;
        b.build_int_binary_operation(x, k, IntBinaryOp::Add, ValueType::I64)
    })
    .unwrap();

    let mut mb = MatcherBuilder::new();
    let x = mb.leaf(KindSpec::Any);
    let k = mb.leaf(KindSpec::Any);
    let sum = mb.binary(IntBinaryOp::Add, x, k);
    let pat = mb.finish(sum);

    let m = Matcher::try_new(&f).unwrap();
    assert_eq!(m.find_all(&pat).len(), 1);
}

#[test]
fn commutative_swap_matches_add_const_other_order() {
    // IR is `Add(const, var)` — pattern is `add(any, const)`.  Must
    // match via commutative operand swap.
    let f = make_empty_fn(|b| {
        let k = b.build_int_const(1u64, ValueType::I64)?;
        let x = b.build_int_const(5u64, ValueType::I64)?;
        // const is slot 0, var is slot 1
        b.build_int_binary_operation(k, x, IntBinaryOp::Add, ValueType::I64)
    })
    .unwrap();

    let mut mb = MatcherBuilder::new();
    let any = mb.leaf(KindSpec::Any);
    let konst = mb.leaf(KindSpec::Exact(NodeKind::IntConst(5)));
    let sum = mb.binary(IntBinaryOp::Add, any, konst);
    let pat = mb.finish(sum);

    let m = Matcher::try_new(&f).unwrap();
    assert_eq!(m.find_all(&pat).len(), 1);
}

#[test]
fn force_ordered_disables_commutative_swap() {
    // IR is `Add(const(1), const(5))`.  Ordered pattern `add(const(5),
    // const(1))` must NOT match (it would only match via a swap, which
    // force_ordered disables).
    let f = make_empty_fn(|b| {
        let a = b.build_int_const(1u64, ValueType::I64)?;
        let c = b.build_int_const(5u64, ValueType::I64)?;
        b.build_int_binary_operation(a, c, IntBinaryOp::Add, ValueType::I64)
    })
    .unwrap();

    let mut mb = MatcherBuilder::new();
    let five = mb.leaf(KindSpec::Exact(NodeKind::IntConst(5)));
    let one = mb.leaf(KindSpec::Exact(NodeKind::IntConst(1)));
    let sum = mb.binary(IntBinaryOp::Add, five, one);
    mb.set_force_ordered(sum);
    let pat = mb.finish(sum);

    let m = Matcher::try_new(&f).unwrap();
    assert_eq!(m.find_all(&pat).len(), 0);
}

#[test]
fn cast_walk_through_matches_under_extend() {
    // IR: `Add(ZeroExtend(var:I32):I64, const:I64)`.  The left operand
    // is a non-const var read wrapped in a ZeroExtend cast (a const
    // would fold instead of producing an `Extend` node).  Pattern wants
    // `add(any:I32-via-cast, const)` — modelled as `add(any, const)`
    // where the `any` leaf must reach the var *through* the cast.  To
    // make the cast load-bearing, the `any` leaf is pinned to the
    // varnode read's I32 width: without walk-through the matcher sees
    // the I64 ZeroExtend output (mismatch), with EXTEND walk-through it
    // unwraps to the I32 inner value.
    use strider_ir::ExtendOp;
    use strider_ir_test_utils::reg_vn;

    let var = reg_vn(0x10, 4); // I32-sized register var
    let (f, _x) = strider_ir_test_utils::make_fn_with_var(var, |b, x| {
        let widened = b.extend_if_needed(x, ValueType::I64, ExtendOp::ZeroExtend)?;
        let k = b.build_int_const(1u64, ValueType::I64)?;
        b.build_int_binary_operation(widened, k, IntBinaryOp::Add, ValueType::I64)
    })
    .unwrap();

    // Left leaf pinned to I32 width; right leaf is the I64 const.
    let build_pat = |mask: bool| {
        let mut mb = MatcherBuilder::new();
        let l = mb.leaf(KindSpec::Any);
        mb.set_value_width(l, 32);
        let r = mb.leaf(KindSpec::Exact(NodeKind::IntConst(1)));
        let sum = mb.binary(IntBinaryOp::Add, l, r);
        let p = mb.finish(sum);
        if mask {
            p.ignore_casts_mask(strider_pattern::matcher::CastMask::EXTEND)
        } else {
            p
        }
    };

    let m = Matcher::try_new(&f).unwrap();
    assert_eq!(m.find_all(&build_pat(false)).len(), 0);
    assert_eq!(m.find_all(&build_pat(true)).len(), 1);
}
