use super::FlagCmpCanonicalize;
use crate::Result;
use crate::test_support::{bool_not, is_bool_not, run_pass, run_passes};
use strider_ir::{IRBuilderExt, IRViewer, UBig};

use strider_ir::node::{NodeId, NodeKind, ValueId, ValueType};
use strider_ir::{FunctionBuilder, Graph, IRWalker, IntBinaryOp, IntCmpOp, IntUnaryOp};
use strider_ir_test_utils::{IrWalkerEx, RegisterSet};

/// `if cond { return x } else { return x }` over `x`, a `ty` load from a
/// constant address, with `cond` built from `x` by `make_cond`.
fn build_if_over_load<T>(
    ty: ValueType,
    make_cond: impl FnOnce(&mut FunctionBuilder, ValueId) -> Result<(ValueId, T)>,
) -> Result<(strider_ir::Function, ValueId, T)> {
    let mut b = RegisterSet::new().build_fn()?;
    let entry = b.create_region_all()?;
    let dispatch = b.create_region_all()?;
    let exit = b.create_region_all()?;
    b.set_entry_region_all(entry)?;
    b.set_region(entry);
    let dummy = b.build_int_const(0xF00Du64, ValueType::int(64))?;
    let x = b.build_load(dummy, rsleigh::VnSpace::RAM, ty)?;
    let (cond, extra) = make_cond(&mut b, x)?;
    b.build_if(cond, dispatch, exit)?;
    b.set_region(dispatch);
    b.build_return(Some(x), &[])?;
    b.set_region(exit);
    b.build_return(Some(x), &[])?;
    Ok((b.build()?, x, extra))
}

/// `ZeroExtend(cmp) << pos` at `I32`.
fn cr_term(b: &mut FunctionBuilder, cmp: ValueId, pos: u64) -> Result<ValueId> {
    let ty = ValueType::int(32);
    let z = b.extend_if_needed(cmp, ty, strider_ir::node::ExtendOp::ZeroExtend)?;
    b.build_const_binop(pos, z, IntBinaryOp::ShiftLeft, ty)
}
/// PowerPC `cmpwi idx, 8` packs LT/GT/EQ/SO into a CR field and a branch
/// tests one bit: `Truncate(ShiftRight(cr, k)):I1`, or from a `CBRANCH`
/// `getCrBit`'s `(cr >> k) & 1` lowered to `Xor(Equal(bit, 0), 1):I1`, since
/// p-code branches on `cond != 0`. Either is that bit's comparison: bit 3 (LT)
/// `Less(idx, 8)`, bit 2 (GT) `Less(8, idx)`.
#[test]
fn ppc_cr_bit_test_canonicalizes_to_intcmp() -> Result<()> {
    use strider_ir::node::ExtendOp;
    let ty = ValueType::int(32);
    for (k, ne_zero) in [(3u64, false), (2, true), (3, true), (2, false)] {
        let (mut fg, idx, eight) = build_if_over_load(ty, |b, idx| {
            let eight = b.build_int_const(8u64, ty)?;
            let lt = b.build_int_cmp_operation(idx, eight, IntCmpOp::Less, ty)?;
            let gt = b.build_int_cmp_operation(eight, idx, IntCmpOp::Less, ty)?;
            let eq = b.build_int_cmp_operation(idx, eight, IntCmpOp::Equal, ty)?;
            let terms = [
                cr_term(b, lt, 3)?,
                cr_term(b, gt, 2)?,
                cr_term(b, eq, 1)?,
                b.extend_if_needed(eq, ty, ExtendOp::ZeroExtend)?,
            ];
            let cr = terms[1..].iter().try_fold(terms[0], |acc, &t| {
                b.build_int_binary_operation(acc, t, IntBinaryOp::Or, ty)
            })?;
            let shr = b.build_const_binop(k, cr, IntBinaryOp::ShiftRight, ty)?;
            let cond = if ne_zero {
                let masked = b.build_const_binop(1u64, shr, IntBinaryOp::And, ty)?;
                let zero = b.build_int_const(0u64, ty)?;
                let is_zero = b.build_int_cmp_operation(masked, zero, IntCmpOp::Equal, ty)?;
                bool_not(b, is_zero)?
            } else {
                b.truncate_if_needed(shr, ValueType::I1)?
            };
            Ok((cond, eight))
        })?;
        run_pass(&FlagCmpCanonicalize, &mut fg)?;
        let if_node = fg.first_kind(|k| matches!(k, NodeKind::If));
        let (l, r) = if k == 3 { (idx, eight) } else { (eight, idx) };
        assert_if_cond_is_intcmp(fg.graph(), if_node, IntCmpOp::Less, l, r);
    }
    Ok(())
}

/// The Thumb "true" flag test over a flag that is no comparison, the C of a
/// `cmp` (`Xor(Less(a, b), 1)`), is the CR-bit arm's one-term pack: it
/// collapses to the flag and keeps the test's addresses.
///
/// Mutation: let `BitTracer::bit` stop at a non-comparison `I1`.
#[test]
fn a_thumb_true_flag_test_collapses_to_a_non_comparison_flag() -> Result<()> {
    let mut fg = nonzero_flag_test(bool_not)?;
    run_pass(&FlagCmpCanonicalize, &mut fg)?;
    let if_node = fg.first_kind(|k| matches!(k, NodeKind::If));
    let cond = fg.graph().producer(if_cond_output(fg.graph(), if_node));
    assert_eq!(*fg.node_kind(cond), NodeKind::IntBinaryOp(IntBinaryOp::Xor));
    let [lt, _] = fg.node_inputs_exact::<2>(cond).expect("binary");
    assert_eq!(
        *fg.node_kind(fg.producer(lt)),
        NodeKind::IntCmpOp(IntCmpOp::Less),
        "the condition is the flag itself, Xor(Less, 1)"
    );
    let fp = fg.side_tables().asm_fingerprint(cond);
    assert!(
        fp.contains(&ADDR_PACK) && fp.contains(&ADDR_TRUNC),
        "the test's addresses survive in the flag's fingerprint; got {fp:?}"
    );
    Ok(())
}

/// `If(Xor(Equal(ZeroExtend(flag):I32, 0), 1))`, the `flag != 0` branch
/// lowering, with the comparison at `ADDR_CMP`, the widened flag at
/// `ADDR_PACK` and its test at `ADDR_TRUNC`.  `flag` is `make_flag`'s
/// boolean over `Less(idx, 8)`.
fn nonzero_flag_test(
    make_flag: impl Fn(&mut FunctionBuilder, ValueId) -> Result<ValueId>,
) -> Result<strider_ir::Function> {
    use strider_ir::node::ExtendOp;
    let ty = ValueType::int(32);
    let mut b = RegisterSet::new().build_fn()?;
    let entry = b.create_region_all()?;
    let taken = b.create_region_all()?;
    let fall = b.create_region_all()?;
    b.set_entry_region_all(entry)?;
    b.set_region(entry);

    b.set_lift_addr(Some(ADDR_CMP));
    let dummy = b.build_int_const(0xF00Du64, ValueType::int(64))?;
    let idx = b.build_load(dummy, rsleigh::VnSpace::RAM, ty)?;
    let eight = b.build_int_const(8u64, ty)?;
    let lt = b.build_int_cmp_operation(idx, eight, IntCmpOp::Less, ty)?;
    let flag = make_flag(&mut b, lt)?;

    b.set_lift_addr(Some(ADDR_PACK));
    let wide = b.extend_if_needed(flag, ty, ExtendOp::ZeroExtend)?;

    b.set_lift_addr(Some(ADDR_TRUNC));
    let zero = b.build_int_const(0u64, ty)?;
    let eq = b.build_int_cmp_operation(wide, zero, IntCmpOp::Equal, ty)?;
    let cond = bool_not(&mut b, eq)?;
    b.build_if(cond, taken, fall)?;

    b.set_lift_addr(Some(ADDR_PACK));
    b.set_region(taken);
    b.build_return(Some(idx), &[])?;
    b.set_region(fall);
    b.build_return(Some(idx), &[])?;
    b.build()
}

const ADDR_CMP: u64 = 0x1111;
const ADDR_PACK: u64 = 0x2222;
const ADDR_TRUNC: u64 = 0x3333;

/// The `flag != 0` spelling of a one-term pack: the widened flag sits below
/// the tested `Equal`, and its address survives in the comparison's
/// fingerprint as the `Truncate` spelling's pack does.
///
/// Mutation: stop the descent at the tested `Equal`.
#[test]
fn a_nonzero_flag_test_keeps_the_widened_flags_fingerprint() -> Result<()> {
    let mut fg = nonzero_flag_test(|_, lt| Ok(lt))?;
    run_pass(&FlagCmpCanonicalize, &mut fg)?;
    let if_node = fg.first_kind(|k| matches!(k, NodeKind::If));
    let cond = fg.graph().producer(if_cond_output(fg.graph(), if_node));
    assert_eq!(*fg.node_kind(cond), NodeKind::IntCmpOp(IntCmpOp::Less));
    let fp = fg.side_tables().asm_fingerprint(cond);
    assert!(
        fp.contains(&ADDR_PACK) && fp.contains(&ADDR_TRUNC) && fp.contains(&ADDR_CMP),
        "the test's addresses survive in the comparison's fingerprint; got {fp:?}"
    );
    Ok(())
}

