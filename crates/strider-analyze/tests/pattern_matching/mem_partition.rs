//! Pattern tests for `MemProjectPat`, `MemUnionPat`, and
//! `Matcher::ignore_mem_boundaries`.
//!
//! These tests construct IR graphs with `MemProject` / `MemUnion` nodes
//! directly via `FunctionBuilder::create_node_attributed` (no dedicated
//! builder method exists — those nodes are produced only by `AliasSplit`
//! in production).  Both node kinds are asm-fingerprint-exempt, so no
//! explicit fingerprint is required on them.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use strider_analyze::pattern::{
    AliasClass, Capture, IntoPat, Matcher, Pat, int_const, load, mem_project, mem_union, store,
};
use strider_ir::node::{NodeKind, NodeOutputKind, NodeOutputType};
use strider_ir_test_utils::RegisterSet;

// ── Low-level graph construction helpers ─────────────────────────────────────

/// Find the `InitialMemory` node in a freshly-built `FunctionBuilder` and
/// return its single memory output.  Uses `all_node_ids()` (not preorder)
/// because `InitialMemory` may not be reachable from the `Entry` node before
/// the function graph is complete.
fn initial_mem_out(b: &strider_ir::FunctionBuilder) -> strider_ir::node::NodeOutputId {
    let node = b
        .graph()
        .all_node_ids()
        .find(|&n| matches!(b.graph().node_kind(n), NodeKind::InitialMemory))
        .expect("InitialMemory not found");
    b.graph().node_outputs(node)[0]
}

/// Build:
///   InitialMemory
///     → MemProject[Stack, Unknown]
///        Stack   → MemUnion
///        Unknown → MemUnion
///     → Return
///
/// Returns the built `Function`.
fn graph_with_mem_project_and_union() -> strider_ir::Function {
    let mut b = RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn_single_region");

    let mem_out = initial_mem_out(&b);

    let mp = b.graph_mut().create_node_attributed(
        NodeKind::MemProject,
        [mem_out],
        [
            NodeOutputKind::Memory(Some(AliasClass::Stack)),
            NodeOutputKind::Memory(Some(AliasClass::Unknown)),
        ],
        &[],
    );
    let mp_outs = b.graph().node_outputs(mp).to_vec();
    let stack_out = mp_outs[0];
    let unknown_out = mp_outs[1];

    let mu = b.graph_mut().create_node_attributed(
        NodeKind::MemUnion,
        [stack_out, unknown_out],
        [NodeOutputKind::Memory(None)],
        &[],
    );
    let mu_out = b.graph().node_outputs(mu)[0];
    b.advance_cur_region_memory(mu_out).expect("advance to MemUnion");

    b.build_return(None, &[]).expect("return");
    b.set_lift_addr(None);
    b.build().expect("build")
}

// ── MemProjectPat tests ───────────────────────────────────────────────────────

#[test]
fn mem_project_pattern_matches() {
    let g = graph_with_mem_project_and_union();
    let pat: Pat = mem_project().into();
    let hits = Matcher::try_new(&g).unwrap().find_all(&pat);
    assert_eq!(hits.len(), 1, "exactly one MemProject in the graph");
}

#[test]
fn mem_project_class_filter_stack_matches() {
    let g = graph_with_mem_project_and_union();
    let pat: Pat = mem_project().class(AliasClass::Stack).into();
    let hits = Matcher::try_new(&g).unwrap().find_all(&pat);
    assert_eq!(hits.len(), 1, "MemProject with Stack output must match");
}

#[test]
fn mem_project_class_filter_unknown_matches() {
    let g = graph_with_mem_project_and_union();
    let pat: Pat = mem_project().class(AliasClass::Unknown).into();
    let hits = Matcher::try_new(&g).unwrap().find_all(&pat);
    assert_eq!(hits.len(), 1, "MemProject with Unknown output must match");
}

// ── MemUnionPat tests ─────────────────────────────────────────────────────────

