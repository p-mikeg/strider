//! Unit tests for [`super::FlagCmpCanonicalize`].
//!
//! Each test builds a small IR fixture that mimics what AArch64's lift
//! produces for `cmp a, b; b.<cond>` — i.e. a flag-tree on the `If`'s cond
//! input — and asserts the pass rewrites the cond to a single canonical
//! `IntCmpOp` node consuming the original `(a, b)` pair.

use super::FlagCmpCanonicalize;
use crate::error::Result;
use crate::pipeline::OptimizerTestExt;
use strider_ir::{IRBuilderExt, IRViewer};

use strider_ir::node::{NodeId, NodeKind, ValueId, ValueType};
use strider_ir::{FunctionBuilder, Graph, IRWalker, IntBinaryOp, IntCmpOp, IntUnaryOp};
use strider_ir_test_utils::RegisterSet;

/// PowerPC `cmpwi` packs LT/GT/EQ/SO into a CR field; the branch extracts one
/// bit via `Truncate(ShiftRight(cr_pack, k)):I1`.  FlagCmpCanonicalize must
/// rewrite that to the bare comparison sitting at the tested bit — the same
/// `IntCmpOp` form every other arch's branch produces.
#[test]
fn ppc_cr_bit_test_canonicalizes_to_intcmp() -> Result<()> {
    use strider_ir::node::ExtendOp;
    let ty = ValueType::I32;
    let mut b = RegisterSet::new().build_fn()?;
    b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
    let entry = b.create_region()?;
    let dispatch = b.create_region()?;
    let exit = b.create_region()?;
    b.set_entry_region(entry)?;
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

    FlagCmpCanonicalize::new().run_one(&mut fg, &mut crate::OptCtx::new(None))?;

    let if_node = fg
        .walk()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::If))
        .expect("If node");
    // Bit 3 (LT) of the CR pack is `Less(idx, 8)`.
    assert_if_cond_is_intcmp(fg.graph(), if_node, IntCmpOp::Less, idx, eight);
    Ok(())
}

/// The CR-pack interior instructions (the `crset`/`cror`/`cmpwi` that build the
/// field, plus the `Or`/`Shift` structure) must keep their asm addresses in the
/// surviving comparison's fingerprint after canonicalization — the superset-only
/// contract.  Stamps the comparison, the pack structure, and the final
/// `Truncate` with three DISTINCT addresses; `replace_value` alone carries only
/// the comparison's own + the `Truncate`'s, dropping the pack's `ADDR_PACK`.
#[test]
fn ppc_cr_bit_canonicalize_preserves_pack_fingerprints() -> Result<()> {
    use strider_ir::node::ExtendOp;
    const ADDR_CMP: u64 = 0x1111;
    const ADDR_PACK: u64 = 0x2222;
    const ADDR_TRUNC: u64 = 0x3333;
    let ty = ValueType::I32;
    let mut b = RegisterSet::new().build_fn()?;
    let entry = b.create_region()?;
    let dispatch = b.create_region()?;
    let exit = b.create_region()?;
    b.set_entry_region(entry)?;
    b.set_region(entry);

    // The tested bit's comparison (and its operands) under ADDR_CMP.
    b.set_lift_addr(Some(ADDR_CMP));
    let dummy = b.build_int_const(0xF00Du64, ValueType::I64)?;
    let idx = b.build_load(dummy, rsleigh::VnSpace::RAM, ty)?;
    let eight = b.build_int_const(8u64, ty)?;
    let lt = b.build_int_cmp_operation(idx, eight, IntCmpOp::Less, ty)?;

    // The rest of the CR pack (other comparisons + Or/Shift structure) under
    // ADDR_PACK — these are the addresses that must NOT be dropped.
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

    // The final bit-extract Truncate under ADDR_TRUNC.
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

    FlagCmpCanonicalize::new().run_one(&mut fg, &mut crate::OptCtx::new(None))?;

    // The surviving comparison is the bit-3 `Less(idx, 8)` (the only reachable
    // `Less` once the pack is culled).
    let cmp_node = fg
        .walk()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::IntCmpOp(IntCmpOp::Less)))
        .expect("the canonicalized comparison survives");
    let fp = fg.asm_fingerprint(cmp_node);
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

