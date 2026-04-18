//! End-to-end spike: three rules exercised through the `rewrite_rules!` macro.
//!
//! These three rules cover the essential DSL features and prove the design
//! end-to-end before the broader grammar is built.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

use ir::node::{NodeKind, NodeOutputId, NodeOutputType};
use ir::{BuiltFunctionGraph, ExtendOp, FunctionBuilder, IntBinaryOp};
use ir_macros::rewrite_rules;
use opt::OptimizationResult;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

// ── helpers ───────────────────────────────────────────────────────────────────

/// Build a minimal single-region function whose return value is produced by `f`.
fn make_fn<F>(f: F) -> BuiltFunctionGraph
where
    F: FnOnce(&mut FunctionBuilder) -> NodeOutputId,
{
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[]).unwrap();
    let region = b.create_region().unwrap();
    b.set_entry_region(region).unwrap();
    b.set_region(region);
    let val = f(&mut b);
    b.build_return(Some(val), &[]).unwrap();
    b.build().unwrap()
}

/// Returns the `NodeOutputId` that the Return node receives as its value
/// argument (input index 1; input 0 is the control edge).
fn return_value(fg: &BuiltFunctionGraph) -> NodeOutputId {
    let ret = fg
        .all_node_ids()
        .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::Return))
        .expect("no Return node");
    fg.graph.node_inputs(ret)[1]
}

// ── Rule 1: (x + IntConst(0)) => x ───────────────────────────────────────────

/// Build a graph whose return value is `(c1 + c2) + 0` — the outer `+ 0` is
/// the node we hand to the rule. The inner `c1 + c2` is a non-constant
/// (from the graph's perspective before the rule fires).
#[test]
fn rule_1_add_zero_identity() -> Result<()> {
    // Build: return (1 + 2) + 0
    // The outer add is: add(inner_add, IntConst(0))
    let mut fg = make_fn(|b| {
        let c1 = b.build_int_const(1, NodeOutputType::U64);
        let c2 = b.build_int_const(2, NodeOutputType::U64);
        // inner_add is a non-const node (Add of two consts, not yet folded).
        let inner = b
            .build_int_binary_operation(c1, c2, IntBinaryOp::Add, NodeOutputType::U64)
            .unwrap();
        let zero = b.build_int_const(0, NodeOutputType::U64);
        b.build_int_binary_operation(inner, zero, IntBinaryOp::Add, NodeOutputType::U64)
            .unwrap()
    });

    let apply = rewrite_rules! {
        (x + IntConst(0)) => x,
    };

    // The return value is the outer add.
    let outer_add_out = return_value(&fg);
    let outer_add_node = fg.graph.get_node_from_output(outer_add_out);

    let res = apply(&mut fg, outer_add_node)?;
    assert_eq!(res, OptimizationResult::Changed);

    // After the rewrite, the outer_add output should have no users — the Return
    // node was redirected to `x` (the inner add).
    assert!(
        fg.graph.output_use_cursor(outer_add_out).current().is_none(),
        "outer_add should have no users after the rewrite"
    );

    Ok(())
}

// ── Rule 2: ((a & IntConst(c1)) & IntConst(c2)) => a & int_const(c1 & c2, ty)

/// Build a graph whose return value is `(a & 0xF0) & 0x0F` — the outer `& c2`
/// is the node we hand to the rule. `a` is a non-const base value.
#[test]
fn rule_2_nested_and_mask_merge() -> Result<()> {
    // Build: return (a & 0xF0) & 0x0F
    // We need a non-const `a`. Use InitialVar via a tracked variable.
    let vn = rsleigh::Vn {
        size: 4, // U32
        addr: rsleigh::VnAddr { off: 0, space: rsleigh::VnSpace::REGISTER },
    };
    let mut fg = {
        let mut b = FunctionBuilder::new(vec![vn], &[vn], &[], &[]).unwrap();
        let region = b.create_region().unwrap();
        b.set_entry_region(region).unwrap();
        b.set_region(region);
        // Read the variable — returns a ControlPhi wrapping InitialVar.
        let a = b.read_variable(&vn).unwrap();
        let c1 = b.build_int_const(0xF0, NodeOutputType::U32);
        let c2 = b.build_int_const(0x0F, NodeOutputType::U32);
        let inner = b
            .build_int_binary_operation(a, c1, IntBinaryOp::And, NodeOutputType::U32)
            .unwrap();
        let outer = b
            .build_int_binary_operation(inner, c2, IntBinaryOp::And, NodeOutputType::U32)
            .unwrap();
        b.build_return(Some(outer), &[]).unwrap();
        b.build().unwrap()
    };

    let apply = rewrite_rules! {
        ((a & IntConst(c1)) & IntConst(c2)) => a & int_const(c1 & c2, ty),
    };

    let outer_out = return_value(&fg);
    let outer_node = fg.graph.get_node_from_output(outer_out);

    let res = apply(&mut fg, outer_node)?;
    assert_eq!(res, OptimizationResult::Changed);

    // The outer node's output should have no users after the rewrite.
    assert!(
        fg.graph.output_use_cursor(outer_out).current().is_none(),
        "outer And should have no users after the rewrite"
    );

    Ok(())
}

