use strider_ir::node::{NodeId, NodeKind, ValueType as T, ValueType};
use strider_ir::{
    EditFunction, FloatCmpOp, Function, IRBuilderExt, IRViewer, IntBinaryOp, IntCmpOp,
};
use strider_ir_test_utils::make_empty_fn;

use crate::matcher::Pattern;
use crate::{Bindings, Capture, MatchPat, Matcher, Template, TemplatePat, var};

/// The one match of `lhs`: its root node, bindings, and root value type.
#[track_caller]
fn match_once(fx: &Function, lhs: &Pattern) -> (NodeId, Bindings, ValueType) {
    let hits = Matcher::new(fx).find_all(lhs).unwrap();
    assert_eq!(hits.len(), 1, "the LHS must match exactly once");
    let root = hits[0].root();
    let [root_value] = fx.node_outputs_exact::<1>(root).unwrap();
    let root_ty = fx.value_kind(root_value).as_value().unwrap();
    (root, hits[0].bindings().clone(), root_ty)
}

/// Instantiates `rhs` over the one match of `lhs` and splices it in, so
/// `validate` sees the fresh nodes.
#[track_caller]
fn rewrite(fx: &mut Function, lhs: &Pattern, rhs: &Template) -> anyhow::Result<NodeId> {
    let (root, bindings, root_ty) = match_once(fx, lhs);
    let new_value = {
        let mut ef = EditFunction::new(fx);
        super::instantiate(rhs, &mut ef, &bindings, root, &[root], root_ty)?
    };
    let [old_value] = fx.node_outputs_exact::<1>(root).unwrap();
    EditFunction::new(fx).replace_value(old_value, new_value);
    strider_ir::validate::validate(fx).expect("a rewritten function must be valid IR");
    Ok(fx.producer(new_value))
}

/// The type of the one constant operand `node` consumes.
#[track_caller]
fn const_operand_ty(fx: &Function, node: NodeId) -> ValueType {
    fx.node_inputs(node)
        .into_iter()
        .find(|&v| fx.node_kind(fx.producer(v)).is_const())
        .map(|v| fx.value_type(v).unwrap())
        .expect("the RHS mints one constant operand")
}

/// `Equal(5:I64, 1:I64):I1` returned; `int_eq(var(x), int_const(1))` binds
/// `x` to the `I64` `5`.
fn cmp_over_i64() -> Function {
    make_empty_fn(|b| {
        let a = b.build_int_const(5u64, T::int(64))?;
        let k = b.build_int_const(1u64, T::int(64))?;
        b.build_int_cmp_operation(a, k, IntCmpOp::Equal, T::int(64))
    })
    .unwrap()
}

/// The reported shape: a fresh constant at a comparison's operand 0, whose
/// width is the operand's and not the `I1` the root carries.
#[test]
fn a_fresh_const_at_a_comparison_operand_takes_the_operand_width() {
    let x = Capture::new();
    let mut fx = cmp_over_i64();
    let lhs = crate::int_eq(var(x), crate::int_const(1u128)).into_pattern();
    let rhs = super::int_sborrow(crate::int_const(7u128), var(x)).into_template();

    let node = rewrite(&mut fx, &lhs, &rhs).unwrap();
    assert_eq!(const_operand_ty(&fx, node), T::int(64));
}

/// The same at operand 1, where the width comes from the operand the
/// comparison heads with.
#[test]
fn a_fresh_const_at_the_second_comparison_operand_takes_the_first_ones_width() {
    let x = Capture::new();
    let mut fx = cmp_over_i64();
    let lhs = crate::int_eq(var(x), crate::int_const(1u128)).into_pattern();
    let rhs = super::int_lt(var(x), crate::int_const(7u128)).into_template();

    let node = rewrite(&mut fx, &lhs, &rhs).unwrap();
    assert_eq!(const_operand_ty(&fx, node), T::int(64));
}

/// A float comparison is the same shape: the fresh `FloatConst` is `F64`,
/// which is also what makes its bits mask at the right width.
#[test]
fn a_fresh_float_const_at_a_comparison_operand_takes_the_operand_width() {
    let one = f64::to_bits(1.0);
    let two = f64::to_bits(2.0);
    let f = Capture::new();
    let mut fx = make_empty_fn(|b| {
        let a = b.build_float_const(one, T::float(64));
        let c = b.build_float_const(two, T::float(64));
        b.build_float_cmp_op(a, c, FloatCmpOp::Equal)
    })
    .unwrap();
    let lhs = crate::float_eq(var(f), crate::float_const(two)).into_pattern();
    let rhs = super::float_lt(var(f), crate::float_const(two)).into_template();

    let node = rewrite(&mut fx, &lhs, &rhs).unwrap();
    assert_eq!(const_operand_ty(&fx, node), T::float(64));
}