/// Same CR pack, but the branch tests the EQ bit (bit 1) — exercises selecting a
/// MIDDLE term, where the `ShiftRight` amount (1) must line up with that term's
/// `ShiftLeft` position (1), not the highest-set one.
#[test]
fn ppc_cr_bit_test_selects_middle_eq_bit() -> Result<()> {
    use strider_ir::node::ExtendOp;
    let ty = ValueType::I32;
    let mut b = RegisterSet::new().build_fn()?;
    b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
    let entry = b.create_region()?;
    let dispatch = b.create_region()?;
    let exit = b.create_region()?;
    b.set_entry_region(entry)?;
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

    FlagCmpCanonicalize::new().run_one(&mut fg, &mut crate::OptCtx::new(None))?;

    let if_node = fg
        .walk()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::If))
        .expect("If node");
    assert_if_cond_is_intcmp(fg.graph(), if_node, IntCmpOp::Equal, idx, eight);
    Ok(())
}

/// Builds the canonical 1-bit logical NOT shape `Xor(operand, IntConst(1)):I1`
/// (post-removal-of-the former BitNot unary-op).
fn build_i1_xor_with_one(fb: &mut FunctionBuilder, operand: ValueId) -> Result<ValueId> {
    let one = fb.build_int_const(u128::MAX, ValueType::I1)?;
    fb.build_int_binary_operation(operand, one, IntBinaryOp::Xor, ValueType::I1)
}

/// True when `node` is the canonical 1-bit logical NOT shape — an
/// `IntBinaryOp::Xor` at `I1` whose RHS (or LHS) is `IntConst(1):I1`.
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

// ── Common fixture builder ────────────────────────────────────────────────

/// Build the canonical AArch64 cmp shape for `a` and `b` and return
/// `(zr, ng, cy, ov, a, b)` — the four flag outputs and the two
/// register reads — so individual tests can wire whichever subset
/// each cond code needs onto the `If`.
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
    // CY = Xor(IntLess(a, b), IntConst(1)):I1  — post lift-time canonicalisation of IntLessEqual(b, a).
    // A logical NOT is `Xor(_, IntConst(1)):I1` (since the former BitNot unary-op
    // was removed in favour of `Xor(_, all_ones)`).
    let alt = fb.build_int_cmp_operation(a, b, IntCmpOp::Less, ValueType::I32)?;
    let one_i1 = fb.build_int_const(u128::MAX, ValueType::I1)?;
    let cy = fb.build_int_binary_operation(alt, one_i1, IntBinaryOp::Xor, ValueType::I1)?;
    let ov = fb.build_int_cmp_operation(a, b, IntCmpOp::Sborrow, ValueType::I32)?;

    Ok((zr, ng, cy, ov))
}

/// Build an entry region that reads two 32-bit register values, computes
/// the four AArch64 flag values, and uses the supplied closure to derive
/// the `If` cond from those flags.  Then build a trivial true/false
/// region pair and return the graph + the unique If node id + the two
/// leaves `a`, `b` (so tests can assert the rewritten cond points at
/// them).
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

/// Asserts that the captured If's cond is `IntCmpOp(op)` with inputs
/// `(expect_lhs, expect_rhs)` in that exact order.
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

// ── Tests ─────────────────────────────────────────────────────────────────

/// Asserts the If's cond is `Xor(IntCmpOp(op, lhs, rhs), IntConst(1)):I1`.
/// Used for the cond shapes whose post-rewrite canonical form is a
/// negated cmp (i.e. a 1-bit logical NOT of the cmp); `IfCondInversion`
/// (in the full pipeline, not in this test) is the pass that finally
/// swaps the If's branches and strips the Xor-with-1.  the former BitNot unary-op
/// was removed in favour of this canonical Xor-with-all-ones shape.
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
    // The non-constant operand is the cmp; the other is the I1
    // IntConst(1) (might be on either side due to dedup).
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

// ── constructed-with-data: per-instance rule ownership ────────────────────

/// A pass built via [`FlagCmpCanonicalize::new`] owns its rule table and
/// canonicalises the same representative EQ flag tree the bare-value form did.
#[test]
fn new_builds_pass_that_canonicalizes() -> Result<()> {
    let (mut fg, if_node, a, b) = build_if_with_flag_cond(|_fb, zr, _ng, _cy, _ov| Ok(zr))?;

    let r = FlagCmpCanonicalize::new().run_one(&mut fg, &mut crate::OptCtx::new(None))?;
    assert!(
        r.changed(),
        "constructed pass should rewrite the EQ flag tree"
    );
    assert_if_cond_is_intcmp(fg.graph(), if_node, IntCmpOp::Equal, a, b);
    Ok(())
}