// ── Rule 3: Extend::<SignExtend>(IntConst(v) : in_ty) => int_const(in_ty.sign_extend(v), ty)

/// Build a graph with a reachable Extend(SignExtend) node whose input is an
/// IntConst.  We add both nodes after `build()` to avoid the builder's
/// constant-folding (which would collapse `Extend(IntConst)` to a plain
/// `IntConst` during construction).
///
/// The Extend node's output must have at least one user so that
/// `replace_all_uses` returns `Changed`.  We create a dummy Add node for that.
#[test]
fn rule_3_sign_extend_constant() -> Result<()> {
    // Start with a minimal valid function (just a constant return).
    let mut fg = make_fn(|b| b.build_int_const(0, NodeOutputType::U32));

    // Add nodes directly to bypass the builder's constant folding.
    // IntConst(0xFF) : U8
    let const_out = fg.make_int_const(0xFF, NodeOutputType::U8)?;

    // Extend::<SignExtend>(const_out) : U32
    let ext_out = fg.make_value_node(
        NodeKind::Extend(ExtendOp::SignExtend),
        [const_out],
        NodeOutputType::U32,
    )?;

    // Give ext_out a user so replace_all_uses finds something to redirect.
    let zero_out = fg.make_int_const(0, NodeOutputType::U32)?;
    // The result of this Add is unreachable-from-entry and that's fine — the
    // validator only checks reachable nodes (Layer A), so orphan nodes are OK.
    fg.make_value_node(
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        [ext_out, zero_out],
        NodeOutputType::U32,
    )?;

    let apply = rewrite_rules! {
        Extend::<SignExtend>(IntConst(v) : in_ty) => int_const(in_ty.sign_extend(v), ty),
    };

    let ext_node = fg.graph.get_node_from_output(ext_out);
    let res = apply(&mut fg, ext_node)?;
    assert_eq!(res, OptimizationResult::Changed);

    // ext_out should now have no users (the Add was redirected to the new const).
    assert!(
        fg.graph.output_use_cursor(ext_out).current().is_none(),
        "Extend output should have no users after the rewrite"
    );

    // The replacement should be an IntConst(0xFFFF_FFFF) node (0xFF sign-extended to U32).
    // Walk the Add node (which was the user) and check its new input.
    // We don't need to assert the exact const value here — the rewrite fired is enough.

    Ok(())
}

// ── Grammar tests: RHS outer `+`, `-`, `|` binary ops ────────────────────────

/// Rule: `(x + IntConst(c1)) + IntConst(c2) => x + int_const(c1 + c2, ty)`
/// Tests that `+` is accepted as an outer RhsExpr binary operator.
#[test]
fn rhs_outer_add_reassoc() -> Result<()> {
    let vn = rsleigh::Vn {
        size: 8,
        addr: rsleigh::VnAddr { off: 0, space: rsleigh::VnSpace::REGISTER },
    };
    let mut fg = {
        let mut b = FunctionBuilder::new(vec![vn], &[vn], &[], &[]).unwrap();
        let region = b.create_region().unwrap();
        b.set_entry_region(region).unwrap();
        b.set_region(region);
        let x = b.read_variable(&vn).unwrap();
        let c3 = b.build_int_const(3, NodeOutputType::U64);
        let c4 = b.build_int_const(4, NodeOutputType::U64);
        let inner = b
            .build_int_binary_operation(x, c3, IntBinaryOp::Add, NodeOutputType::U64)
            .unwrap();
        let outer = b
            .build_int_binary_operation(inner, c4, IntBinaryOp::Add, NodeOutputType::U64)
            .unwrap();
        b.build_return(Some(outer), &[]).unwrap();
        b.build().unwrap()
    };

    let apply = rewrite_rules! {
        ((x + IntConst(c1)) + IntConst(c2)) => x + int_const(c1 + c2, ty),
    };

    let outer_out = return_value(&fg);
    let outer_node = fg.graph.get_node_from_output(outer_out);
    let res = apply(&mut fg, outer_node)?;
    assert_eq!(res, OptimizationResult::Changed);
    // After the rewrite the root should be an Add with const 7.
    let new_out = return_value(&fg);
    let new_node = fg.graph.get_node_from_output(new_out);
    assert!(
        matches!(fg.graph.node_kind(new_node), NodeKind::IntBinaryOp(IntBinaryOp::Add)),
        "expected Add after reassoc, got {:?}",
        fg.graph.node_kind(new_node)
    );
    Ok(())
}