/// A `Truncate`-spelled CR-bit test whose tested bit holds a flag that is no
/// comparison, widened through `I8` first, folds to that flag: bit 2 of
/// `ZeroExtend(ZeroExtend(f:I1):I8) << 2 | ZeroExtend(eq) << 1` is `f`.
///
/// Mutation: let `BitTracer::bit` stop at a non-comparison `I1`.
#[test]
fn a_truncate_cr_test_folds_to_a_chained_zero_extended_flag() -> Result<()> {
    use strider_ir::node::ExtendOp;
    let ty = ValueType::int(32);
    let mut b = RegisterSet::new().build_fn()?;
    let entry = b.create_region_all()?;
    let taken = b.create_region_all()?;
    let fall = b.create_region_all()?;
    b.set_entry_region_all(entry)?;
    b.set_region(entry);
    let dummy = b.build_int_const(0xF00Du64, ValueType::int(64))?;
    let idx = b.build_load(dummy, rsleigh::VnSpace::RAM, ty)?;
    let eight = b.build_int_const(8u64, ty)?;
    let lt = b.build_int_cmp_operation(idx, eight, IntCmpOp::Less, ty)?;
    let flag = bool_not(&mut b, lt)?;
    let eq = b.build_int_cmp_operation(idx, eight, IntCmpOp::Equal, ty)?;
    let narrow = b.extend_if_needed(flag, ValueType::int(8), ExtendOp::ZeroExtend)?;
    let wide = b.extend_if_needed(narrow, ty, ExtendOp::ZeroExtend)?;
    let two = b.build_int_const(2u64, ty)?;
    let at2 = b.build_int_binary_operation(wide, two, IntBinaryOp::ShiftLeft, ty)?;
    let eq_w = b.extend_if_needed(eq, ty, ExtendOp::ZeroExtend)?;
    let at1 = b.build_const_binop(1u64, eq_w, IntBinaryOp::ShiftLeft, ty)?;
    let pack = b.build_int_binary_operation(at2, at1, IntBinaryOp::Or, ty)?;
    let shr = b.build_int_binary_operation(pack, two, IntBinaryOp::ShiftRight, ty)?;
    let cond = b.truncate_if_needed(shr, ValueType::I1)?;
    b.build_if(cond, taken, fall)?;
    b.set_region(taken);
    b.build_return(Some(idx), &[])?;
    b.set_region(fall);
    b.build_return(Some(idx), &[])?;
    let mut fg = b.build()?;

    run_pass(&FlagCmpCanonicalize, &mut fg)?;
    let if_node = fg.first_kind(|k| matches!(k, NodeKind::If));
    let cond = fg.graph().producer(if_cond_output(fg.graph(), if_node));
    assert_eq!(
        *fg.node_kind(cond),
        NodeKind::IntBinaryOp(IntBinaryOp::Xor),
        "the condition is the flag Xor(Less, 1)"
    );
    Ok(())
}

/// Superset-only contract: the CR-pack instructions' addresses must survive in
/// the comparison's fingerprint.  Three distinct addresses, because
/// `replace_value` alone carries the comparison's own and the `Truncate`'s but
/// drops `ADDR_PACK`.
#[test]
fn ppc_cr_bit_canonicalize_preserves_pack_fingerprints() -> Result<()> {
    let ty = ValueType::int(32);
    let mut b = RegisterSet::new().build_fn()?;
    let entry = b.create_region_all()?;
    let dispatch = b.create_region_all()?;
    let exit = b.create_region_all()?;
    b.set_entry_region_all(entry)?;
    b.set_region(entry);

    b.set_lift_addr(Some(ADDR_CMP));
    let dummy = b.build_int_const(0xF00Du64, ValueType::int(64))?;
    let idx = b.build_load(dummy, rsleigh::VnSpace::RAM, ty)?;
    let eight = b.build_int_const(8u64, ty)?;
    let lt = b.build_int_cmp_operation(idx, eight, IntCmpOp::Less, ty)?;

    b.set_lift_addr(Some(ADDR_PACK));
    let gt = b.build_int_cmp_operation(eight, idx, IntCmpOp::Less, ty)?;
    let eq = b.build_int_cmp_operation(idx, eight, IntCmpOp::Equal, ty)?;
    let lt_s = cr_term(&mut b, lt, 3)?;
    let gt_s = cr_term(&mut b, gt, 2)?;
    let eq_s = cr_term(&mut b, eq, 1)?;
    let or1 = b.build_int_binary_operation(lt_s, gt_s, IntBinaryOp::Or, ty)?;
    let or2 = b.build_int_binary_operation(or1, eq_s, IntBinaryOp::Or, ty)?;
    let shr = b.build_const_binop(3u64, or2, IntBinaryOp::ShiftRight, ty)?;

    b.set_lift_addr(Some(ADDR_TRUNC));
    let cond = b.truncate_if_needed(shr, ValueType::I1)?;
    b.build_if(cond, dispatch, exit)?;

    b.set_lift_addr(Some(ADDR_PACK));
    b.set_region(dispatch);
    b.build_return(Some(idx), &[])?;
    b.set_region(exit);
    b.build_return(Some(idx), &[])?;
    let mut fg = b.build()?;

    run_pass(&FlagCmpCanonicalize, &mut fg)?;

    // The bit-3 `Less(idx, 8)` is the only reachable `Less` once the pack is culled.
    let cmp_node = fg.first_kind(|k| matches!(k, NodeKind::IntCmpOp(IntCmpOp::Less)));
    let fp = fg.side_tables().asm_fingerprint(cmp_node);
    assert!(
        fp.contains(&ADDR_PACK),
        "the CR-pack instructions' address {ADDR_PACK:#x} must survive in the \
         comparison's fingerprint (superset contract); got {fp:?}"
    );
    assert!(
        fp.contains(&ADDR_CMP) && fp.contains(&ADDR_TRUNC),
        "the comparison's own ({ADDR_CMP:#x}) and the Truncate's ({ADDR_TRUNC:#x}) \
         addresses must also be present; got {fp:?}"
    );
    Ok(())
}

/// Tests a MIDDLE term (EQ, bit 1): the `ShiftRight` amount must line up with
/// that term's `ShiftLeft` position, not the highest-set one.
#[test]
fn ppc_cr_bit_test_selects_middle_eq_bit() -> Result<()> {
    let ty = ValueType::int(32);
    let (mut fg, idx, eight) = build_if_over_load(ty, |b, idx| {
        let eight = b.build_int_const(8u64, ty)?;
        let lt = b.build_int_cmp_operation(idx, eight, IntCmpOp::Less, ty)?;
        let eq = b.build_int_cmp_operation(idx, eight, IntCmpOp::Equal, ty)?;
        let lt_s = cr_term(b, lt, 3)?;
        let eq_s = cr_term(b, eq, 1)?;
        let cr = b.build_int_binary_operation(lt_s, eq_s, IntBinaryOp::Or, ty)?;
        let shr = b.build_const_binop(1u64, cr, IntBinaryOp::ShiftRight, ty)?;
        let cond = b.truncate_if_needed(shr, ValueType::I1)?;
        Ok((cond, eight))
    })?;

    run_pass(&FlagCmpCanonicalize, &mut fg)?;

    let if_node = fg.first_kind(|k| matches!(k, NodeKind::If));
    assert_if_cond_is_intcmp(fg.graph(), if_node, IntCmpOp::Equal, idx, eight);
    Ok(())
}

/// The canonical AArch64 `cmp a, b` flag quad `(ZR, NG, CY, OV)`; each test
/// wires up whichever subset its cond code reads.
fn build_cmp_flags(
    fb: &mut FunctionBuilder,
    a: ValueId,
    b: ValueId,
) -> Result<(ValueId, ValueId, ValueId, ValueId)> {
    let neg_b = fb.build_int_unary_operation(b, IntUnaryOp::Neg, ValueType::int(32))?;
    let diff = fb.build_int_binary_operation(a, neg_b, IntBinaryOp::Add, ValueType::int(32))?;
    let zero = fb.build_int_const(0u64, ValueType::int(32))?;

    let zr = fb.build_int_cmp_operation(diff, zero, IntCmpOp::Equal, ValueType::int(32))?;
    let ng = fb.build_int_cmp_operation(diff, zero, IntCmpOp::Sless, ValueType::int(32))?;
    // CY = Xor(IntLess(a, b), IntConst(1)):I1, the lift-time canonicalisation
    // of IntLessEqual(b, a).
    let alt = fb.build_int_cmp_operation(a, b, IntCmpOp::Less, ValueType::int(32))?;
    let cy = fb.build_const_binop(u128::MAX, alt, IntBinaryOp::Xor, ValueType::I1)?;
    let ov = fb.build_int_cmp_operation(a, b, IntCmpOp::Sborrow, ValueType::int(32))?;

    Ok((zr, ng, cy, ov))
}

/// An `If` whose cond the closure derives from the four AArch64 flags over two
/// 32-bit register reads.  Returns the pre-flag leaves `a`, `b`.
fn build_if_with_flag_cond<F>(
    make_cond: F,
) -> Result<(strider_ir::Function, NodeId, ValueId, ValueId)>
where
    F: FnOnce(
        &mut FunctionBuilder,
        ValueId, // ZR
        ValueId, // NG
        ValueId, // CY
        ValueId, // OV
    ) -> Result<ValueId>,
{
    let a_vn = strider_ir_test_utils::reg_vn(0x1000, 4);
    let b_vn = strider_ir_test_utils::reg_vn(0x1008, 4);
    let (fg, if_node, (a, b)) = RegisterSet::new()
        .tracked(a_vn)
        .tracked(b_vn)
        .build_if_then_else_returns(|fb| {
            let a = fb.read_variable(&a_vn)?;
            let b = fb.read_variable(&b_vn)?;
            let (zr, ng, cy, ov) = build_cmp_flags(fb, a, b)?;
            let cond = make_cond(fb, zr, ng, cy, ov)?;
            Ok((cond, (a, b)))
        })?;
    Ok((fg, if_node, a, b))
}

fn if_cond_output(graph: &Graph, if_node: NodeId) -> ValueId {
    let [_ctrl, cond_value] = graph
        .node_inputs_exact::<2>(if_node)
        .expect("If has exactly two inputs");
    cond_value
}

fn if_cond_node_kind(graph: &Graph, if_node: NodeId) -> NodeKind {
    let cond_value = if_cond_output(graph, if_node);
    *graph.node_kind(graph.producer(cond_value))
}

/// Operand order is asserted exactly: several rules swap it.
fn assert_if_cond_is_intcmp(
    graph: &Graph,
    if_node: NodeId,
    op: IntCmpOp,
    expect_lhs: ValueId,
    expect_rhs: ValueId,
) {
    let cond_value = if_cond_output(graph, if_node);
    let cond_node = graph.producer(cond_value);
    assert_eq!(
        *graph.node_kind(cond_node),
        NodeKind::IntCmpOp(op),
        "If cond should be IntCmpOp({op:?})",
    );
    let [lhs, rhs] = graph
        .node_inputs_exact::<2>(cond_node)
        .expect("IntCmpOp has 2 inputs");
    assert_eq!(lhs, expect_lhs, "lhs of canonicalised cmp");
    assert_eq!(rhs, expect_rhs, "rhs of canonicalised cmp");
}