#[test]
fn mem_union_pattern_matches() {
    let g = graph_with_mem_project_and_union();
    let pat: Pat = mem_union().into();
    let hits = Matcher::try_new(&g).unwrap().find_all(&pat);
    assert_eq!(hits.len(), 1, "exactly one MemUnion in the graph");
}

#[test]
fn mem_union_class_filter_stack_matches() {
    let g = graph_with_mem_project_and_union();
    let pat: Pat = mem_union().class(AliasClass::Stack).into();
    let hits = Matcher::try_new(&g).unwrap().find_all(&pat);
    assert_eq!(hits.len(), 1, "MemUnion with Stack input must match");
}

#[test]
fn mem_union_class_filter_unknown_matches() {
    let g = graph_with_mem_project_and_union();
    let pat: Pat = mem_union().class(AliasClass::Unknown).into();
    let hits = Matcher::try_new(&g).unwrap().find_all(&pat);
    assert_eq!(hits.len(), 1, "MemUnion with Unknown input must match");
}

// ── ignore_mem_boundaries: MemProject skip ────────────────────────────────────

/// Build:  Store(0x100) → MemProject[Stack, Unknown] → Stack → Load(0x200)
///
/// The Load's mem_in is the MemProject's Stack output.  Without
/// `ignore_mem_boundaries`, `load().mem_in(store())` fails because the Load's
/// direct mem_in is MemProject, not Store.  With the flag, the matcher skips
/// through MemProject and reaches Store.
fn graph_load_behind_mem_project() -> strider_ir::Function {
    let mut b = RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn_single_region");

    let addr1 = b.build_int_const(0x100u64, NodeOutputType::U64).expect("addr1");
    let v1 = b.build_int_const(1u64, NodeOutputType::U32).expect("v1");
    b.build_store(addr1, v1, rsleigh::VnSpace::RAM).expect("store");

    // Find the store node and get its memory output.
    let store_node = b
        .graph()
        .all_node_ids()
        .find(|&n| matches!(b.graph().node_kind(n), NodeKind::Store(_)))
        .expect("store node");
    let store_mem_out = b.graph().node_outputs(store_node)[0];

    // Insert a MemProject between Store and Load.
    let mp = b.graph_mut().create_node_attributed(
        NodeKind::MemProject,
        [store_mem_out],
        [
            NodeOutputKind::Memory(Some(AliasClass::Stack)),
            NodeOutputKind::Memory(Some(AliasClass::Unknown)),
        ],
        &[],
    );
    let mp_outs = b.graph().node_outputs(mp).to_vec();
    let stack_out = mp_outs[0];

    // Load reads from the MemProject's Stack output.
    b.advance_cur_region_memory(stack_out).expect("advance to Stack");
    let addr2 = b.build_int_const(0x200u64, NodeOutputType::U64).expect("addr2");
    let lv = b.build_load(addr2, rsleigh::VnSpace::RAM, NodeOutputType::U32).expect("load");

    // Terminate: MemUnion to merge partitions, then return.
    let unknown_out = mp_outs[1];
    let mu = b.graph_mut().create_node_attributed(
        NodeKind::MemUnion,
        [stack_out, unknown_out],
        [NodeOutputKind::Memory(None)],
        &[],
    );
    let mu_out = b.graph().node_outputs(mu)[0];
    b.advance_cur_region_memory(mu_out).expect("advance to MemUnion");

    b.build_return(Some(lv), &[]).expect("return");
    b.set_lift_addr(None);
    b.build().expect("build")
}