/// Two independently-constructed instances each own their own rule table —
/// proving the data is per-instance, not a shared thread-local.  Running one
/// then a fresh second on equivalent fixtures both produce the same rewrite.
#[test]
fn two_independent_instances_each_canonicalize() -> Result<()> {
    let pass_a = FlagCmpCanonicalize::new();
    let pass_b = FlagCmpCanonicalize::new();

    let (mut fg_a, if_a, a_a, b_a) = build_if_with_flag_cond(|_fb, zr, _ng, _cy, _ov| Ok(zr))?;
    assert!(
        pass_a
            .run_one(&mut fg_a, &mut crate::OptCtx::new(None))?
            .changed()
    );
    assert_if_cond_is_intcmp(fg_a.graph(), if_a, IntCmpOp::Equal, a_a, b_a);

    let (mut fg_b, if_b, a_b, b_b) = build_if_with_flag_cond(|_fb, zr, _ng, _cy, _ov| Ok(zr))?;
    assert!(
        pass_b
            .run_one(&mut fg_b, &mut crate::OptCtx::new(None))?
            .changed()
    );
    assert_if_cond_is_intcmp(fg_b.graph(), if_b, IntCmpOp::Equal, a_b, b_b);
    Ok(())
}

#[test]
fn flag_cmp_eq_rewrites_to_int_equal() -> Result<()> {
    // AArch64 `b.eq` cond is the bare ZR flag = `Equal(Add(a, Neg(b)), 0)`.
    let (mut fg, if_node, a, b) = build_if_with_flag_cond(|_fb, zr, _ng, _cy, _ov| Ok(zr))?;

    let r = FlagCmpCanonicalize::new().run_one(&mut fg, &mut crate::OptCtx::new(None))?;
    assert!(r.changed(), "pass should rewrite the EQ flag tree");

    assert_if_cond_is_intcmp(fg.graph(), if_node, IntCmpOp::Equal, a, b);
    Ok(())
}

#[test]
fn flag_cmp_ne_rewrites_to_neg_int_equal() -> Result<()> {
    // AArch64 `b.ne` cond is `BitNot(ZR)` = `BitNot(Equal(Add(a, Neg(b)), 0))`.
    let (mut fg, if_node, a, b) =
        build_if_with_flag_cond(|fb, zr, _ng, _cy, _ov| build_i1_xor_with_one(fb, zr))?;

    let r = FlagCmpCanonicalize::new().run_one(&mut fg, &mut crate::OptCtx::new(None))?;
    assert!(r.changed(), "pass should rewrite the NE flag tree");

    assert_if_cond_is_neg_intcmp(&fg, if_node, IntCmpOp::Equal, a, b);
    Ok(())
}

#[test]
fn flag_cmp_hi_rewrites_to_int_less_swapped() -> Result<()> {
    // AArch64 `b.hi` cond is `BoolAnd(CY, BitNot(ZR))`.  After ZR is
    // simplified to `Equal(a, b)` and the BoolAnd rule fires, the cond
    // is `IntLess(b, a)` (= `a > b unsigned`).
    let (mut fg, if_node, a, b) = build_if_with_flag_cond(|fb, zr, _ng, cy, _ov| {
        let neg_zr = build_i1_xor_with_one(fb, zr)?;
        fb.build_int_binary_operation(cy, neg_zr, IntBinaryOp::And, ValueType::I1)
    })?;

    let r = FlagCmpCanonicalize::new().run_one(&mut fg, &mut crate::OptCtx::new(None))?;
    assert!(r.changed(), "pass should rewrite the HI flag tree");

    // Note swapped operands: `a > b` becomes `IntLess(b, a)`.
    assert_if_cond_is_intcmp(fg.graph(), if_node, IntCmpOp::Less, b, a);
    Ok(())
}