/// `Extend` pins its input APART from its output, so the root's width is
/// the one width the operand cannot have. Nothing else offers one.
#[test]
fn a_fresh_const_under_an_extend_is_refused() {
    let x = Capture::new();
    let mut fx = cmp_over_i64();
    let lhs = crate::int_eq(var(x), crate::int_const(1u128)).into_pattern();
    // `var(x)` types the comparison, so the extend's operand is the one
    // unanchored node left.
    let rhs =
        super::int_eq(super::int_zero_extend(crate::int_const(7u128)), var(x)).into_template();

    let err = rewrite(&mut fx, &lhs, &rhs).unwrap_err().to_string();
    assert!(err.contains("Extend"), "got: {err}");
    assert!(err.contains("has no width"), "got: {err}");
    assert!(err.contains("capture_typed"), "got: {err}");
}

/// Two fresh constants compared against each other anchor nothing, so the
/// width would be a guess.
#[test]
fn a_comparison_of_two_fresh_consts_is_refused() {
    let x = Capture::new();
    let mut fx = cmp_over_i64();
    let lhs = crate::int_eq(var(x), crate::int_const(1u128)).into_pattern();
    let rhs = super::int_eq(crate::int_const(1u128), crate::int_const(2u128)).into_template();

    let err = rewrite(&mut fx, &lhs, &rhs).unwrap_err().to_string();
    assert!(err.contains("has no width"), "got: {err}");
}

/// An arithmetic root is evaluated at its own output width, so a fresh
/// operand there still inherits the rewrite root's type.
#[test]
fn a_fresh_const_under_an_arithmetic_root_still_inherits_the_root() {
    let x = Capture::new();
    let mut fx = make_empty_fn(|b| {
        let a = b.build_int_const(5u64, T::int(64))?;
        let k = b.build_int_const(1u64, T::int(64))?;
        b.build_int_binary_operation(a, k, IntBinaryOp::Add, T::int(64))
    })
    .unwrap();
    let lhs = crate::int_add(var(x), crate::int_const(1u128)).into_pattern();
    let rhs = super::int_add(var(x), crate::int_const(2u128)).into_template();

    let node = rewrite(&mut fx, &lhs, &rhs).unwrap();
    assert_eq!(const_operand_ty(&fx, node), T::int(64));
    assert!(matches!(
        fx.node_kind(node),
        NodeKind::IntBinaryOp(IntBinaryOp::Add)
    ));
}

/// `Sless(5:I32, 0:I32):I1`; `int_slt(var(a), int_const(0))` binds `a` to
/// the `I32` `5`.
fn sign_test_over_i32() -> Function {
    make_empty_fn(|b| {
        let a = b.build_int_const(5u64, T::int(32))?;
        let z = b.build_int_const(0u64, T::int(32))?;
        b.build_int_cmp_operation(a, z, IntCmpOp::Sless, T::int(32))
    })
    .unwrap()
}

/// The type and value of the count of the one shift at or below `node`.
#[track_caller]
fn shift_count(fx: &Function, node: NodeId) -> (ValueType, u128) {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if let NodeKind::IntBinaryOp(op) = fx.node_kind(n)
            && op.is_shift()
        {
            let count = fx.node_inputs(n)[1];
            return (
                fx.value_type(count).unwrap(),
                fx.int_const_u128(count).unwrap(),
            );
        }
        stack.extend(fx.node_inputs(n).into_iter().map(|v| fx.producer(v)));
    }
    panic!("the RHS builds a shift");
}

/// A shift count is evaluated at the shift's width, not at the `I1` the
/// root carries: masked to `I1`, 31 would test bit 1.
#[test]
fn a_fresh_shift_count_under_an_i1_root_keeps_its_value() {
    let a = Capture::new();
    let mut fx = sign_test_over_i32();
    let lhs = crate::int_slt(var(a), crate::int_const(0u128)).into_pattern();
    let rhs = super::int_truncate(super::int_shr(var(a), crate::int_const(31u128))).into_template();

    let node = rewrite(&mut fx, &lhs, &rhs).unwrap();
    assert_eq!(shift_count(&fx, node), (T::int(32), 31));
}

/// The same count inside a bit test, where the shift is two operands
/// below the `I1` root.
#[test]
fn a_fresh_shift_count_in_a_bit_test_keeps_its_value() {
    let a = Capture::new();
    let mut fx = sign_test_over_i32();
    let lhs = crate::int_slt(var(a), crate::int_const(0u128)).into_pattern();
    let rhs = super::int_eq(
        super::int_and(
            super::int_shr(var(a), crate::int_const(5u128)),
            crate::int_const(1u128),
        ),
        crate::int_const(0u128),
    )
    .into_template();

    let node = rewrite(&mut fx, &lhs, &rhs).unwrap();
    assert_eq!(shift_count(&fx, node), (T::int(32), 5));
}

