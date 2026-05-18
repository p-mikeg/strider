//! Phase 3 Task 3.3a parity test — `FlagCmpCanonicalize` v1 vs
//! `FlagCmpCanonicalizeEgg` v2.
//!
//! v1 implements 9 multi-node rewrites over the value-slice subgraph
//! (the cond input of an `If` node).  v2 reproduces every rewrite
//! via an egg-based saturation + materialisation pass.  Both MUST
//! produce structurally identical IR for the test fixtures.
//!
//! Rules (numbered to match v1's `RULES` table):
//!   1. EQ identity:  Equal(diff, 0) → Equal(a, b)
//!   2. HI:           BoolAnd(BoolNeg(Less(a,b)), BoolNeg(Equal(diff,0))) → Less(b, a)
//!   3. LS:           BoolOr(Less(a,b), Equal(diff,0)) → BoolNeg(Less(b, a))
//!   4. LT:           BoolNeg(Equal(CastToInt(Sless(diff,0)), CastToInt(Sborrow(a,b)))) → Sless(a, b)
//!   5. GE:           Equal(CastToInt(Sless(diff,0)), CastToInt(Sborrow(a,b))) → BoolNeg(Sless(a, b))
//!   6. GT:           BoolAnd(BoolNeg(Equal(diff,0)), GE_lhs) → Sless(b, a)
//!   7. LE:           BoolOr(Equal(diff,0), BoolNeg(GE_lhs)) → BoolNeg(Sless(b, a))
//!   8. Thumb false:  IntEqual(CastToInt(b), 0) → BoolNeg(b)
//!   9. Thumb true:   BoolNeg(IntEqual(CastToInt(b), 0)) → b
//!
//! Each parity test builds a fixture twice, runs v1 on the first copy
//! and v2 on the second, then asserts the post-pass cond-node kind and
//! its leaf inputs match.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

use strider_analyze::opt::{
    FlagCmpCanonicalize, OptimizerRaw, flag_cmp_canonicalize_egg::FlagCmpCanonicalizeEgg,
};
use strider_ir::node::{NodeId, NodeKind, NodeOutputId, NodeOutputType};
use strider_ir::{
    BoolBinaryOp, BoolUnaryOp, BuiltFunctionGraph, FunctionBuilder, IntBinaryOp, IntCmpOp,
    IntUnaryOp,
};

// ── Fixture builder ──────────────────────────────────────────────────────────

/// Builds the canonical 4-flag AArch64 cmp shape and returns
/// `(zr, ng, cy, ov)` flag outputs.
fn build_cmp_flags(
    fb: &mut FunctionBuilder,
    a: NodeOutputId,
    b: NodeOutputId,
) -> (NodeOutputId, NodeOutputId, NodeOutputId, NodeOutputId) {
    let neg_b = fb
        .build_int_unary_operation(b, IntUnaryOp::Neg, NodeOutputType::U32)
        .unwrap();
    let diff = fb
        .build_int_binary_operation(a, neg_b, IntBinaryOp::Add, NodeOutputType::U32)
        .unwrap();
    let zero = fb.build_int_const(0u64, NodeOutputType::U32).unwrap();
    let zr = fb
        .build_int_cmp_operation(diff, zero, IntCmpOp::Equal, NodeOutputType::U32)
        .unwrap();
    let ng = fb
        .build_int_cmp_operation(diff, zero, IntCmpOp::Sless, NodeOutputType::U32)
        .unwrap();
    // CY = BoolNeg(IntLess(a, b)) (post lift-time canonicalisation).
    let alt = fb
        .build_int_cmp_operation(a, b, IntCmpOp::Less, NodeOutputType::U32)
        .unwrap();
    let cy = fb
        .build_boolean_unary_operation(alt, BoolUnaryOp::Neg)
        .unwrap();
    let ov = fb
        .build_int_cmp_operation(a, b, IntCmpOp::Sborrow, NodeOutputType::U32)
        .unwrap();
    (zr, ng, cy, ov)
}

