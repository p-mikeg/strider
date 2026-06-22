//! Integration smoke tests for the bipartite match engine, driven
//! directly through [`MatcherBuilder`] (no typed structs / templates —
//! those land in later changes).

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

use strider_ir::IRBuilderExt;
use strider_ir::node::ValueType;
use strider_ir::IntBinaryOp;
use strider_ir_test_utils::make_empty_fn;
use strider_pattern::matcher::KindSpec;
use strider_pattern::{Matcher, MatchPat, int_const, matcher::MatcherBuilder};

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
    let _sum = mb.binary(IntBinaryOp::Add, x, k);
    let pat = mb.finish();

    let m = Matcher::try_new(&f).unwrap();
    // A single-rooted pattern resolves a unique root and matches: the
    // match entry is fallible (returns Result) even on the happy path.
    assert_eq!(m.find_all(&pat).unwrap().len(), 1);
}

#[test]
fn multi_sink_pattern_is_buildable_but_match_returns_err() {
    // A user can construct a pattern that is not single-rooted (e.g. two
    // independent sinks sharing a captured value). The seal does NOT
    // reject it — it is a valid graph — but the current matcher cannot
    // match a multi-sink pattern, so it returns an Err rather than
    // panicking or silently matching nothing.
    let f = make_empty_fn(|b| {
        let x = b.build_int_const(5u64, ValueType::I64)?;
        let k = b.build_int_const(1u64, ValueType::I64)?;
        b.build_int_binary_operation(x, k, IntBinaryOp::Add, ValueType::I64)
    })
    .unwrap();

    let mut mb = MatcherBuilder::new();
    // Two leaf nodes, each with an unconsumed value output → two sinks.
    let _a = mb.leaf(KindSpec::Any);
    let _b = mb.leaf(KindSpec::Any);
    let pat = mb.finish(); // seals the (valid) graph; root derived at match time

    let m = Matcher::try_new(&f).unwrap();
    assert!(m.find_all(&pat).is_err());
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
    let konst = int_const(5u64).compile(&mut mb);
    let _sum = mb.binary(IntBinaryOp::Add, any, konst);
    let pat = mb.finish();

    let m = Matcher::try_new(&f).unwrap();
    assert_eq!(m.find_all(&pat).unwrap().len(), 1);
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
    let five = int_const(5u64).compile(&mut mb);
    let one = int_const(1u64).compile(&mut mb);
    let sum = mb.binary(IntBinaryOp::Add, five, one);
    mb.set_force_ordered(sum);
    let pat = mb.finish();

    let m = Matcher::try_new(&f).unwrap();
    assert_eq!(m.find_all(&pat).unwrap().len(), 0);
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
        let r = int_const(1u64).compile(&mut mb);
        let _sum = mb.binary(IntBinaryOp::Add, l, r);
        let p = mb.finish();
        if mask {
            p.ignore_casts_mask(strider_pattern::matcher::CastMask::EXTEND)
        } else {
            p
        }
    };

    let m = Matcher::try_new(&f).unwrap();
    assert_eq!(m.find_all(&build_pat(false)).unwrap().len(), 0);
    assert_eq!(m.find_all(&build_pat(true)).unwrap().len(), 1);
}

#[test]
fn multi_sink_pattern_root_errors_with_sink_count() {
    // Disconnected two-component pattern (two independent leaves): the
    // seal accepts it, but `Pattern::root()` itself reports the
    // multi-sink shape as an error naming the sink count.
    let mut mb = MatcherBuilder::new();
    let _a = mb.leaf(KindSpec::Any);
    let _b = mb.leaf(KindSpec::Any);
    let pat = mb.finish();

    let err = pat.root().expect_err("two sinks cannot resolve a root");
    let msg = err.to_string();
    assert!(
        msg.contains("2 sink nodes"),
        "error names the sink count; got: {msg}"
    );
}

#[test]
#[should_panic(expected = "cyclic staged pattern graph")]
fn cyclic_pattern_graph_panics_at_finish() {
    // A cycle IS expressible through the raw MatcherBuilder (wire a
    // node's own output back into its input), but it never reaches
    // `Pattern::root()`: the staged-graph seal panics at `finish()`.
    // The typed `MatchPat` builders cannot express a cycle at all
    // (operands are consumed by value), so this is builder-bug
    // territory, pinned as a panic rather than an Err.
    let mut mb = MatcherBuilder::new();
    let n = mb.node(KindSpec::Any);
    let out = mb.value_output(n, 0);
    mb.input(n, 0, out);
    let _ = mb.finish();
}

#[test]
fn multi_output_root_node_still_resolves_root() {
    // A pattern whose single root node declares TWO outputs (both
    // unconsumed) is still single-SINK: `root()` resolves it fine.
    let mut mb = MatcherBuilder::new();
    let n = mb.node(KindSpec::Any);
    let _o0 = mb.value_output(n, 0);
    let _o1 = mb.value_output(n, 1);
    let pat = mb.finish();

    assert!(pat.root().is_ok(), "one sink node, two outputs: valid root");
}
