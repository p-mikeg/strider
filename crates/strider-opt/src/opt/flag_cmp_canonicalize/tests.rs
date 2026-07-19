use super::FlagCmpCanonicalize;
use crate::error::Result;
use strider_ir::{IRBuilderExt, IRViewer};

use strider_ir::node::{NodeId, NodeKind, ValueId, ValueType};
use strider_ir::{FunctionBuilder, Graph, IRWalker, IntBinaryOp, IntCmpOp, IntUnaryOp};
use strider_ir_test_utils::RegisterSet;

/// PowerPC `cmpwi` packs LT/GT/EQ/SO into a CR field; the branch extracts one
/// bit via `Truncate(ShiftRight(cr_pack, k)):I1`.
#[test]
fn ppc_cr_bit_test_canonicalizes_to_intcmp() -> Result<()> {
    use strider_ir::node::ExtendOp;
    let ty = ValueType::I32;
    let mut b = RegisterSet::new().build_fn()?;
    b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
    let entry = b.create_region_all()?;
    let dispatch = b.create_region_all()?;
    let exit = b.create_region_all()?;
    b.set_entry_region_all(entry)?;
    b.set_region(entry);

    let dummy = b.build_int_const(0xF00Du64, ValueType::I64)?;
    let idx = b.build_load(dummy, rsleigh::VnSpace::RAM, ty)?;
    let eight = b.build_int_const(8u64, ty)?;
    let cr_bit = |b: &mut FunctionBuilder, cmp, pos: u64| -> Result<ValueId> {
        let z = b.extend_if_needed(cmp, ty, ExtendOp::ZeroExtend)?;
        let p = b.build_int_const(pos, ty)?;
        b.build_int_binary_operation(z, p, IntBinaryOp::ShiftLeft, ty)
    };

    let lt = b.build_int_cmp_operation(idx, eight, IntCmpOp::Less, ty)?;
    let gt = b.build_int_cmp_operation(eight, idx, IntCmpOp::Less, ty)?;
    let eq = b.build_int_cmp_operation(idx, eight, IntCmpOp::Equal, ty)?;
    let lt_s = cr_bit(&mut b, lt, 3)?;
    let gt_s = cr_bit(&mut b, gt, 2)?;
    let eq_s = cr_bit(&mut b, eq, 1)?;
    let so = b.extend_if_needed(eq, ty, ExtendOp::ZeroExtend)?; // bit 0
    let or1 = b.build_int_binary_operation(lt_s, gt_s, IntBinaryOp::Or, ty)?;
    let or2 = b.build_int_binary_operation(or1, eq_s, IntBinaryOp::Or, ty)?;
    let cr = b.build_int_binary_operation(or2, so, IntBinaryOp::Or, ty)?;
    let three = b.build_int_const(3u64, ty)?;
    let shr = b.build_int_binary_operation(cr, three, IntBinaryOp::ShiftRight, ty)?;
    let cond = b.truncate_if_needed(shr, ValueType::I1)?;
    b.build_if(cond, dispatch, exit)?;

    b.set_region(dispatch);
    b.build_return(Some(idx), &[])?;
    b.set_region(exit);
    b.build_return(Some(idx), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    crate::pipeline::run_one(
        &FlagCmpCanonicalize::new(),
        &mut fg,
        &mut crate::OptCtx::new(None),
    )?;

    let if_node = fg
        .walk()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::If))
        .expect("If node");
    // Bit 3 (LT) of the CR pack is `Less(idx, 8)`.
    assert_if_cond_is_intcmp(fg.graph(), if_node, IntCmpOp::Less, idx, eight);
    Ok(())
}

/// Superset-only contract: the CR-pack instructions' addresses must survive in
/// the comparison's fingerprint.  Three distinct addresses, because
/// `replace_value` alone carries the comparison's own and the `Truncate`'s but
/// drops `ADDR_PACK`.
#[test]
fn ppc_cr_bit_canonicalize_preserves_pack_fingerprints() -> Result<()> {
    use strider_ir::node::ExtendOp;
    const ADDR_CMP: u64 = 0x1111;
    const ADDR_PACK: u64 = 0x2222;
    const ADDR_TRUNC: u64 = 0x3333;
    let ty = ValueType::I32;
    let mut b = RegisterSet::new().build_fn()?;
    let entry = b.create_region_all()?;
    let dispatch = b.create_region_all()?;
    let exit = b.create_region_all()?;
    b.set_entry_region_all(entry)?;
    b.set_region(entry);

    b.set_lift_addr(Some(ADDR_CMP));
    let dummy = b.build_int_const(0xF00Du64, ValueType::I64)?;
    let idx = b.build_load(dummy, rsleigh::VnSpace::RAM, ty)?;
    let eight = b.build_int_const(8u64, ty)?;
    let lt = b.build_int_cmp_operation(idx, eight, IntCmpOp::Less, ty)?;

    // The rest of the pack: these are the addresses that must NOT be dropped.
    b.set_lift_addr(Some(ADDR_PACK));
    let gt = b.build_int_cmp_operation(eight, idx, IntCmpOp::Less, ty)?;
    let eq = b.build_int_cmp_operation(idx, eight, IntCmpOp::Equal, ty)?;
    let cr_bit = |b: &mut FunctionBuilder, cmp, pos: u64| -> Result<ValueId> {
        let z = b.extend_if_needed(cmp, ty, ExtendOp::ZeroExtend)?;
        let p = b.build_int_const(pos, ty)?;
        b.build_int_binary_operation(z, p, IntBinaryOp::ShiftLeft, ty)
    };
    let lt_s = cr_bit(&mut b, lt, 3)?;
    let gt_s = cr_bit(&mut b, gt, 2)?;
    let eq_s = cr_bit(&mut b, eq, 1)?;
    let or1 = b.build_int_binary_operation(lt_s, gt_s, IntBinaryOp::Or, ty)?;
    let or2 = b.build_int_binary_operation(or1, eq_s, IntBinaryOp::Or, ty)?;
    let three = b.build_int_const(3u64, ty)?;
    let shr = b.build_int_binary_operation(or2, three, IntBinaryOp::ShiftRight, ty)?;

    b.set_lift_addr(Some(ADDR_TRUNC));
    let cond = b.truncate_if_needed(shr, ValueType::I1)?;
    b.build_if(cond, dispatch, exit)?;

    b.set_lift_addr(Some(ADDR_PACK));
    b.set_region(dispatch);
    b.build_return(Some(idx), &[])?;
    b.set_region(exit);
    b.build_return(Some(idx), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    crate::pipeline::run_one(
        &FlagCmpCanonicalize::new(),
        &mut fg,
        &mut crate::OptCtx::new(None),
    )?;

    // The bit-3 `Less(idx, 8)` is the only reachable `Less` once the pack is culled.
    let cmp_node = fg
        .walk()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::IntCmpOp(IntCmpOp::Less)))
        .expect("the canonicalized comparison survives");
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
    use strider_ir::node::ExtendOp;
    let ty = ValueType::I32;
    let mut b = RegisterSet::new().build_fn()?;
    b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
    let entry = b.create_region_all()?;
    let dispatch = b.create_region_all()?;
    let exit = b.create_region_all()?;
    b.set_entry_region_all(entry)?;
    b.set_region(entry);

    let dummy = b.build_int_const(0xF00Du64, ValueType::I64)?;
    let idx = b.build_load(dummy, rsleigh::VnSpace::RAM, ty)?;
    let eight = b.build_int_const(8u64, ty)?;
    let cr_bit = |b: &mut FunctionBuilder, cmp, pos: u64| -> Result<ValueId> {
        let z = b.extend_if_needed(cmp, ty, ExtendOp::ZeroExtend)?;
        let p = b.build_int_const(pos, ty)?;
        b.build_int_binary_operation(z, p, IntBinaryOp::ShiftLeft, ty)
    };
    let lt = b.build_int_cmp_operation(idx, eight, IntCmpOp::Less, ty)?;
    let eq = b.build_int_cmp_operation(idx, eight, IntCmpOp::Equal, ty)?;
    let lt_s = cr_bit(&mut b, lt, 3)?;
    let eq_s = cr_bit(&mut b, eq, 1)?;
    let cr = b.build_int_binary_operation(lt_s, eq_s, IntBinaryOp::Or, ty)?;
    let one = b.build_int_const(1u64, ty)?;
    let shr = b.build_int_binary_operation(cr, one, IntBinaryOp::ShiftRight, ty)?;
    let cond = b.truncate_if_needed(shr, ValueType::I1)?;
    b.build_if(cond, dispatch, exit)?;

    b.set_region(dispatch);
    b.build_return(Some(idx), &[])?;
    b.set_region(exit);
    b.build_return(Some(idx), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    crate::pipeline::run_one(
        &FlagCmpCanonicalize::new(),
        &mut fg,
        &mut crate::OptCtx::new(None),
    )?;

    let if_node = fg
        .walk()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::If))
        .expect("If node");
    assert_if_cond_is_intcmp(fg.graph(), if_node, IntCmpOp::Equal, idx, eight);
    Ok(())
}