/// Builds an entry region that computes the cmp flags and uses
/// `make_cond` to derive the `If` cond from them.
fn build_if_with_flag_cond<F>(
    make_cond: F,
) -> (BuiltFunctionGraph, NodeId, NodeOutputId, NodeOutputId)
where
    F: FnOnce(
        &mut FunctionBuilder,
        NodeOutputId,
        NodeOutputId,
        NodeOutputId,
        NodeOutputId,
    ) -> NodeOutputId,
{
    let a_vn = strider_ir::test_utils::reg_vn(0x1000, 4);
    let b_vn = strider_ir::test_utils::reg_vn(0x1008, 4);
    let mut fb = FunctionBuilder::new_raw(vec![a_vn, b_vn], &[], &[], &[], None, 0).unwrap();
    let entry = fb.create_region().unwrap();
    let t = fb.create_region().unwrap();
    let f = fb.create_region().unwrap();
    fb.set_entry_region(entry).unwrap();
    fb.set_region(entry);
    fb.set_lift_addr(Some(strider_ir::test_utils::SENTINEL_LIFT_ADDR));
    let a = fb.read_variable(&a_vn).unwrap();
    let b = fb.read_variable(&b_vn).unwrap();
    let (zr, ng, cy, ov) = build_cmp_flags(&mut fb, a, b);
    let cond = make_cond(&mut fb, zr, ng, cy, ov);
    fb.build_if(cond, t, f).unwrap();
    fb.set_region(t);
    let one = fb.build_int_const(1u64, NodeOutputType::U64).unwrap();
    fb.build_return(Some(one), &[]).unwrap();
    fb.set_region(f);
    let two = fb.build_int_const(2u64, NodeOutputType::U64).unwrap();
    fb.build_return(Some(two), &[]).unwrap();
    fb.set_lift_addr(None);
    let fg = fb.build().unwrap();
    let if_node = find_unique_if(&fg);
    (fg, if_node, a, b)
}

fn find_unique_if(fg: &BuiltFunctionGraph) -> NodeId {
    let ifs: Vec<NodeId> = fg
        .all_node_ids()
        .filter(|&n| matches!(fg.node_kind(n), NodeKind::If))
        .collect();
    assert_eq!(ifs.len(), 1);
    ifs[0]
}

fn if_cond_output(fg: &BuiltFunctionGraph, if_node: NodeId) -> NodeOutputId {
    let [_ctrl, cond_out] = fg.graph.node_inputs_exact::<2>(if_node).unwrap();
    cond_out
}

/// Returns a description of the cond shape: top-level NodeKind +
/// (if it is an IntCmpOp) the lhs/rhs leaf outputs in input order.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CondShape {
    /// `IntCmpOp(op)(lhs, rhs)` directly.
    IntCmp(IntCmpOp, NodeOutputId, NodeOutputId),
    /// `BoolNeg(IntCmpOp(op)(lhs, rhs))`.
    NegIntCmp(IntCmpOp, NodeOutputId, NodeOutputId),
    /// Anything else (left alone or unexpected).
    Other(NodeKind),
}

fn classify_cond(fg: &BuiltFunctionGraph, if_node: NodeId) -> CondShape {
    let cond_out = if_cond_output(fg, if_node);
    let cond_node = fg.graph.get_node_from_output(cond_out);
    let kind = *fg.graph.node_kind(cond_node);
    match kind {
        NodeKind::IntCmpOp(op) => {
            let [l, r] = fg.graph.node_inputs_exact::<2>(cond_node).unwrap();
            CondShape::IntCmp(op, l, r)
        }
        NodeKind::BoolUnaryOp(BoolUnaryOp::Neg) => {
            let [inner] = fg.graph.node_inputs_exact::<1>(cond_node).unwrap();
            let inner_node = fg.graph.get_node_from_output(inner);
            let inner_kind = *fg.graph.node_kind(inner_node);
            match inner_kind {
                NodeKind::IntCmpOp(op) => {
                    let [l, r] = fg.graph.node_inputs_exact::<2>(inner_node).unwrap();
                    CondShape::NegIntCmp(op, l, r)
                }
                _ => CondShape::Other(kind),
            }
        }
        _ => CondShape::Other(kind),
    }
}

// ── Parity tests — one per rewrite rule ─────────────────────────────────────

