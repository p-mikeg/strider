//! Black-box: `Graph::make_int_const(val, ty)` masks `val` to the
//! declared type's bit width before keying the dedup cache, so
//! semantically-equal constants with different in-memory representations
//! collapse to one node.
//!
//! Regression — `make_int_const(0x1FF, U8)` and `make_int_const(0xFF, U8)`
//! used to produce two distinct `IntConst` nodes (0x1FF and 0xFF), each
//! advertising a `U8` output, defeating the dedup cache and yielding
//! two values whose runtime semantics are identical.

#![allow(clippy::unwrap_used)]

use ir::FunctionBuilder;
use ir::node::{NodeKind, NodeOutputType};

#[test]
fn make_int_const_masks_value_before_dedup_u8() {
    let mut b = FunctionBuilder::empty().unwrap();
    let g = b.graph_mut();

    let a = g.make_int_const(0xFF, NodeOutputType::U8).unwrap();
    let b_out = g.make_int_const(0x1FF, NodeOutputType::U8).unwrap();
    assert_eq!(
        a, b_out,
        "0xFF and 0x1FF must dedup as U8 (both represent the byte 0xFF)"
    );

    let node = g.get_node_from_output(a);
    let kind = g.node_kind(node);
    assert!(
        matches!(kind, NodeKind::IntConst(0xFF)),
        "deduped node payload must be the masked value 0xFF, got {kind:?}"
    );
}

#[test]
fn make_int_const_masks_value_before_dedup_u32() {
    let mut b = FunctionBuilder::empty().unwrap();
    let g = b.graph_mut();

    let a = g.make_int_const(0xFFFF_FFFF, NodeOutputType::U32).unwrap();
    let b_out = g
        .make_int_const(0x1_FFFF_FFFF, NodeOutputType::U32)
        .unwrap();
    assert_eq!(
        a, b_out,
        "0xFFFF_FFFF and 0x1_FFFF_FFFF must dedup as U32 (both represent the same low 32 bits)"
    );
}

#[test]
fn make_int_const_distinct_values_distinct_nodes() {
    let mut b = FunctionBuilder::empty().unwrap();
    let g = b.graph_mut();

    let a = g.make_int_const(0xFF, NodeOutputType::U8).unwrap();
    let b_out = g.make_int_const(0xFE, NodeOutputType::U8).unwrap();
    assert_ne!(
        a, b_out,
        "distinct masked values must produce distinct IntConst nodes"
    );
}