/// The canonical 1-bit logical NOT shape, `Xor(operand, IntConst(1)):I1`.
fn build_i1_xor_with_one(fb: &mut FunctionBuilder, operand: ValueId) -> Result<ValueId> {
    let one = fb.build_int_const(u128::MAX, ValueType::I1)?;
    fb.build_int_binary_operation(operand, one, IntBinaryOp::Xor, ValueType::I1)
}

/// The constant may sit on either side: Xor is commutative in the dedup cache.
fn is_i1_xor_with_one(fg: &strider_ir::Function, node: NodeId) -> bool {
    if !matches!(fg.node_kind(node), NodeKind::IntBinaryOp(IntBinaryOp::Xor)) {
        return false;
    }
    let Ok([lhs, rhs]) = fg.graph().node_inputs_exact::<2>(node) else {
        return false;
    };
    let is_one = |value: ValueId| {
        fg.value_type_opt(value).is_some_and(|t| t.is_bool()) && fg.int_const_u128(value) == Some(1)
    };
    is_one(lhs) || is_one(rhs)
}

/// The canonical AArch64 `cmp a, b` flag quad `(ZR, NG, CY, OV)`; each test
/// wires up whichever subset its cond code reads.
fn build_cmp_flags(
    fb: &mut FunctionBuilder,
    a: ValueId,
    b: ValueId,
) -> Result<(ValueId, ValueId, ValueId, ValueId)> {
    let neg_b = fb.build_int_unary_operation(b, IntUnaryOp::Neg, ValueType::I32)?;
    let diff = fb.build_int_binary_operation(a, neg_b, IntBinaryOp::Add, ValueType::I32)?;
    let zero = fb.build_int_const(0u64, ValueType::I32)?;

    let zr = fb.build_int_cmp_operation(diff, zero, IntCmpOp::Equal, ValueType::I32)?;
    let ng = fb.build_int_cmp_operation(diff, zero, IntCmpOp::Sless, ValueType::I32)?;
    // CY = Xor(IntLess(a, b), IntConst(1)):I1, the lift-time canonicalisation
    // of IntLessEqual(b, a).
    let alt = fb.build_int_cmp_operation(a, b, IntCmpOp::Less, ValueType::I32)?;
    let one_i1 = fb.build_int_const(u128::MAX, ValueType::I1)?;
    let cy = fb.build_int_binary_operation(alt, one_i1, IntBinaryOp::Xor, ValueType::I1)?;
    let ov = fb.build_int_cmp_operation(a, b, IntCmpOp::Sborrow, ValueType::I32)?;

    Ok((zr, ng, cy, ov))
}

/// An `If` whose cond the closure derives from the four AArch64 flags over two
/// 32-bit register reads.  Returns the leaves `a`, `b` so tests can assert the
/// rewritten cond points at them.
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

/// Asserts the If's cond is `Xor(IntCmpOp(op, lhs, rhs), IntConst(1)):I1`, for
/// the shapes whose canonical form is a negated cmp.  Stripping that Xor and
/// swapping the branches is `IfCondInversion`'s job, not this pass's, so these
/// tests stop at the negated form.
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
        is_i1_xor_with_one(function, xor_node),
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

#[test]
fn new_builds_pass_that_canonicalizes() -> Result<()> {
    let (mut fg, if_node, a, b) = build_if_with_flag_cond(|_fb, zr, _ng, _cy, _ov| Ok(zr))?;

    let r = crate::pipeline::run_one(
        &FlagCmpCanonicalize::new(),
        &mut fg,
        &mut crate::OptCtx::new(None),
    )?;
    assert!(
        r.changed(),
        "constructed pass should rewrite the EQ flag tree"
    );
    assert_if_cond_is_intcmp(fg.graph(), if_node, IntCmpOp::Equal, a, b);
    Ok(())
}

/// Pins that the rule table is per-instance, not shared global state.
#[test]
fn two_independent_instances_each_canonicalize() -> Result<()> {
    let pass_a = FlagCmpCanonicalize::new();
    let pass_b = FlagCmpCanonicalize::new();

    let (mut fg_a, if_a, a_a, b_a) = build_if_with_flag_cond(|_fb, zr, _ng, _cy, _ov| Ok(zr))?;
    assert!(crate::pipeline::run_one(&pass_a, &mut fg_a, &mut crate::OptCtx::new(None))?.changed());
    assert_if_cond_is_intcmp(fg_a.graph(), if_a, IntCmpOp::Equal, a_a, b_a);

    let (mut fg_b, if_b, a_b, b_b) = build_if_with_flag_cond(|_fb, zr, _ng, _cy, _ov| Ok(zr))?;
    assert!(crate::pipeline::run_one(&pass_b, &mut fg_b, &mut crate::OptCtx::new(None))?.changed());
    assert_if_cond_is_intcmp(fg_b.graph(), if_b, IntCmpOp::Equal, a_b, b_b);
    Ok(())
}

