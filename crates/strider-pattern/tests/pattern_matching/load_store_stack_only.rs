//! The side-table is stamped manually via `set_stack_slot` instead of by
//! running `StackOffsetDetect`, keeping these tests on matcher behaviour.

use strider_ir::node::{NodeId, NodeKind, ValueType};
use strider_ir::{IRViewer, IRWalker};
use strider_pattern::{Capture, CaptureExt, MatchPat, Matcher, load, store};

use super::support::Tb;

/// Two RAM loads, 0x1000 tagged Stack and 0x2000 tagged Heap.
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
    let heap_base = function.node_inputs(heap_node)[1];
    // The slot is value-keyed on the address; `stack_offset(node)` derives from it.
    function
        .side_tables_mut()
        .set_stack_slot(stack_base, stack_base, 0x10);
    function
        .side_tables_mut()
        .set_heap_slot(heap_base, heap_base, 0);
    (function, stack_node, heap_node)
}

/// Two RAM stores, 0x1000 tagged Stack (offset 0x10) and 0x2000 tagged Heap.
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
    let heap_base = function.node_inputs(heap_store)[1];
    // The slot is value-keyed on the address; `stack_offset(node)` derives from it.
    function
        .side_tables_mut()
        .set_stack_slot(stack_base, stack_base, 0x10);
    function
        .side_tables_mut()
        .set_heap_slot(heap_base, heap_base, 0);
    (function, stack_store, heap_store)
}

/// Three RAM loads: 0x1000 tagged Stack, 0x2000 tagged Heap, 0x3000 untagged.
/// Returns `(function, stack_load, heap_load, plain_load)`.
fn loads_stack_heap_plain() -> (strider_ir::Function, NodeId, NodeId, NodeId) {
    let mut t = Tb::empty();
    let a_stack = t.u64(0x1000);
    let a_heap = t.u64(0x2000);
    let a_plain = t.u64(0x3000);
    let v0 = t.load_ram(a_stack, ValueType::I64);
    let v1 = t.load_ram(a_heap, ValueType::I64);
    let v2 = t.load_ram(a_plain, ValueType::I64);
    let s0 = t.add(v0, v1);
    let sum = t.add(s0, v2);
    let mut function = t.ret_val(sum);

    let by_addr = |want: u128| -> NodeId {
        function
            .walk()
            .filter(|&n| matches!(function.node_kind(n), NodeKind::Load(_)))
            .find(|&n| function.int_const_u128(function.node_inputs(n)[1]) == Some(want))
            .expect("load node")
    };
    let (stack_load, heap_load, plain_load) = (by_addr(0x1000), by_addr(0x2000), by_addr(0x3000));
    let stack_base = function.node_inputs(stack_load)[1];
    let heap_base = function.node_inputs(heap_load)[1];
    function
        .side_tables_mut()
        .set_stack_slot(stack_base, stack_base, 0x10);
    function
        .side_tables_mut()
        .set_heap_slot(heap_base, heap_base, 0x20);
    (function, stack_load, heap_load, plain_load)
}

#[test]
fn heap_only_matches_only_the_heap_load() {
    let (g, _stack, _heap, _plain) = loads_stack_heap_plain();
    let matcher = Matcher::new(&g);
    let hits = matcher.find_all(&load().heap_only().build()).unwrap();
    assert_eq!(
        hits.len(),
        1,
        "heap_only() matches only the heap-tagged load"
    );
}

