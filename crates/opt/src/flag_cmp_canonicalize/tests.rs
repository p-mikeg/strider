//! Unit tests for [`super::FlagCmpCanonicalize`].
//!
//! Each test builds a small IR fixture that mimics what AArch64's lift
//! produces for `cmp a, b; b.<cond>` — i.e. a flag-tree on the `If`'s cond
//! input — and asserts the pass rewrites the cond to a single canonical
//! `IntCmpOp` node consuming the original `(a, b)` pair.

use super::FlagCmpCanonicalize;
use crate::error::Result;
use crate::pipeline::Optimizer;

use ir::{BuiltFunctionGraph, FunctionBuilder};
use ir::node::{NodeId, NodeKind, NodeOutputId, NodeOutputType};
use ir::{IntBinaryOp, IntCmpOp, IntUnaryOp};

// ── Common fixture builder ────────────────────────────────────────────────

/// Build the canonical AArch64 cmp shape for `a` and `b` and return
/// `(zr, ng, cy, ov, a, b)` — the four flag outputs and the two
/// register reads — so individual tests can wire whichever subset
/// each cond code needs onto the `If`.
fn build_cmp_flags(
    fb: &mut FunctionBuilder,
    a: NodeOutputId,
    b: NodeOutputId,
) -> Result<(NodeOutputId, NodeOutputId, NodeOutputId, NodeOutputId)> {
    let neg_b = fb.build_int_unary_operation(b, IntUnaryOp::Neg, NodeOutputType::U32)?;
    let diff = fb.build_int_binary_operation(a, neg_b, IntBinaryOp::Add, NodeOutputType::U32)?;
    let zero = fb.build_int_const(0u64, NodeOutputType::U32)?;

    let zr = fb.build_int_cmp_operation(diff, zero, IntCmpOp::Equal, NodeOutputType::U32)?;
    let ng = fb.build_int_cmp_operation(diff, zero, IntCmpOp::Sless, NodeOutputType::U32)?;
    // CY = BoolNeg(IntLess(a, b))  — post lift-time canonicalisation of IntLessEqual(b, a).
    let alt = fb.build_int_cmp_operation(a, b, IntCmpOp::Less, NodeOutputType::U32)?;
    let cy = fb.build_boolean_unary_operation(alt, ir::BoolUnaryOp::Neg)?;
    let ov = fb.build_int_cmp_operation(a, b, IntCmpOp::Sborrow, NodeOutputType::U32)?;

    Ok((zr, ng, cy, ov))
}

/// Build an entry region that reads two 32-bit register values, computes
/// the four AArch64 flag values, and uses the supplied closure to derive
/// the `If` cond from those flags.  Then build a trivial true/false
/// region pair and return the graph + the unique If node id + the two
/// leaves `a`, `b` (so tests can assert the rewritten cond points at
/// them).
fn build_if_with_flag_cond<F>(make_cond: F) -> Result<(BuiltFunctionGraph, NodeId, NodeOutputId, NodeOutputId)>
where
    F: FnOnce(
        &mut FunctionBuilder,
        NodeOutputId, // ZR
        NodeOutputId, // NG
        NodeOutputId, // CY
        NodeOutputId, // OV
    ) -> Result<NodeOutputId>,
{
    let a_vn = ir::test_utils::reg_vn(0x1000, 4);
    let b_vn = ir::test_utils::reg_vn(0x1008, 4);
    let mut fb = FunctionBuilder::new_raw(vec![a_vn, b_vn], &[], &[], &[], None, 0)?;
    let entry = fb.create_region()?;
    let t = fb.create_region()?;
    let f = fb.create_region()?;
    fb.set_entry_region(entry)?;

    fb.set_region(entry);
    let a = fb.read_variable(&a_vn)?;
    let b = fb.read_variable(&b_vn)?;
    let (zr, ng, cy, ov) = build_cmp_flags(&mut fb, a, b)?;
    let cond = make_cond(&mut fb, zr, ng, cy, ov)?;
    fb.build_if(cond, t, f)?;

    fb.set_region(t);
    let one = fb.build_int_const(1u64, NodeOutputType::U64)?;
    fb.build_return(Some(one), &[])?;

    fb.set_region(f);
    let two = fb.build_int_const(2u64, NodeOutputType::U64)?;
    fb.build_return(Some(two), &[])?;

    let fg = fb.build()?;
    let if_node = find_unique_if(&fg);
    Ok((fg, if_node, a, b))
}

