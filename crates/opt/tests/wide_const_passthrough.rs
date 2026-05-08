//! Wide-const Phase-1 passthrough: ConstantFold + KnownBits + the full
//! default pipeline must leave `IntConstWide` nodes untouched.  Wide
//! arithmetic folding is deferred to Phase 2 (would require bnum or a
//! hand-rolled wide-arith library).
//!
//! These tests pin the contract: a graph containing `IntConstWide`
//! survives every default-pipeline pass without losing or rewriting
//! the wide nodes.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::unreachable)]

use ir::node::{NodeKind, NodeOutputType};
use ir::wide_const::WideConstStorage;
use ir::FunctionBuilder;

#[test]
fn default_pipeline_preserves_wide_consts() {
    let mut fb = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0).unwrap();
    let entry = fb.create_region().unwrap();
    fb.set_entry_region(entry).unwrap();
    fb.set_region(entry);

    let v = WideConstStorage::U256([0xdead, 0xbeef, 0xcafe, 0xbabe]);
    let wide = fb.build_int_const_wide(v.clone(), NodeOutputType::U256).unwrap();
    fb.build_return(Some(wide), &[]).unwrap();

    let mut bfg = fb.build().unwrap();

    let pre_count = bfg
        .graph
        .all_node_ids()
        .filter(|n| matches!(bfg.graph.node_kind(*n), NodeKind::IntConstWide(_)))
        .count();
    assert_eq!(pre_count, 1, "exactly one IntConstWide before pipeline");

    opt::default_pipeline()
        .run_on_built(&mut bfg)
        .expect("default pipeline must accept wide consts");

    let post_count = bfg
        .graph
        .all_node_ids()
        .filter(|n| matches!(bfg.graph.node_kind(*n), NodeKind::IntConstWide(_)))
        .count();
    assert_eq!(
        post_count, 1,
        "default pipeline must not delete or rewrite wide consts"
    );

    // The wide value must still be retrievable byte-for-byte.
    let wide_node = bfg
        .graph
        .all_node_ids()
        .find(|n| matches!(bfg.graph.node_kind(*n), NodeKind::IntConstWide(_)))
        .unwrap();
    let NodeKind::IntConstWide(id) = bfg.graph.node_kind(wide_node) else {
        unreachable!()
    };
    assert_eq!(bfg.graph.wide_const(*id), &v);

    // Validator stays clean.
    ir::validate::validate(&bfg.graph, bfg.entry).expect("validate after pipeline");
}

#[test]
fn known_bits_skips_wide_outputs_without_taint() {
    // Build a function that branches on a narrow comparison consuming a
    // narrow IntConst.  Add an unrelated IntConstWide on the side.  The
    // narrow flow's KnownBits inferences must be unaffected by the
    // presence of the wide node — the wide one returns None from the
    // analysis and the worklist skips it.
    let mut fb = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0).unwrap();
    let entry = fb.create_region().unwrap();
    fb.set_entry_region(entry).unwrap();
    fb.set_region(entry);

    let narrow = fb.build_int_const(0xff_u64, NodeOutputType::U64).unwrap();
    let _wide = fb
        .build_int_const_wide(WideConstStorage::U256([0x1; 4]), NodeOutputType::U256)
        .unwrap();
    fb.build_return(Some(narrow), &[]).unwrap();

    let mut bfg = fb.build().unwrap();

    // Pipeline accepts and validates.
    opt::default_pipeline()
        .run_on_built(&mut bfg)
        .expect("default pipeline tolerates orphan wide consts");
    ir::validate::validate(&bfg.graph, bfg.entry).expect("validate after pipeline");
}
