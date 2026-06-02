//! Tests for the `stack_only` / `stack_offset` filters on `LoadPat` and `StorePat`,
//! plus the regular-`Capture` + `Function::stack_offset` side-table recovery
//! pattern (capture the matched node, then read its offset off the side-table).
//!
//! The `Function::stack_offset` side-table is populated manually via
//! `Function::set_stack_offset` — the same side-table that `StackOffsetDetect`
//! populates in production.  We bypass `StackOffsetDetect` here so tests stay
//! focused on the pattern-matcher behaviour rather than the optimizer.

use strider_pattern::{Capture, Matcher, load, store};
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
    let v_stack = t.load_ram(addr_stack, NodeOutputType::I64);
    let v_heap = t.load_ram(addr_heap, NodeOutputType::I64);
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
    let stack_base = function.node_inputs(stack_node)[1];
    function.set_stack_offset(stack_node, stack_base, 0x10);
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
    let v = t.load_ram(addr_stack, NodeOutputType::I64);
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
    let stack_base = function.node_inputs(stack_store)[1];
    function.set_stack_offset(stack_store, stack_base, 0x10);
    (function, stack_store, heap_store)
}

// ── load().stack_only() ───────────────────────────────────────────────────────

/// `load().stack_only()` must match only the stack-annotated load.
#[test]
fn stack_only_matches_only_stack_loads() {
    let (g, _stack_node, _heap_node) = two_loads_one_stack();
    let matcher = Matcher::try_new(&g).expect("matcher");
    let pat = load().stack_only().build();
    let hits = matcher.find_all(&pat);
    assert_eq!(hits.len(), 1, "stack_only() must reject the heap load");
}

/// `load()` without `stack_only` matches both loads.
#[test]
fn unconstrained_load_matches_both_loads() {
    let (g, _stack_node, _heap_node) = two_loads_one_stack();
    let matcher = Matcher::try_new(&g).expect("matcher");
    let pat = load().build();
    let hits = matcher.find_all(&pat);
    assert_eq!(hits.len(), 2, "unconstrained load() must match both loads");
}

// ── store().stack_only() ──────────────────────────────────────────────────────

/// `store().stack_only()` must match only the stack-annotated store.
#[test]
fn stack_only_matches_only_stack_stores() {
    let (g, _stack_store, _heap_store) = two_stores_one_stack();
    let matcher = Matcher::try_new(&g).expect("matcher");
    let pat = store().stack_only().build();
    let hits = matcher.find_all(&pat);
    assert_eq!(hits.len(), 1, "stack_only() must reject the heap store");
}

// ── store().stack_offset(k) — exact-offset filter ────────────────────────────

/// The `.stack_offset(k)` filter restricts to a single concrete offset.
#[test]
fn offset_exact_filter_store() {
    let (g, _stack_store, _heap_store) = two_stores_one_stack();
    let matcher = Matcher::try_new(&g).expect("matcher");

    let pat_match = store().stack_offset(0x10).build();
    let hits_match = matcher.find_all(&pat_match);
    assert_eq!(hits_match.len(), 1, "stack_offset(0x10) must match the annotated store");

    let pat_miss = store().stack_offset(0x20).build();
    let hits_miss = matcher.find_all(&pat_miss);
    assert_eq!(hits_miss.len(), 0, "stack_offset(0x20) must reject the store");
}

// ── Capture + Function::stack_offset side-table recovery ─────────────────────

/// Capturing a stack-relative store with a regular `Capture` lets the caller
/// recover its SP offset by reading `Function::stack_offset` on the bound node.
/// One accessor, no dedicated capture / journal.
#[test]
fn capture_then_read_stack_offset_via_side_table() {
    let (g, stack_store, _heap_store) = two_stores_one_stack();
    let matcher = Matcher::try_new(&g).expect("matcher");
    let node_cap = Capture::new();
    let pat = store().stack_only().capture(node_cap).build();
    let hits = matcher.find_all(&pat);
    assert_eq!(hits.len(), 1, "stack_only must restrict to the annotated store");
    let m = &hits[0];
    let bound = m.node(node_cap, &g).expect("captured node");
    assert_eq!(bound, stack_store, "capture must bind the stack store");
    let (_base, offset) = g.stack_offset(bound).expect("side-table entry");
    assert_eq!(offset, 0x10_i64, "side-table offset must round-trip");
}

/// The same recovery applies to loads.
#[test]
fn capture_then_read_stack_offset_via_side_table_load() {
    let (g, stack_load, _heap_load) = two_loads_one_stack();
    let matcher = Matcher::try_new(&g).expect("matcher");
    let node_cap = Capture::new();
    let pat = load().stack_only().capture(node_cap).build();
    let hits = matcher.find_all(&pat);
    assert_eq!(hits.len(), 1);
    let m = &hits[0];
    let bound = m.node(node_cap, &g).expect("captured node");
    assert_eq!(bound, stack_load);
    let (_base, offset) = g.stack_offset(bound).expect("side-table entry");
    assert_eq!(offset, 0x10_i64);
}