#[test]
fn flag_cmp_eq_rewrites_to_int_equal() -> Result<()> {
    // AArch64 `b.eq` cond is the bare ZR flag = `Equal(Add(a, Neg(b)), 0)`.
    let (mut fg, if_node, a, b) = build_if_with_flag_cond(|_fb, zr, _ng, _cy, _ov| Ok(zr))?;

    let r = crate::pipeline::run_one(
        &FlagCmpCanonicalize::new(),
        &mut fg,
        &mut crate::OptCtx::new(None),
    )?;
    assert!(r.changed(), "pass should rewrite the EQ flag tree");

    assert_if_cond_is_intcmp(fg.graph(), if_node, IntCmpOp::Equal, a, b);
    Ok(())
}

#[test]
fn flag_cmp_ne_rewrites_to_neg_int_equal() -> Result<()> {
    // AArch64 `b.ne` cond is `BitNot(ZR)` = `BitNot(Equal(Add(a, Neg(b)), 0))`.
    let (mut fg, if_node, a, b) =
        build_if_with_flag_cond(|fb, zr, _ng, _cy, _ov| build_i1_xor_with_one(fb, zr))?;

    let r = crate::pipeline::run_one(
        &FlagCmpCanonicalize::new(),
        &mut fg,
        &mut crate::OptCtx::new(None),
    )?;
    assert!(r.changed(), "pass should rewrite the NE flag tree");

    assert_if_cond_is_neg_intcmp(&fg, if_node, IntCmpOp::Equal, a, b);
    Ok(())
}

#[test]
fn flag_cmp_hi_rewrites_to_int_less_swapped() -> Result<()> {
    // AArch64 `b.hi` cond is `BoolAnd(CY, BitNot(ZR))`, i.e. unsigned `a > b`.
    let (mut fg, if_node, a, b) = build_if_with_flag_cond(|fb, zr, _ng, cy, _ov| {
        let neg_zr = build_i1_xor_with_one(fb, zr)?;
        fb.build_int_binary_operation(cy, neg_zr, IntBinaryOp::And, ValueType::I1)
    })?;

    let r = crate::pipeline::run_one(
        &FlagCmpCanonicalize::new(),
        &mut fg,
        &mut crate::OptCtx::new(None),
    )?;
    assert!(r.changed(), "pass should rewrite the HI flag tree");

    // Operands swap: `a > b` becomes `IntLess(b, a)`.
    assert_if_cond_is_intcmp(fg.graph(), if_node, IntCmpOp::Less, b, a);
    Ok(())
}

#[test]
fn flag_cmp_hi_rewrites_after_constant_fold_runs_first() -> Result<()> {
    // The HI rule's LHS reads `a`/`b` across two subtrees (`IntLess(a, b)` and
    // `Add(a, Neg(b))`), so both bindings must still agree after ConstantFold.
    // Runs the passes in production order to pin that.
    let (mut fg, if_node, a, b) = build_if_with_flag_cond(|fb, zr, _ng, cy, _ov| {
        let neg_zr = build_i1_xor_with_one(fb, zr)?;
        fb.build_int_binary_operation(cy, neg_zr, IntBinaryOp::And, ValueType::I1)
    })?;

    crate::pipeline::run_one(
        &crate::ConstantFold::new(),
        &mut fg,
        &mut crate::OptCtx::new(None),
    )?;
    let r = crate::pipeline::run_one(
        &FlagCmpCanonicalize::new(),
        &mut fg,
        &mut crate::OptCtx::new(None),
    )?;
    assert!(
        r.changed(),
        "HI rewrite must survive a prior ConstantFold pass"
    );
    assert_if_cond_is_intcmp(fg.graph(), if_node, IntCmpOp::Less, b, a);
    Ok(())
}

#[test]
fn flag_cmp_ls_rewrites_to_neg_int_less_swapped() -> Result<()> {
    // AArch64 `b.ls` cond is `BoolOr(BitNot(CY), ZR)`.  ConstantFold cancels
    // the double BitNot over CY, leaving `BoolOr(IntLess(a, b), Equal(a, b))`.
    let (mut fg, if_node, a, b) = build_if_with_flag_cond(|fb, zr, _ng, cy, _ov| {
        let neg_cy = build_i1_xor_with_one(fb, cy)?;
        fb.build_int_binary_operation(neg_cy, zr, IntBinaryOp::Or, ValueType::I1)
    })?;

    crate::pipeline::run_one(
        &crate::ConstantFold::new(),
        &mut fg,
        &mut crate::OptCtx::new(None),
    )?;
    let r = crate::pipeline::run_one(
        &FlagCmpCanonicalize::new(),
        &mut fg,
        &mut crate::OptCtx::new(None),
    )?;
    assert!(r.changed(), "pass should rewrite the LS flag tree");

    assert_if_cond_is_neg_intcmp(&fg, if_node, IntCmpOp::Less, b, a);
    Ok(())
}

#[test]
fn flag_cmp_lt_rewrites_to_int_sless() -> Result<()> {
    // AArch64 `b.lt` cond is `BitNot(Equal(NG, OV))`.  The I8 widening mirrors
    // the real lift, which passes the flag varnode's 1-byte size as the
    // comparison's operand width.
    let (mut fg, if_node, a, b) = build_if_with_flag_cond(|fb, _zr, ng, _cy, ov| {
        let ng = fb.convert_to_int_if_needed(ng, ValueType::I8)?;
        let ov = fb.convert_to_int_if_needed(ov, ValueType::I8)?;
        let eq = fb.build_int_cmp_operation(ng, ov, IntCmpOp::Equal, ValueType::I8)?;
        build_i1_xor_with_one(fb, eq)
    })?;

    let r = crate::pipeline::run_one(
        &FlagCmpCanonicalize::new(),
        &mut fg,
        &mut crate::OptCtx::new(None),
    )?;
    assert!(r.changed(), "pass should rewrite the LT flag tree");

    assert_if_cond_is_intcmp(fg.graph(), if_node, IntCmpOp::Sless, a, b);
    Ok(())
}

#[test]
fn flag_cmp_ge_rewrites_to_neg_int_sless() -> Result<()> {
    // AArch64 `b.ge` cond is `Equal(NG, OV)`.
    let (mut fg, if_node, a, b) = build_if_with_flag_cond(|fb, _zr, ng, _cy, ov| {
        let ng = fb.convert_to_int_if_needed(ng, ValueType::I8)?;
        let ov = fb.convert_to_int_if_needed(ov, ValueType::I8)?;
        fb.build_int_cmp_operation(ng, ov, IntCmpOp::Equal, ValueType::I8)
    })?;

    let r = crate::pipeline::run_one(
        &FlagCmpCanonicalize::new(),
        &mut fg,
        &mut crate::OptCtx::new(None),
    )?;
    assert!(r.changed(), "pass should rewrite the GE flag tree");

    assert_if_cond_is_neg_intcmp(&fg, if_node, IntCmpOp::Sless, a, b);
    Ok(())
}