/// `shl` and `sshr` count the same way as `shr`.
#[test]
fn a_fresh_shift_count_keeps_its_value_under_every_shift() {
    let a = Capture::new();
    let lhs = crate::int_slt(var(a), crate::int_const(0u128)).into_pattern();
    for rhs in [
        super::int_truncate(super::int_shl(var(a), crate::int_const(31u128))).into_template(),
        super::int_truncate(super::int_sshr(var(a), crate::int_const(31u128))).into_template(),
    ] {
        let mut fx = sign_test_over_i32();
        let node = rewrite(&mut fx, &lhs, &rhs).unwrap();
        assert_eq!(shift_count(&fx, node), (T::int(32), 31));
    }
}

/// A count that is itself a shift: the outer shift types the inner one,
/// whose own count then takes that width.
#[test]
fn a_count_built_from_a_shift_takes_the_outer_shifts_width() {
    let a = Capture::new();
    let mut fx = sign_test_over_i32();
    let lhs = crate::int_slt(var(a), crate::int_const(0u128)).into_pattern();
    let rhs = super::int_truncate(super::int_shr(
        var(a),
        super::int_shl(crate::int_const(1u128), crate::int_const(2u128)),
    ))
    .into_template();

    let node = rewrite(&mut fx, &lhs, &rhs).unwrap();
    let shr = fx.producer(fx.node_inputs(node)[0]);
    let shl_out = fx.node_inputs(shr)[1];
    assert_eq!(fx.value_type(shl_out).unwrap(), T::int(32));
    let shl = fx.producer(shl_out);
    assert_eq!(shift_count(&fx, shl), (T::int(32), 2));
}

/// A count that does not fit the shift's width saturates, as p-code tests
/// the whole count: masked, `2^32 + 3` would shift an `I32` by 3.
#[test]
fn a_fresh_shift_count_too_wide_for_the_shift_saturates() {
    let a = Capture::new();
    let mut fx = sign_test_over_i32();
    let lhs = crate::int_slt(var(a), crate::int_const(0u128)).into_pattern();
    let rhs = super::int_truncate(super::int_shr(var(a), crate::int_const(0x1_0000_0003u128)))
        .into_template();

    let node = rewrite(&mut fx, &lhs, &rhs).unwrap();
    assert_eq!(shift_count(&fx, node), (T::int(32), 3 | 32));
}

/// `Sless(5, 0):I1` at `width`, as [`sign_test_over_i32`].
fn sign_test_over(width: u16) -> Function {
    make_empty_fn(|b| {
        let a = b.build_int_const(5u64, T::int(width))?;
        let z = b.build_int_const(0u64, T::int(width))?;
        b.build_int_cmp_operation(a, z, IntCmpOp::Sless, T::int(width))
    })
    .unwrap()
}

/// A count the shift's width holds is kept as written, the width itself
/// and past it included; one it does not hold saturates, `2^w` and a
/// non-power-of-two width included, under every shift.
/// Mutation: saturate every count at or past the width.
#[test]
fn a_fresh_shift_count_saturates_only_where_the_width_cannot_hold_it() {
    type Shift = fn(Capture, u128) -> Template;
    let a = Capture::new();
    let shr: Shift =
        |a, c| super::int_truncate(super::int_shr(var(a), crate::int_const(c))).into_template();
    let shl: Shift =
        |a, c| super::int_truncate(super::int_shl(var(a), crate::int_const(c))).into_template();
    let sshr: Shift =
        |a, c| super::int_truncate(super::int_sshr(var(a), crate::int_const(c))).into_template();
    let cases = [
        (shr, 32, 32, 32),
        (shr, 32, 64, 64),
        (shr, 32, 1 << 32, 32),
        (shr, 24, (1 << 24) + 3, 3 | 24),
        (shl, 32, 0x1_0000_0003, 3 | 32),
        (sshr, 32, 0x1_0000_0003, 3 | 32),
    ];
    for (shift, width, count, want) in cases {
        let lhs = crate::int_slt(var(a), crate::int_const(0u128)).into_pattern();
        let rhs = shift(a, count);
        let mut fx = sign_test_over(width);
        let node = rewrite(&mut fx, &lhs, &rhs).unwrap();
        assert_eq!(
            shift_count(&fx, node),
            (T::int(width), want),
            "{count:#x} at i{width}"
        );
    }
}

/// `capture_typed` over a capture is refused, not a panic: a capture keeps
/// the type of the value it bound.
#[test]
fn capture_typed_over_a_capture_is_refused() {
    let x = Capture::new();
    let mut fx = cmp_over_i64();
    let lhs = crate::int_eq(var(x), crate::int_const(1u128)).into_pattern();
    let rhs =
        super::int_eq(crate::capture_typed(x, var(x)), crate::int_const(1u128)).into_template();

    let err = rewrite(&mut fx, &lhs, &rhs).unwrap_err().to_string();
    assert!(err.contains("targets a capture"), "got: {err}");
}