fn find_unique_if(fg: &BuiltFunctionGraph) -> NodeId {
    let ifs: Vec<NodeId> = fg
        .all_node_ids()
        .filter(|&n| matches!(fg.graph.node_kind(n), NodeKind::If))
        .collect();
    assert_eq!(ifs.len(), 1, "fixture must have exactly one If node");
    ifs[0]
}

fn if_cond_output(fg: &BuiltFunctionGraph, if_node: NodeId) -> NodeOutputId {
    let [_ctrl, cond_out] = fg
        .graph
        .node_inputs_exact::<2>(if_node)
        .expect("If has exactly two inputs");
    cond_out
}

fn if_cond_node_kind(fg: &BuiltFunctionGraph, if_node: NodeId) -> NodeKind {
    let cond_out = if_cond_output(fg, if_node);
    *fg.graph.node_kind(fg.graph.get_node_from_output(cond_out))
}

/// Asserts that the captured If's cond is `IntCmpOp(op)` with inputs
/// `(expect_lhs, expect_rhs)` in that exact order.
fn assert_if_cond_is_intcmp(
    fg: &BuiltFunctionGraph,
    if_node: NodeId,
    op: IntCmpOp,
    expect_lhs: NodeOutputId,
    expect_rhs: NodeOutputId,
) {
    let cond_out = if_cond_output(fg, if_node);
    let cond_node = fg.graph.get_node_from_output(cond_out);
    assert_eq!(
        *fg.graph.node_kind(cond_node),
        NodeKind::IntCmpOp(op),
        "If cond should be IntCmpOp({op:?})",
    );
    let [lhs, rhs] = fg
        .graph
        .node_inputs_exact::<2>(cond_node)
        .expect("IntCmpOp has 2 inputs");
    assert_eq!(lhs, expect_lhs, "lhs of canonicalised cmp");
    assert_eq!(rhs, expect_rhs, "rhs of canonicalised cmp");
}

// ── Tests ─────────────────────────────────────────────────────────────────

/// Asserts the If's cond is `BoolNeg(IntCmpOp(op, lhs, rhs))`.  Used for
/// the cond shapes whose post-rewrite canonical form is a negated cmp;
/// `IfCondInversion` (in the full pipeline, not in this test) is the
/// pass that finally swaps the If's branches and strips the BoolNeg.
fn assert_if_cond_is_neg_intcmp(
    fg: &BuiltFunctionGraph,
    if_node: NodeId,
    op: IntCmpOp,
    expect_lhs: NodeOutputId,
    expect_rhs: NodeOutputId,
) {
    let cond_out = if_cond_output(fg, if_node);
    let neg_node = fg.graph.get_node_from_output(cond_out);
    assert_eq!(
        *fg.graph.node_kind(neg_node),
        NodeKind::BoolUnaryOp(ir::BoolUnaryOp::Neg),
        "If cond should be BoolNeg(...)",
    );
    let [inner] = fg
        .graph
        .node_inputs_exact::<1>(neg_node)
        .expect("BoolNeg has 1 input");
    let inner_node = fg.graph.get_node_from_output(inner);
    assert_eq!(
        *fg.graph.node_kind(inner_node),
        NodeKind::IntCmpOp(op),
        "Inner cond should be IntCmpOp({op:?})",
    );
    let [lhs, rhs] = fg
        .graph
        .node_inputs_exact::<2>(inner_node)
        .expect("IntCmpOp has 2 inputs");
    assert_eq!(lhs, expect_lhs, "lhs of canonicalised cmp");
    assert_eq!(rhs, expect_rhs, "rhs of canonicalised cmp");
}