#[test]
fn flag_cmp_hi_rewrites_after_constant_fold_runs_first() -> Result<()> {
    // C2 regression — pin Rule 2 (HI) shared-capture against the
    // production pipeline order.  `default_pipeline()` runs `ConstantFold`
    // before `FlagCmpCanonicalize`, so this test runs them in the same
    // order and asserts the rewrite still fires.
    //
    // The shared-capture concern: Rule 2's LHS reads `var(a)` / `var(b)`
    // in two subtrees (`IntLess(a, b)` and `Add(a, Neg(b))`).  Both
    // bindings must agree across subtrees.  IR node dedup and
    // ConstantFold's algebraic-only rewrites preserve that agreement —
    // this test pins the contract.
    let (mut fg, if_node, a, b) = build_if_with_flag_cond(|fb, zr, _ng, cy, _ov| {
        let neg_zr = build_i1_xor_with_one(fb, zr)?;
        fb.build_int_binary_operation(cy, neg_zr, IntBinaryOp::And, ValueType::I1)
    })?;

    crate::ConstantFold::new().run_one(&mut fg, &mut crate::OptCtx::new(None))?;
    let r = FlagCmpCanonicalize::new().run_one(&mut fg, &mut crate::OptCtx::new(None))?;
    assert!(
        r.changed(),
        "HI rewrite must survive a prior ConstantFold pass"
    );
    assert_if_cond_is_intcmp(fg.graph(), if_node, IntCmpOp::Less, b, a);
    Ok(())
}

#[test]
fn flag_cmp_ls_rewrites_to_neg_int_less_swapped() -> Result<()> {
    // AArch64 `b.ls` cond is `BoolOr(BitNot(CY), ZR)`.  After CY's
    // canonical form (`BitNot(IntLess(a, b))`) cancels the BitNot via
    // ConstantFold (`BitNot(BitNot(x)) → x` at I1), the inner OR becomes
    // `BoolOr(IntLess(a, b), Equal(a, b))` and our rule rewrites it to
    // `BitNot(IntLess(b, a))`.
    let (mut fg, if_node, a, b) = build_if_with_flag_cond(|fb, zr, _ng, cy, _ov| {
        let neg_cy = build_i1_xor_with_one(fb, cy)?;
        fb.build_int_binary_operation(neg_cy, zr, IntBinaryOp::Or, ValueType::I1)
    })?;

    // Run ConstantFold first to collapse `BitNot(BitNot(IntLess(a, b))) → IntLess(a, b)` at I1.
    crate::ConstantFold::new().run_one(&mut fg, &mut crate::OptCtx::new(None))?;
    let r = FlagCmpCanonicalize::new().run_one(&mut fg, &mut crate::OptCtx::new(None))?;
    assert!(r.changed(), "pass should rewrite the LS flag tree");

    assert_if_cond_is_neg_intcmp(&fg, if_node, IntCmpOp::Less, b, a);
    Ok(())
}

#[test]
fn flag_cmp_lt_rewrites_to_int_sless() -> Result<()> {
    // AArch64 `b.lt` cond is `BitNot(Equal(NG, OV))`.  Real lift passes
    // `insn.inputs[0].size` (1 byte for the flag varnodes) as the operand
    // width to `build_int_cmp_operation`, so the IR has
    // `Equal(CastToInt(NG, I8), CastToInt(OV, I8))`.  The fixture matches.
    let (mut fg, if_node, a, b) = build_if_with_flag_cond(|fb, _zr, ng, _cy, ov| {
        let ng = fb.convert_to_int_if_needed(ng, ValueType::I8)?;
        let ov = fb.convert_to_int_if_needed(ov, ValueType::I8)?;
        let eq = fb.build_int_cmp_operation(ng, ov, IntCmpOp::Equal, ValueType::I8)?;
        build_i1_xor_with_one(fb, eq)
    })?;

    let r = FlagCmpCanonicalize::new().run_one(&mut fg, &mut crate::OptCtx::new(None))?;
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

    let r = FlagCmpCanonicalize::new().run_one(&mut fg, &mut crate::OptCtx::new(None))?;
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

    let r = FlagCmpCanonicalize::new().run_one(&mut fg, &mut crate::OptCtx::new(None))?;
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

    let r = FlagCmpCanonicalize::new().run_one(&mut fg, &mut crate::OptCtx::new(None))?;
    assert!(r.changed(), "pass should rewrite the LE flag tree");

    assert_if_cond_is_neg_intcmp(&fg, if_node, IntCmpOp::Sless, b, a);
    Ok(())
}

// ── Negative tests — flags that should NOT be rewritten ───────────────────

