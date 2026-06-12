//! Unit tests for [`CommonSubexpr`].

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use strider_ir::node::{ExtendOp, IntBinaryOp, ValueType};
use strider_ir::{EditFunction, IRBuilderExt, IRWalker};
use strider_ir_test_utils::{RegisterSet, reg_vn};

use crate::pipeline::OptimizerTestExt;

fn reachable(fg: &strider_ir::Function, node: strider_ir::node::NodeId) -> bool {
    fg.walk().any(|n| n == node)
}

/// The motivating case: a rewrite turns one node into a structural twin of an
/// existing one (here by redirecting `add2`'s `c2` operand to `c1`, the way
/// `PhiCollapse` redirects a trivial phi).  The construction cache never sees
/// the change, so two identical `Add(a, c1)` nodes coexist — and the pass must
/// merge them back into one.
#[test]
fn merges_structural_twin_left_by_a_rewrite() -> crate::Result<()> {
    let a_vn = reg_vn(0x10, 8);
    let mut b = RegisterSet::new().tracked(a_vn).build_fn_single_region()?;
    let a = b.read_variable(&a_vn)?;
    let c1 = b.build_int_const(1u64, ValueType::I64)?;
    let c2 = b.build_int_const(2u64, ValueType::I64)?;
    let add1 = b.build_int_binary_operation(a, c1, IntBinaryOp::Add, ValueType::I64)?;
    let add2 = b.build_int_binary_operation(a, c2, IntBinaryOp::Add, ValueType::I64)?;
    // A downstream consumer keeps BOTH twins reachable from entry.
    let add3 = b.build_int_binary_operation(add1, add2, IntBinaryOp::Add, ValueType::I64)?;
    b.build_return(Some(add3), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let add1_node = fg.producer(add1);
    let add2_node = fg.producer(add2);
    let add3_node = fg.producer(add3);
    assert_ne!(add1_node, add2_node, "fixture starts with distinct producers");

    // Rewire `add2`'s `c2` operand to `c1` — `add2` becomes `Add(a, c1)`, a
    // structural twin of `add1`, but is NOT re-deduplicated.
    {
        let mut ef = EditFunction::new(&mut fg)?;
        ef.replace_all_uses(c2, c1)?;
    }
    assert!(reachable(&fg, add1_node) && reachable(&fg, add2_node));

    let result = CommonSubexpr.run_one(&mut fg, &mut crate::OptCtx::new(None))?;

    assert!(result.changed(), "pass must report a merge");
    // Exactly one of the two twins survives (whichever RPO visited first).
    let survivors = [add1_node, add2_node]
        .into_iter()
        .filter(|&n| reachable(&fg, n))
        .count();
    assert_eq!(survivors, 1, "exactly one twin must remain");
    // The consumer's two operands now resolve to that single survivor.
    let ins: Vec<_> = fg.node_inputs(add3_node).into_iter().collect();
    assert_eq!(ins.len(), 2);
    assert_eq!(ins[0], ins[1], "both operands now point at the surviving twin");
    Ok(())
}

/// Negative: two nodes that share kind + inputs but differ in OUTPUT type are
/// NOT the same value and must not be merged (the dedup key includes the
/// output kind, mirroring the construction cache).
#[test]
fn does_not_merge_when_output_type_differs() -> crate::Result<()> {
    let a_vn = reg_vn(0x10, 8);
    let mut b = RegisterSet::new().tracked(a_vn).build_fn_single_region()?;
    let a = b.read_variable(&a_vn)?;
    // Two truncations of the same I64 value to DIFFERENT widths.
    let t32 = b.truncate_if_needed(a, ValueType::I32)?;
    let t16 = b.truncate_if_needed(a, ValueType::I16)?;
    let w32 = b.extend_if_needed(t32, ValueType::I64, ExtendOp::ZeroExtend)?;
    let w16 = b.extend_if_needed(t16, ValueType::I64, ExtendOp::ZeroExtend)?;
    let sum = b.build_int_binary_operation(w32, w16, IntBinaryOp::Add, ValueType::I64)?;
    b.build_return(Some(sum), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let truncs = |fg: &strider_ir::Function| {
        fg.walk()
            .filter(|&n| matches!(fg.node_kind(n), NodeKind::Truncate))
            .count()
    };
    assert_eq!(truncs(&fg), 2, "fixture has two differently-typed truncs");

    CommonSubexpr.run_one(&mut fg, &mut crate::OptCtx::new(None))?;

    assert_eq!(
        truncs(&fg),
        2,
        "differently-typed truncations must not be merged"
    );
    Ok(())
}