/// With a real heap slot present, `stack_only` must reject it (kind-aware),
/// and `non_stack` must accept it (heap is not stack) while still rejecting
/// the untagged load, whose address carries no verdict either way.
#[test]
fn stack_and_non_stack_are_kind_aware_with_heap() {
    let (g, _stack, _heap, _plain) = loads_stack_heap_plain();
    let matcher = Matcher::new(&g);
    let stack_hits = matcher.find_all(&load().stack_only().build()).unwrap();
    assert_eq!(stack_hits.len(), 1, "stack_only() rejects the heap load");
    let non_stack_hits = matcher.find_all(&load().non_stack().build()).unwrap();
    assert_eq!(
        non_stack_hits.len(),
        1,
        "non_stack() keeps the heap load and drops the untagged one"
    );
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
fn non_stack_matches_only_heap_loads() {
    let (g, _stack_node, _heap_node) = two_loads_one_stack();
    let matcher = Matcher::new(&g);
    let pat = load().non_stack().build();
    let hits = matcher.find_all(&pat).unwrap();
    assert_eq!(
        hits.len(),
        1,
        "non_stack() must reject the proven-stack load"
    );
}

#[test]
fn non_stack_matches_only_heap_stores() {
    let (g, _stack_store, _heap_store) = two_stores_one_stack();
    let matcher = Matcher::new(&g);
    let pat = store().non_stack().build();
    let hits = matcher.find_all(&pat).unwrap();
    assert_eq!(
        hits.len(),
        1,
        "non_stack() must reject the proven-stack store"
    );
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

/// `.stack_offset(k)` restricts to one concrete offset rather than to any
/// stack address.
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
/// `Function::stack_offset` on the bound node.
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

/// `.filter()` must COMPOSE with the builder's own constraint, not replace it.
///
/// A node carries one predicate slot, and `NodePat` installs its core
/// constraint there, so a `.filter()` layered on top has to narrow
/// `stack_only`'s region check rather than take the slot from it.
#[test]
fn filter_composes_with_stack_only_rather_than_replacing_it() {
    let (g, _stack, _heap, _plain) = loads_stack_heap_plain();
    let matcher = Matcher::new(&g);

    let bare = matcher.find_all(&load().stack_only().build()).unwrap();
    assert_eq!(bare.len(), 1, "baseline: one stack-tagged load");

    // An always-true filter must not widen the match set.
    let filtered = matcher
        .find_all(&load().stack_only().filter(|_, _| true).into_pattern())
        .unwrap();
    assert_eq!(
        filtered.len(),
        1,
        "an always-true .filter() must keep stack_only's region check"
    );

    // An always-false filter must not narrow past zero, i.e. it really runs.
    let none = matcher
        .find_all(&load().stack_only().filter(|_, _| false).into_pattern())
        .unwrap();
    assert!(
        none.is_empty(),
        "an always-false .filter() must still apply"
    );
}

/// `Unknown` is the memo default, so before any decomposition runs nothing is
/// proven non-stack.
#[test]
fn non_stack_rejects_an_undecomposed_access() {
    let mut t = Tb::empty();
    let addr = t.u64(0x1000);
    let v = t.load_ram(addr, ValueType::I64);
    let function = t.ret_val(v);
    let matcher = Matcher::new(&function);
    assert!(
        matcher
            .find_all(&load().non_stack().build())
            .unwrap()
            .is_empty(),
        "non_stack() must not accept an access whose address was never decomposed"
    );
}

/// The region filter is one slot, so the last call wins. Only
/// `stack_offset` -> `stack_only` preserves what came before.
#[test]
fn the_last_region_filter_call_wins() {
    let (function, stack_load, heap_load) = two_loads_one_stack();
    let m = Matcher::new(&function);
    let at = |hits: Vec<strider_pattern::Match>| -> Vec<NodeId> {
        hits.iter().map(strider_pattern::Match::root).collect()
    };

    assert_eq!(
        at(m.find_all(&load().heap_only().stack_only().build())
            .unwrap()),
        vec![stack_load]
    );
    assert_eq!(
        at(m.find_all(&load().stack_only().heap_only().build())
            .unwrap()),
        vec![heap_load]
    );
    // `non_stack` drops a pinned offset with the rest of the stack filter.
    assert_eq!(
        at(m.find_all(&load().stack_offset(0x10).non_stack().build())
            .unwrap()),
        vec![heap_load]
    );
    // The one direction that keeps state.
    assert_eq!(
        at(m.find_all(&load().stack_offset(0x10).stack_only().build())
            .unwrap()),
        vec![stack_load]
    );
    assert!(
        m.find_all(&load().stack_offset(0x99).stack_only().build())
            .unwrap()
            .is_empty()
    );
}