/// Run a rule via the supplied optimizer; allow multiple iterations for
/// rules that depend on a prior rule firing (e.g. Thumb shapes use Rule 9
/// then Rule 1).
fn run_to_fp(opt: &dyn OptimizerRaw, fg: &mut BuiltFunctionGraph) {
    for _ in 0..16 {
        let r = opt.optimize_raw(&mut fg.graph, fg.entry).unwrap();
        if !r.changed() {
            return;
        }
    }
    panic!("optimizer failed to reach fixed point");
}

#[test]
fn parity_rule1_eq_zr_identity() {
    let (mut fg_v1, if_v1, a, b) =
        build_if_with_flag_cond(|_fb, zr, _ng, _cy, _ov| zr);
    run_to_fp(&FlagCmpCanonicalize, &mut fg_v1);
    let v1_shape = classify_cond(&fg_v1, if_v1);

    let (mut fg_v2, if_v2, a2, b2) =
        build_if_with_flag_cond(|_fb, zr, _ng, _cy, _ov| zr);
    run_to_fp(&FlagCmpCanonicalizeEgg::new(), &mut fg_v2);
    let v2_shape = classify_cond(&fg_v2, if_v2);

    assert_eq!(v1_shape, CondShape::IntCmp(IntCmpOp::Equal, a, b));
    assert_eq!(v2_shape, CondShape::IntCmp(IntCmpOp::Equal, a2, b2));
}

#[test]
fn parity_rule2_hi() {
    let (mut fg_v1, if_v1, a, b) = build_if_with_flag_cond(|fb, zr, _ng, cy, _ov| {
        let neg_zr = fb
            .build_boolean_unary_operation(zr, BoolUnaryOp::Neg)
            .unwrap();
        fb.build_boolean_operation(cy, neg_zr, BoolBinaryOp::And)
            .unwrap()
    });
    run_to_fp(&FlagCmpCanonicalize, &mut fg_v1);
    let v1_shape = classify_cond(&fg_v1, if_v1);

    let (mut fg_v2, if_v2, a2, b2) = build_if_with_flag_cond(|fb, zr, _ng, cy, _ov| {
        let neg_zr = fb
            .build_boolean_unary_operation(zr, BoolUnaryOp::Neg)
            .unwrap();
        fb.build_boolean_operation(cy, neg_zr, BoolBinaryOp::And)
            .unwrap()
    });
    run_to_fp(&FlagCmpCanonicalizeEgg::new(), &mut fg_v2);
    let v2_shape = classify_cond(&fg_v2, if_v2);

    assert_eq!(v1_shape, CondShape::IntCmp(IntCmpOp::Less, b, a));
    assert_eq!(v2_shape, CondShape::IntCmp(IntCmpOp::Less, b2, a2));
}

#[test]
fn parity_rule3_ls_after_constant_fold() {
    // LS requires ConstantFold to collapse BoolNeg(BoolNeg(IntLess)) first.
    let build = || {
        build_if_with_flag_cond(|fb, zr, _ng, cy, _ov| {
            let neg_cy = fb
                .build_boolean_unary_operation(cy, BoolUnaryOp::Neg)
                .unwrap();
            fb.build_boolean_operation(neg_cy, zr, BoolBinaryOp::Or).unwrap()
        })
    };
    let (mut fg_v1, if_v1, a, b) = build();
    strider_analyze::opt::ConstantFold
        .optimize_raw(&mut fg_v1.graph, fg_v1.entry)
        .unwrap();
    run_to_fp(&FlagCmpCanonicalize, &mut fg_v1);
    let v1_shape = classify_cond(&fg_v1, if_v1);

    let (mut fg_v2, if_v2, a2, b2) = build();
    strider_analyze::opt::ConstantFold
        .optimize_raw(&mut fg_v2.graph, fg_v2.entry)
        .unwrap();
    run_to_fp(&FlagCmpCanonicalizeEgg::new(), &mut fg_v2);
    let v2_shape = classify_cond(&fg_v2, if_v2);

    assert_eq!(v1_shape, CondShape::NegIntCmp(IntCmpOp::Less, b, a));
    assert_eq!(v2_shape, CondShape::NegIntCmp(IntCmpOp::Less, b2, a2));
}

