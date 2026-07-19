//! Incremental re-canonicalization: a rewrite that rewires a live node's
//! inputs into a structural twin of an existing node is merged at the next
//! `EditFunction::clean()` drain. Nodes differing in OUTPUT type are distinct
//! values and are never merged.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use strider_ir::node::{NodeKind, ValueType};
use strider_ir::{EditFunction, IRBuilderExt, IRViewer, IRWalker, IntBinaryOp};
use strider_ir_test_utils::{RegisterSet, reg_vn};

fn reachable(fg: &strider_ir::Function, node: strider_ir::node::NodeId) -> bool {
    fg.walk().any(|n| n == node)
}

/// Redirecting `add2`'s `c2` operand to `c1` makes it a structural twin of
/// `add1` that the construction cache never re-canonicalised. `clean()`
/// must merge them.
#[test]
fn clean_merges_structural_twin_left_by_a_rewrite() {
    let a_vn = reg_vn(0x10, 8);
    let mut b = RegisterSet::new()
        .tracked(a_vn)
        .build_fn_single_region()
        .unwrap();
    let a = b.read_variable(&a_vn).unwrap();
    let c1 = b.build_int_const(1u64, ValueType::I64).unwrap();
    let c2 = b.build_int_const(2u64, ValueType::I64).unwrap();
    let add1 = b
        .build_int_binary_operation(a, c1, IntBinaryOp::Add, ValueType::I64)
        .unwrap();
    let add2 = b
        .build_int_binary_operation(a, c2, IntBinaryOp::Add, ValueType::I64)
        .unwrap();
    // Keeps BOTH twins reachable from entry.
    let add3 = b
        .build_int_binary_operation(add1, add2, IntBinaryOp::Add, ValueType::I64)
        .unwrap();
    b.build_return(Some(add3), &[]).unwrap();
    b.set_lift_addr(None);
    let mut fg = b.build().unwrap();

    let add1_node = fg.producer(add1);
    let add2_node = fg.producer(add2);
    let add3_node = fg.producer(add3);
    assert_ne!(
        add1_node, add2_node,
        "fixture starts with distinct producers"
    );

    {
        let mut ef = EditFunction::new(&mut fg);
        // add2 becomes Add(a, c1), a twin of add1.
        ef.replace_all_uses(c2, c1).unwrap();
        ef.clean(); // merges the twin
    }

    let survivors = [add1_node, add2_node]
        .into_iter()
        .filter(|&n| reachable(&fg, n))
        .count();
    assert_eq!(survivors, 1, "clean() must merge the structural twin");
    let ins: Vec<_> = fg.node_inputs(add3_node).into_iter().collect();
    assert_eq!(ins.len(), 2);
    assert_eq!(
        ins[0], ins[1],
        "both operands of the consumer now point at the surviving twin"
    );
}

/// Two `Truncate`s to DIFFERENT widths are different values, since the dedup
/// key includes the output kind. Even once a rewrite makes their inputs
/// identical, `clean()` must not merge them.
#[test]
fn clean_does_not_merge_when_output_type_differs() {
    let a_vn = reg_vn(0x10, 8);
    let b_vn = reg_vn(0x18, 8);
    let mut b = RegisterSet::new()
        .tracked(a_vn)
        .tracked(b_vn)
        .build_fn_single_region()
        .unwrap();
    let a = b.read_variable(&a_vn).unwrap();
    let bb = b.read_variable(&b_vn).unwrap();
    // Distinct inputs for now: Truncate(a):I32 and Truncate(bb):I16.
    let t32 = b.truncate_if_needed(a, ValueType::I32).unwrap();
    let t16 = b.truncate_if_needed(bb, ValueType::I16).unwrap();
    let w32 = b
        .extend_if_needed(t32, ValueType::I64, strider_ir::node::ExtendOp::ZeroExtend)
        .unwrap();
    let w16 = b
        .extend_if_needed(t16, ValueType::I64, strider_ir::node::ExtendOp::ZeroExtend)
        .unwrap();
    let sum = b
        .build_int_binary_operation(w32, w16, IntBinaryOp::Add, ValueType::I64)
        .unwrap();
    b.build_return(Some(sum), &[]).unwrap();
    b.set_lift_addr(None);
    let mut fg = b.build().unwrap();

    let t32_node = fg.producer(t32);
    let t16_node = fg.producer(t16);
    let truncs = |fg: &strider_ir::Function| {
        fg.walk()
            .filter(|&n| matches!(fg.node_kind(n), NodeKind::Truncate))
            .count()
    };
    assert_eq!(truncs(&fg), 2, "fixture has two differently-typed truncs");

    {
        let mut ef = EditFunction::new(&mut fg);
        // t16 becomes Truncate(a):I16: same input as t32 but a different
        // output width, so canonicalization must keep them separate.
        ef.replace_all_uses(bb, a).unwrap();
        ef.clean();
    }

    assert!(
        reachable(&fg, t32_node) && reachable(&fg, t16_node),
        "differently-typed truncations must not be merged"
    );
    assert_eq!(truncs(&fg), 2, "both truncs survive");
}

#[test]
fn default_pipeline_has_ten_inloop_passes() {
    let p = strider_opt::default_pipeline();
    assert_eq!(
        p.passes().len(),
        10,
        "DedupNodes removed -> 10 in-loop passes"
    );
    assert_eq!(p.post_passes().len(), 3);
}