#[test]
fn flag_cmp_cs_is_left_alone_as_bool_neg_int_less() -> Result<()> {
    // Region = bare CY = `BitNot(IntLess(a, b))`.  Already in `(a, b)` form;
    // `IfCondInversion` (a separate pass) handles the outer BitNot.
    let (mut fg, if_node, _a, _b) = build_if_with_flag_cond(|_fb, _zr, _ng, cy, _ov| Ok(cy))?;

    let r = FlagCmpCanonicalize::new().run_one(&mut fg, &mut crate::OptCtx::new(None))?;
    assert!(!r.changed(), "CS already canonical; pass must not fire");

    // CY is the canonical 1-bit Xor-with-1 of IntLess (post lift-time
    // canonicalisation), which the pass leaves untouched.
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
    // MI = bare NG = `IntSless(Add(a, Neg(b)), 0)`.  This is NOT
    // equivalent to `IntSless(a, b)` (subtraction overflow), so the
    // pass must leave it untouched.
    let (mut fg, if_node, _a, _b) = build_if_with_flag_cond(|_fb, _zr, ng, _cy, _ov| Ok(ng))?;

    let r = FlagCmpCanonicalize::new().run_one(&mut fg, &mut crate::OptCtx::new(None))?;
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

// ── ARM Thumb shapes — flag tested against 0:1 ───────────────────────────

#[test]
fn flag_cmp_thumb_beq_reduces_to_int_equal() -> Result<()> {
    // ARM Thumb's `B.EQ` lifts as `IntNotEqual(ZR, 0:1)`.  Lift-time
    // canonicalisation lowers that to `BitNot(IntEqual(CastToInt(ZR), 0))`.
    // After two pass iterations (rule 9 strips the bool-neg-eq-zero, then
    // rule 1 simplifies ZR's `Equal(diff, 0)`), the cond is `Equal(a, b)`.
    let (mut fg, if_node, a, b) = build_if_with_flag_cond(|fb, zr, _ng, _cy, _ov| {
        // Mimic `IntNotEqual(ZR, 0:1)` post-canonicalisation:
        //   BitNot(IntEqual(CastToInt(ZR, I8), 0:I8))
        let zero = fb.build_int_const(0u64, ValueType::I8)?;
        let zr = fb.convert_to_int_if_needed(zr, ValueType::I8)?;
        let eq = fb.build_int_cmp_operation(zr, zero, IntCmpOp::Equal, ValueType::I8)?;
        build_i1_xor_with_one(fb, eq)
    })?;

    // Run my pass twice (or run it once via the pipeline's fixed-point loop).
    // Two iterations let rule 9 fire on the outer BitNot(IntEqual(CastToInt(ZR), 0))
    // first, then rule 1 simplify the inner Equal(diff, 0).
    let _ = FlagCmpCanonicalize::new().run_one(&mut fg, &mut crate::OptCtx::new(None))?;
    let _ = FlagCmpCanonicalize::new().run_one(&mut fg, &mut crate::OptCtx::new(None))?;

    assert_if_cond_is_intcmp(fg.graph(), if_node, IntCmpOp::Equal, a, b);
    Ok(())
}

#[test]
fn flag_cmp_vs_is_left_alone_as_sborrow() -> Result<()> {
    // VS = bare OV = `IntSborrow(a, b)`.  Already in `(a, b)` form,
    // nothing to simplify.
    let (mut fg, if_node, _a, _b) = build_if_with_flag_cond(|_fb, _zr, _ng, _cy, ov| Ok(ov))?;

    let r = FlagCmpCanonicalize::new().run_one(&mut fg, &mut crate::OptCtx::new(None))?;
    assert!(!r.changed(), "VS already canonical; pass must not fire");

    assert_eq!(
        if_cond_node_kind(fg.graph(), if_node),
        NodeKind::IntCmpOp(IntCmpOp::Sborrow),
    );
    Ok(())
}

// ── Decomposed-form rules (10–13) ─────────────────────────────────────────
//
// ARM/Thumb lift comparison branches with inverted sense, so by the time this
// pass runs ConstantFold has already decomposed the flag tree into direct
// comparisons on `(a, b)`.  These tests build that decomposed shape and pin
// both the rewrite and the swapped operand order.

/// Build an If whose cond the closure derives directly from the two register
/// reads `(a, b)` — for the decomposed-form rules whose inputs are plain
/// comparisons on `(a, b)`, not the raw flag tree.
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
    // (a != b) && !(a < b)  ≡  a > b  ≡  b < a  →  Sless(b, a)
    let (mut fg, if_node, a, b) = build_if_with_ab_cond(|fb, a, b| {
        let eq = fb.build_int_cmp_operation(a, b, IntCmpOp::Equal, ValueType::I32)?;
        let neq = build_i1_xor_with_one(fb, eq)?;
        let lt = fb.build_int_cmp_operation(a, b, IntCmpOp::Sless, ValueType::I32)?;
        let nlt = build_i1_xor_with_one(fb, lt)?;
        fb.build_int_binary_operation(neq, nlt, IntBinaryOp::And, ValueType::I1)
    })?;
    let r = FlagCmpCanonicalize::new().run_one(&mut fg, &mut crate::OptCtx::new(None))?;
    assert!(r.changed(), "decomposed GT should canonicalize");
    assert_if_cond_is_intcmp(fg.graph(), if_node, IntCmpOp::Sless, b, a);
    Ok(())
}