/// Asserts the If's cond is `Xor(IntCmpOp(op, lhs, rhs), IntConst(1)):I1`.
/// Stripping that Xor and swapping the branches is `IfCondInversion`'s job, so
/// these tests stop at the negated form.
fn assert_if_cond_is_neg_intcmp(
    function: &strider_ir::Function,
    if_node: NodeId,
    op: IntCmpOp,
    expect_lhs: ValueId,
    expect_rhs: ValueId,
) {
    let cond_value = if_cond_output(function.graph(), if_node);
    let xor_node = function.producer(cond_value);
    assert!(
        is_bool_not(function, xor_node),
        "If cond should be the 1-bit Xor-with-1 (logical NOT) shape, got {:?}",
        function.node_kind(xor_node),
    );
    let [lhs_value, rhs_value] = function
        .graph()
        .node_inputs_exact::<2>(xor_node)
        .expect("Xor has 2 inputs");
    // The non-constant operand is the cmp; dedup may put it on either side.
    let is_one_const = |value: ValueId| {
        function.int_const_u128(value) == Some(1)
            && function
                .value_kind(value)
                .as_value()
                .is_some_and(|t| t.is_bool())
    };
    let cmp_value = if is_one_const(rhs_value) {
        lhs_value
    } else {
        rhs_value
    };
    let inner_node = function.producer(cmp_value);
    assert_eq!(
        *function.node_kind(inner_node),
        NodeKind::IntCmpOp(op),
        "Inner cond should be IntCmpOp({op:?})",
    );
    let [lhs, rhs] = function
        .graph()
        .node_inputs_exact::<2>(inner_node)
        .expect("IntCmpOp has 2 inputs");
    assert_eq!(lhs, expect_lhs, "lhs of canonicalised cmp");
    assert_eq!(rhs, expect_rhs, "rhs of canonicalised cmp");
}

/// `Equal(NG, OV)` at the flag varnode's 1-byte width, as the real lift
/// passes it.
fn ng_eq_ov(fb: &mut FunctionBuilder, ng: ValueId, ov: ValueId) -> Result<ValueId> {
    let ng = fb.convert_to_int_if_needed(ng, ValueType::int(8))?;
    let ov = fb.convert_to_int_if_needed(ov, ValueType::int(8))?;
    fb.build_int_cmp_operation(ng, ov, IntCmpOp::Equal, ValueType::int(8))
}

/// Each AArch64 conditional branch's flag tree over `cmp a, b` is one
/// comparison of `a` and `b`: `(op, swapped, negated)`. A negated form stops
/// at `Xor(cmp, 1)`, which `IfCondInversion` strips. LS needs ConstantFold
/// first to cancel `Xor(Xor(Less(a, b), 1), 1)`; HI is run both ways, since
/// its two subtrees must still bind one `(a, b)` after ConstantFold.
#[test]
fn aarch64_cond_codes_rewrite_to_one_comparison() -> Result<()> {
    type Cond = fn(&mut FunctionBuilder, ValueId, ValueId, ValueId, ValueId) -> Result<ValueId>;
    use IntCmpOp::{Equal, Less, Sless};
    #[rustfmt::skip]
    let cases: [(&str, Cond, bool, IntCmpOp, bool, bool); 8] = [
        // `Xor(ZR, 1)`.
        ("ne", |fb, zr, _, _, _| bool_not(fb, zr), false, Equal, false, true),
        // `And(CY, Xor(ZR, 1))`, unsigned `a > b`.
        ("hi", |fb, zr, _, cy, _| {
            let nz = bool_not(fb, zr)?;
            fb.build_int_binary_operation(cy, nz, IntBinaryOp::And, ValueType::I1)
        }, false, Less, true, false),
        ("hi_after_cf", |fb, zr, _, cy, _| {
            let nz = bool_not(fb, zr)?;
            fb.build_int_binary_operation(cy, nz, IntBinaryOp::And, ValueType::I1)
        }, true, Less, true, false),
        // `Or(Xor(CY, 1), ZR)`.
        ("ls", |fb, zr, _, cy, _| {
            let ncy = bool_not(fb, cy)?;
            fb.build_int_binary_operation(ncy, zr, IntBinaryOp::Or, ValueType::I1)
        }, true, Less, true, true),
        // `Xor(Equal(NG, OV), 1)`.
        ("lt", |fb, _, ng, _, ov| {
            let eq = ng_eq_ov(fb, ng, ov)?;
            bool_not(fb, eq)
        }, false, Sless, false, false),
        // `Equal(NG, OV)`.
        ("ge", |fb, _, ng, _, ov| ng_eq_ov(fb, ng, ov), false, Sless, false, true),
        // `And(Xor(ZR, 1), Equal(NG, OV))`.
        ("gt", |fb, zr, ng, _, ov| {
            let nz = bool_not(fb, zr)?;
            let eq = ng_eq_ov(fb, ng, ov)?;
            fb.build_int_binary_operation(nz, eq, IntBinaryOp::And, ValueType::I1)
        }, false, Sless, true, false),
        // `Or(ZR, Xor(Equal(NG, OV), 1))`.
        ("le", |fb, zr, ng, _, ov| {
            let eq = ng_eq_ov(fb, ng, ov)?;
            let neq = bool_not(fb, eq)?;
            fb.build_int_binary_operation(zr, neq, IntBinaryOp::Or, ValueType::I1)
        }, false, Sless, true, true),
    ];
    for (name, cond, cf_first, op, swapped, negated) in cases {
        let (mut fg, if_node, a, b) = build_if_with_flag_cond(cond)?;
        if cf_first {
            run_pass(&crate::ConstantFold, &mut fg)?;
        }
        assert!(run_pass(&FlagCmpCanonicalize, &mut fg)?, "{name}");
        let (l, r) = if swapped { (b, a) } else { (a, b) };
        if negated {
            assert_if_cond_is_neg_intcmp(&fg, if_node, op, l, r);
        } else {
            assert_if_cond_is_intcmp(fg.graph(), if_node, op, l, r);
        }
    }
    Ok(())
}

#[test]
fn flag_cmp_cs_is_left_alone_as_bool_neg_int_less() -> Result<()> {
    // CS is bare CY = `Xor(IntLess(a, b), 1)`, already in `(a, b)` form;
    // `IfCondInversion` handles the outer Xor.
    let (mut fg, if_node, _a, _b) = build_if_with_flag_cond(|_fb, _zr, _ng, cy, _ov| Ok(cy))?;

    let r = run_pass(&FlagCmpCanonicalize, &mut fg)?;
    assert!(!r, "CS already canonical; pass must not fire");

    let cond_value = if_cond_output(fg.graph(), if_node);
    let cond_node = fg.producer(cond_value);
    assert!(
        is_bool_not(&fg, cond_node),
        "CY cond should be the I1 Xor-with-1 shape, got {:?}",
        fg.node_kind(cond_node),
    );
    Ok(())
}

/// MI is bare NG = `Sless(Add(a, Neg(b)), 0)`, not `Sless(a, b)` once the
/// subtraction overflows; VS is bare OV = `Sborrow(a, b)`, already in
/// `(a, b)` form. Neither is rewritten.
#[test]
fn flag_cmp_mi_and_vs_are_left_alone() -> Result<()> {
    type Cond = fn(&mut FunctionBuilder, ValueId, ValueId, ValueId, ValueId) -> Result<ValueId>;
    let cases: [(Cond, IntCmpOp); 2] = [
        (|_, _, ng, _, _| Ok(ng), IntCmpOp::Sless),
        (|_, _, _, _, ov| Ok(ov), IntCmpOp::Sborrow),
    ];
    for (cond, op) in cases {
        let (mut fg, if_node, _a, _b) = build_if_with_flag_cond(cond)?;
        assert!(!run_pass(&FlagCmpCanonicalize, &mut fg)?, "{op:?}");
        assert_eq!(
            if_cond_node_kind(fg.graph(), if_node),
            NodeKind::IntCmpOp(op)
        );
    }
    Ok(())
}

#[test]
fn flag_cmp_thumb_beq_reduces_to_int_equal() -> Result<()> {
    // ARM Thumb `B.EQ` lifts as `IntNotEqual(ZR, 0:1)`, canonicalised to
    // `Xor(IntEqual(ZeroExtend(ZR):I8, 0:I8), 1)`.
    let (mut fg, if_node, a, b) = build_if_with_flag_cond(|fb, zr, _ng, _cy, _ov| {
        let zero = fb.build_int_const(0u64, ValueType::int(8))?;
        let zr = fb.convert_to_int_if_needed(zr, ValueType::int(8))?;
        let eq = fb.build_int_cmp_operation(zr, zero, IntCmpOp::Equal, ValueType::int(8))?;
        bool_not(fb, eq)
    })?;

    // Two iterations: the CR-bit arm strips the outer
    // `Xor(IntEqual(..., 0), 1)`, then the EQ rule simplifies the inner
    // `Equal(diff, 0)`.  A real pipeline gets this from its fixed-point loop.
    let _ = run_pass(&FlagCmpCanonicalize, &mut fg)?;
    let _ = run_pass(&FlagCmpCanonicalize, &mut fg)?;

    assert_if_cond_is_intcmp(fg.graph(), if_node, IntCmpOp::Equal, a, b);
    Ok(())
}

// The decomposed flag shape: the boolean tree's leaves are comparisons on the
// original `(a, b)` pair, not on the subtraction and its flags.

/// An `If` whose cond the closure builds from two register reads.
fn build_if_with_ab_cond<F>(
    make_cond: F,
) -> Result<(strider_ir::Function, NodeId, ValueId, ValueId)>
where
    F: FnOnce(&mut FunctionBuilder, ValueId, ValueId) -> Result<ValueId>,
{
    let a_vn = strider_ir_test_utils::reg_vn(0x1000, 4);
    let b_vn = strider_ir_test_utils::reg_vn(0x1008, 4);
    let (fg, if_node, (a, b)) = RegisterSet::new()
        .tracked(a_vn)
        .tracked(b_vn)
        .build_if_then_else_returns(|fb| {
            let a = fb.read_variable(&a_vn)?;
            let b = fb.read_variable(&b_vn)?;
            let cond = make_cond(fb, a, b)?;
            Ok((cond, (a, b)))
        })?;
    Ok((fg, if_node, a, b))
}

/// The decomposed signed and unsigned GT / LE trees, leaves comparing the
/// original `(a, b)`: `(a != b) && !(a < b)` is `b < a`, and
/// `(a == b) || (a < b)` is `!(b < a)`.
#[test]
fn flag_cmp_decomposed_trees_rewrite_swapped() -> Result<()> {
    for op in [IntCmpOp::Sless, IntCmpOp::Less] {
        for gt in [true, false] {
            let (mut fg, if_node, a, b) = build_if_with_ab_cond(|fb, a, b| {
                let ty = ValueType::int(32);
                let eq = fb.build_int_cmp_operation(a, b, IntCmpOp::Equal, ty)?;
                let lt = fb.build_int_cmp_operation(a, b, op, ty)?;
                if gt {
                    let neq = bool_not(fb, eq)?;
                    let nlt = bool_not(fb, lt)?;
                    fb.build_int_binary_operation(neq, nlt, IntBinaryOp::And, ValueType::I1)
                } else {
                    fb.build_int_binary_operation(eq, lt, IntBinaryOp::Or, ValueType::I1)
                }
            })?;
            assert!(run_pass(&FlagCmpCanonicalize, &mut fg)?, "{op:?} gt={gt}");
            if gt {
                assert_if_cond_is_intcmp(fg.graph(), if_node, op, b, a);
            } else {
                assert_if_cond_is_neg_intcmp(&fg, if_node, op, b, a);
            }
        }
    }
    Ok(())
}