#[test]
fn ignore_mem_boundaries_skips_mem_project() {
    let g = graph_load_behind_mem_project();

    let pat: Pat = load()
        .addr(int_const(0x200u64))
        .mem_in(store().addr(int_const(0x100u64)))
        .into();

    // Without skip: Load.mem_in is MemProject, not Store → no match.
    let strict = Matcher::try_new(&g).unwrap().find_all(&pat);
    assert_eq!(strict.len(), 0, "strict: MemProject blocks store match as mem_in");

    // With skip: MemProject is transparent → Store matches as mem_in.
    let skip = Matcher::try_new(&g)
        .unwrap()
        .ignore_mem_boundaries()
        .find_all(&pat);
    assert_eq!(skip.len(), 1, "skip: MemProject walked through → Store matches as mem_in");
}

// ── ignore_mem_boundaries: MemUnion skip ─────────────────────────────────────

/// Build a graph where Store → MemUnion → Load.  Without the flag,
/// `load().mem_in(store())` fails because the Load's mem_in is MemUnion,
/// not Store.  With the flag, the matcher skips through MemUnion to reach
/// Store.
#[test]
fn ignore_mem_boundaries_skips_mem_union() {
    let mut b = RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn_single_region");

    let addr1 = b.build_int_const(0x100u64, NodeOutputType::U64).expect("addr1");
    let v1 = b.build_int_const(1u64, NodeOutputType::U32).expect("v1");
    b.build_store(addr1, v1, rsleigh::VnSpace::RAM).expect("store");
    let store_node = b
        .graph()
        .all_node_ids()
        .find(|&n| matches!(b.graph().node_kind(n), NodeKind::Store(_)))
        .expect("store node");
    let store_mem_out = b.graph().node_outputs(store_node)[0];

    // Insert a single-input MemUnion between Store and Load.
    let mu = b.graph_mut().create_node_attributed(
        NodeKind::MemUnion,
        [store_mem_out],
        [NodeOutputKind::Memory(None)],
        &[],
    );
    let mu_out = b.graph().node_outputs(mu)[0];
    b.advance_cur_region_memory(mu_out).expect("advance to MemUnion");

    let addr2 = b.build_int_const(0x200u64, NodeOutputType::U64).expect("addr2");
    let lv = b
        .build_load(addr2, rsleigh::VnSpace::RAM, NodeOutputType::U32)
        .expect("load");
    b.build_return(Some(lv), &[]).expect("return");
    b.set_lift_addr(None);
    let g = b.build().expect("build");

    let pat: Pat = load()
        .addr(int_const(0x200u64))
        .mem_in(store().addr(int_const(0x100u64)))
        .into();

    // Strict: fails — Load's mem_in is MemUnion, not Store.
    let strict = Matcher::try_new(&g).unwrap().find_all(&pat);
    assert_eq!(strict.len(), 0, "strict: MemUnion blocks store match");

    // With skip: MemUnion is transparent → Store matches.
    let skip = Matcher::try_new(&g)
        .unwrap()
        .ignore_mem_boundaries()
        .find_all(&pat);
    assert_eq!(skip.len(), 1, "skip: MemUnion walked through → Store matches");
}

/// `ignore_mem_boundaries` must not affect value (non-memory) edges.
#[test]
fn ignore_mem_boundaries_does_not_affect_value_edges() {
    use strider_analyze::pattern::add;

    let mut b = RegisterSet::new().build_fn_single_region().unwrap();
    let a = b.build_int_const(5u64, NodeOutputType::U64).unwrap();
    let c2 = b.build_int_const(3u64, NodeOutputType::U64).unwrap();
    let sum = b
        .build_int_binary_operation(a, c2, strider_ir::IntBinaryOp::Add, NodeOutputType::U64)
        .unwrap();
    b.build_return(Some(sum), &[]).unwrap();
    b.set_lift_addr(None);
    let g = b.build().unwrap();

    let v = Capture::new();
    let pat: Pat = add(int_const(5u64), int_const(3u64)).capture(v);
    let hits = Matcher::try_new(&g)
        .unwrap()
        .ignore_mem_boundaries()
        .find_all(&pat);
    assert_eq!(hits.len(), 1, "value patterns unaffected by ignore_mem_boundaries");
}