#[test]
fn flag_cmp_gt_rewrites_to_int_sless_swapped() -> Result<()> {
    // AArch64 `b.gt` cond is `BoolAnd(BitNot(ZR), Equal(NG, OV))`.
    let (mut fg, if_node, a, b) = build_if_with_flag_cond(|fb, zr, ng, _cy, ov| {
        let neg_zr = build_i1_xor_with_one(fb, zr)?;
        let ng = fb.convert_to_int_if_needed(ng, ValueType::I8)?;
        let ov = fb.convert_to_int_if_needed(ov, ValueType::I8)?;
        let eq = fb.build_int_cmp_operation(ng, ov, IntCmpOp::Equal, ValueType::I8)?;
        fb.build_int_binary_operation(neg_zr, eq, IntBinaryOp::And, ValueType::I1)
    })?;

    let r = crate::pipeline::run_one(
        &FlagCmpCanonicalize::new(),
        &mut fg,
        &mut crate::OptCtx::new(None),
    )?;
    assert!(r.changed(), "pass should rewrite the GT flag tree");

    assert_if_cond_is_intcmp(fg.graph(), if_node, IntCmpOp::Sless, b, a);
    Ok(())
}

#[test]
fn flag_cmp_le_rewrites_to_neg_int_sless_swapped() -> Result<()> {
    // AArch64 `b.le` cond is `BoolOr(ZR, BitNot(Equal(NG, OV)))`.
    let (mut fg, if_node, a, b) = build_if_with_flag_cond(|fb, zr, ng, _cy, ov| {
        let ng = fb.convert_to_int_if_needed(ng, ValueType::I8)?;
        let ov = fb.convert_to_int_if_needed(ov, ValueType::I8)?;
        let eq = fb.build_int_cmp_operation(ng, ov, IntCmpOp::Equal, ValueType::I8)?;
        let neg_eq = build_i1_xor_with_one(fb, eq)?;
        fb.build_int_binary_operation(zr, neg_eq, IntBinaryOp::Or, ValueType::I1)
    })?;

    let r = crate::pipeline::run_one(
        &FlagCmpCanonicalize::new(),
        &mut fg,
        &mut crate::OptCtx::new(None),
    )?;
    assert!(r.changed(), "pass should rewrite the LE flag tree");

    assert_if_cond_is_neg_intcmp(&fg, if_node, IntCmpOp::Sless, b, a);
    Ok(())
}

#[test]
fn flag_cmp_cs_is_left_alone_as_bool_neg_int_less() -> Result<()> {
    // CS is bare CY = `BitNot(IntLess(a, b))`, already in `(a, b)` form;
    // `IfCondInversion` handles the outer BitNot.
    let (mut fg, if_node, _a, _b) = build_if_with_flag_cond(|_fb, _zr, _ng, cy, _ov| Ok(cy))?;

    let r = crate::pipeline::run_one(
        &FlagCmpCanonicalize::new(),
        &mut fg,
        &mut crate::OptCtx::new(None),
    )?;
    assert!(!r.changed(), "CS already canonical; pass must not fire");

    let cond_value = if_cond_output(fg.graph(), if_node);
    let cond_node = fg.producer(cond_value);
    assert!(
        is_i1_xor_with_one(&fg, cond_node),
        "CY cond should be the I1 Xor-with-1 shape, got {:?}",
        fg.node_kind(cond_node),
    );
    Ok(())
}

#[test]
fn flag_cmp_mi_is_left_alone_as_int_sless_diff() -> Result<()> {
    // MI is bare NG = `IntSless(Add(a, Neg(b)), 0)`, which is NOT equivalent to
    // `IntSless(a, b)` once the subtraction overflows.
    let (mut fg, if_node, _a, _b) = build_if_with_flag_cond(|_fb, _zr, ng, _cy, _ov| Ok(ng))?;

    let r = crate::pipeline::run_one(
        &FlagCmpCanonicalize::new(),
        &mut fg,
        &mut crate::OptCtx::new(None),
    )?;
    assert!(
        !r.changed(),
        "MI is not algebraically reducible; pass must not fire"
    );

    assert_eq!(
        if_cond_node_kind(fg.graph(), if_node),
        NodeKind::IntCmpOp(IntCmpOp::Sless),
    );
    Ok(())
}

#[test]
fn flag_cmp_thumb_beq_reduces_to_int_equal() -> Result<()> {
    // ARM Thumb `B.EQ` lifts as `IntNotEqual(ZR, 0:1)`, canonicalised to
    // `BitNot(IntEqual(CastToInt(ZR, I8), 0:I8))`.
    let (mut fg, if_node, a, b) = build_if_with_flag_cond(|fb, zr, _ng, _cy, _ov| {
        let zero = fb.build_int_const(0u64, ValueType::I8)?;
        let zr = fb.convert_to_int_if_needed(zr, ValueType::I8)?;
        let eq = fb.build_int_cmp_operation(zr, zero, IntCmpOp::Equal, ValueType::I8)?;
        build_i1_xor_with_one(fb, eq)
    })?;

    // Two iterations: the Thumb flag-test rule strips the outer
    // `BitNot(IntEqual(..., 0))`, then the EQ rule simplifies the inner
    // `Equal(diff, 0)`.  A real pipeline gets this from its fixed-point loop.
    let _ = crate::pipeline::run_one(
        &FlagCmpCanonicalize::new(),
        &mut fg,
        &mut crate::OptCtx::new(None),
    )?;
    let _ = crate::pipeline::run_one(
        &FlagCmpCanonicalize::new(),
        &mut fg,
        &mut crate::OptCtx::new(None),
    )?;

    assert_if_cond_is_intcmp(fg.graph(), if_node, IntCmpOp::Equal, a, b);
    Ok(())
}

#[test]
fn flag_cmp_vs_is_left_alone_as_sborrow() -> Result<()> {
    // VS is bare OV = `IntSborrow(a, b)`, already in `(a, b)` form.
    let (mut fg, if_node, _a, _b) = build_if_with_flag_cond(|_fb, _zr, _ng, _cy, ov| Ok(ov))?;

    let r = crate::pipeline::run_one(
        &FlagCmpCanonicalize::new(),
        &mut fg,
        &mut crate::OptCtx::new(None),
    )?;
    assert!(!r.changed(), "VS already canonical; pass must not fire");

    assert_eq!(
        if_cond_node_kind(fg.graph(), if_node),
        NodeKind::IntCmpOp(IntCmpOp::Sborrow),
    );
    Ok(())
}

// ARM/Thumb lift comparison branches with inverted sense, so ConstantFold has
// already decomposed the flag tree into direct comparisons on `(a, b)` by the
// time this pass runs.  The following fixtures build that decomposed shape.

/// An `If` whose cond comes straight from two register reads, no flag tree.
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

#[test]
fn flag_cmp_decomposed_gt_rewrites_to_sless_swapped() -> Result<()> {
    // (a != b) && !(a < b)  ≡  a > b  ≡  b < a  ->  Sless(b, a)
    let (mut fg, if_node, a, b) = build_if_with_ab_cond(|fb, a, b| {
        let eq = fb.build_int_cmp_operation(a, b, IntCmpOp::Equal, ValueType::I32)?;
        let neq = build_i1_xor_with_one(fb, eq)?;
        let lt = fb.build_int_cmp_operation(a, b, IntCmpOp::Sless, ValueType::I32)?;
        let nlt = build_i1_xor_with_one(fb, lt)?;
        fb.build_int_binary_operation(neq, nlt, IntBinaryOp::And, ValueType::I1)
    })?;
    let r = crate::pipeline::run_one(
        &FlagCmpCanonicalize::new(),
        &mut fg,
        &mut crate::OptCtx::new(None),
    )?;
    assert!(r.changed(), "decomposed GT should canonicalize");
    assert_if_cond_is_intcmp(fg.graph(), if_node, IntCmpOp::Sless, b, a);
    Ok(())
}