/// An output no template node consumes has nothing to take a width from.
#[test]
fn an_untyped_output_no_node_consumes_is_refused() {
    let x = Capture::new();
    let mut fx = make_empty_fn(|b| {
        let a = b.build_int_const(5u64, T::int(64))?;
        let k = b.build_int_const(1u64, T::int(64))?;
        b.build_int_binary_operation(a, k, IntBinaryOp::Add, T::int(64))
    })
    .unwrap();
    let lhs = crate::int_add(var(x), crate::int_const(1u128)).into_pattern();
    let mut b = super::TemplateBuilder::new();
    let l = b.capture(x);
    let r = b.dynamic_leaf(super::TemplateKind::FnIntConst(Box::new(|_| {
        Ok(strider_ir::UBig::from(2u8))
    })));
    let add = b.node(NodeKind::IntBinaryOp(IntBinaryOp::Add));
    b.input(add, 0, l);
    b.input(add, 1, r);
    let _consumed = b.value_output(add, 0);
    let _unconsumed = b.value_output(add, 0);
    let rhs = b.finish();

    let err = rewrite(&mut fx, &lhs, &rhs).unwrap_err().to_string();
    assert!(err.contains("no template node consumes it"), "got: {err}");
}

/// A node that declares no output has no width of its own, and the root's
/// would be a guess.
/// Mutation: give such a node one output of the root's type again.
#[test]
fn a_node_declaring_no_output_is_refused() {
    let x = Capture::new();
    let mut fx = make_empty_fn(|b| {
        let a = b.build_int_const(5u64, T::int(64))?;
        let k = b.build_int_const(1u64, T::int(64))?;
        b.build_int_binary_operation(a, k, IntBinaryOp::Add, T::int(64))
    })
    .unwrap();
    let lhs = crate::int_add(var(x), crate::int_const(1u128)).into_pattern();
    let mut b = super::TemplateBuilder::new();
    let l = b.capture(x);
    let r = b.capture(x);
    let add = b.node(NodeKind::IntBinaryOp(IntBinaryOp::Add));
    b.input(add, 0, l);
    b.input(add, 1, r);
    let rhs = b.finish();

    let err = rewrite(&mut fx, &lhs, &rhs).unwrap_err().to_string();
    assert!(err.contains("declares no output"), "got: {err}");
}

/// A width taken from a capture bound to memory has no width to take.
#[test]
fn a_width_from_a_capture_bound_to_memory_is_refused() {
    let m = Capture::new();
    let mut fx = make_empty_fn(|b| {
        let a = b.build_int_const(0x10u64, T::int(64))?;
        b.build_load(a, rsleigh::VnSpace::RAM, T::int(32))
    })
    .unwrap();
    let lhs = crate::load().mem(var(m)).build();
    let rhs = super::int_add(
        crate::capture_typed(m, crate::int_const(1u128)),
        crate::int_const(2u128),
    )
    .into_template();

    let err = rewrite(&mut fx, &lhs, &rhs).unwrap_err().to_string();
    assert!(err.contains("bound no typed value"), "got: {err}");
}

/// A `Popcount` operand owes its output no width, so a fresh constant
/// there has none to take.
#[test]
fn a_fresh_popcount_operand_is_refused() {
    let x = Capture::new();
    let mut fx = cmp_over_i64();
    let lhs = crate::int_eq(var(x), crate::int_const(1u128)).into_pattern();
    let rhs =
        super::int_eq(var(x), super::int_popcount(crate::int_const(0xffu128))).into_template();

    let err = rewrite(&mut fx, &lhs, &rhs).unwrap_err().to_string();
    assert!(err.contains("Popcount input 0"), "got: {err}");
    assert!(err.contains("has no width"), "got: {err}");
    assert!(err.contains("capture_typed"), "got: {err}");
}

/// Typed through a capture, the same operand builds at that width.
#[test]
fn a_capture_typed_popcount_operand_takes_the_capture_width() {
    let x = Capture::new();
    let mut fx = cmp_over_i64();
    let lhs = crate::int_eq(var(x), crate::int_const(1u128)).into_pattern();
    let rhs = super::int_eq(
        var(x),
        super::int_popcount(crate::capture_typed(x, crate::int_const(0xffu128))),
    )
    .into_template();

    let node = rewrite(&mut fx, &lhs, &rhs).unwrap();
    let popcount = fx.producer(fx.node_inputs(node)[1]);
    let operand = fx.node_inputs(popcount)[0];
    assert_eq!(fx.value_type(operand).unwrap(), T::int(64));
    assert_eq!(fx.int_const_u128(operand), Some(0xff));
}