/// Decomposed GT with one leaf reading a third register, `(a != b) && !(a < c)`,
/// so the shared `(a, b)` capture cannot bind.  Nothing may fire.
#[test]
fn flag_cmp_incomplete_tree_foreign_leaf_left_alone() -> Result<()> {
    let a_vn = strider_ir_test_utils::reg_vn(0x1000, 4);
    let b_vn = strider_ir_test_utils::reg_vn(0x1008, 4);
    let c_vn = strider_ir_test_utils::reg_vn(0x1010, 4);
    let (mut fg, if_node, _leaves) = RegisterSet::new()
        .tracked(a_vn)
        .tracked(b_vn)
        .tracked(c_vn)
        .build_if_then_else_returns(|fb| {
            let a = fb.read_variable(&a_vn)?;
            let b = fb.read_variable(&b_vn)?;
            let c = fb.read_variable(&c_vn)?;
            let eq = fb.build_int_cmp_operation(a, b, IntCmpOp::Equal, ValueType::int(32))?;
            let neq = bool_not(fb, eq)?;
            let lt = fb.build_int_cmp_operation(a, c, IntCmpOp::Sless, ValueType::int(32))?;
            let nlt = bool_not(fb, lt)?;
            let cond = fb.build_int_binary_operation(neq, nlt, IntBinaryOp::And, ValueType::I1)?;
            Ok((cond, ()))
        })?;

    let cmp_count_before = fg.count_kind(|k| matches!(k, NodeKind::IntCmpOp(_)));
    let r = run_pass(&FlagCmpCanonicalize, &mut fg)?;
    assert!(!r, "mismatched-capture flag tree must not canonicalize");
    assert_eq!(
        if_cond_node_kind(fg.graph(), if_node),
        NodeKind::IntBinaryOp(IntBinaryOp::And),
        "cond must stay the original And"
    );
    assert_eq!(
        fg.count_kind(|k| matches!(k, NodeKind::IntCmpOp(_))),
        cmp_count_before,
        "no new IntCmpOp may materialise"
    );
    Ok(())
}

/// `cmp x, N; jbe` (LS) or Thumb `bhi` (HI) with the ZF term's `Neg(N)`
/// already folded, which neither the EQ rule nor the plain decomposed rules
/// match: LS is `Or(Less(x, N), Equal(Add(b, C1 - N + skew), 0))` and HI its
/// De Morgan dual `And(!Less(x, N), !Equal(..))`. `x` is `b`, or for a switch
/// whose cases start at a nonzero base (gcc's `sub b, K; cmp b-K, N`) the
/// offset index `Add(b, C1)`, a node distinct from the `Equal`'s base.
/// Returns the function, the `If`, `x` and `N`.
fn folded_tree(
    hi: bool,
    ty: ValueType,
    c1: Option<i64>,
    n: u64,
    skew: u128,
) -> Result<(strider_ir::Function, NodeId, ValueId, ValueId)> {
    let b_vn = strider_ir_test_utils::reg_vn(0x1000, ty.byte_size() as u32);
    let (fg, if_node, (x, n_const)) = RegisterSet::new()
        .tracked(b_vn)
        .build_if_then_else_returns(|fb| {
            let b = fb.read_variable(&b_vn)?;
            let c1 = c1.map_or(0, |c| i128::from(c) as u128);
            let x = if c1 == 0 {
                b
            } else {
                fb.build_const_binop(c1, b, IntBinaryOp::Add, ty)?
            };
            let n_const = fb.build_int_const(u128::from(n), ty)?;
            let less = fb.build_int_cmp_operation(x, n_const, IntCmpOp::Less, ty)?;
            // The folded ZF term reads the SAME `b`.
            let c2 = c1.wrapping_sub(u128::from(n)).wrapping_add(skew);
            let y = fb.build_const_binop(c2, b, IntBinaryOp::Add, ty)?;
            let zero = fb.build_int_const(0u128, ty)?;
            let eq = fb.build_int_cmp_operation(y, zero, IntCmpOp::Equal, ty)?;
            let cond = if hi {
                let neg_less = bool_not(fb, less)?;
                let neg_eq = bool_not(fb, eq)?;
                fb.build_int_binary_operation(neg_less, neg_eq, IntBinaryOp::And, ValueType::I1)?
            } else {
                fb.build_int_binary_operation(less, eq, IntBinaryOp::Or, ValueType::I1)?
            };
            Ok((cond, (x, n_const)))
        })?;
    Ok((fg, if_node, x, n_const))
}

/// A folded LS tree is `!Less(N, x)` and a folded HI tree `Less(N, x)`,
/// reusing the captured `x` and `N` rather than fresh nodes. `N = 1` makes
/// `-N` all-ones at the width, which the guard compares modulo the width.
#[test]
fn folded_ls_and_hi_trees_canonicalize() -> Result<()> {
    let (i32, i64) = (ValueType::int(32), ValueType::int(64));
    #[rustfmt::skip]
    let cases = [
        (false, i32, None, 8), (false, i64, None, 3),
        (false, i32, None, 1), (false, i64, None, 1),
        // Cases 10..=17: `x = b - 10`, `N = 7`.
        (false, i32, Some(-10), 7), (false, i64, Some(-100), 5),
        (true, i32, None, 7), (true, i64, None, 3),
        // Thumb `and r0,#7; subs r0,#1; cmp r0,#5; bhi`: `x = b - 1`.
        (true, i32, Some(-1), 6), (true, i64, Some(-20), 5),
    ];
    for (hi, ty, c1, n) in cases {
        let (mut fg, if_node, x, n_const) = folded_tree(hi, ty, c1, n, 0)?;
        assert!(
            run_pass(&FlagCmpCanonicalize, &mut fg)?,
            "hi={hi} {ty:?} C1={c1:?} N={n}"
        );
        if hi {
            assert_if_cond_is_intcmp(fg.graph(), if_node, IntCmpOp::Less, n_const, x);
        } else {
            assert_if_cond_is_neg_intcmp(&fg, if_node, IntCmpOp::Less, n_const, x);
        }
    }
    Ok(())
}

/// `cmp a, #N; b.ls` on a zero-extended `a` as lifted, before ConstantFold
/// has folded `Neg(N)`: run with ConstantFold and KnownBits first, as the
/// default pipeline does, the tree still collapses to `!Less(N, a)`.
#[test]
fn a_raw_ls_tree_on_a_zero_extended_operand_collapses_after_constant_fold() -> Result<()> {
    let ty = ValueType::int(64);
    let vn = strider_ir_test_utils::reg_vn(0x1000, 4);
    let (mut fg, if_node, idx) = {
        let (fg, if_node, idx) =
            RegisterSet::new()
                .tracked(vn)
                .build_if_then_else_returns(|fb| {
                    let w = fb.read_variable(&vn)?;
                    let idx = fb.extend_if_needed(w, ty, strider_ir::ExtendOp::ZeroExtend)?;
                    let n = fb.build_int_const(9u64, ty)?;
                    let less = fb.build_int_cmp_operation(idx, n, IntCmpOp::Less, ty)?;
                    let neg_n = fb.build_int_unary_operation(n, IntUnaryOp::Neg, ty)?;
                    let diff = fb.build_int_binary_operation(idx, neg_n, IntBinaryOp::Add, ty)?;
                    let zero = fb.build_int_const(0u64, ty)?;
                    let eq = fb.build_int_cmp_operation(diff, zero, IntCmpOp::Equal, ty)?;
                    let cond =
                        fb.build_int_binary_operation(less, eq, IntBinaryOp::Or, ValueType::I1)?;
                    Ok((cond, idx))
                })?;
        (fg, if_node, idx)
    };
    run_passes(
        &mut fg,
        &[
            &crate::ConstantFold,
            &crate::KnownBits,
            &FlagCmpCanonicalize,
        ],
    )?;
    let n = fg
        .walk()
        .flat_map(|node| fg.node_outputs(node).to_vec())
        .find(|&v| fg.int_const_u128(v) == Some(9) && fg.value_type_opt(v) == Some(ty))
        .expect("the compared constant survives");
    assert_if_cond_is_neg_intcmp(&fg, if_node, IntCmpOp::Less, n, idx);
    Ok(())
}

/// With `C2 != C1 - N` the `Equal` does not test `x == N`, so the guard must
/// reject. The inner `Equal(Add(b, C2), 0)` is still reshaped to
/// `Equal(b, -C2)` by the compare-with-const rule, so this asserts the idiom
/// survived rather than that nothing changed.
#[test]
fn flag_cmp_offset_folded_ls_tree_rejects_wrong_offset() -> Result<()> {
    let (mut fg, if_node, _, _) = folded_tree(false, ValueType::int(32), Some(-10), 7, 1)?;
    run_pass(&FlagCmpCanonicalize, &mut fg)?;
    let cond_node = fg.producer(fg.if_cond(if_node));
    assert_eq!(
        fg.node_kind(cond_node),
        &NodeKind::IntBinaryOp(IntBinaryOp::Or),
        "wrong-offset LS tree must NOT fold to a single comparison"
    );
    Ok(())
}

/// An `Equal` against a constant solves for `x`, the fresh constant at the
/// operand width (`I32`), not the root's `I1`: `x + 3 == 4` is `x == 1`,
/// `x ^ 3 == 5` is `x == 6`, and `-x == 5` is `x == -5`.
#[test]
fn eq_const_solves_for_x() -> Result<()> {
    let ty = ValueType::int(32);
    for (op, want) in [
        (Some(IntBinaryOp::Add), 1u128),
        (Some(IntBinaryOp::Xor), 6),
        (None, 0xFFFF_FFFB),
    ] {
        let (mut fg, x, ()) = build_if_over_load(ty, |b, x| {
            let lhs = match op {
                Some(op) => b.build_const_binop(3u64, x, op, ty)?,
                None => b.build_int_unary_operation(x, IntUnaryOp::Neg, ty)?,
            };
            let k = b.build_int_const(
                if op == Some(IntBinaryOp::Add) {
                    4u64
                } else {
                    5
                },
                ty,
            )?;
            Ok((b.build_int_cmp_operation(lhs, k, IntCmpOp::Equal, ty)?, ()))
        })?;
        run_pass(&FlagCmpCanonicalize, &mut fg)?;
        let cond_node = fg.producer(fg.if_cond(fg.first_kind(|k| matches!(k, NodeKind::If))));
        assert_eq!(
            fg.node_kind(cond_node),
            &NodeKind::IntCmpOp(IntCmpOp::Equal),
            "{op:?}"
        );
        let [l, r] = fg.node_inputs_exact::<2>(cond_node)?;
        let k = if l == x { r } else { l };
        assert!(l == x || r == x, "{op:?}: the operand is x");
        assert_eq!(
            (fg.int_const_u128(k), fg.value_type_opt(k)),
            (Some(want), Some(ty)),
            "{op:?}"
        );
    }
    Ok(())
}