/// Decomposed GT with one leaf reading a third register, `(a != b) && !(a < c)`,
/// so the shared `(a, b)` capture cannot bind.  Nothing may fire.
#[test]
fn flag_cmp_incomplete_tree_foreign_leaf_left_alone() -> Result<()> {
    use strider_ir_test_utils::IrWalkerEx;
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
            let eq = fb.build_int_cmp_operation(a, b, IntCmpOp::Equal, ValueType::I32)?;
            let neq = build_i1_xor_with_one(fb, eq)?;
            let lt = fb.build_int_cmp_operation(a, c, IntCmpOp::Sless, ValueType::I32)?;
            let nlt = build_i1_xor_with_one(fb, lt)?;
            let cond = fb.build_int_binary_operation(neq, nlt, IntBinaryOp::And, ValueType::I1)?;
            Ok((cond, ()))
        })?;

    let cmp_count_before = fg.count_kind(|k| matches!(k, NodeKind::IntCmpOp(_)));
    let r = crate::pipeline::run_one(
        &FlagCmpCanonicalize::new(),
        &mut fg,
        &mut crate::OptCtx::new(None),
    )?;
    assert!(
        !r.changed(),
        "mismatched-capture flag tree must not canonicalize"
    );
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

#[test]
fn flag_cmp_decomposed_le_rewrites_to_neg_sless_swapped() -> Result<()> {
    // (a == b) || (a < b)  ≡  a <= b  ≡  !(b < a)  ->  BitNot(Sless(b, a))
    let (mut fg, if_node, a, b) = build_if_with_ab_cond(|fb, a, b| {
        let eq = fb.build_int_cmp_operation(a, b, IntCmpOp::Equal, ValueType::I32)?;
        let lt = fb.build_int_cmp_operation(a, b, IntCmpOp::Sless, ValueType::I32)?;
        fb.build_int_binary_operation(eq, lt, IntBinaryOp::Or, ValueType::I1)
    })?;
    let r = crate::pipeline::run_one(
        &FlagCmpCanonicalize::new(),
        &mut fg,
        &mut crate::OptCtx::new(None),
    )?;
    assert!(r.changed(), "decomposed LE should canonicalize");
    assert_if_cond_is_neg_intcmp(&fg, if_node, IntCmpOp::Sless, b, a);
    Ok(())
}

#[test]
fn flag_cmp_decomposed_hi_rewrites_to_less_swapped() -> Result<()> {
    // unsigned: (a != b) && !(a < b)  ->  Less(b, a)
    let (mut fg, if_node, a, b) = build_if_with_ab_cond(|fb, a, b| {
        let eq = fb.build_int_cmp_operation(a, b, IntCmpOp::Equal, ValueType::I32)?;
        let neq = build_i1_xor_with_one(fb, eq)?;
        let lt = fb.build_int_cmp_operation(a, b, IntCmpOp::Less, ValueType::I32)?;
        let nlt = build_i1_xor_with_one(fb, lt)?;
        fb.build_int_binary_operation(neq, nlt, IntBinaryOp::And, ValueType::I1)
    })?;
    let r = crate::pipeline::run_one(
        &FlagCmpCanonicalize::new(),
        &mut fg,
        &mut crate::OptCtx::new(None),
    )?;
    assert!(r.changed(), "decomposed HI should canonicalize");
    assert_if_cond_is_intcmp(fg.graph(), if_node, IntCmpOp::Less, b, a);
    Ok(())
}

#[test]
fn flag_cmp_decomposed_ls_rewrites_to_neg_less_swapped() -> Result<()> {
    // unsigned: (a == b) || (a < b)  ->  BitNot(Less(b, a))
    let (mut fg, if_node, a, b) = build_if_with_ab_cond(|fb, a, b| {
        let eq = fb.build_int_cmp_operation(a, b, IntCmpOp::Equal, ValueType::I32)?;
        let lt = fb.build_int_cmp_operation(a, b, IntCmpOp::Less, ValueType::I32)?;
        fb.build_int_binary_operation(eq, lt, IntBinaryOp::Or, ValueType::I1)
    })?;
    let r = crate::pipeline::run_one(
        &FlagCmpCanonicalize::new(),
        &mut fg,
        &mut crate::OptCtx::new(None),
    )?;
    assert!(r.changed(), "decomposed LS should canonicalize");
    assert_if_cond_is_neg_intcmp(&fg, if_node, IntCmpOp::Less, b, a);
    Ok(())
}

// `cmp idx, N; ja` lifts the unsigned LS tree `(idx < N) || (idx == N)`, but
// ConstantFold folds the ZF term's `Neg(IntConst(N))` to `IntConst(-N)`, giving
// `Equal(Add(idx, IntConst(-N)), 0)`.  Neither the EQ rule nor the plain
// decomposed-LS rule matches that, hence the dedicated constant-folded rules.

/// Builds `Or(Less(idx, N), Equal(Add(idx, -N), 0))` at `ty` and asserts it
/// folds to `¬Less(N, idx)`.
fn check_folded_ls_tree(ty: ValueType, n: u64) -> Result<()> {
    let idx_vn = strider_ir_test_utils::reg_vn(0x1000, ty.byte_size() as u32);
    let (mut fg, if_node, idx, n_const) = {
        let (fg, if_node, (idx, n_const)) = RegisterSet::new()
            .tracked(idx_vn)
            .build_if_then_else_returns(|fb| {
                let idx = fb.read_variable(&idx_vn)?;
                let n_const = fb.build_int_const(u128::from(n), ty)?;
                let less = fb.build_int_cmp_operation(idx, n_const, IntCmpOp::Less, ty)?;
                // The constant-folded ZF term.
                let neg_n = (0u128).wrapping_sub(u128::from(n));
                let neg_n_const = fb.build_int_const(neg_n, ty)?;
                let diff = fb.build_int_binary_operation(idx, neg_n_const, IntBinaryOp::Add, ty)?;
                let zero = fb.build_int_const(0u128, ty)?;
                let eq = fb.build_int_cmp_operation(diff, zero, IntCmpOp::Equal, ty)?;
                let cond =
                    fb.build_int_binary_operation(less, eq, IntBinaryOp::Or, ValueType::I1)?;
                Ok((cond, (idx, n_const)))
            })?;
        (fg, if_node, idx, n_const)
    };

    let r = crate::pipeline::run_one(
        &FlagCmpCanonicalize::new(),
        &mut fg,
        &mut crate::OptCtx::new(None),
    )?;
    assert!(
        r.changed(),
        "{ty:?} N={n}: constant-folded LS tree should canonicalize"
    );
    // `n_const` (not a fresh node): the rule reuses the captured constant.
    assert_if_cond_is_neg_intcmp(&fg, if_node, IntCmpOp::Less, n_const, idx);
    Ok(())
}

#[test]
fn flag_cmp_constant_folded_ls_tree_i32() -> Result<()> {
    check_folded_ls_tree(ValueType::I32, 8)
}

