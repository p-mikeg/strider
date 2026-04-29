//! `Load` / `Store` pattern matching.
//!
//! Covers: `load()` with no constraints, `.space()`, `.addr()`, `.capture()`
//! on the value slot; `store()` with `.space/.addr/.data`; store-then-load
//! aliasing; wrong-space and addr-mismatch rejection.

use ir::node::NodeOutputType;
use pattern::*;

use super::support::{Tb, assertions as a, shapes};

// ── Load ──────────────────────────────────────────────────────────────────────

#[test]
fn load_unconstrained_matches() {
    let g = shapes::store_then_load_ram(0x100, 42);
    // The load is the value-producing node; store is also present but
    // `load()` is kind-filtered to `Load`.
    a::matches(&g, load(), 1);
}

#[test]
fn load_space_matches_ram() {
    let g = shapes::store_then_load_ram(0x100, 42);
    a::matches(&g, load().space(rsleigh::VnSpace::RAM), 1);
}

#[test]
fn load_wrong_space_rejects() {
    let g = shapes::store_then_load_ram(0x100, 42);
    // UNIQUE space has no loads — must reject.
    a::none(&g, load().space(rsleigh::VnSpace::UNIQUE));
}

#[test]
fn load_addr_matches_literal() {
    let g = shapes::store_then_load_ram(0x100, 42);
    a::matches(&g, load().addr(int_const(0x100)), 1);
    a::none(&g, load().addr(int_const(0x999)));
}

#[test]
fn load_captures_value_slot() {
    let g = shapes::store_then_load_ram(0x100, 42);
    let v = Var::new();
    let m = a::unique(&g, load().addr(int_const(0x100)).capture(v));
    // The captured output is the Load's value slot; reading it back
    // points at the Load node.
    let out = m.get(v).expect("value slot capture");
    assert!(matches!(
        g.graph.kind_of_output(out),
        ir::node::NodeKind::Load(_)
    ));
}

#[test]
fn load_with_patterned_addr() {
    // Load from (sp + 8): addr is itself a pattern.
    let mut t = Tb::empty();
    let base = t.u64(0x100);
    let off = t.u64(8);
    let addr = t.add(base, off);
    let v = t.load_ram(addr, NodeOutputType::U64);
    let g = t.ret_val(v);

    a::matches(&g, load().addr(add(int_const(0x100), int_const(8))), 1);
    // Wrong sub-pattern → reject.
    a::none(&g, load().addr(add(int_const(0x100), int_const(9))));
}

// ── Store ─────────────────────────────────────────────────────────────────────

#[test]
fn store_unconstrained_matches() {
    let g = shapes::store_then_load_ram(0x100, 42);
    a::matches(&g, store(), 1);
}

#[test]
fn store_addr_matches() {
    let g = shapes::store_then_load_ram(0x100, 42);
    a::matches(&g, store().addr(int_const(0x100)), 1);
    a::none(&g, store().addr(int_const(0x999)));
}

#[test]
fn store_data_matches() {
    let g = shapes::store_then_load_ram(0x100, 42);
    a::matches(&g, store().data(int_const(42)), 1);
    a::none(&g, store().data(int_const(1)));
}

#[test]
fn store_addr_and_data_together() {
    let g = shapes::store_then_load_ram(0x100, 42);
    a::matches(&g, store().addr(int_const(0x100)).data(int_const(42)), 1);
    // Right addr, wrong data → reject.
    a::none(&g, store().addr(int_const(0x100)).data(int_const(99)));
}

#[test]
fn store_space_matches() {
    let g = shapes::store_then_load_ram(0x100, 42);
    a::matches(&g, store().space(rsleigh::VnSpace::RAM), 1);
    a::none(&g, store().space(rsleigh::VnSpace::UNIQUE));
}

// ── Store-then-load aliasing pattern ──────────────────────────────────────────

#[test]
fn store_then_load_same_addr_match() {
    let g = shapes::store_then_load_ram(0x200, 77);
    // Must simultaneously find a store with data=77 and a load at the same
    // address.
    a::matches(&g, store().addr(int_const(0x200)).data(int_const(77)), 1);
    a::matches(&g, load().addr(int_const(0x200)), 1);
}

// ── Load without a preceding store ───────────────────────────────────────────

#[test]
fn load_only_graph_matches() {
    let mut t = Tb::empty();
    let addr = t.u64(0x100);
    let v = t.load_ram(addr, NodeOutputType::U64);
    let g = t.ret_val(v);

    a::matches(&g, load(), 1);
    // There is no store in this graph.
    a::none(&g, store());
}