/// Incomplete flag tree: the decomposed-GT shape with one leaf swapped for
/// an UNRELATED value — `(a != b) && !(a < c)` (the inner compare reads a
/// third register `c`, breaking the shared `(a, b)` capture).  The pass
/// must not fire: no change reported, the cond stays the `And`, and no new
/// `IntCmpOp` node materialises.
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
            // Foreign leaf: the Sless compares (a, c), not (a, b).
            let lt = fb.build_int_cmp_operation(a, c, IntCmpOp::Sless, ValueType::I32)?;
            let nlt = build_i1_xor_with_one(fb, lt)?;
            let cond = fb.build_int_binary_operation(neq, nlt, IntBinaryOp::And, ValueType::I1)?;
            Ok((cond, ()))
        })?;

    let cmp_count_before = fg.count_kind(|k| matches!(k, NodeKind::IntCmpOp(_)));
    let r = FlagCmpCanonicalize::new().run_one(&mut fg, &mut crate::OptCtx::new(None))?;
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
    // (a == b) || (a < b)  ≡  a <= b  ≡  !(b < a)  →  BitNot(Sless(b, a))
    let (mut fg, if_node, a, b) = build_if_with_ab_cond(|fb, a, b| {
        let eq = fb.build_int_cmp_operation(a, b, IntCmpOp::Equal, ValueType::I32)?;
        let lt = fb.build_int_cmp_operation(a, b, IntCmpOp::Sless, ValueType::I32)?;
        fb.build_int_binary_operation(eq, lt, IntBinaryOp::Or, ValueType::I1)
    })?;
    let r = FlagCmpCanonicalize::new().run_one(&mut fg, &mut crate::OptCtx::new(None))?;
    assert!(r.changed(), "decomposed LE should canonicalize");
    assert_if_cond_is_neg_intcmp(&fg, if_node, IntCmpOp::Sless, b, a);
    Ok(())
}

#[test]
fn flag_cmp_decomposed_hi_rewrites_to_less_swapped() -> Result<()> {
    // unsigned: (a != b) && !(a < b)  →  Less(b, a)
    let (mut fg, if_node, a, b) = build_if_with_ab_cond(|fb, a, b| {
        let eq = fb.build_int_cmp_operation(a, b, IntCmpOp::Equal, ValueType::I32)?;
        let neq = build_i1_xor_with_one(fb, eq)?;
        let lt = fb.build_int_cmp_operation(a, b, IntCmpOp::Less, ValueType::I32)?;
        let nlt = build_i1_xor_with_one(fb, lt)?;
        fb.build_int_binary_operation(neq, nlt, IntBinaryOp::And, ValueType::I1)
    })?;
    let r = FlagCmpCanonicalize::new().run_one(&mut fg, &mut crate::OptCtx::new(None))?;
    assert!(r.changed(), "decomposed HI should canonicalize");
    assert_if_cond_is_intcmp(fg.graph(), if_node, IntCmpOp::Less, b, a);
    Ok(())
}

#[test]
fn flag_cmp_decomposed_ls_rewrites_to_neg_less_swapped() -> Result<()> {
    // unsigned: (a == b) || (a < b)  →  BitNot(Less(b, a))
    let (mut fg, if_node, a, b) = build_if_with_ab_cond(|fb, a, b| {
        let eq = fb.build_int_cmp_operation(a, b, IntCmpOp::Equal, ValueType::I32)?;
        let lt = fb.build_int_cmp_operation(a, b, IntCmpOp::Less, ValueType::I32)?;
        fb.build_int_binary_operation(eq, lt, IntBinaryOp::Or, ValueType::I1)
    })?;
    let r = FlagCmpCanonicalize::new().run_one(&mut fg, &mut crate::OptCtx::new(None))?;
    assert!(r.changed(), "decomposed LS should canonicalize");
    assert_if_cond_is_neg_intcmp(&fg, if_node, IntCmpOp::Less, b, a);
    Ok(())
}