#[test]
fn flag_cmp_constant_folded_ls_tree_i64() -> Result<()> {
    check_folded_ls_tree(ValueType::I64, 3)
}

#[test]
fn flag_cmp_constant_folded_ls_tree_wrapping_neg() -> Result<()> {
    // N = 1 makes -N all-ones at the width; the guard compares mod width, so
    // it must still match.
    check_folded_ls_tree(ValueType::I32, 1)?;
    check_folded_ls_tree(ValueType::I64, 1)
}

// A switch whose cases start at a nonzero base `K`: gcc emits
// `sub b, K; cmp (b-K), N; ja`, so the compared value is the offset index
// `X = Add(b, -K)`, while the ZF term folds to `Equal(Add(b, C2), 0)`.  The
// `Less` operand and the `Equal` base are therefore distinct nodes, which the
// non-offset rules cannot bind.

/// Builds `Or(Less(Add(b, C1), N), Equal(Add(b, C1 - N), 0))` and asserts it
/// folds to `¬Less(N, Add(b, C1))` on the offset index.
fn check_offset_folded_ls_tree(ty: ValueType, c1: u128, n: u64) -> Result<()> {
    let b_vn = strider_ir_test_utils::reg_vn(0x1000, ty.byte_size() as u32);
    let (mut fg, if_node, x_val, n_const) = {
        let (fg, if_node, (x_val, n_const)) = RegisterSet::new()
            .tracked(b_vn)
            .build_if_then_else_returns(|fb| {
                let b = fb.read_variable(&b_vn)?;
                let c1_const = fb.build_int_const(c1, ty)?;
                let x = fb.build_int_binary_operation(b, c1_const, IntBinaryOp::Add, ty)?;
                let n_const = fb.build_int_const(u128::from(n), ty)?;
                let less = fb.build_int_cmp_operation(x, n_const, IntCmpOp::Less, ty)?;
                // The folded ZF term is based on the SAME `b`, with C2 = C1 - N.
                let c2 = c1.wrapping_sub(u128::from(n));
                let c2_const = fb.build_int_const(c2, ty)?;
                let y = fb.build_int_binary_operation(b, c2_const, IntBinaryOp::Add, ty)?;
                let zero = fb.build_int_const(0u128, ty)?;
                let eq = fb.build_int_cmp_operation(y, zero, IntCmpOp::Equal, ty)?;
                let cond =
                    fb.build_int_binary_operation(less, eq, IntBinaryOp::Or, ValueType::I1)?;
                Ok((cond, (x, n_const)))
            })?;
        (fg, if_node, x_val, n_const)
    };

    let r = crate::pipeline::run_one(
        &FlagCmpCanonicalize::new(),
        &mut fg,
        &mut crate::OptCtx::new(None),
    )?;
    assert!(
        r.changed(),
        "{ty:?} C1={c1} N={n}: offset-base LS tree should canonicalize"
    );
    // `x_val` (not a fresh node): the rule reuses the captured offset index.
    assert_if_cond_is_neg_intcmp(&fg, if_node, IntCmpOp::Less, n_const, x_val);
    Ok(())
}

#[test]
fn flag_cmp_offset_folded_ls_tree_i32() -> Result<()> {
    // Cases 10..=17: index `X = b - 10`, range bound `N = 7`.
    check_offset_folded_ls_tree(ValueType::I32, (0u128).wrapping_sub(10), 7)
}

#[test]
fn flag_cmp_offset_folded_ls_tree_i64() -> Result<()> {
    check_offset_folded_ls_tree(ValueType::I64, (0u128).wrapping_sub(100), 5)
}

// The De-Morgan dual of the constant-folded LS case: Thumb
// `cmp idx, N; bhi default` lifts `(idx >= N) AND (idx != N)`.  Neither the raw
// nor the decomposed HI rule matches once the ZF term is folded, since both
// expect it as `Equal(a, b)`.

/// Builds `And(¬Less(idx, N), ¬Equal(Add(idx, -N), 0))` and asserts it folds to
/// `Less(N, idx)`.
fn check_folded_hi_tree(ty: ValueType, n: u64) -> Result<()> {
    let idx_vn = strider_ir_test_utils::reg_vn(0x1000, ty.byte_size() as u32);
    let (mut fg, if_node, idx, n_const) = {
        let (fg, if_node, (idx, n_const)) = RegisterSet::new()
            .tracked(idx_vn)
            .build_if_then_else_returns(|fb| {
                let idx = fb.read_variable(&idx_vn)?;
                let n_const = fb.build_int_const(u128::from(n), ty)?;
                let less = fb.build_int_cmp_operation(idx, n_const, IntCmpOp::Less, ty)?;
                let neg_less = build_i1_xor_with_one(fb, less)?;
                // The constant-folded ZF term.
                let neg_n = (0u128).wrapping_sub(u128::from(n));
                let neg_n_const = fb.build_int_const(neg_n, ty)?;
                let diff = fb.build_int_binary_operation(idx, neg_n_const, IntBinaryOp::Add, ty)?;
                let zero = fb.build_int_const(0u128, ty)?;
                let eq = fb.build_int_cmp_operation(diff, zero, IntCmpOp::Equal, ty)?;
                let neg_eq = build_i1_xor_with_one(fb, eq)?;
                let cond = fb.build_int_binary_operation(
                    neg_less,
                    neg_eq,
                    IntBinaryOp::And,
                    ValueType::I1,
                )?;
                Ok((cond, (idx, n_const)))
            })?;
        (fg, if_node, idx, n_const)
    };

    let r = crate::pipeline::run_one(
        &FlagCmpCanonicalize::new(),
        &mut fg,
        &mut crate::OptCtx::new(None),
    )?;
    assert!(
        r.changed(),
        "{ty:?} N={n}: constant-folded HI tree should canonicalize"
    );
    // `n_const` (not a fresh node): the rule reuses the captured constant.
    assert_if_cond_is_intcmp(fg.graph(), if_node, IntCmpOp::Less, n_const, idx);
    Ok(())
}

#[test]
fn flag_cmp_constant_folded_hi_tree_i32() -> Result<()> {
    check_folded_hi_tree(ValueType::I32, 7)
}

#[test]
fn flag_cmp_constant_folded_hi_tree_i64() -> Result<()> {
    check_folded_hi_tree(ValueType::I64, 3)
}

// The offset-base HI case: a masked / offset switch, e.g. Thumb
// `and r0,#7; subs r0,#1; cmp r0,#N-1; bhi`.