/// `Sless(x << C, 0):I1 -> Xor(Equal(And(x, mask), 0), 1):I1`, mask=1<<(W-1-C).
/// Here W=32, C=3, so mask = 1<<28 = 0x1000_0000.
#[test]
fn sless_of_left_shift_is_a_sign_bit_test() -> Result<()> {
    let ty = ValueType::int(32);
    let (mut fg, x, ()) = build_if_over_load(ty, |b, x| {
        let shl = b.build_const_binop(3u64, x, IntBinaryOp::ShiftLeft, ty)?;
        let zero = b.build_int_const(0u64, ty)?;
        let sless = b.build_int_cmp_operation(shl, zero, IntCmpOp::Sless, ty)?;
        Ok((sless, ()))
    })?;

    run_pass(&FlagCmpCanonicalize, &mut fg)?;

    let if_node = fg.first_kind(|k| matches!(k, NodeKind::If));
    let xor = fg.producer(fg.if_cond(if_node));
    assert!(
        matches!(fg.node_kind(xor), NodeKind::IntBinaryOp(IntBinaryOp::Xor)),
        "cond must be an Xor, got {:?}",
        fg.node_kind(xor)
    );
    let xor_in: Vec<_> = fg.node_inputs(xor).into_iter().collect();
    assert!(
        xor_in.iter().any(|&o| fg.int_const_u128(o) == Some(1)),
        "xor with 1"
    );
    let eq = xor_in
        .iter()
        .find_map(|&o| {
            matches!(fg.kind_of_value(o), NodeKind::IntCmpOp(IntCmpOp::Equal))
                .then(|| fg.producer(o))
        })
        .expect("Equal operand of Xor");
    let eq_in: Vec<_> = fg.node_inputs(eq).into_iter().collect();
    assert!(
        eq_in.iter().any(|&o| fg.int_const_u128(o) == Some(0)),
        "eq to 0"
    );
    let and = eq_in
        .iter()
        .find_map(|&o| {
            matches!(fg.kind_of_value(o), NodeKind::IntBinaryOp(IntBinaryOp::And))
                .then(|| fg.producer(o))
        })
        .expect("And operand of Equal");
    let and_in: Vec<_> = fg.node_inputs(and).into_iter().collect();
    assert!(and_in.contains(&x), "And on x");
    assert!(
        and_in
            .iter()
            .any(|&o| fg.int_const_u128(o) == Some(0x1000_0000)),
        "mask must be 1<<(31-3) = 0x1000_0000"
    );
    Ok(())
}

/// At or above the width `x << 40` is 0, making the test const-false, which is
/// a different rewrite; the sign-bit canonicalization must stay out of it.
#[test]
fn sless_of_oversized_left_shift_is_not_a_sign_bit_test() -> Result<()> {
    let ty = ValueType::int(32);
    let (mut fg, _, ()) = build_if_over_load(ty, |b, x| {
        let shl = b.build_const_binop(40u64, x, IntBinaryOp::ShiftLeft, ty)?;
        let zero = b.build_int_const(0u64, ty)?;
        let sless = b.build_int_cmp_operation(shl, zero, IntCmpOp::Sless, ty)?;
        Ok((sless, ()))
    })?;

    run_pass(&FlagCmpCanonicalize, &mut fg)?;
    let if_node = fg.first_kind(|k| matches!(k, NodeKind::If));
    assert!(
        matches!(
            fg.node_kind(fg.producer(fg.if_cond(if_node))),
            NodeKind::IntCmpOp(IntCmpOp::Sless)
        ),
        "oversized-shift Sless must be left intact"
    );
    Ok(())
}

/// The residue and the sign-bit mask are computed at the declared width, so
/// all three rules reach an `I256` compare and the constants they mint carry
/// every limb.
#[test]
fn wide_const_rewrites_reach_past_128_bits() -> Result<()> {
    let ty = ValueType::int(256);
    let all_ones = (UBig::ONE << 256usize) - UBig::ONE;
    // `Equal(Add(x, 1), 0)` and `Equal(Neg(x), 1)` both solve to `x == -1`;
    // `Sless(ShiftLeft(x, 1), 0)` tests bit 254 of `x`.
    type Shape = fn(&mut FunctionBuilder, ValueId) -> Result<ValueId>;
    let shapes: [(Shape, UBig); 3] = [
        (
            |b, x| {
                let c1 = b.build_int_const(1u64, ValueType::int(256))?;
                let add =
                    b.build_int_binary_operation(x, c1, IntBinaryOp::Add, ValueType::int(256))?;
                let c2 = b.build_int_const(0u64, ValueType::int(256))?;
                b.build_int_cmp_operation(add, c2, IntCmpOp::Equal, ValueType::int(256))
            },
            all_ones.clone(),
        ),
        (
            |b, x| {
                let neg = b.build_int_unary_operation(x, IntUnaryOp::Neg, ValueType::int(256))?;
                let c = b.build_int_const(1u64, ValueType::int(256))?;
                b.build_int_cmp_operation(neg, c, IntCmpOp::Equal, ValueType::int(256))
            },
            all_ones.clone(),
        ),
        (
            |b, x| {
                let c = b.build_int_const(1u64, ValueType::int(256))?;
                let shl = b.build_int_binary_operation(
                    x,
                    c,
                    IntBinaryOp::ShiftLeft,
                    ValueType::int(256),
                )?;
                let zero = b.build_int_const(0u64, ValueType::int(256))?;
                b.build_int_cmp_operation(shl, zero, IntCmpOp::Sless, ValueType::int(256))
            },
            UBig::ONE << 254usize,
        ),
    ];

    for (i, (build_cond, want)) in shapes.into_iter().enumerate() {
        let (mut fg, _, cond) = build_if_over_load(ty, |b, x| {
            let cond = build_cond(b, x)?;
            Ok((cond, cond))
        })?;
        let _ = cond;
        let outcome = run_pass(&FlagCmpCanonicalize, &mut fg)?;
        assert!(outcome, "shape {i}: I256 compare must rewrite");
        let minted: Vec<UBig> = fg
            .walk()
            .filter(|&n| matches!(fg.node_kind(n), NodeKind::IntConst(_)))
            .filter_map(|n| fg.int_const(fg.node_outputs(n)[0]).cloned())
            .collect();
        assert!(
            minted.contains(&want),
            "shape {i}: the rewrite must mint the 256-bit constant; got {minted:?}"
        );
    }
    Ok(())
}

/// A shift amount at or past the operand width yields 0 under p-code
/// (`OpBehaviorIntLeft` / `IntRight` guard at `8 * sizeout`), so
/// `Truncate(ShiftRight(zext(lt):I32 << 35, 35)):I1` is the constant 0, not
/// `lt`.  Reading the term's bit position as `q + pos` without consulting the
/// shift's own width claims a bit that does not exist.
#[test]
fn cr_bit_test_declines_a_shift_past_the_width() -> Result<()> {
    use strider_ir::node::ExtendOp;
    let ty = ValueType::int(32);
    let (mut fg, _, ()) = build_if_over_load(ty, |b, idx| {
        let eight = b.build_int_const(8u64, ty)?;
        let lt = b.build_int_cmp_operation(idx, eight, IntCmpOp::Less, ty)?;
        let z = b.extend_if_needed(lt, ty, ExtendOp::ZeroExtend)?;
        let pack = b.build_const_binop(35u64, z, IntBinaryOp::ShiftLeft, ty)?;
        let k = b.build_int_const(35u64, ty)?;
        let shr = b.build_int_binary_operation(pack, k, IntBinaryOp::ShiftRight, ty)?;
        let cond = b.truncate_if_needed(shr, ValueType::I1)?;
        Ok((cond, ()))
    })?;

    run_pass(&FlagCmpCanonicalize, &mut fg)?;

    let if_node = fg.first_kind(|k| matches!(k, NodeKind::If));
    let kind = *fg.node_kind(fg.producer(fg.if_cond(if_node)));
    assert!(
        !matches!(kind, NodeKind::IntCmpOp(_)),
        "the packed bit is a hard 0, so the condition must not become the comparison; got {kind:?}",
    );
    Ok(())
}

/// A count past the width zeroes every bit, the tested one included even
/// where the count's low 32 bits are 0: `Truncate(zext(lt):I64 << 2^32)` is
/// 0, not `lt`.
#[test]
fn cr_bit_test_declines_a_count_past_the_width_with_zero_low_bits() -> Result<()> {
    use strider_ir::node::ExtendOp;
    let ty = ValueType::int(64);
    let (mut fg, _, ()) = build_if_over_load(ty, |b, idx| {
        let eight = b.build_int_const(8u64, ty)?;
        let lt = b.build_int_cmp_operation(idx, eight, IntCmpOp::Less, ty)?;
        let z = b.extend_if_needed(lt, ty, ExtendOp::ZeroExtend)?;
        let shl = b.build_const_binop(1u64 << 32, z, IntBinaryOp::ShiftLeft, ty)?;
        Ok((b.truncate_if_needed(shl, ValueType::I1)?, ()))
    })?;

    run_pass(&FlagCmpCanonicalize, &mut fg)?;

    let if_node = fg.first_kind(|k| matches!(k, NodeKind::If));
    let kind = *fg.node_kind(fg.producer(fg.if_cond(if_node)));
    assert_eq!(kind, NodeKind::Truncate, "the shifted bit is a hard 0");
    Ok(())
}

/// `Equal(zext(x), 0)` is `!x` only for an `I1` `x`: over an `I8` it stays.
#[test]
fn an_i8_false_flag_test_is_not_a_complement() -> Result<()> {
    use strider_ir::node::ExtendOp;
    let ty = ValueType::int(32);
    let (mut fg, _, ()) = build_if_over_load(ValueType::int(8), |b, x| {
        let wide = b.extend_if_needed(x, ty, ExtendOp::ZeroExtend)?;
        let zero = b.build_int_const(0u64, ty)?;
        Ok((
            b.build_int_cmp_operation(wide, zero, IntCmpOp::Equal, ty)?,
            (),
        ))
    })?;

    run_pass(&FlagCmpCanonicalize, &mut fg)?;

    let if_node = fg.first_kind(|k| matches!(k, NodeKind::If));
    let kind = *fg.node_kind(fg.producer(fg.if_cond(if_node)));
    assert_eq!(kind, NodeKind::IntCmpOp(IntCmpOp::Equal));
    Ok(())
}

