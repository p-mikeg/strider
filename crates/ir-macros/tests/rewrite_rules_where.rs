//! Tests for the `where <Expr>` post-match guard extension of `rewrite_rules!`.
//!
//! Grammar:
//! ```text
//! Rule := LhsPat 'where' <Expr> '=>' RhsExpr
//! ```
//!
//! The guard is evaluated AFTER LHS captures (`l`, `r`, `ty`, etc.) are bound.
//! If the guard is `false`:
//! - For commutative patterns, the other operand ordering is tried.
//! - For non-commutative / simple patterns, `OptimizationResult::NoChange` is returned.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

use ir::node::{NodeKind, NodeOutputType};
use ir::{FunctionBuilder, IntBinaryOp};
use ir_macros::rewrite_rules;
use opt::OptimizationResult;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Build a minimal `BuiltFunctionGraph` with no real body.
fn empty_fg() -> ir::BuiltFunctionGraph {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[]).unwrap();
    let region = b.create_region().unwrap();
    b.set_entry_region(region).unwrap();
    b.set_region(region);
    let v = b.build_int_const(0, NodeOutputType::U32);
    b.build_return(Some(v), &[]).unwrap();
    b.build().unwrap()
}

// ── Positive case: guard passes, rule fires ──────────────────────────────────

/// Pattern: `(IntConst(l) + IntConst(r)) where ty.fits_u64() => int_const(l + r, ty)`.
/// U32 fits in u64, so the guard passes and the rule fires → Changed.
#[test]
fn where_clause_fires_when_guard_true() -> Result<()> {
    let mut fg = empty_fg();

    let c1 = fg.make_int_const(3, NodeOutputType::U32)?;
    let c2 = fg.make_int_const(5, NodeOutputType::U32)?;
    let add_out = fg.make_value_node(
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        [c1, c2],
        NodeOutputType::U32,
    )?;

    // Give add_out a user so replace_all_uses has something to redirect.
    let sink = fg.make_int_const(0, NodeOutputType::U32)?;
    fg.make_value_node(
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        [add_out, sink],
        NodeOutputType::U32,
    )?;

    let apply = rewrite_rules! {
        (IntConst(l) + IntConst(r)) where ty.fits_u64() => int_const(l + r, ty),
    };

    let add_node = fg.graph.get_node_from_output(add_out);
    let res = apply(&mut fg, add_node)?;
    assert_eq!(res, OptimizationResult::Changed);

    Ok(())
}

// ── Negative case: guard fails, rule does not fire ───────────────────────────

/// Same pattern but U128 operands: `ty.fits_u64()` returns `false` for U128,
/// so the guard rejects the match and the result is `NoChange`.
#[test]
fn where_clause_skips_when_guard_false() -> Result<()> {
    let mut fg = empty_fg();

    let c1 = fg.make_int_const(3, NodeOutputType::U128)?;
    let c2 = fg.make_int_const(5, NodeOutputType::U128)?;
    let add_out = fg.make_value_node(
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        [c1, c2],
        NodeOutputType::U128,
    )?;

    // Give add_out a user.
    let sink = fg.make_int_const(0, NodeOutputType::U128)?;
    fg.make_value_node(
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        [add_out, sink],
        NodeOutputType::U128,
    )?;

    let apply = rewrite_rules! {
        (IntConst(l) + IntConst(r)) where ty.fits_u64() => int_const(l + r, ty),
    };

    let add_node = fg.graph.get_node_from_output(add_out);
    let res = apply(&mut fg, add_node)?;
    // Guard fails (U128 doesn't fit u64): rule must NOT fire.
    assert_eq!(res, OptimizationResult::NoChange);

    Ok(())
}

// ── Commutative + where guard: ordering fallthrough ──────────────────────────

/// Pattern: `(IntConst(l) + IntConst(r)) where l < r => int_const(r - l, ty)`.
/// Note: `r - l` uses the RHS value-level subtraction (`wrapping_sub`).
/// With c1=3, c2=5, the first ordering gives l=3, r=5 → 3 < 5 → true → fires.
/// With c1=5, c2=3, the first ordering gives l=5, r=3 → 5 < 3 → false (guard fails),
/// then the second ordering gives l=3, r=5 → 3 < 5 → true → fires.
/// This verifies that a failed guard in one ordering falls through to the next.
#[test]
fn where_clause_commutative_fallthrough() -> Result<()> {
    // Case A: operands already in order (first ordering fires immediately).
    {
        let mut fg = empty_fg();
        let c1 = fg.make_int_const(3, NodeOutputType::U32)?; // l=3
        let c2 = fg.make_int_const(5, NodeOutputType::U32)?; // r=5
        let add_out = fg.make_value_node(
            NodeKind::IntBinaryOp(IntBinaryOp::Add),
            [c1, c2],
            NodeOutputType::U32,
        )?;
        let sink = fg.make_int_const(0, NodeOutputType::U32)?;
        fg.make_value_node(
            NodeKind::IntBinaryOp(IntBinaryOp::Add),
            [add_out, sink],
            NodeOutputType::U32,
        )?;

        let apply = rewrite_rules! {
            (IntConst(l) + IntConst(r)) where l < r => int_const(r - l, ty),
        };

        let add_node = fg.graph.get_node_from_output(add_out);
        let res = apply(&mut fg, add_node)?;
        assert_eq!(res, OptimizationResult::Changed, "case A: first ordering should fire");
    }

    // Case B: operands in reverse order — first ordering fails the guard,
    // commutative flip should find the ordering where l < r.
    {
        let mut fg = empty_fg();
        let c1 = fg.make_int_const(5, NodeOutputType::U32)?; // placed first
        let c2 = fg.make_int_const(3, NodeOutputType::U32)?; // placed second
        let add_out = fg.make_value_node(
            NodeKind::IntBinaryOp(IntBinaryOp::Add),
            [c1, c2],
            NodeOutputType::U32,
        )?;
        let sink = fg.make_int_const(0, NodeOutputType::U32)?;
        fg.make_value_node(
            NodeKind::IntBinaryOp(IntBinaryOp::Add),
            [add_out, sink],
            NodeOutputType::U32,
        )?;

        let apply = rewrite_rules! {
            (IntConst(l) + IntConst(r)) where l < r => int_const(r - l, ty),
        };

        let add_node = fg.graph.get_node_from_output(add_out);
        let res = apply(&mut fg, add_node)?;
        // The second ordering (l=3, r=5) should pass the guard and fire.
        assert_eq!(res, OptimizationResult::Changed, "case B: second ordering should fire via commutative fallthrough");
    }

    // Case C: both orderings fail the guard → NoChange.
    {
        let mut fg = empty_fg();
        let c1 = fg.make_int_const(5, NodeOutputType::U32)?;
        let c2 = fg.make_int_const(5, NodeOutputType::U32)?; // l == r, never l < r
        let add_out = fg.make_value_node(
            NodeKind::IntBinaryOp(IntBinaryOp::Add),
            [c1, c2],
            NodeOutputType::U32,
        )?;
        let sink = fg.make_int_const(0, NodeOutputType::U32)?;
        fg.make_value_node(
            NodeKind::IntBinaryOp(IntBinaryOp::Add),
            [add_out, sink],
            NodeOutputType::U32,
        )?;

        let apply = rewrite_rules! {
            (IntConst(l) + IntConst(r)) where l < r => int_const(r - l, ty),
        };

        let add_node = fg.graph.get_node_from_output(add_out);
        let res = apply(&mut fg, add_node)?;
        assert_eq!(res, OptimizationResult::NoChange, "case C: both orderings fail guard");
    }

    Ok(())
}