/// Builds `And(¬Less(Add(b,C1), N), ¬Equal(Add(b, C1-N), 0))` and asserts it
/// folds to `Less(N, Add(b,C1))`.
fn check_offset_folded_hi_tree(ty: ValueType, c1: u128, n: u64) -> Result<()> {
    let b_vn = strider_ir_test_utils::reg_vn(0x1000, ty.byte_size() as u32);
    let (mut fg, if_node, x_val, n_const) = {
        let (fg, if_node, (x_val, n_const)) = RegisterSet::new()
            .tracked(b_vn)
            .build_if_then_else_returns(|fb| {
                let b = fb.read_variable(&b_vn)?;
                let c1_const = fb.build_int_const(c1, ty)?;
                let x = fb.build_int_binary_operation(b, c1_const, IntBinaryOp::Add, ty)?;
                let n_const = fb.build_int_const(u128::from(n), ty)?;
                let less = fb.build_int_cmp_operation(x, n_const, IntCmpOp::Less, ty)?;
                let neg_less = build_i1_xor_with_one(fb, less)?;
                let c2 = c1.wrapping_sub(u128::from(n));
                let c2_const = fb.build_int_const(c2, ty)?;
                let y = fb.build_int_binary_operation(b, c2_const, IntBinaryOp::Add, ty)?;
                let zero = fb.build_int_const(0u128, ty)?;
                let eq = fb.build_int_cmp_operation(y, zero, IntCmpOp::Equal, ty)?;
                let neg_eq = build_i1_xor_with_one(fb, eq)?;
                let cond = fb.build_int_binary_operation(
                    neg_less,
                    neg_eq,
                    IntBinaryOp::And,
                    ValueType::I1,
                )?;
                Ok((cond, (x, n_const)))
            })?;
        (fg, if_node, x_val, n_const)
    };

    let r = crate::pipeline::run_one(
        &FlagCmpCanonicalize::new(),
        &mut fg,
        &mut crate::OptCtx::new(None),
    )?;
    assert!(
        r.changed(),
        "{ty:?} C1={c1} N={n}: offset-base HI tree should canonicalize"
    );
    assert_if_cond_is_intcmp(fg.graph(), if_node, IntCmpOp::Less, n_const, x_val);
    Ok(())
}

#[test]
fn flag_cmp_offset_folded_hi_tree_i32() -> Result<()> {
    // Index `X = (kind & 7) - 1`, range bound `N = 6`.
    check_offset_folded_hi_tree(ValueType::I32, (0u128).wrapping_sub(1), 6)
}

#[test]
fn flag_cmp_offset_folded_hi_tree_i64() -> Result<()> {
    check_offset_folded_hi_tree(ValueType::I64, (0u128).wrapping_sub(20), 5)
}

#[test]
fn flag_cmp_offset_folded_ls_tree_rejects_wrong_offset() -> Result<()> {
    // With C2 != C1 - N the Equal does not test `X == N`, so the guard must
    // reject.
    let ty = ValueType::I32;
    let b_vn = strider_ir_test_utils::reg_vn(0x1000, 4);
    let c1 = (0u128).wrapping_sub(10);
    let n = 7u64;
    let mut fg = RegisterSet::new()
        .tracked(b_vn)
        .build_if_then_else_returns(|fb| {
            let b = fb.read_variable(&b_vn)?;
            let c1_const = fb.build_int_const(c1, ty)?;
            let x = fb.build_int_binary_operation(b, c1_const, IntBinaryOp::Add, ty)?;
            let n_const = fb.build_int_const(u128::from(n), ty)?;
            let less = fb.build_int_cmp_operation(x, n_const, IntCmpOp::Less, ty)?;
            // Deliberately off by one: C2 should be C1 - N.
            let c2 = c1.wrapping_sub(u128::from(n)).wrapping_add(1);
            let c2_const = fb.build_int_const(c2, ty)?;
            let y = fb.build_int_binary_operation(b, c2_const, IntBinaryOp::Add, ty)?;
            let zero = fb.build_int_const(0u128, ty)?;
            let eq = fb.build_int_cmp_operation(y, zero, IntCmpOp::Equal, ty)?;
            let cond = fb.build_int_binary_operation(less, eq, IntBinaryOp::Or, ValueType::I1)?;
            Ok((cond, ()))
        })
        .map(|(fg, _, ())| fg)?;

    crate::pipeline::run_one(
        &FlagCmpCanonicalize::new(),
        &mut fg,
        &mut crate::OptCtx::new(None),
    )?;
    // Asserts the idiom survived rather than "nothing changed": the inner
    // `Equal(Add(b,C2),0)` is still reshaped to `Equal(b,-C2)` by the
    // compare-with-const rule, which is value-preserving and not the LS fold.
    let if_node = fg
        .walk()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::If))
        .expect("If node");
    let cond_node = fg.producer(fg.if_cond(if_node));
    assert!(
        matches!(
            fg.node_kind(cond_node),
            NodeKind::IntBinaryOp(IntBinaryOp::Or)
        ),
        "wrong-offset LS tree must NOT fold to a single comparison; got {:?}",
        fg.node_kind(cond_node)
    );
    Ok(())
}

/// `Equal(Add(x, C1), C2) -> Equal(x, C2 - C1)`.  The width assertion is the
/// point: the fresh const must take the operand width (I32), not the `Equal`
/// root's `I1` output width.
#[test]
fn eq_add_const_solves_for_x() -> Result<()> {
    let ty = ValueType::I32;
    let mut b = RegisterSet::new().build_fn()?;
    b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
    let entry = b.create_region_all()?;
    let dispatch = b.create_region_all()?;
    let exit = b.create_region_all()?;
    b.set_entry_region_all(entry)?;
    b.set_region(entry);

    let dummy = b.build_int_const(0xF00Du64, ValueType::I64)?;
    let x = b.build_load(dummy, rsleigh::VnSpace::RAM, ty)?;
    let c3 = b.build_int_const(3u64, ty)?;
    let c4 = b.build_int_const(4u64, ty)?;
    let add = b.build_int_binary_operation(x, c3, IntBinaryOp::Add, ty)?;
    let eq = b.build_int_cmp_operation(add, c4, IntCmpOp::Equal, ty)?;
    b.build_if(eq, dispatch, exit)?;
    b.set_region(dispatch);
    b.build_return(Some(x), &[])?;
    b.set_region(exit);
    b.build_return(Some(x), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    crate::pipeline::run_one(
        &FlagCmpCanonicalize::new(),
        &mut fg,
        &mut crate::OptCtx::new(None),
    )?;

    let if_node = fg
        .walk()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::If))
        .expect("If node");
    let cond = fg.if_cond(if_node);
    let cond_node = fg.producer(cond);
    assert!(
        matches!(fg.node_kind(cond_node), NodeKind::IntCmpOp(IntCmpOp::Equal)),
        "cond must stay an Equal, got {:?}",
        fg.node_kind(cond_node)
    );
    let inputs = fg.node_inputs(cond_node);
    let (l, r) = (inputs[0], inputs[1]);
    let const_is_one_i32 = |o: ValueId| {
        matches!(fg.kind_of_value(o), NodeKind::IntConst(_))
            && fg.int_const_u128(o) == Some(1)
            && fg.value_type_opt(o) == Some(ValueType::I32)
    };
    let ok = (l == x && const_is_one_i32(r)) || (r == x && const_is_one_i32(l));
    assert!(
        ok,
        "expected Equal(x, IntConst(1):I32); got lhs={:?}, rhs={:?}",
        fg.kind_of_value(l),
        fg.kind_of_value(r)
    );
    strider_ir::validate::validate(&fg)?;
    Ok(())
}

