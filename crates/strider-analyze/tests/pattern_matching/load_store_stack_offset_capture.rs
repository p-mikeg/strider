//! Tests for the `stack_only`, `offset_capture` features on `LoadPat` and `StorePat`.
//!
//! The `Function::stack_offset` side-table is populated manually via
//! `Function::set_stack_offset` — the same side-table that `StackOffsetDetect`
//! populates in production.  We bypass `StackOffsetDetect` here so tests stay
//! focused on the pattern-matcher behaviour rather than the optimizer.

use strider_analyze::pattern::{Capture, IntoPat, Matcher, OffsetCapture, load, store};
use strider_ir::node::{NodeId, NodeKind, NodeOutputType};

use super::support::Tb;

// ── Graph helpers ─────────────────────────────────────────────────────────────

/// A graph containing one RAM Load (with a stack-offset entry) and one RAM
/// Load (without a stack-offset entry).
///
/// Returns `(graph, stack_load_node, heap_load_node)`.
fn two_loads_one_stack() -> (strider_ir::Function, NodeId, NodeId) {
    let mut t = Tb::empty();
    let addr_stack = t.u64(0x1000);
    let addr_heap = t.u64(0x2000);
    let v_stack = t.load_ram(addr_stack, NodeOutputType::U64);
    let v_heap = t.load_ram(addr_heap, NodeOutputType::U64);
    let sum = t.add(v_stack, v_heap);
    let mut function = t.ret_val(sum);

    // Find the load nodes; mark the 0x1000 load as stack-relative.
    let loads: Vec<NodeId> = function
        .walk()
        .filter(|&n| matches!(function.node_kind(n), NodeKind::Load(_)))
        .collect();
    assert_eq!(loads.len(), 2, "expected exactly 2 Load nodes");

    // Identify which load is from 0x1000 vs 0x2000 by inspecting their
    // address input: the address is an IntConst at inputs[1].
    let mut stack_node = None;
    let mut heap_node = None;
    for &load_node in &loads {
        let inputs = function.node_inputs(load_node);
        let addr_out = inputs[1];
        if let NodeKind::IntConst(v) = function.kind_of_output(addr_out) {
            if *v == 0x1000 {
                stack_node = Some(load_node);
            } else {
                heap_node = Some(load_node);
            }
        }
    }
    let stack_node = stack_node.expect("stack load node");
    let heap_node = heap_node.expect("heap load node");
    function.set_stack_offset(stack_node, 0x10);
    (function, stack_node, heap_node)
}

/// A graph containing two RAM Stores: one with a stack-offset entry (offset
/// 0x10) and one without.
///
/// Returns `(graph, stack_store_node, heap_store_node)`.
fn two_stores_one_stack() -> (strider_ir::Function, NodeId, NodeId) {
    let mut t = Tb::empty();
    let addr_stack = t.u64(0x1000);
    let addr_heap = t.u64(0x2000);
    let data = t.u64(0xAB);
    t.store_ram(addr_stack, data);
    t.store_ram(addr_heap, data);
    let v = t.load_ram(addr_stack, NodeOutputType::U64);
    let mut function = t.ret_val(v);

    // Identify the two stores.
    let stores: Vec<NodeId> = function
        .walk()
        .filter(|&n| matches!(function.node_kind(n), NodeKind::Store(_)))
        .collect();
    assert_eq!(stores.len(), 2, "expected exactly 2 Store nodes");

    let mut stack_store = None;
    let mut heap_store = None;
    for &store_node in &stores {
        let inputs = function.node_inputs(store_node);
        let addr_out = inputs[1];
        if let NodeKind::IntConst(v) = function.kind_of_output(addr_out) {
            if *v == 0x1000 {
                stack_store = Some(store_node);
            } else {
                heap_store = Some(store_node);
            }
        }
    }
    let stack_store = stack_store.expect("stack store node");
    let heap_store = heap_store.expect("heap store node");
    function.set_stack_offset(stack_store, 0x10);
    (function, stack_store, heap_store)
}

// ── load().stack_only() ───────────────────────────────────────────────────────

/// `load().stack_only()` must match only the stack-annotated load.
#[test]
fn stack_only_matches_only_stack_loads() {
    let (g, _stack_node, _heap_node) = two_loads_one_stack();
    let matcher = Matcher::try_new(&g).expect("matcher");
    let pat: strider_analyze::pattern::Pat = load().stack_only().into();
    let hits = matcher.find_all(&pat);
    assert_eq!(hits.len(), 1, "stack_only() must reject the heap load");
}

/// `load()` without `stack_only` matches both loads.
#[test]
fn unconstrained_load_matches_both_loads() {
    let (g, _stack_node, _heap_node) = two_loads_one_stack();
    let matcher = Matcher::try_new(&g).expect("matcher");
    let pat: strider_analyze::pattern::Pat = load().into();
    let hits = matcher.find_all(&pat);
    assert_eq!(hits.len(), 2, "unconstrained load() must match both loads");
}

// ── store().stack_only() ──────────────────────────────────────────────────────