/// `Xor(IntEqual(pack, 0), 1)` is `pack != 0`, the OR of EVERY bit of `pack`,
/// not bit 0 of it.  A term above the tested bit makes the root true where the
/// bit-0 comparison is false, so the pack must survive.
#[test]
fn cr_bit_test_declines_a_ne_zero_pack_carrying_a_higher_term() -> Result<()> {
    use strider_ir::node::ExtendOp;
    let ty = ValueType::int(32);
    let (mut fg, _, ()) = build_if_over_load(ty, |b, idx| {
        let eight = b.build_int_const(8u64, ty)?;
        let lt = b.build_int_cmp_operation(idx, eight, IntCmpOp::Less, ty)?;
        let gt = b.build_int_cmp_operation(eight, idx, IntCmpOp::Less, ty)?;
        let lt_z = b.extend_if_needed(lt, ty, ExtendOp::ZeroExtend)?;
        let gt_z = b.extend_if_needed(gt, ty, ExtendOp::ZeroExtend)?;
        let three = b.build_int_const(3u64, ty)?;
        let gt_s = b.build_int_binary_operation(gt_z, three, IntBinaryOp::ShiftLeft, ty)?;
        let pack = b.build_int_binary_operation(lt_z, gt_s, IntBinaryOp::Or, ty)?;
        let zero = b.build_int_const(0u64, ty)?;
        let is_zero = b.build_int_cmp_operation(pack, zero, IntCmpOp::Equal, ty)?;
        let true_i1 = b.build_boolean_const(true);
        let cond =
            b.build_int_binary_operation(is_zero, true_i1, IntBinaryOp::Xor, ValueType::I1)?;
        Ok((cond, ()))
    })?;

    run_pass(&FlagCmpCanonicalize, &mut fg)?;

    let if_node = fg.first_kind(|k| matches!(k, NodeKind::If));
    let kind = *fg.node_kind(fg.producer(fg.if_cond(if_node)));
    assert!(
        !matches!(kind, NodeKind::IntCmpOp(_)),
        "`pack != 0` also observes the term at bit 3, so the condition must not \
         become the bit-0 comparison; got {kind:?}",
    );
    Ok(())
}

/// `Truncate(ShiftRight(pack, k)):I1` over `idx`, with `pack` built from the
/// comparisons `lt`, `gt` and `eq` of `idx` against 8 and the opaque `cr`,
/// after FlagCmpCanonicalize: the If's condition and the three comparisons.
fn cr_test(
    k: u64,
    pack: impl FnOnce(&mut FunctionBuilder, [ValueId; 4]) -> Result<ValueId>,
) -> Result<(strider_ir::Function, ValueId, [ValueId; 3])> {
    let ty = ValueType::int(32);
    let (mut fg, _, cmps) = build_if_over_load(ty, |b, idx| {
        let eight = b.build_int_const(8u64, ty)?;
        let lt = b.build_int_cmp_operation(idx, eight, IntCmpOp::Less, ty)?;
        let gt = b.build_int_cmp_operation(eight, idx, IntCmpOp::Less, ty)?;
        let eq = b.build_int_cmp_operation(idx, eight, IntCmpOp::Equal, ty)?;
        b.set_lift_addr(Some(ADDR_OPAQUE));
        let at = b.build_int_const(0xBEEFu64, ValueType::int(64))?;
        let cr = b.build_load(at, rsleigh::VnSpace::RAM, ty)?;
        b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
        let packed = pack(b, [lt, gt, eq, cr])?;
        let k = b.build_int_const(k, ty)?;
        let shr = b.build_int_binary_operation(packed, k, IntBinaryOp::ShiftRight, ty)?;
        Ok((b.truncate_if_needed(shr, ValueType::I1)?, [lt, gt, eq]))
    })?;
    run_pass(&FlagCmpCanonicalize, &mut fg)?;
    let if_node = fg.first_kind(|k| matches!(k, NodeKind::If));
    let cond = fg.if_cond(if_node);
    Ok((fg, cond, cmps))
}

const ADDR_OPAQUE: u64 = 0x0BAD_0BAD;

/// A record form updates one CR bit in place, `(cr & ~(1 << p)) | (b << p)`:
/// bit `p` is `b` whatever the old field held.
#[test]
fn a_record_form_cr_bit_update_canonicalizes() -> Result<()> {
    let (fg, cond, [_, gt, _]) = cr_test(2, |b, [_, gt, _, cr]| {
        let ty = ValueType::int(32);
        let kept = b.build_const_binop(!(1u64 << 2) & 0xFFFF_FFFF, cr, IntBinaryOp::And, ty)?;
        let bit = cr_term(b, gt, 2)?;
        b.build_int_binary_operation(kept, bit, IntBinaryOp::Or, ty)
    })?;
    assert_eq!(cond, gt);
    // The kept field's bits were never read, so its load's address stays out.
    let fp = fg.side_tables().asm_fingerprint(fg.producer(gt));
    assert!(!fp.contains(&ADDR_OPAQUE), "{fp:?}");
    Ok(())
}

/// Terms that overlap below the tested bit say nothing about it.
#[test]
fn cr_bit_test_reads_past_terms_overlapping_at_another_bit() -> Result<()> {
    let (_, cond, [lt, _, _]) = cr_test(3, |b, [lt, gt, eq, _]| {
        let ty = ValueType::int(32);
        let lt = cr_term(b, lt, 3)?;
        let low = [cr_term(b, gt, 0)?, cr_term(b, eq, 0)?];
        let low = b.build_int_binary_operation(low[0], low[1], IntBinaryOp::Or, ty)?;
        b.build_int_binary_operation(lt, low, IntBinaryOp::Or, ty)
    })?;
    assert_eq!(cond, lt);
    Ok(())
}

/// Two comparisons at the tested bit: the bit is their OR, not either one.
#[test]
fn cr_bit_test_declines_two_terms_at_the_tested_bit() -> Result<()> {
    let (_, cond, [lt, gt, _]) = cr_test(3, |b, [lt, gt, _, _]| {
        let ty = ValueType::int(32);
        let terms = [cr_term(b, lt, 3)?, cr_term(b, gt, 3)?];
        b.build_int_binary_operation(terms[0], terms[1], IntBinaryOp::Or, ty)
    })?;
    assert!(cond != lt && cond != gt);
    Ok(())
}

/// A pack deeper than the trace budget is left alone, so one test costs at
/// most the budget.
#[test]
fn cr_bit_test_past_its_budget_declines() -> Result<()> {
    let (_, cond, [lt, _, _]) = cr_test(3, |b, [lt, _, eq, _]| {
        let ty = ValueType::int(32);
        let mut acc = cr_term(b, lt, 3)?;
        for _ in 0..super::MAX_BIT_VISITS {
            let term = cr_term(b, eq, 1)?;
            acc = b.build_int_binary_operation(term, acc, IntBinaryOp::Or, ty)?;
        }
        Ok(acc)
    })?;
    assert_ne!(cond, lt);
    Ok(())
}

/// `n` stacked `ZeroExtend(Xor(Equal(x, 0), 1))` layers: from the second on,
/// each `Xor` is the Thumb true-flag test over the layer below.
fn flag_test_chain(n: usize) -> Result<strider_ir::Function> {
    use strider_ir::node::ExtendOp;
    let r = strider_ir_test_utils::reg_vn(0, 8);
    let mut b = RegisterSet::new()
        .tracked(r)
        .arg(r)
        .build_fn_single_region()?;
    let mut acc = b.read_variable(&r)?;
    for _ in 0..n {
        let zero = b.build_int_const(0u64, ValueType::int(64))?;
        let eq = b.build_int_cmp_operation(acc, zero, IntCmpOp::Equal, ValueType::int(64))?;
        let not = bool_not(&mut b, eq)?;
        acc = b.extend_if_needed(not, ValueType::int(64), ExtendOp::ZeroExtend)?;
    }
    b.build_return(Some(acc), &[])?;
    b.build()
}

/// Outermost-first, a redirect onto an inner flag test carries every consumer
/// earlier redirects moved there unless each rewrite is cleaned before the
/// next; counted in use edges moved, which is what the collapse costs.
#[test]
fn a_chain_of_flag_tests_collapses_in_linear_use_moves() -> Result<()> {
    fn collapse(n: usize) -> Result<u64> {
        let mut fg = flag_test_chain(n)?;
        let mut edit = crate::EditFunction::new(&mut fg);
        edit.cull_dead();
        crate::peephole::run_peephole(&FlagCmpCanonicalize, &mut edit, &crate::OptCtx::new(None))?;
        edit.clean();
        let moved = edit.uses_moved();
        drop(edit);
        if let Err(errors) = strider_ir::validate::validate(&fg) {
            panic!("the collapse left invalid IR: {}", errors.report(&fg));
        }
        let xors = fg
            .walk_kind(|k| matches!(k, NodeKind::IntBinaryOp(IntBinaryOp::Xor)))
            .count();
        assert_eq!(xors, 1, "the chain collapses onto its innermost test");
        Ok(moved)
    }
    let small = collapse(500)?;
    let large = collapse(4_000)?;
    // 8x the chain: linear is 8x, quadratic 64x.
    assert!(
        large <= small * 12,
        "8x the chain moved {:.1}x the uses ({small} -> {large})",
        large as f64 / small as f64,
    );
    Ok(())
}

// `cmp a, N` against a constant: ConstantFold has folded `Add(a, Neg(N))` to
// `Add(a, M)`, so the signed rules see `Sless(Add(a, M), 0)` beside
// `Sborrow(a, N)`.  `cmn a, N` sets the same flags from `Add(a, N)` and
// `Scarry(a, N)`.

/// Which overflow flag the tree carries: `cmp` (`Sborrow`) or `cmn`
/// (`Scarry`).
#[derive(Clone, Copy, Debug)]
enum OverflowFlag {
    Sborrow,
    Scarry,
}