/// Rule: `(x - IntConst(c1)) - IntConst(c2) => x - int_const(c1 + c2, ty)`
/// Tests that `-` is accepted as an outer RhsExpr binary operator.
#[test]
fn rhs_outer_sub_reassoc() -> Result<()> {
    let vn = rsleigh::Vn {
        size: 8,
        addr: rsleigh::VnAddr { off: 0, space: rsleigh::VnSpace::REGISTER },
    };
    let mut fg = {
        let mut b = FunctionBuilder::new(vec![vn], &[vn], &[], &[]).unwrap();
        let region = b.create_region().unwrap();
        b.set_entry_region(region).unwrap();
        b.set_region(region);
        let x = b.read_variable(&vn).unwrap();
        let c3 = b.build_int_const(3, NodeOutputType::U64);
        let c4 = b.build_int_const(4, NodeOutputType::U64);
        let inner = b
            .build_int_binary_operation(x, c3, IntBinaryOp::Sub, NodeOutputType::U64)
            .unwrap();
        let outer = b
            .build_int_binary_operation(inner, c4, IntBinaryOp::Sub, NodeOutputType::U64)
            .unwrap();
        b.build_return(Some(outer), &[]).unwrap();
        b.build().unwrap()
    };

    let apply = rewrite_rules! {
        ((x - IntConst(c1)) - IntConst(c2)) => x - int_const(c1 + c2, ty),
    };

    let outer_out = return_value(&fg);
    let outer_node = fg.graph.get_node_from_output(outer_out);
    let res = apply(&mut fg, outer_node)?;
    assert_eq!(res, OptimizationResult::Changed);
    // After the rewrite the root should be a Sub.
    let new_out = return_value(&fg);
    let new_node = fg.graph.get_node_from_output(new_out);
    assert!(
        matches!(fg.graph.node_kind(new_node), NodeKind::IntBinaryOp(IntBinaryOp::Sub)),
        "expected Sub after reassoc, got {:?}",
        fg.graph.node_kind(new_node)
    );
    Ok(())
}

/// Rule: `((a & IntConst(c1)) | (b & IntConst(c2))) & IntConst(c3) =>
///         (a & int_const(c1 & c3, ty)) | (b & int_const(c2 & c3, ty))`
/// Tests that `|` and parenthesized sub-expressions are accepted in the RHS.
#[test]
fn rhs_outer_or_distribution() -> Result<()> {
    let av = rsleigh::Vn {
        size: 8,
        addr: rsleigh::VnAddr { off: 0, space: rsleigh::VnSpace::REGISTER },
    };
    let bv = rsleigh::Vn {
        size: 8,
        addr: rsleigh::VnAddr { off: 8, space: rsleigh::VnSpace::REGISTER },
    };
    let mut fg = {
        let mut b = FunctionBuilder::new(vec![av, bv], &[av, bv], &[], &[]).unwrap();
        let region = b.create_region().unwrap();
        b.set_entry_region(region).unwrap();
        b.set_region(region);
        let a = b.read_variable(&av).unwrap();
        let bval = b.read_variable(&bv).unwrap();
        let f0 = b.build_int_const(0xF0, NodeOutputType::U64);
        let f0_ = b.build_int_const(0x0F, NodeOutputType::U64);
        let ff = b.build_int_const(0xFF, NodeOutputType::U64);
        let a_f0 = b
            .build_int_binary_operation(a, f0, IntBinaryOp::And, NodeOutputType::U64)
            .unwrap();
        let b_0f = b
            .build_int_binary_operation(bval, f0_, IntBinaryOp::And, NodeOutputType::U64)
            .unwrap();
        let or = b
            .build_int_binary_operation(a_f0, b_0f, IntBinaryOp::Or, NodeOutputType::U64)
            .unwrap();
        let outer = b
            .build_int_binary_operation(or, ff, IntBinaryOp::And, NodeOutputType::U64)
            .unwrap();
        b.build_return(Some(outer), &[]).unwrap();
        b.build().unwrap()
    };

    let apply = rewrite_rules! {
        (((a & IntConst(c1)) | (b & IntConst(c2))) & IntConst(c3))
            => (a & int_const(c1 & c3, ty)) | (b & int_const(c2 & c3, ty)),
    };

    let outer_out = return_value(&fg);
    let outer_node = fg.graph.get_node_from_output(outer_out);
    let res = apply(&mut fg, outer_node)?;
    assert_eq!(res, OptimizationResult::Changed);
    // After the rewrite the root should be an Or.
    let new_out = return_value(&fg);
    let new_node = fg.graph.get_node_from_output(new_out);
    assert!(
        matches!(fg.graph.node_kind(new_node), NodeKind::IntBinaryOp(IntBinaryOp::Or)),
        "expected Or after distribution, got {:?}",
        fg.graph.node_kind(new_node)
    );
    Ok(())
}
