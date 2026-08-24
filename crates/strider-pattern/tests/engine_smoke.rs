#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

use strider_ir::node::ValueType;
use strider_ir::{IRBuilderExt, IntBinaryOp};
use strider_ir_test_utils::make_empty_fn;
use strider_pattern::matcher::{KindSpec, MatcherBuilder};
use strider_pattern::{MatchPat, Matcher, int_const};

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

    let m = Matcher::new(&f);
    assert_eq!(m.find_all(&pat).unwrap().len(), 1);
}

#[test]
fn multi_sink_pattern_is_buildable_but_match_returns_err() {
    // A multi-sink pattern is a valid graph, so the seal accepts it. The
    // matcher cannot match one, and must say so with an Err rather than
    // panicking or silently matching nothing.
    let f = make_empty_fn(|b| {
        let x = b.build_int_const(5u64, ValueType::I64)?;
        let k = b.build_int_const(1u64, ValueType::I64)?;
        b.build_int_binary_operation(x, k, IntBinaryOp::Add, ValueType::I64)
    })
    .unwrap();

    let mut mb = MatcherBuilder::new();
    // Two leaves with unconsumed value outputs, so two sinks.
    let _a = mb.leaf(KindSpec::Any);
    let _b = mb.leaf(KindSpec::Any);
    let pat = mb.finish();

    let m = Matcher::new(&f);
    assert!(m.find_all(&pat).is_err());
}

#[test]
fn commutative_swap_matches_add_const_other_order() {
    // IR `Add(IntConst(1), IntConst(5))` against pattern `int_add(anything, const(5))`.
    let f = make_empty_fn(|b| {
        let k = b.build_int_const(1u64, ValueType::I64)?;
        let x = b.build_int_const(5u64, ValueType::I64)?;
        b.build_int_binary_operation(k, x, IntBinaryOp::Add, ValueType::I64)
    })
    .unwrap();

    let mut mb = MatcherBuilder::new();
    let any = mb.leaf(KindSpec::Any);
    let konst = int_const(5u64).compile(&mut mb);
    let _sum = mb.binary(IntBinaryOp::Add, any, konst);
    let pat = mb.finish();

    let m = Matcher::new(&f);
    assert_eq!(m.find_all(&pat).unwrap().len(), 1);
}

#[test]
fn force_ordered_disables_commutative_swap() {
    // `int_add(const(5), const(1))` against IR `Add(const(1), const(5))` would only
    // match via a swap, which force_ordered disables.
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

    let m = Matcher::new(&f);
    assert_eq!(m.find_all(&pat).unwrap().len(), 0);
}

#[test]
fn cast_walk_through_matches_under_extend() {
    // IR is `Add(ZeroExtend(var:I32):I64, const:I64)`; the operand is a var read
    // rather than a const because a const would fold instead of producing an
    // `Extend`. Pinning the `anything` leaf to the var's I32 width makes the cast
    // load-bearing: without walk-through the matcher sees the I64 ZeroExtend
    // output, with EXTEND walk-through it unwraps to the I32 inner value.
    use strider_ir::ExtendOp;
    use strider_ir_test_utils::reg_vn;

    let var = reg_vn(0x10, 4);
    let (f, _x) = strider_ir_test_utils::make_fn_with_var(var, |b, x| {
        let widened = b.extend_if_needed(x, ValueType::I64, ExtendOp::ZeroExtend)?;
        let k = b.build_int_const(1u64, ValueType::I64)?;
        b.build_int_binary_operation(widened, k, IntBinaryOp::Add, ValueType::I64)
    })
    .unwrap();

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

    let m = Matcher::new(&f);
    assert_eq!(m.find_all(&build_pat(false)).unwrap().len(), 0);
    assert_eq!(m.find_all(&build_pat(true)).unwrap().len(), 1);
}

#[test]
fn multi_sink_pattern_root_errors_with_sink_count() {
    // The seal accepts a disconnected pattern; `root()` is where it is caught.
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
    // A cycle is expressible through the raw MatcherBuilder but never reaches
    // `Pattern::root()`: the staged-graph seal panics at `finish()`. The typed
    // `MatchPat` builders cannot express a cycle at all (operands are consumed
    // by value), so this is builder-bug territory, pinned as a panic not an Err.
    let mut mb = MatcherBuilder::new();
    let n = mb.node(KindSpec::Any);
    let out = mb.value_output(n, 0);
    mb.input(n, 0, out);
    let _ = mb.finish();
}

#[test]
fn multi_output_root_node_still_resolves_root() {
    // One root node with two unconsumed outputs is still single-SINK.
    let mut mb = MatcherBuilder::new();
    let n = mb.node(KindSpec::Any);
    let _o0 = mb.value_output(n, 0);
    let _o1 = mb.value_output(n, 1);
    let pat = mb.finish();

    assert!(pat.root().is_ok(), "one sink node, two outputs: valid root");
}