#[test]
fn parity_rule4_lt() {
    let build = || {
        build_if_with_flag_cond(|fb, _zr, ng, _cy, ov| {
            let eq = fb
                .build_int_cmp_operation(ng, ov, IntCmpOp::Equal, NodeOutputType::U8)
                .unwrap();
            fb.build_boolean_unary_operation(eq, BoolUnaryOp::Neg).unwrap()
        })
    };
    let (mut fg_v1, if_v1, a, b) = build();
    run_to_fp(&FlagCmpCanonicalize, &mut fg_v1);
    let v1_shape = classify_cond(&fg_v1, if_v1);

    let (mut fg_v2, if_v2, a2, b2) = build();
    run_to_fp(&FlagCmpCanonicalizeEgg::new(), &mut fg_v2);
    let v2_shape = classify_cond(&fg_v2, if_v2);

    assert_eq!(v1_shape, CondShape::IntCmp(IntCmpOp::Sless, a, b));
    assert_eq!(v2_shape, CondShape::IntCmp(IntCmpOp::Sless, a2, b2));
}

#[test]
fn parity_rule5_ge() {
    let build = || {
        build_if_with_flag_cond(|fb, _zr, ng, _cy, ov| {
            fb.build_int_cmp_operation(ng, ov, IntCmpOp::Equal, NodeOutputType::U8)
                .unwrap()
        })
    };
    let (mut fg_v1, if_v1, a, b) = build();
    run_to_fp(&FlagCmpCanonicalize, &mut fg_v1);
    let v1_shape = classify_cond(&fg_v1, if_v1);

    let (mut fg_v2, if_v2, a2, b2) = build();
    run_to_fp(&FlagCmpCanonicalizeEgg::new(), &mut fg_v2);
    let v2_shape = classify_cond(&fg_v2, if_v2);

    assert_eq!(v1_shape, CondShape::NegIntCmp(IntCmpOp::Sless, a, b));
    assert_eq!(v2_shape, CondShape::NegIntCmp(IntCmpOp::Sless, a2, b2));
}

#[test]
fn parity_rule6_gt() {
    let build = || {
        build_if_with_flag_cond(|fb, zr, ng, _cy, ov| {
            let neg_zr = fb
                .build_boolean_unary_operation(zr, BoolUnaryOp::Neg)
                .unwrap();
            let eq = fb
                .build_int_cmp_operation(ng, ov, IntCmpOp::Equal, NodeOutputType::U8)
                .unwrap();
            fb.build_boolean_operation(neg_zr, eq, BoolBinaryOp::And).unwrap()
        })
    };
    let (mut fg_v1, if_v1, a, b) = build();
    run_to_fp(&FlagCmpCanonicalize, &mut fg_v1);
    let v1_shape = classify_cond(&fg_v1, if_v1);

    let (mut fg_v2, if_v2, a2, b2) = build();
    run_to_fp(&FlagCmpCanonicalizeEgg::new(), &mut fg_v2);
    let v2_shape = classify_cond(&fg_v2, if_v2);

    assert_eq!(v1_shape, CondShape::IntCmp(IntCmpOp::Sless, b, a));
    assert_eq!(v2_shape, CondShape::IntCmp(IntCmpOp::Sless, b2, a2));
}

#[test]
fn parity_rule7_le() {
    let build = || {
        build_if_with_flag_cond(|fb, zr, ng, _cy, ov| {
            let eq = fb
                .build_int_cmp_operation(ng, ov, IntCmpOp::Equal, NodeOutputType::U8)
                .unwrap();
            let neg_eq = fb
                .build_boolean_unary_operation(eq, BoolUnaryOp::Neg)
                .unwrap();
            fb.build_boolean_operation(zr, neg_eq, BoolBinaryOp::Or).unwrap()
        })
    };
    let (mut fg_v1, if_v1, a, b) = build();
    run_to_fp(&FlagCmpCanonicalize, &mut fg_v1);
    let v1_shape = classify_cond(&fg_v1, if_v1);

    let (mut fg_v2, if_v2, a2, b2) = build();
    run_to_fp(&FlagCmpCanonicalizeEgg::new(), &mut fg_v2);
    let v2_shape = classify_cond(&fg_v2, if_v2);

    assert_eq!(v1_shape, CondShape::NegIntCmp(IntCmpOp::Sless, b, a));
    assert_eq!(v2_shape, CondShape::NegIntCmp(IntCmpOp::Sless, b2, a2));
}

