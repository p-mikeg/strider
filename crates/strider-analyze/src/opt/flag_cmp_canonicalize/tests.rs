//! Unit tests for [`super::FlagCmpCanonicalize`].
//!
//! Each test builds a small IR fixture that mimics what AArch64's lift
//! produces for `cmp a, b; b.<cond>` — i.e. a flag-tree on the `If`'s cond
//! input — and asserts the pass rewrites the cond to a single canonical
//! `IntCmpOp` node consuming the original `(a, b)` pair.

use super::FlagCmpCanonicalize;
use crate::opt::error::Result;
use crate::opt::pipeline::Optimizer;

use strider_ir::{Graph, FunctionBuilder};
use strider_ir::node::{NodeId, NodeKind, ValueId, ValueType};
use strider_ir::{IntBinaryOp, IntCmpOp, IntUnaryOp};
use strider_ir_test_utils::RegisterSet;

/// Builds the canonical 1-bit logical NOT shape `Xor(operand, IntConst(1)):I1`
/// (post-removal-of-the former BitNot unary-op).
fn build_i1_xor_with_one(
    fb: &mut FunctionBuilder,
    operand: ValueId,
) -> Result<ValueId> {
    let one = fb.build_all_ones_const(ValueType::I1)?;
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
        fg.value_kind(value)
            .as_value()
            .is_some_and(|t| t.is_bool())
            && matches!(*fg.kind_of_value(value), NodeKind::IntConst(1))
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
    let one_i1 = fb.build_all_ones_const(ValueType::I1)?;
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
fn build_if_with_flag_cond<F>(make_cond: F) -> Result<(strider_ir::Function, NodeId, ValueId, ValueId)>
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
        .graph().node_inputs_exact::<2>(xor_node)
        .expect("Xor has 2 inputs");
    // The non-constant operand is the cmp; the other is the I1
    // IntConst(1) (might be on either side due to dedup).
    let is_one_const = |value: ValueId| {
        matches!(*function.kind_of_value(value), NodeKind::IntConst(1))
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
        .graph().node_inputs_exact::<2>(inner_node)
        .expect("IntCmpOp has 2 inputs");
    assert_eq!(lhs, expect_lhs, "lhs of canonicalised cmp");
    assert_eq!(rhs, expect_rhs, "rhs of canonicalised cmp");
}

// ── constructed-with-data: per-instance rule ownership ────────────────────

/// A pass built via [`FlagCmpCanonicalize::new`] owns its rule table and
/// canonicalises the same representative EQ flag tree the bare-value form did.
#[test]
fn new_builds_pass_that_canonicalizes() -> Result<()> {
    let (mut fg, if_node, a, b) =
        build_if_with_flag_cond(|_fb, zr, _ng, _cy, _ov| Ok(zr))?;

    let r = FlagCmpCanonicalize::new().optimize(&mut fg, &crate::opt::OptCtx::empty())?;
    assert!(r.changed(), "constructed pass should rewrite the EQ flag tree");
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

    let (mut fg_a, if_a, a_a, b_a) =
        build_if_with_flag_cond(|_fb, zr, _ng, _cy, _ov| Ok(zr))?;
    assert!(pass_a
        .optimize(&mut fg_a, &crate::opt::OptCtx::empty())?
        .changed());
    assert_if_cond_is_intcmp(fg_a.graph(), if_a, IntCmpOp::Equal, a_a, b_a);

    let (mut fg_b, if_b, a_b, b_b) =
        build_if_with_flag_cond(|_fb, zr, _ng, _cy, _ov| Ok(zr))?;
    assert!(pass_b
        .optimize(&mut fg_b, &crate::opt::OptCtx::empty())?
        .changed());
    assert_if_cond_is_intcmp(fg_b.graph(), if_b, IntCmpOp::Equal, a_b, b_b);
    Ok(())
}

#[test]
fn flag_cmp_eq_rewrites_to_int_equal() -> Result<()> {
    // AArch64 `b.eq` cond is the bare ZR flag = `Equal(Add(a, Neg(b)), 0)`.
    let (mut fg, if_node, a, b) =
        build_if_with_flag_cond(|_fb, zr, _ng, _cy, _ov| Ok(zr))?;

    let r = FlagCmpCanonicalize::new().optimize(&mut fg, &crate::opt::OptCtx::empty())?;
    assert!(r.changed(), "pass should rewrite the EQ flag tree");

    assert_if_cond_is_intcmp(fg.graph(), if_node, IntCmpOp::Equal, a, b);
    Ok(())
}

#[test]
fn flag_cmp_ne_rewrites_to_neg_int_equal() -> Result<()> {
    // AArch64 `b.ne` cond is `BitNot(ZR)` = `BitNot(Equal(Add(a, Neg(b)), 0))`.
    let (mut fg, if_node, a, b) = build_if_with_flag_cond(|fb, zr, _ng, _cy, _ov| {
        build_i1_xor_with_one(fb, zr)
    })?;

    let r = FlagCmpCanonicalize::new().optimize(&mut fg, &crate::opt::OptCtx::empty())?;
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

    let r = FlagCmpCanonicalize::new().optimize(&mut fg, &crate::opt::OptCtx::empty())?;
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

    crate::opt::ConstantFold::new().optimize(&mut fg, &crate::opt::OptCtx::empty())?;
    let r = FlagCmpCanonicalize::new().optimize(&mut fg, &crate::opt::OptCtx::empty())?;
    assert!(r.changed(), "HI rewrite must survive a prior ConstantFold pass");
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
    crate::opt::ConstantFold::new().optimize(&mut fg, &crate::opt::OptCtx::empty())?;
    let r = FlagCmpCanonicalize::new().optimize(&mut fg, &crate::opt::OptCtx::empty())?;
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

    let r = FlagCmpCanonicalize::new().optimize(&mut fg, &crate::opt::OptCtx::empty())?;
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

    let r = FlagCmpCanonicalize::new().optimize(&mut fg, &crate::opt::OptCtx::empty())?;
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

    let r = FlagCmpCanonicalize::new().optimize(&mut fg, &crate::opt::OptCtx::empty())?;
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

    let r = FlagCmpCanonicalize::new().optimize(&mut fg, &crate::opt::OptCtx::empty())?;
    assert!(r.changed(), "pass should rewrite the LE flag tree");

    assert_if_cond_is_neg_intcmp(&fg, if_node, IntCmpOp::Sless, b, a);
    Ok(())
}

// ── Negative tests — flags that should NOT be rewritten ───────────────────

#[test]
fn flag_cmp_cs_is_left_alone_as_bool_neg_int_less() -> Result<()> {
    // Region = bare CY = `BitNot(IntLess(a, b))`.  Already in `(a, b)` form;
    // `IfCondInversion` (a separate pass) handles the outer BitNot.
    let (mut fg, if_node, _a, _b) =
        build_if_with_flag_cond(|_fb, _zr, _ng, cy, _ov| Ok(cy))?;

    let r = FlagCmpCanonicalize::new().optimize(&mut fg, &crate::opt::OptCtx::empty())?;
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
    let (mut fg, if_node, _a, _b) =
        build_if_with_flag_cond(|_fb, _zr, ng, _cy, _ov| Ok(ng))?;

    let r = FlagCmpCanonicalize::new().optimize(&mut fg, &crate::opt::OptCtx::empty())?;
    assert!(!r.changed(), "MI is not algebraically reducible; pass must not fire");

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
    let _ = FlagCmpCanonicalize::new().optimize(&mut fg, &crate::opt::OptCtx::empty())?;
    let _ = FlagCmpCanonicalize::new().optimize(&mut fg, &crate::opt::OptCtx::empty())?;

    assert_if_cond_is_intcmp(fg.graph(), if_node, IntCmpOp::Equal, a, b);
    Ok(())
}

#[test]
fn flag_cmp_vs_is_left_alone_as_sborrow() -> Result<()> {
    // VS = bare OV = `IntSborrow(a, b)`.  Already in `(a, b)` form,
    // nothing to simplify.
    let (mut fg, if_node, _a, _b) =
        build_if_with_flag_cond(|_fb, _zr, _ng, _cy, ov| Ok(ov))?;

    let r = FlagCmpCanonicalize::new().optimize(&mut fg, &crate::opt::OptCtx::empty())?;
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
    let r = FlagCmpCanonicalize::new().optimize(&mut fg, &crate::opt::OptCtx::empty())?;
    assert!(r.changed(), "decomposed GT should canonicalize");
    assert_if_cond_is_intcmp(fg.graph(), if_node, IntCmpOp::Sless, b, a);
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
    let r = FlagCmpCanonicalize::new().optimize(&mut fg, &crate::opt::OptCtx::empty())?;
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
    let r = FlagCmpCanonicalize::new().optimize(&mut fg, &crate::opt::OptCtx::empty())?;
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
    let r = FlagCmpCanonicalize::new().optimize(&mut fg, &crate::opt::OptCtx::empty())?;
    assert!(r.changed(), "decomposed LS should canonicalize");
    assert_if_cond_is_neg_intcmp(&fg, if_node, IntCmpOp::Less, b, a);
    Ok(())
}