#[test]
fn flag_cmp_eq_rewrites_to_int_equal() -> Result<()> {
    // AArch64 `b.eq` cond is the bare ZR flag = `Equal(Add(a, Neg(b)), 0)`.
    let (mut fg, if_node, a, b) =
        build_if_with_flag_cond(|_fb, zr, _ng, _cy, _ov| Ok(zr))?;

    let r = FlagCmpCanonicalize.optimize(&mut fg.graph, fg.entry)?;
    assert!(r.changed(), "pass should rewrite the EQ flag tree");

    assert_if_cond_is_intcmp(&fg, if_node, IntCmpOp::Equal, a, b);
    Ok(())
}

#[test]
fn flag_cmp_ne_rewrites_to_neg_int_equal() -> Result<()> {
    // AArch64 `b.ne` cond is `BoolNeg(ZR)` = `BoolNeg(Equal(Add(a, Neg(b)), 0))`.
    let (mut fg, if_node, a, b) = build_if_with_flag_cond(|fb, zr, _ng, _cy, _ov| {
        fb.build_boolean_unary_operation(zr, ir::BoolUnaryOp::Neg)
    })?;

    let r = FlagCmpCanonicalize.optimize(&mut fg.graph, fg.entry)?;
    assert!(r.changed(), "pass should rewrite the NE flag tree");

    assert_if_cond_is_neg_intcmp(&fg, if_node, IntCmpOp::Equal, a, b);
    Ok(())
}

#[test]
fn flag_cmp_hi_rewrites_to_int_less_swapped() -> Result<()> {
    // AArch64 `b.hi` cond is `BoolAnd(CY, BoolNeg(ZR))`.  After ZR is
    // simplified to `Equal(a, b)` and the BoolAnd rule fires, the cond
    // is `IntLess(b, a)` (= `a > b unsigned`).
    let (mut fg, if_node, a, b) = build_if_with_flag_cond(|fb, zr, _ng, cy, _ov| {
        let neg_zr = fb.build_boolean_unary_operation(zr, ir::BoolUnaryOp::Neg)?;
        fb.build_boolean_operation(cy, neg_zr, ir::BoolBinaryOp::And)
    })?;

    let r = FlagCmpCanonicalize.optimize(&mut fg.graph, fg.entry)?;
    assert!(r.changed(), "pass should rewrite the HI flag tree");

    // Note swapped operands: `a > b` becomes `IntLess(b, a)`.
    assert_if_cond_is_intcmp(&fg, if_node, IntCmpOp::Less, b, a);
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
        let neg_zr = fb.build_boolean_unary_operation(zr, ir::BoolUnaryOp::Neg)?;
        fb.build_boolean_operation(cy, neg_zr, ir::BoolBinaryOp::And)
    })?;

    crate::ConstantFold.optimize(&mut fg.graph, fg.entry)?;
    let r = FlagCmpCanonicalize.optimize(&mut fg.graph, fg.entry)?;
    assert!(r.changed(), "HI rewrite must survive a prior ConstantFold pass");
    assert_if_cond_is_intcmp(&fg, if_node, IntCmpOp::Less, b, a);
    Ok(())
}

#[test]
fn flag_cmp_ls_rewrites_to_neg_int_less_swapped() -> Result<()> {
    // AArch64 `b.ls` cond is `BoolOr(BoolNeg(CY), ZR)`.  After CY's
    // canonical form (`BoolNeg(IntLess(a, b))`) cancels the BoolNeg via
    // ConstantFold (`BoolNeg(BoolNeg(x)) → x`), the inner OR becomes
    // `BoolOr(IntLess(a, b), Equal(a, b))` and our rule rewrites it to
    // `BoolNeg(IntLess(b, a))`.
    let (mut fg, if_node, a, b) = build_if_with_flag_cond(|fb, zr, _ng, cy, _ov| {
        let neg_cy = fb.build_boolean_unary_operation(cy, ir::BoolUnaryOp::Neg)?;
        fb.build_boolean_operation(neg_cy, zr, ir::BoolBinaryOp::Or)
    })?;

    // Run ConstantFold first to collapse `BoolNeg(BoolNeg(IntLess(a, b))) → IntLess(a, b)`.
    crate::ConstantFold.optimize(&mut fg.graph, fg.entry)?;
    let r = FlagCmpCanonicalize.optimize(&mut fg.graph, fg.entry)?;
    assert!(r.changed(), "pass should rewrite the LS flag tree");

    assert_if_cond_is_neg_intcmp(&fg, if_node, IntCmpOp::Less, b, a);
    Ok(())
}

