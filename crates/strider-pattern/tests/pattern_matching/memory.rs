//! `Load` / `Store` pattern matching.
//!
//! Covers: `load()` with no constraints, `.space()`, `.addr()`, `.capture()`
//! on the value slot; `store()` with `.space/.addr/.data`; store-then-load
//! aliasing; wrong-space and addr-mismatch rejection.

use strider_pattern::*;
use strider_ir::node::NodeOutputType;

use super::support::{Tb, assertions as a, shapes};

// ── Load ──────────────────────────────────────────────────────────────────────

#[test]
fn load_unconstrained_matches() {
    let function = shapes::store_then_load_ram(0x100, 42);
    // The load is the value-producing node; store is also present but
    // `load()` is kind-filtered to `Load`.
    a::matches(&function, load().build(), 1);
}

#[test]
fn load_space_matches_ram() {
    let function = shapes::store_then_load_ram(0x100, 42);
    a::matches(&function, load().space(rsleigh::VnSpace::RAM).build(), 1);
}

#[test]
fn load_wrong_space_rejects() {
    let function = shapes::store_then_load_ram(0x100, 42);
    // UNIQUE space has no loads — must reject.
    a::none(&function, load().space(rsleigh::VnSpace::UNIQUE).build());
}

#[test]
fn load_addr_matches_literal() {
    let function = shapes::store_then_load_ram(0x100, 42);
    a::matches(&function, load().addr(int_const(0x100u128)).build(), 1);
    a::none(&function, load().addr(int_const(0x999u128)).build());
}

#[test]
fn load_captures_value_slot() {
    let function = shapes::store_then_load_ram(0x100, 42);
    let v = Capture::new();
    let m = a::unique(&function, load().addr(int_const(0x100u128)).capture(v).build());
    // The captured output is the Load's value slot; reading it back
    // points at the Load node.
    let out = m.output(v).expect("value slot capture");
    assert!(matches!(
        function.kind_of_output(out),
        strider_ir::node::NodeKind::Load(_)
    ));
}

#[test]
fn load_with_patterned_addr() {
    // Load from `base + 8`: addr is itself a pattern.
    let mut t = Tb::empty();
    let base = t.u64(0x100);
    let off = t.u64(8);
    let addr = t.add(base, off);
    let v = t.load_ram(addr, NodeOutputType::I64);
    let function = t.ret_val(v);

    a::matches(
        &function,
        load().addr(add(int_const(0x100u128), int_const(8u128))).build(),
        1,
    );
    // Wrong sub-pattern → reject.
    a::none(
        &function,
        load().addr(add(int_const(0x100u128), int_const(9u128))).build(),
    );
}

// ── Store ─────────────────────────────────────────────────────────────────────

#[test]
fn store_unconstrained_matches() {
    let function = shapes::store_then_load_ram(0x100, 42);
    a::matches(&function, store().build(), 1);
}

#[test]
fn store_addr_matches() {
    let function = shapes::store_then_load_ram(0x100, 42);
    a::matches(&function, store().addr(int_const(0x100u128)).build(), 1);
    a::none(&function, store().addr(int_const(0x999u128)).build());
}

#[test]
fn store_data_matches() {
    let function = shapes::store_then_load_ram(0x100, 42);
    a::matches(&function, store().data(int_const(42u128)).build(), 1);
    a::none(&function, store().data(int_const(1u128)).build());
}

#[test]
fn store_addr_and_data_together() {
    let function = shapes::store_then_load_ram(0x100, 42);
    a::matches(
        &function,
        store().addr(int_const(0x100u128)).data(int_const(42u128)).build(),
        1,
    );
    // Right addr, wrong data → reject.
    a::none(
        &function,
        store().addr(int_const(0x100u128)).data(int_const(99u128)).build(),
    );
}

#[test]
fn store_space_matches() {
    let function = shapes::store_then_load_ram(0x100, 42);
    a::matches(&function, store().space(rsleigh::VnSpace::RAM).build(), 1);
    a::none(&function, store().space(rsleigh::VnSpace::UNIQUE).build());
}

// ── Store-then-load aliasing pattern ──────────────────────────────────────────

#[test]
fn store_then_load_same_addr_match() {
    let function = shapes::store_then_load_ram(0x200, 77);
    // Must simultaneously find a store with data=77 and a load at the same
    // address.
    a::matches(
        &function,
        store().addr(int_const(0x200u128)).data(int_const(77u128)).build(),
        1,
    );
    a::matches(&function, load().addr(int_const(0x200u128)).build(), 1);
}

// ── Load without a preceding store ───────────────────────────────────────────

#[test]
fn load_only_graph_matches() {
    let mut t = Tb::empty();
    let addr = t.u64(0x100);
    let v = t.load_ram(addr, NodeOutputType::I64);
    let function = t.ret_val(v);

    a::matches(&function, load().build(), 1);
    // There is no store in this graph.
    a::none(&function, store().build());
}