/// The GE tree `Equal(ZeroExtend(Sless(Add(a, add_c), 0)), ZeroExtend(flag(x,
/// n)))` at `ty`, or its LT negation, as an `If` cond, where `x` is `a` or,
/// given `offset`, `Add(a, offset)`.  Returns `x`.
fn build_signed_imm_tree(
    ty: ValueType,
    flag: OverflowFlag,
    offset: Option<u128>,
    add_c: u128,
    n: u128,
    lt: bool,
) -> Result<(strider_ir::Function, NodeId, ValueId)> {
    let a_vn = strider_ir_test_utils::reg_vn(0x1000, ty.byte_size() as u32);
    let (fg, if_node, x) = RegisterSet::new()
        .tracked(a_vn)
        .build_if_then_else_returns(|fb| {
            let a = fb.read_variable(&a_vn)?;
            let x = match offset {
                Some(c1) => fb.build_const_binop(c1, a, IntBinaryOp::Add, ty)?,
                None => a,
            };
            let sum = fb.build_const_binop(add_c, a, IntBinaryOp::Add, ty)?;
            let zero = fb.build_int_const(0u128, ty)?;
            let ng = fb.build_int_cmp_operation(sum, zero, IntCmpOp::Sless, ty)?;
            let n_const = fb.build_int_const(n, ty)?;
            let op = match flag {
                OverflowFlag::Sborrow => IntCmpOp::Sborrow,
                OverflowFlag::Scarry => IntCmpOp::Scarry,
            };
            let ov = fb.build_int_cmp_operation(x, n_const, op, ty)?;
            let ng = fb.convert_to_int_if_needed(ng, ValueType::int(8))?;
            let ov = fb.convert_to_int_if_needed(ov, ValueType::int(8))?;
            let ge = fb.build_int_cmp_operation(ng, ov, IntCmpOp::Equal, ValueType::int(8))?;
            let cond = if lt { bool_not(fb, ge)? } else { ge };
            Ok((cond, x))
        })?;
    Ok((fg, if_node, x))
}

/// Runs the pass; the rewritten cond as `(negated, lhs, rhs)` for
/// `[Xor(]Sless(lhs, rhs)[, 1)]`, or `None` when it left the tree alone.
fn run_to_sless(
    fg: &mut strider_ir::Function,
    if_node: NodeId,
) -> Result<Option<(bool, ValueId, ValueId)>> {
    let r = crate::pipeline::run_one(&FlagCmpCanonicalize, fg, &crate::OptCtx::new(None))?;
    if !r {
        return Ok(None);
    }
    let mut cmp = fg.producer(if_cond_output(fg.graph(), if_node));
    let negated = is_bool_not(fg, cmp);
    if negated {
        let [l, r] = fg.graph().node_inputs_exact::<2>(cmp).expect("Xor");
        cmp = fg.producer(if fg.int_const_u128(r) == Some(1) {
            l
        } else {
            r
        });
    }
    assert_eq!(*fg.node_kind(cmp), NodeKind::IntCmpOp(IntCmpOp::Sless));
    let [lhs, rhs] = fg.graph().node_inputs_exact::<2>(cmp).expect("Sless");
    Ok(Some((negated, lhs, rhs)))
}

/// The rewritten cond as `(negated, K)` for `[Xor(]Sless(x, K)[, 1)]`, or
/// `None` when the pass left the tree alone.
fn signed_imm_rewrite(
    ty: ValueType,
    flag: OverflowFlag,
    offset: Option<u128>,
    add_c: u128,
    n: u128,
    lt: bool,
) -> Result<Option<(bool, u128)>> {
    let (mut fg, if_node, x) = build_signed_imm_tree(ty, flag, offset, add_c, n, lt)?;
    let Some((negated, lhs, k)) = run_to_sless(&mut fg, if_node)? else {
        return Ok(None);
    };
    assert_eq!(lhs, x, "the compared value stays on the left");
    let k = fg.int_const_u128(k).expect("a constant right operand");
    Ok(Some((negated, k)))
}

fn sext8(v: u128) -> i128 {
    i128::from(v as u8 as i8)
}

/// Every `(a, N)` pair at 8 bits: wherever the pass rewrites a GE or LT tree,
/// the result agrees with the tree for every `a`, and it rewrites every
/// `cmp` tree and every `cmn` tree but `N = INT_MIN`.
#[test]
fn signed_imm_rewrites_agree_with_the_flag_tree_at_8_bits() -> Result<()> {
    let ty = ValueType::int(8);
    for flag in [OverflowFlag::Sborrow, OverflowFlag::Scarry] {
        for n in 0..=255u128 {
            let add_c = match flag {
                OverflowFlag::Sborrow => n.wrapping_neg() & 0xff,
                OverflowFlag::Scarry => n,
            };
            for lt in [false, true] {
                let got = signed_imm_rewrite(ty, flag, None, add_c, n, lt)?;
                let declines = matches!(flag, OverflowFlag::Scarry) && n == 0x80;
                let Some((negated, k)) = got else {
                    assert!(declines, "{flag:?} N={n:#x} lt={lt} left alone");
                    continue;
                };
                assert!(!declines, "{flag:?} N=INT_MIN has no Sless form");
                for a in 0..=255u128 {
                    let sum = sext8((a + add_c) & 0xff);
                    let exact = match flag {
                        OverflowFlag::Sborrow => sext8(a) - sext8(n),
                        OverflowFlag::Scarry => sext8(a) + sext8(n),
                    };
                    let ov = !(-128..=127).contains(&exact);
                    let tree = ((sum < 0) == ov) != lt;
                    let rewritten = (sext8(a) < sext8(k)) != negated;
                    assert_eq!(
                        rewritten, tree,
                        "{flag:?} N={n:#x} lt={lt} a={a:#x}: K={k:#x} negated={negated}"
                    );
                }
            }
        }
    }
    Ok(())
}

/// `M` must be `-N`: any other `Add` constant is a different tree.
#[test]
fn signed_imm_rewrite_requires_add_constant_to_negate_n() -> Result<()> {
    for ty in [ValueType::int(32), ValueType::int(64)] {
        for lt in [false, true] {
            let mask = ty.int_mask_u128().expect("narrow");
            let neg5 = 5u128.wrapping_neg() & mask;
            assert_eq!(
                signed_imm_rewrite(ty, OverflowFlag::Sborrow, None, neg5, 5, lt)?,
                Some((!lt, 5)),
                "{ty:?} lt={lt}"
            );
            for add_c in [neg5 + 1, 5, 0] {
                assert_eq!(
                    signed_imm_rewrite(ty, OverflowFlag::Sborrow, None, add_c, 5, lt)?,
                    None,
                    "{ty:?} lt={lt} M={add_c:#x}"
                );
            }
        }
    }
    Ok(())
}

/// `cmn a, 5` compares against `-5`; the `Add` constant must be `N` itself.
#[test]
fn cmn_imm_rewrites_to_sless_against_minus_n() -> Result<()> {
    for ty in [ValueType::int(32), ValueType::int(64)] {
        let mask = ty.int_mask_u128().expect("narrow");
        let neg5 = 5u128.wrapping_neg() & mask;
        for lt in [false, true] {
            assert_eq!(
                signed_imm_rewrite(ty, OverflowFlag::Scarry, None, 5, 5, lt)?,
                Some((!lt, neg5)),
                "{ty:?} lt={lt}"
            );
            assert_eq!(
                signed_imm_rewrite(ty, OverflowFlag::Scarry, None, 6, 5, lt)?,
                None,
                "{ty:?} lt={lt}: mismatched Add constant"
            );
            let int_min = mask ^ (mask >> 1);
            assert_eq!(
                signed_imm_rewrite(ty, OverflowFlag::Scarry, None, int_min, int_min, lt)?,
                None,
                "{ty:?} lt={lt}: N = INT_MIN"
            );
        }
    }
    Ok(())
}

/// `cmp X, N` on `X = Add(a, C1)`, the flag operand reassociated to
/// `Add(a, C1 - N)`: every `(a, N)` at 8 bits for a few `C1`, and a wrong
/// constant declines.
#[test]
fn offset_imm_rewrites_agree_with_the_flag_tree_at_8_bits() -> Result<()> {
    let ty = ValueType::int(8);
    for c1 in [1u128, 0x7f, 0x80, 0xff] {
        for n in 0..=255u128 {
            let add_c = c1.wrapping_sub(n) & 0xff;
            for lt in [false, true] {
                let off = Some(c1);
                assert_eq!(
                    signed_imm_rewrite(ty, OverflowFlag::Sborrow, off, (add_c + 1) & 0xff, n, lt)?,
                    None,
                    "C1={c1:#x} N={n:#x} lt={lt}: wrong Add constant"
                );
                let (negated, k) =
                    signed_imm_rewrite(ty, OverflowFlag::Sborrow, off, add_c, n, lt)?
                        .unwrap_or_else(|| panic!("C1={c1:#x} N={n:#x} lt={lt} left alone"));
                for a in 0..=255u128 {
                    let x = (a + c1) & 0xff;
                    let sum = sext8((a + add_c) & 0xff);
                    let ov = !(-128..=127).contains(&(sext8(x) - sext8(n)));
                    let tree = ((sum < 0) == ov) != lt;
                    let rewritten = (sext8(x) < sext8(k)) != negated;
                    assert_eq!(rewritten, tree, "C1={c1:#x} N={n:#x} lt={lt} a={a:#x}");
                }
            }
        }
    }
    Ok(())
}