#[test]
fn flag_cmp_lt_rewrites_to_int_sless() -> Result<()> {
    // AArch64 `b.lt` cond is `BoolNeg(Equal(NG, OV))`.  Real lift passes
    // `insn.inputs[0].size` (1 byte for the flag varnodes) as the operand
    // width to `build_int_cmp_operation`, so the IR has
    // `Equal(CastToInt(NG, U8), CastToInt(OV, U8))`.  The fixture matches.
    let (mut fg, if_node, a, b) = build_if_with_flag_cond(|fb, _zr, ng, _cy, ov| {
        let eq = fb.build_int_cmp_operation(ng, ov, IntCmpOp::Equal, NodeOutputType::U8)?;
        fb.build_boolean_unary_operation(eq, ir::BoolUnaryOp::Neg)
    })?;

    let r = FlagCmpCanonicalize.optimize(&mut fg.graph, fg.entry)?;
    assert!(r.changed(), "pass should rewrite the LT flag tree");

    assert_if_cond_is_intcmp(&fg, if_node, IntCmpOp::Sless, a, b);
    Ok(())
}

#[test]
fn flag_cmp_ge_rewrites_to_neg_int_sless() -> Result<()> {
    // AArch64 `b.ge` cond is `Equal(NG, OV)`.
    let (mut fg, if_node, a, b) = build_if_with_flag_cond(|fb, _zr, ng, _cy, ov| {
        fb.build_int_cmp_operation(ng, ov, IntCmpOp::Equal, NodeOutputType::U8)
    })?;

    let r = FlagCmpCanonicalize.optimize(&mut fg.graph, fg.entry)?;
    assert!(r.changed(), "pass should rewrite the GE flag tree");

    assert_if_cond_is_neg_intcmp(&fg, if_node, IntCmpOp::Sless, a, b);
    Ok(())
}

#[test]
fn flag_cmp_gt_rewrites_to_int_sless_swapped() -> Result<()> {
    // AArch64 `b.gt` cond is `BoolAnd(BoolNeg(ZR), Equal(NG, OV))`.
    let (mut fg, if_node, a, b) = build_if_with_flag_cond(|fb, zr, ng, _cy, ov| {
        let neg_zr = fb.build_boolean_unary_operation(zr, ir::BoolUnaryOp::Neg)?;
        let eq = fb.build_int_cmp_operation(ng, ov, IntCmpOp::Equal, NodeOutputType::U8)?;
        fb.build_boolean_operation(neg_zr, eq, ir::BoolBinaryOp::And)
    })?;

    let r = FlagCmpCanonicalize.optimize(&mut fg.graph, fg.entry)?;
    assert!(r.changed(), "pass should rewrite the GT flag tree");

    assert_if_cond_is_intcmp(&fg, if_node, IntCmpOp::Sless, b, a);
    Ok(())
}

#[test]
fn flag_cmp_le_rewrites_to_neg_int_sless_swapped() -> Result<()> {
    // AArch64 `b.le` cond is `BoolOr(ZR, BoolNeg(Equal(NG, OV)))`.
    let (mut fg, if_node, a, b) = build_if_with_flag_cond(|fb, zr, ng, _cy, ov| {
        let eq = fb.build_int_cmp_operation(ng, ov, IntCmpOp::Equal, NodeOutputType::U8)?;
        let neg_eq = fb.build_boolean_unary_operation(eq, ir::BoolUnaryOp::Neg)?;
        fb.build_boolean_operation(zr, neg_eq, ir::BoolBinaryOp::Or)
    })?;

    let r = FlagCmpCanonicalize.optimize(&mut fg.graph, fg.entry)?;
    assert!(r.changed(), "pass should rewrite the LE flag tree");

    assert_if_cond_is_neg_intcmp(&fg, if_node, IntCmpOp::Sless, b, a);
    Ok(())
}

// ── Negative tests — flags that should NOT be rewritten ───────────────────