#[test]
fn parity_rule8_thumb_beq_two_iterations() {
    // BEQ → IntEqual(CastToInt(ZR), 0) post-canonicalisation gives
    // BoolNeg(IntEqual(CastToInt(ZR), 0)).  Rule 9 strips that to ZR,
    // then Rule 1 simplifies the inner Equal(diff, 0) → Equal(a, b).
    let build = || {
        build_if_with_flag_cond(|fb, zr, _ng, _cy, _ov| {
            let zero = fb.build_int_const(0u64, NodeOutputType::U8).unwrap();
            let eq = fb
                .build_int_cmp_operation(zr, zero, IntCmpOp::Equal, NodeOutputType::U8)
                .unwrap();
            fb.build_boolean_unary_operation(eq, BoolUnaryOp::Neg).unwrap()
        })
    };
    let (mut fg_v1, if_v1, a, b) = build();
    run_to_fp(&FlagCmpCanonicalize, &mut fg_v1);
    let v1_shape = classify_cond(&fg_v1, if_v1);

    let (mut fg_v2, if_v2, a2, b2) = build();
    run_to_fp(&FlagCmpCanonicalizeEgg::new(), &mut fg_v2);
    let v2_shape = classify_cond(&fg_v2, if_v2);

    assert_eq!(v1_shape, CondShape::IntCmp(IntCmpOp::Equal, a, b));
    assert_eq!(v2_shape, CondShape::IntCmp(IntCmpOp::Equal, a2, b2));
}

// ── Negative tests: shapes that should NOT be rewritten by either pass ──────

#[test]
fn parity_cs_left_alone() {
    // CS = bare CY = BoolNeg(IntLess(a, b)).  Already canonical.
    let (mut fg_v1, if_v1, _a, _b) =
        build_if_with_flag_cond(|_fb, _zr, _ng, cy, _ov| cy);
    let r1 = FlagCmpCanonicalize
        .optimize_raw(&mut fg_v1.graph, fg_v1.entry)
        .unwrap();
    let v1_shape = classify_cond(&fg_v1, if_v1);

    let (mut fg_v2, if_v2, _a2, _b2) =
        build_if_with_flag_cond(|_fb, _zr, _ng, cy, _ov| cy);
    let r2 = FlagCmpCanonicalizeEgg::new()
        .optimize_raw(&mut fg_v2.graph, fg_v2.entry)
        .unwrap();
    let v2_shape = classify_cond(&fg_v2, if_v2);

    assert!(!r1.changed(), "v1: CS already canonical");
    assert!(!r2.changed(), "v2: CS already canonical");
    assert_eq!(v1_shape, v2_shape, "parity: CS shape unchanged");
}

#[test]
fn parity_mi_left_alone() {
    // MI = bare NG = Sless(diff, 0).  Not algebraically reducible.
    let (mut fg_v1, if_v1, _a, _b) =
        build_if_with_flag_cond(|_fb, _zr, ng, _cy, _ov| ng);
    let r1 = FlagCmpCanonicalize
        .optimize_raw(&mut fg_v1.graph, fg_v1.entry)
        .unwrap();
    let v1_shape = classify_cond(&fg_v1, if_v1);

    let (mut fg_v2, if_v2, _a2, _b2) =
        build_if_with_flag_cond(|_fb, _zr, ng, _cy, _ov| ng);
    let r2 = FlagCmpCanonicalizeEgg::new()
        .optimize_raw(&mut fg_v2.graph, fg_v2.entry)
        .unwrap();
    let v2_shape = classify_cond(&fg_v2, if_v2);

    assert!(!r1.changed(), "v1: MI not reducible");
    assert!(!r2.changed(), "v2: MI not reducible");
    assert_eq!(v1_shape, v2_shape, "parity: MI shape unchanged");
}