/// After the GE/LT rewrite and solve-for-x on the EQ term, an offset LE is
/// `Or(Equal(a, C3), Sless(X, N))` and GT `And(Xor(Equal(a, C3), 1),
/// Xor(Sless(X, N), 1))`; with `C3 == N - C1` they fold to `Sless(N, X)`.
/// Every `(a, N)` at 8 bits for a few `C1`, and a wrong `C3` declines.
#[test]
fn offset_le_gt_rewrites_agree_at_8_bits() -> Result<()> {
    let ty = ValueType::int(8);
    let a_vn = strider_ir_test_utils::reg_vn(0x1000, 1);
    let build = |c1: u128, c3: u128, n: u128, gt: bool| {
        RegisterSet::new()
            .tracked(a_vn)
            .build_if_then_else_returns(|fb| {
                let a = fb.read_variable(&a_vn)?;
                let c1 = fb.build_int_const(c1, ty)?;
                let x = fb.build_int_binary_operation(a, c1, IntBinaryOp::Add, ty)?;
                let c3 = fb.build_int_const(c3, ty)?;
                let eq = fb.build_int_cmp_operation(a, c3, IntCmpOp::Equal, ty)?;
                let n = fb.build_int_const(n, ty)?;
                let lt = fb.build_int_cmp_operation(x, n, IntCmpOp::Sless, ty)?;
                let cond = if gt {
                    let ne = bool_not(fb, eq)?;
                    let ge = bool_not(fb, lt)?;
                    fb.build_int_binary_operation(ne, ge, IntBinaryOp::And, ValueType::I1)?
                } else {
                    fb.build_int_binary_operation(eq, lt, IntBinaryOp::Or, ValueType::I1)?
                };
                Ok((cond, x))
            })
    };
    for c1 in [1u128, 0x7f, 0x80, 0xff] {
        for n in 0..=255u128 {
            let c3 = n.wrapping_sub(c1) & 0xff;
            for gt in [false, true] {
                let (mut fg, if_node, _x) = build(c1, (c3 + 1) & 0xff, n, gt)?;
                assert!(
                    run_to_sless(&mut fg, if_node)?.is_none(),
                    "C1={c1:#x} N={n:#x} gt={gt}: wrong Equal constant"
                );
                let (mut fg, if_node, x) = build(c1, c3, n, gt)?;
                let (negated, k, rhs) = run_to_sless(&mut fg, if_node)?
                    .unwrap_or_else(|| panic!("C1={c1:#x} N={n:#x} gt={gt} left alone"));
                assert_eq!(rhs, x, "the compared value moves to the right");
                let k = fg.int_const_u128(k).expect("a constant left operand");
                for a in 0..=255u128 {
                    let xv = sext8((a + c1) & 0xff);
                    let tree = if gt {
                        a != c3 && xv >= sext8(n)
                    } else {
                        a == c3 || xv < sext8(n)
                    };
                    let rewritten = (sext8(k) < xv) != negated;
                    assert_eq!(rewritten, tree, "C1={c1:#x} N={n:#x} gt={gt} a={a:#x}");
                }
            }
        }
    }
    Ok(())
}

/// The pass seeds exactly the kinds some rule roots at: an `Equal` /
/// `Sless` / `Carry`, an `And`/`Or`/`Xor`, or the CR-bit `Truncate`. No rule
/// roots at another unsigned or overflow compare.
#[test]
fn seeds_the_kinds_its_rules_root_at() {
    for kind in crate::test_support::sample_kinds() {
        let rooted = matches!(
            kind,
            NodeKind::IntCmpOp(IntCmpOp::Equal | IntCmpOp::Sless | IntCmpOp::Carry)
                | NodeKind::Truncate
                | NodeKind::IntBinaryOp(IntBinaryOp::And | IntBinaryOp::Or | IntBinaryOp::Xor)
        );
        assert_eq!(super::RULES.with(|r| r.admits(&kind)), rooted, "{kind:?}");
    }
}

/// Runs FlagCmp over an `If` on `make_cond(x)`, `x` an `I256` load, and
/// asserts the condition is untouched.
fn assert_wide_cr_test_left_alone(
    make_cond: impl FnOnce(&mut FunctionBuilder, ValueId) -> Result<ValueId>,
) -> Result<()> {
    let (mut fg, _, cond) =
        build_if_over_load(ValueType::int(256), |b, x| make_cond(b, x).map(|c| (c, c)))?;
    run_pass(&FlagCmpCanonicalize, &mut fg)?;
    let if_node = fg.first_kind(|k| matches!(k, NodeKind::If));
    assert_eq!(if_cond_output(fg.graph(), if_node), cond);
    Ok(())
}

/// A small constant traced at a bit past 127 is zero there.
///
/// Mutation: shift the constant by `k` unchecked.
#[test]
fn a_cr_test_reading_a_constant_past_bit_127_does_not_fold() -> Result<()> {
    let ty = ValueType::int(256);
    assert_wide_cr_test_left_alone(|b, x| {
        let mask = b.build_int_const(0xFFu64, ty)?;
        let m = b.build_int_binary_operation(x, mask, IntBinaryOp::And, ty)?;
        let s = b.build_const_binop(200u64, m, IntBinaryOp::ShiftRight, ty)?;
        b.truncate_if_needed(s, ValueType::I1)
    })?;
    // `(x << 128) & 0xFF != 0`: bits 0..128 trace to zero, then bit 128
    // reads the mask.
    assert_wide_cr_test_left_alone(|b, x| {
        let s = b.build_const_binop(128u64, x, IntBinaryOp::ShiftLeft, ty)?;
        let m = b.build_const_binop(0xFFu64, s, IntBinaryOp::And, ty)?;
        let zero = b.build_int_const(0u64, ty)?;
        let eq = b.build_int_cmp_operation(m, zero, IntCmpOp::Equal, ty)?;
        bool_not(b, eq)
    })?;
    // Bit 0 already decides `x | 1 != 0` is no single flag.
    assert_wide_cr_test_left_alone(|b, x| {
        let o = b.build_const_binop(1u64, x, IntBinaryOp::Or, ty)?;
        let zero = b.build_int_const(0u64, ty)?;
        let eq = b.build_int_cmp_operation(o, zero, IntCmpOp::Equal, ty)?;
        bool_not(b, eq)
    })
}

/// The unsigned flag conditions after `cmn a, N` / `adds a, N` at `I8`:
/// `C = Carry(a, N)` (either operand order) and `Z = Equal(Add(a, N), 0)`.
#[derive(Clone, Copy, Debug)]
enum CarryCond {
    /// CS / CC.
    C {
        swapped: bool,
    },
    /// HI / LS: `C && !Z` / `!C || Z`.
    Hi,
    Ls,
    /// x86 `ja` / `jbe` after `add`: `!C && !Z` / `C || Z`.
    Above,
    BelowOrEqual,
}

/// `(negated, lhs, rhs)` of `[Xor(]Less(lhs, rhs)[, 1)]`, a constant operand
/// as its value and `a` as `None`.
type LessForm = (bool, Option<u128>, Option<u128>);

/// `If(cond)` for `cond` over `a` and `N`, then the pass; the rewritten
/// condition as `(negated, lhs, rhs)` for `[Xor(]Less(lhs, rhs)[, 1)]`, the
/// operands as constants (`None` for `a`), or `None` when the condition is
/// not one `Less`.
fn carry_rewrite(cond: CarryCond, n: u128) -> Result<Option<LessForm>> {
    let ty = ValueType::int(8);
    let a_vn = strider_ir_test_utils::reg_vn(0x1000, 1);
    let (mut fg, if_node, a) = RegisterSet::new()
        .tracked(a_vn)
        .build_if_then_else_returns(|fb| {
            let a = fb.read_variable(&a_vn)?;
            let n_const = fb.build_int_const(n, ty)?;
            let (l, r) = match cond {
                CarryCond::C { swapped: true } => (n_const, a),
                _ => (a, n_const),
            };
            let c = fb.build_int_cmp_operation(l, r, IntCmpOp::Carry, ty)?;
            let sum = fb.build_int_binary_operation(a, n_const, IntBinaryOp::Add, ty)?;
            let zero = fb.build_int_const(0u128, ty)?;
            let z = fb.build_int_cmp_operation(sum, zero, IntCmpOp::Equal, ty)?;
            let cond = match cond {
                CarryCond::C { .. } => c,
                CarryCond::Hi => {
                    let nz = bool_not(fb, z)?;
                    fb.build_int_binary_operation(c, nz, IntBinaryOp::And, ValueType::I1)?
                }
                CarryCond::Ls => {
                    let nc = bool_not(fb, c)?;
                    fb.build_int_binary_operation(nc, z, IntBinaryOp::Or, ValueType::I1)?
                }
                CarryCond::Above => {
                    let (nc, nz) = (bool_not(fb, c)?, bool_not(fb, z)?);
                    fb.build_int_binary_operation(nc, nz, IntBinaryOp::And, ValueType::I1)?
                }
                CarryCond::BelowOrEqual => {
                    fb.build_int_binary_operation(c, z, IntBinaryOp::Or, ValueType::I1)?
                }
            };
            Ok((cond, a))
        })?;
    crate::pipeline::run_one(&FlagCmpCanonicalize, &mut fg, &crate::OptCtx::new(None))?;
    let mut cmp = fg.producer(if_cond_output(fg.graph(), if_node));
    let negated = is_bool_not(&fg, cmp);
    if negated {
        let [l, r] = fg.graph().node_inputs_exact::<2>(cmp).expect("Xor");
        cmp = fg.producer(if fg.int_const_u128(r) == Some(1) {
            l
        } else {
            r
        });
    }
    if *fg.node_kind(cmp) != NodeKind::IntCmpOp(IntCmpOp::Less) {
        return Ok(None);
    }
    let [lhs, rhs] = fg.graph().node_inputs_exact::<2>(cmp).expect("Less");
    let operand = |v: ValueId| {
        if v == a {
            Ok(None)
        } else {
            fg.int_const_u128(v)
                .map(Some)
                .ok_or_else(|| anyhow::anyhow!("an operand neither `a` nor a constant"))
        }
    };
    Ok(Some((negated, operand(lhs)?, operand(rhs)?)))
}

/// Every `(a, N)` pair at 8 bits: a carry-based condition against a nonzero
/// `N` becomes one unsigned `Less` against `-N` that agrees with the flag
/// tree for every `a`; `Carry(a, 0)` is left alone.
#[test]
fn carry_conditions_are_one_unsigned_less_at_8_bits() -> Result<()> {
    let conds = [
        CarryCond::C { swapped: false },
        CarryCond::C { swapped: true },
        CarryCond::Hi,
        CarryCond::Ls,
        CarryCond::Above,
        CarryCond::BelowOrEqual,
    ];
    for cond in conds {
        for n in 0..=255u128 {
            let got = carry_rewrite(cond, n)?;
            if n == 0 {
                assert_eq!(got, None, "{cond:?}: Carry(a, 0) is false, no Less");
                continue;
            }
            let (negated, lhs, rhs) =
                got.unwrap_or_else(|| panic!("{cond:?} N={n:#x}: not one Less"));
            let k = n.wrapping_neg() & 0xff;
            assert!(
                [lhs, rhs] == [None, Some(k)] || [lhs, rhs] == [Some(k), None],
                "{cond:?} N={n:#x}: compares against -N, got {lhs:?} {rhs:?}"
            );
            for a in 0..=255u128 {
                let carry = a + n > 0xff;
                let zero = (a + n) & 0xff == 0;
                let tree = match cond {
                    CarryCond::C { .. } => carry,
                    CarryCond::Hi => carry && !zero,
                    CarryCond::Ls => !carry || zero,
                    CarryCond::Above => !carry && !zero,
                    CarryCond::BelowOrEqual => carry || zero,
                };
                let (l, r) = (lhs.unwrap_or(a), rhs.unwrap_or(a));
                assert_eq!(
                    (l < r) != negated,
                    tree,
                    "{cond:?} N={n:#x} a={a:#x}: Less({l:#x}, {r:#x}) negated={negated}"
                );
            }
        }
    }
    Ok(())
}