#[test]
fn flag_cmp_cs_is_left_alone_as_bool_neg_int_less() -> Result<()> {
    // CS = bare CY = `BoolNeg(IntLess(a, b))`.  Already in `(a, b)` form;
    // `IfCondInversion` (a separate pass) handles the outer BoolNeg.
    let (mut fg, if_node, _a, _b) =
        build_if_with_flag_cond(|_fb, _zr, _ng, cy, _ov| Ok(cy))?;

    let r = FlagCmpCanonicalize.optimize(&mut fg.graph, fg.entry)?;
    assert!(!r.changed(), "CS already canonical; pass must not fire");

    assert_eq!(
        if_cond_node_kind(&fg, if_node),
        NodeKind::BoolUnaryOp(ir::BoolUnaryOp::Neg),
    );
    Ok(())
}

#[test]
fn flag_cmp_mi_is_left_alone_as_int_sless_diff() -> Result<()> {
    // MI = bare NG = `IntSless(Add(a, Neg(b)), 0)`.  This is NOT
    // equivalent to `IntSless(a, b)` (subtraction overflow), so the
    // pass must leave it untouched.
    let (mut fg, if_node, _a, _b) =
        build_if_with_flag_cond(|_fb, _zr, ng, _cy, _ov| Ok(ng))?;

    let r = FlagCmpCanonicalize.optimize(&mut fg.graph, fg.entry)?;
    assert!(!r.changed(), "MI is not algebraically reducible; pass must not fire");

    assert_eq!(
        if_cond_node_kind(&fg, if_node),
        NodeKind::IntCmpOp(IntCmpOp::Sless),
    );
    Ok(())
}

// ── ARM Thumb shapes — flag tested against 0:1 ───────────────────────────

#[test]
fn flag_cmp_thumb_beq_reduces_to_int_equal() -> Result<()> {
    // ARM Thumb's `B.EQ` lifts as `IntNotEqual(ZR, 0:1)`.  Lift-time
    // canonicalisation lowers that to `BoolNeg(IntEqual(CastToInt(ZR), 0))`.
    // After two pass iterations (rule 9 strips the bool-neg-eq-zero, then
    // rule 1 simplifies ZR's `Equal(diff, 0)`), the cond is `Equal(a, b)`.
    let (mut fg, if_node, a, b) = build_if_with_flag_cond(|fb, zr, _ng, _cy, _ov| {
        // Mimic `IntNotEqual(ZR, 0:1)` post-canonicalisation:
        //   BoolNeg(IntEqual(CastToInt(ZR, U8), 0:U8))
        let zero = fb.build_int_const(0u64, NodeOutputType::U8)?;
        let eq = fb.build_int_cmp_operation(zr, zero, IntCmpOp::Equal, NodeOutputType::U8)?;
        fb.build_boolean_unary_operation(eq, ir::BoolUnaryOp::Neg)
    })?;

    // Run my pass twice (or run it once via the pipeline's fixed-point loop).
    // Two iterations let rule 9 fire on the outer BoolNeg(IntEqual(CastToInt(ZR), 0))
    // first, then rule 1 simplify the inner Equal(diff, 0).
    let _ = FlagCmpCanonicalize.optimize(&mut fg.graph, fg.entry)?;
    let _ = FlagCmpCanonicalize.optimize(&mut fg.graph, fg.entry)?;

    assert_if_cond_is_intcmp(&fg, if_node, IntCmpOp::Equal, a, b);
    Ok(())
}

#[test]
fn flag_cmp_vs_is_left_alone_as_sborrow() -> Result<()> {
    // VS = bare OV = `IntSborrow(a, b)`.  Already in `(a, b)` form,
    // nothing to simplify.
    let (mut fg, if_node, _a, _b) =
        build_if_with_flag_cond(|_fb, _zr, _ng, _cy, ov| Ok(ov))?;

    let r = FlagCmpCanonicalize.optimize(&mut fg.graph, fg.entry)?;
    assert!(!r.changed(), "VS already canonical; pass must not fire");

    assert_eq!(
        if_cond_node_kind(&fg, if_node),
        NodeKind::IntCmpOp(IntCmpOp::Sborrow),
    );
    Ok(())
}