/// `store().stack_only()` must match only the stack-annotated store.
#[test]
fn stack_only_matches_only_stack_stores() {
    let (g, _stack_store, _heap_store) = two_stores_one_stack();
    let matcher = Matcher::try_new(&g).expect("matcher");
    let pat: strider_analyze::pattern::Pat = store().stack_only().into();
    let hits = matcher.find_all(&pat);
    assert_eq!(hits.len(), 1, "stack_only() must reject the heap store");
}

// ── store().stack_offset(k) — exact-offset filter (existing) ─────────────────

/// The existing `.stack_offset(k)` filter still works after adding the new
/// fields.
#[test]
fn offset_exact_filter_store() {
    let (g, _stack_store, _heap_store) = two_stores_one_stack();
    let matcher = Matcher::try_new(&g).expect("matcher");

    let pat_match: strider_analyze::pattern::Pat = store().stack_offset(0x10).into();
    let hits_match = matcher.find_all(&pat_match);
    assert_eq!(hits_match.len(), 1, "stack_offset(0x10) must match the annotated store");

    let pat_miss: strider_analyze::pattern::Pat = store().stack_offset(0x20).into();
    let hits_miss = matcher.find_all(&pat_miss);
    assert_eq!(hits_miss.len(), 0, "stack_offset(0x20) must reject the store");
}

// ── store().offset_capture(c) ────────────────────────────────────────────────

/// `store().offset_capture(c)` must bind the offset into the match and the
/// captured value must equal the side-table entry.
#[test]
fn offset_capture_round_trip_store() {
    let (g, _stack_store, _heap_store) = two_stores_one_stack();
    let matcher = Matcher::try_new(&g).expect("matcher");
    let oc = OffsetCapture::new();
    let pat: strider_analyze::pattern::Pat = store().offset_capture(oc).into();
    let hits = matcher.find_all(&pat);
    assert_eq!(hits.len(), 1, "offset_capture implies stack_only; only 1 store qualifies");
    assert_eq!(
        hits[0].captured_offset(oc),
        Some(0x10_i64),
        "captured offset must match the side-table value"
    );
}

/// `store().offset_capture(c)` must fail on non-stack stores (implies stack_only).
#[test]
fn offset_capture_implies_stack_only_store() {
    let (g, _stack_store, _heap_store) = two_stores_one_stack();
    let matcher = Matcher::try_new(&g).expect("matcher");
    let oc = OffsetCapture::new();
    let pat: strider_analyze::pattern::Pat = store().offset_capture(oc).into();
    let hits = matcher.find_all(&pat);
    // Only the stack store matches; the heap store has no stack_offset entry.
    assert_eq!(hits.len(), 1);
    // Verify the heap store didn't slip through by checking the matched node.
    let matched_node = hits[0].root();
    let inputs = g.node_inputs(matched_node);
    let addr_out = inputs[1];
    if let NodeKind::IntConst(v) = g.kind_of_output(addr_out) {
        assert_eq!(*v, 0x1000_u128, "matched node must be the stack store, not the heap store");
    } else {
        panic!("unexpected addr input kind");
    }
}

// ── load().offset_capture(c) ─────────────────────────────────────────────────

/// `load().offset_capture(c)` round-trip for loads.
#[test]
fn offset_capture_round_trip_load() {
    let (g, _stack_node, _heap_node) = two_loads_one_stack();
    let matcher = Matcher::try_new(&g).expect("matcher");
    let oc = OffsetCapture::new();
    let pat: strider_analyze::pattern::Pat = load().offset_capture(oc).into();
    let hits = matcher.find_all(&pat);
    assert_eq!(hits.len(), 1, "offset_capture implies stack_only; only 1 load qualifies");
    assert_eq!(
        hits[0].captured_offset(oc),
        Some(0x10_i64),
        "captured offset must match the side-table value"
    );
}

/// `captured_offset` returns `None` for an unbound `OffsetCapture`.
#[test]
fn captured_offset_returns_none_for_unbound_capture() {
    let (g, _stack_node, _heap_node) = two_loads_one_stack();
    let matcher = Matcher::try_new(&g).expect("matcher");
    let oc_bound = OffsetCapture::new();
    let oc_unbound = OffsetCapture::new();
    let pat: strider_analyze::pattern::Pat = load().offset_capture(oc_bound).into();
    let hits = matcher.find_all(&pat);
    assert_eq!(hits.len(), 1);
    assert_eq!(
        hits[0].captured_offset(oc_unbound),
        None,
        "unbound OffsetCapture must yield None"
    );
}

// ── Capture (node id) alongside offset_capture ────────────────────────────────

/// Combining `.capture(c)` (for the node id) with `.offset_capture(oc)` works:
/// both bindings are available on the same match.
#[test]
fn node_capture_and_offset_capture_coexist() {
    let (g, _stack_store, _heap_store) = two_stores_one_stack();
    let matcher = Matcher::try_new(&g).expect("matcher");
    let node_cap = Capture::new();
    let off_cap = OffsetCapture::new();
    let pat: strider_analyze::pattern::Pat =
        store().offset_capture(off_cap).capture(node_cap);
    let hits = matcher.find_all(&pat);
    assert_eq!(hits.len(), 1);
    let m = &hits[0];
    assert!(m.node(node_cap).is_some(), "node capture must be bound");
    assert_eq!(m.captured_offset(off_cap), Some(0x10_i64));
}
