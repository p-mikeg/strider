//! The `stack_only` / `stack_offset` filters on `LoadPat` / `StorePat`, plus
//! side-table recovery: capture the matched node, then read its offset off
//! `Function::stack_offset`.
//!
//! The side-table is stamped manually via `set_stack_slot` instead of by
//! running `StackOffsetDetect`, keeping these tests on matcher behaviour.

use strider_ir::node::{NodeId, NodeKind, ValueType};
use strider_ir::{IRViewer, IRWalker};
use strider_pattern::{Capture, Matcher, load, store};

use super::support::Tb;

/// Two RAM loads, only the 0x1000 one carrying a stack-offset entry.
/// Returns `(function, stack_load, heap_load)`.
fn two_loads_one_stack() -> (strider_ir::Function, NodeId, NodeId) {
    let mut t = Tb::empty();
    let addr_stack = t.u64(0x1000);
    let addr_heap = t.u64(0x2000);
    let v_stack = t.load_ram(addr_stack, ValueType::I64);
    let v_heap = t.load_ram(addr_heap, ValueType::I64);
    let sum = t.add(v_stack, v_heap);
    let mut function = t.ret_val(sum);

    let loads: Vec<NodeId> = function
        .walk()
        .filter(|&n| matches!(function.node_kind(n), NodeKind::Load(_)))
        .collect();
    assert_eq!(loads.len(), 2, "expected exactly 2 Load nodes");

    // The address is an IntConst at inputs[1].
    let mut stack_node = None;
    let mut heap_node = None;
    for &load_node in &loads {
        let inputs = function.node_inputs(load_node);
        let addr_value = inputs[1];
        if let Some(v) = function.int_const_u128(addr_value) {
            if v == 0x1000 {
                stack_node = Some(load_node);
            } else {
                heap_node = Some(load_node);
            }
        }
    }
    let stack_node = stack_node.expect("stack load node");
    let heap_node = heap_node.expect("heap load node");
    let stack_base = function.node_inputs(stack_node)[1];
    // The slot is value-keyed on the address; `stack_offset(node)` derives from it.
    function
        .side_tables_mut()
        .set_stack_slot(stack_base, stack_base, 0x10);
    (function, stack_node, heap_node)
}

/// Two RAM stores, only the 0x1000 one carrying a stack-offset entry (0x10).
/// Returns `(function, stack_store, heap_store)`.
fn two_stores_one_stack() -> (strider_ir::Function, NodeId, NodeId) {
    let mut t = Tb::empty();
    let addr_stack = t.u64(0x1000);
    let addr_heap = t.u64(0x2000);
    let data = t.u64(0xAB);
    t.store_ram(addr_stack, data);
    t.store_ram(addr_heap, data);
    let v = t.load_ram(addr_stack, ValueType::I64);
    let mut function = t.ret_val(v);

    let stores: Vec<NodeId> = function
        .walk()
        .filter(|&n| matches!(function.node_kind(n), NodeKind::Store(_)))
        .collect();
    assert_eq!(stores.len(), 2, "expected exactly 2 Store nodes");

    let mut stack_store = None;
    let mut heap_store = None;
    for &store_node in &stores {
        let inputs = function.node_inputs(store_node);
        let addr_value = inputs[1];
        if let Some(v) = function.int_const_u128(addr_value) {
            if v == 0x1000 {
                stack_store = Some(store_node);
            } else {
                heap_store = Some(store_node);
            }
        }
    }
    let stack_store = stack_store.expect("stack store node");
    let heap_store = heap_store.expect("heap store node");
    let stack_base = function.node_inputs(stack_store)[1];
    // The slot is value-keyed on the address; `stack_offset(node)` derives from it.
    function
        .side_tables_mut()
        .set_stack_slot(stack_base, stack_base, 0x10);
    (function, stack_store, heap_store)
}

#[test]
fn stack_only_matches_only_stack_loads() {
    let (g, _stack_node, _heap_node) = two_loads_one_stack();
    let matcher = Matcher::new(&g);
    let pat = load().stack_only().build();
    let hits = matcher.find_all(&pat).unwrap();
    assert_eq!(hits.len(), 1, "stack_only() must reject the heap load");
}

#[test]
fn unconstrained_load_matches_both_loads() {
    let (g, _stack_node, _heap_node) = two_loads_one_stack();
    let matcher = Matcher::new(&g);
    let pat = load().build();
    let hits = matcher.find_all(&pat).unwrap();
    assert_eq!(hits.len(), 2, "unconstrained load() must match both loads");
}

#[test]
fn stack_only_matches_only_stack_stores() {
    let (g, _stack_store, _heap_store) = two_stores_one_stack();
    let matcher = Matcher::new(&g);
    let pat = store().stack_only().build();
    let hits = matcher.find_all(&pat).unwrap();
    assert_eq!(hits.len(), 1, "stack_only() must reject the heap store");
}

/// `.stack_offset(k)` restricts to one concrete offset, not just "is stack".
#[test]
fn offset_exact_filter_store() {
    let (g, _stack_store, _heap_store) = two_stores_one_stack();
    let matcher = Matcher::new(&g);

    let pat_match = store().stack_offset(0x10).build();
    let hits_match = matcher.find_all(&pat_match).unwrap();
    assert_eq!(
        hits_match.len(),
        1,
        "stack_offset(0x10) must match the annotated store"
    );

    let pat_miss = store().stack_offset(0x20).build();
    let hits_miss = matcher.find_all(&pat_miss).unwrap();
    assert_eq!(
        hits_miss.len(),
        0,
        "stack_offset(0x20) must reject the store"
    );
}

/// A regular `Capture` is enough to recover a store's SP offset: read
/// `Function::stack_offset` on the bound node. No dedicated capture kind.
#[test]
fn capture_then_read_stack_offset_via_side_table() {
    let (g, stack_store, _heap_store) = two_stores_one_stack();
    let matcher = Matcher::new(&g);
    let node_cap = Capture::new();
    let pat = store().stack_only().capture(node_cap).build();
    let hits = matcher.find_all(&pat).unwrap();
    assert_eq!(
        hits.len(),
        1,
        "stack_only must restrict to the annotated store"
    );
    let m = &hits[0];
    let bound = m.node(node_cap, g.graph()).expect("captured node");
    assert_eq!(bound, stack_store, "capture must bind the stack store");
    let (_base, offset) = g.stack_offset(bound).expect("side-table entry");
    assert_eq!(offset, 0x10_i128, "side-table offset must round-trip");
}

/// The same recovery applies to loads.
#[test]
fn capture_then_read_stack_offset_via_side_table_load() {
    let (g, stack_load, _heap_load) = two_loads_one_stack();
    let matcher = Matcher::new(&g);
    let node_cap = Capture::new();
    let pat = load().stack_only().capture(node_cap).build();
    let hits = matcher.find_all(&pat).unwrap();
    assert_eq!(hits.len(), 1);
    let m = &hits[0];
    let bound = m.node(node_cap, g.graph()).expect("captured node");
    assert_eq!(bound, stack_load);
    let (_base, offset) = g.stack_offset(bound).expect("side-table entry");
    assert_eq!(offset, 0x10_i128);
}