// ── Constant-folded `ja`/`jbe` flag tree (rule 14) ──────────────────────────
//
// `cmp idx, N; ja` lifts the unsigned LS tree `(idx < N) || (idx == N)`.  By
// the time this pass runs, ConstantFold has folded the ZF term's
// `Neg(IntConst(N))` to `IntConst(-N)`, so the equality is
// `Equal(Add(idx, IntConst(-N)), 0)` — neither rule 1 (`Equal(Add(a, Neg(b)),
// 0) → Equal(a, b)`) nor the plain decomposed-LS rule can match.  Rule 14
// recognises this folded shape directly and rewrites it to `BitNot(Less(N,
// idx))` (= `idx <= N`), reusing the captured `IntConst(N)` node.

/// Builds `Or(Less(idx, IntConst(N)), Equal(Add(idx, IntConst(-N)), 0))` at
/// `ty` and asserts the pass folds it to the neg-less shape `¬Less(N, idx)`.
fn check_folded_ls_tree(ty: ValueType, n: u64) -> Result<()> {
    let idx_vn = strider_ir_test_utils::reg_vn(0x1000, ty.byte_size() as u32);
    let (mut fg, if_node, idx, n_const) = {
        let (fg, if_node, (idx, n_const)) = RegisterSet::new()
            .tracked(idx_vn)
            .build_if_then_else_returns(|fb| {
                let idx = fb.read_variable(&idx_vn)?;
                let n_const = fb.build_int_const(u128::from(n), ty)?;
                let less = fb.build_int_cmp_operation(idx, n_const, IntCmpOp::Less, ty)?;
                // Equal(Add(idx, IntConst(-N)), 0) — the constant-folded ZF term.
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

    let r = FlagCmpCanonicalize::new().run_one(&mut fg, &mut crate::OptCtx::new(None))?;
    assert!(
        r.changed(),
        "{ty:?} N={n}: constant-folded LS tree should canonicalize"
    );
    // Folds to ¬Less(N, idx) = idx <= N, reusing the captured IntConst(N).
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
    // N = 1 → -N wraps to all-ones at the width; the guard must still match
    // (M ≡ -N mod width).
    check_folded_ls_tree(ValueType::I32, 1)?;
    check_folded_ls_tree(ValueType::I64, 1)
}

// ── Offset-base constant-folded LS flag tree (rule 15) ──────────────────────
//
// A `switch` whose cases start at a nonzero base `K`: gcc emits
// `sub b, K; cmp (b-K), N; ja`, so the compared value is the OFFSET index
// `X = Add(b, -K)` rather than `b` itself.  The ZF term `X == N` folds to
// `Equal(Add(b, C2), 0)` with `C2 = -K - N`, so the `Less` operand `Add(b, -K)`
// and the `Equal` base `b` are DISTINCT nodes — rule 14's shared `a` can't bind
// both.  Rule 15 keys on the shared base and rewrites to `X <= N` on the index.

/// Builds `Or(Less(Add(b, C1), N), Equal(Add(b, C1 - N), 0))` and asserts the
/// pass folds it to `¬Less(N, Add(b, C1))` (= `X <= N`) on the offset index.
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
                // C2 = C1 - N — the folded ZF term's base, on the SAME `b`.
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

    let r = FlagCmpCanonicalize::new().run_one(&mut fg, &mut crate::OptCtx::new(None))?;
    assert!(
        r.changed(),
        "{ty:?} C1={c1} N={n}: offset-base LS tree should canonicalize"
    );
    // Folds to ¬Less(N, X) = X <= N, reusing the captured offset index X.
    assert_if_cond_is_neg_intcmp(&fg, if_node, IntCmpOp::Less, n_const, x_val);
    Ok(())
}

#[test]
fn flag_cmp_offset_folded_ls_tree_i32() -> Result<()> {
    // cases 10..=17: index `X = b - 10`, range bound `N = 7`.  C1 = -10.
    check_offset_folded_ls_tree(ValueType::I32, (0u128).wrapping_sub(10), 7)
}

#[test]
fn flag_cmp_offset_folded_ls_tree_i64() -> Result<()> {
    check_offset_folded_ls_tree(ValueType::I64, (0u128).wrapping_sub(100), 5)
}

// ── Constant-folded `bhi`/`ja` HI flag tree (rule 16) ───────────────────────
//
// Thumb `cmp idx, N; bhi default` lifts the unsigned HI tree
// `(idx >= N) AND (idx != N)` = `idx > N`.  By the time this pass runs,
// ConstantFold has folded the ZF term to `Equal(Add(idx, IntConst(-N)), 0)`
// — so neither the raw HI rule (2) nor the decomposed HI rule (12) matches
// (both expect the ZF term as `Equal(a, b)`).  Rule 16 recognises the folded
// HI shape `And(BitNot(Less(idx, N)), BitNot(Equal(Add(idx, -N), 0)))` and
// rewrites it to `Less(N, idx)` (= `idx > N`), the dual of the const-folded
// LS rule 14.

/// Builds `And(¬Less(idx, N), ¬Equal(Add(idx, -N), 0))` and asserts the pass
/// folds it to `Less(N, idx)`.
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
                // Equal(Add(idx, IntConst(-N)), 0) — the constant-folded ZF term.
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

    let r = FlagCmpCanonicalize::new().run_one(&mut fg, &mut crate::OptCtx::new(None))?;
    assert!(
        r.changed(),
        "{ty:?} N={n}: constant-folded HI tree should canonicalize"
    );
    // `idx > N` becomes `Less(N, idx)`, reusing the captured `IntConst(N)`.
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

// ── Offset-base constant-folded HI flag tree (rule 17) ──────────────────────
//
// The offset-base dual of rule 16 (and the HI sibling of rule 15): a masked /
// offset switch where the compared value is `X = Add(b, C1)` (e.g. Thumb
// `and r0,#7; subs r0,#1; cmp r0,#N-1; bhi`).  The ZF term folds to
// `Equal(Add(b, C2), 0)` with `C2 = C1 - N`, so the `Less` operand `Add(b, C1)`
// and the `Equal` base `b` are distinct nodes (rule 16 can't bind both).
// Rule 17 keys on the shared base and rewrites to `Less(N, X)`.

/// Builds `And(¬Less(Add(b,C1), N), ¬Equal(Add(b, C1-N), 0))` and asserts the
/// pass folds it to `Less(N, Add(b,C1))`.
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

    let r = FlagCmpCanonicalize::new().run_one(&mut fg, &mut crate::OptCtx::new(None))?;
    assert!(
        r.changed(),
        "{ty:?} C1={c1} N={n}: offset-base HI tree should canonicalize"
    );
    assert_if_cond_is_intcmp(fg.graph(), if_node, IntCmpOp::Less, n_const, x_val);
    Ok(())
}

#[test]
fn flag_cmp_offset_folded_hi_tree_i32() -> Result<()> {
    // Thumb masked switch: index `X = (kind&7) - 1`, range bound `N = 6`.
    check_offset_folded_hi_tree(ValueType::I32, (0u128).wrapping_sub(1), 6)
}

#[test]
fn flag_cmp_offset_folded_hi_tree_i64() -> Result<()> {
    check_offset_folded_hi_tree(ValueType::I64, (0u128).wrapping_sub(20), 5)
}

#[test]
fn flag_cmp_offset_folded_ls_tree_rejects_wrong_offset() -> Result<()> {
    // If C2 != C1 - N the Equal does NOT test `X == N`, so the guard must
    // reject and leave the condition unchanged.  Build with a deliberately
    // wrong C2 (off by one) and assert no rewrite.
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
            // WRONG: C2 should be C1 - N; use C1 - N + 1.
            let c2 = c1.wrapping_sub(u128::from(n)).wrapping_add(1);
            let c2_const = fb.build_int_const(c2, ty)?;
            let y = fb.build_int_binary_operation(b, c2_const, IntBinaryOp::Add, ty)?;
            let zero = fb.build_int_const(0u128, ty)?;
            let eq = fb.build_int_cmp_operation(y, zero, IntCmpOp::Equal, ty)?;
            let cond = fb.build_int_binary_operation(less, eq, IntBinaryOp::Or, ValueType::I1)?;
            Ok((cond, ()))
        })
        .map(|(fg, _, ())| fg)?;

    let r = FlagCmpCanonicalize::new().run_one(&mut fg, &mut crate::OptCtx::new(None))?;
    assert!(!r.changed(), "a wrong offset must not be canonicalized");
    Ok(())
}