/// `Equal(Xor(x, C1), C2) -> Equal(x, C1 ^ C2)`: `xor(x,3) == 5` gives `x == 6`.
#[test]
fn eq_xor_const_solves_for_x() -> Result<()> {
    let ty = ValueType::I32;
    let mut b = RegisterSet::new().build_fn()?;
    b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
    let entry = b.create_region_all()?;
    let dispatch = b.create_region_all()?;
    let exit = b.create_region_all()?;
    b.set_entry_region_all(entry)?;
    b.set_region(entry);

    let dummy = b.build_int_const(0xF00Du64, ValueType::I64)?;
    let x = b.build_load(dummy, rsleigh::VnSpace::RAM, ty)?;
    let c3 = b.build_int_const(3u64, ty)?;
    let c5 = b.build_int_const(5u64, ty)?;
    let xored = b.build_int_binary_operation(x, c3, IntBinaryOp::Xor, ty)?;
    let eq = b.build_int_cmp_operation(xored, c5, IntCmpOp::Equal, ty)?;
    b.build_if(eq, dispatch, exit)?;
    b.set_region(dispatch);
    b.build_return(Some(x), &[])?;
    b.set_region(exit);
    b.build_return(Some(x), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    crate::pipeline::run_one(
        &FlagCmpCanonicalize::new(),
        &mut fg,
        &mut crate::OptCtx::new(None),
    )?;

    let if_node = fg
        .walk()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::If))
        .expect("If");
    let cond_node = fg.producer(fg.if_cond(if_node));
    assert!(matches!(
        fg.node_kind(cond_node),
        NodeKind::IntCmpOp(IntCmpOp::Equal)
    ));
    let inputs: Vec<_> = fg.node_inputs(cond_node).into_iter().collect();
    assert!(inputs.contains(&x), "operand must be x (xor stripped)");
    assert!(
        inputs.iter().any(|&o| fg.int_const_u128(o) == Some(6)),
        "const must be 3 ^ 5 = 6"
    );
    strider_ir::validate::validate(&fg)?;
    Ok(())
}

/// `Equal(Neg(x), C) -> Equal(x, -C)`: `-x == 5` gives `x == -5`, masked to I32.
#[test]
fn eq_neg_solves_for_x() -> Result<()> {
    let ty = ValueType::I32;
    let mut b = RegisterSet::new().build_fn()?;
    b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
    let entry = b.create_region_all()?;
    let dispatch = b.create_region_all()?;
    let exit = b.create_region_all()?;
    b.set_entry_region_all(entry)?;
    b.set_region(entry);

    let dummy = b.build_int_const(0xF00Du64, ValueType::I64)?;
    let x = b.build_load(dummy, rsleigh::VnSpace::RAM, ty)?;
    let c5 = b.build_int_const(5u64, ty)?;
    let negated = b.build_int_unary_operation(x, IntUnaryOp::Neg, ty)?;
    let eq = b.build_int_cmp_operation(negated, c5, IntCmpOp::Equal, ty)?;
    b.build_if(eq, dispatch, exit)?;
    b.set_region(dispatch);
    b.build_return(Some(x), &[])?;
    b.set_region(exit);
    b.build_return(Some(x), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    crate::pipeline::run_one(
        &FlagCmpCanonicalize::new(),
        &mut fg,
        &mut crate::OptCtx::new(None),
    )?;

    let if_node = fg
        .walk()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::If))
        .expect("If");
    let cond_node = fg.producer(fg.if_cond(if_node));
    let inputs: Vec<_> = fg.node_inputs(cond_node).into_iter().collect();
    assert!(inputs.contains(&x), "operand must be x (neg stripped)");
    assert!(
        inputs
            .iter()
            .any(|&o| fg.int_const_u128(o) == Some(0xFFFF_FFFB)),
        "const must be -5 masked to I32 = 0xFFFF_FFFB"
    );
    strider_ir::validate::validate(&fg)?;
    Ok(())
}

/// `Sless(x << C, 0):I1 -> Xor(Equal(And(x, mask), 0), 1):I1`, mask=1<<(W-1-C).
/// Here W=32, C=3, so mask = 1<<28 = 0x1000_0000.
#[test]
fn sless_of_left_shift_is_a_sign_bit_test() -> Result<()> {
    let ty = ValueType::I32;
    let mut b = RegisterSet::new().build_fn()?;
    b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
    let entry = b.create_region_all()?;
    let dispatch = b.create_region_all()?;
    let exit = b.create_region_all()?;
    b.set_entry_region_all(entry)?;
    b.set_region(entry);

    let dummy = b.build_int_const(0xF00Du64, ValueType::I64)?;
    let x = b.build_load(dummy, rsleigh::VnSpace::RAM, ty)?;
    let c3 = b.build_int_const(3u64, ty)?;
    let shl = b.build_int_binary_operation(x, c3, IntBinaryOp::ShiftLeft, ty)?;
    let zero = b.build_int_const(0u64, ty)?;
    let sless = b.build_int_cmp_operation(shl, zero, IntCmpOp::Sless, ty)?;
    b.build_if(sless, dispatch, exit)?;
    b.set_region(dispatch);
    b.build_return(Some(x), &[])?;
    b.set_region(exit);
    b.build_return(Some(x), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    crate::pipeline::run_one(
        &FlagCmpCanonicalize::new(),
        &mut fg,
        &mut crate::OptCtx::new(None),
    )?;

    let if_node = fg
        .walk()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::If))
        .expect("If");
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
    strider_ir::validate::validate(&fg)?;
    Ok(())
}

/// At or above the width `x << 40` is 0, making the test const-false, which is
/// a different rewrite; the sign-bit canonicalization must stay out of it.
#[test]
fn sless_of_oversized_left_shift_is_not_a_sign_bit_test() -> Result<()> {
    let ty = ValueType::I32;
    let mut b = RegisterSet::new().build_fn()?;
    b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
    let entry = b.create_region_all()?;
    let dispatch = b.create_region_all()?;
    let exit = b.create_region_all()?;
    b.set_entry_region_all(entry)?;
    b.set_region(entry);
    let dummy = b.build_int_const(0xF00Du64, ValueType::I64)?;
    let x = b.build_load(dummy, rsleigh::VnSpace::RAM, ty)?;
    let c40 = b.build_int_const(40u64, ty)?;
    let shl = b.build_int_binary_operation(x, c40, IntBinaryOp::ShiftLeft, ty)?;
    let zero = b.build_int_const(0u64, ty)?;
    let sless = b.build_int_cmp_operation(shl, zero, IntCmpOp::Sless, ty)?;
    b.build_if(sless, dispatch, exit)?;
    b.set_region(dispatch);
    b.build_return(Some(x), &[])?;
    b.set_region(exit);
    b.build_return(Some(x), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    crate::pipeline::run_one(
        &FlagCmpCanonicalize::new(),
        &mut fg,
        &mut crate::OptCtx::new(None),
    )?;
    let if_node = fg
        .walk()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::If))
        .expect("If");
    assert!(
        matches!(
            fg.node_kind(fg.producer(fg.if_cond(if_node))),
            NodeKind::IntCmpOp(IntCmpOp::Sless)
        ),
        "oversized-shift Sless must be left intact"
    );
    Ok(())
}
