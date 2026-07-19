//! `Load` / `Store` pattern matching.

use strider_ir::IRViewer;
use strider_ir::node::ValueType;
use strider_pattern::*;

use super::support::{Tb, assertions as a, reg_vn, shapes};

#[test]
fn load_unconstrained_matches() {
    let function = shapes::store_then_load_ram(0x100, 42);
    // Graph also holds a Store; load() must not pick it up.
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
    let m = a::unique(
        &function,
        load().addr(int_const(0x100u128)).capture(v).build(),
    );
    let value = m.value(v).expect("value slot capture");
    assert!(matches!(
        function.kind_of_value(value),
        strider_ir::node::NodeKind::Load(_)
    ));
}

#[test]
fn load_with_patterned_addr() {
    // addr is itself a pattern: base + 8.
    let mut t = Tb::empty();
    let base = t.u64(0x100);
    let off = t.u64(8);
    let addr = t.add(base, off);
    let v = t.load_ram(addr, ValueType::I64);
    let function = t.ret_val(v);

    a::matches(
        &function,
        load()
            .addr(add(int_const(0x100u128), int_const(8u128)))
            .build(),
        1,
    );
    a::none(
        &function,
        load()
            .addr(add(int_const(0x100u128), int_const(9u128)))
            .build(),
    );
}

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
        store()
            .addr(int_const(0x100u128))
            .data(int_const(42u128))
            .build(),
        1,
    );
    a::none(
        &function,
        store()
            .addr(int_const(0x100u128))
            .data(int_const(99u128))
            .build(),
    );
}

#[test]
fn store_space_matches() {
    let function = shapes::store_then_load_ram(0x100, 42);
    a::matches(&function, store().space(rsleigh::VnSpace::RAM).build(), 1);
    a::none(&function, store().space(rsleigh::VnSpace::UNIQUE).build());
}

#[test]
fn store_then_load_same_addr_match() {
    let function = shapes::store_then_load_ram(0x200, 77);
    a::matches(
        &function,
        store()
            .addr(int_const(0x200u128))
            .data(int_const(77u128))
            .build(),
        1,
    );
    a::matches(&function, load().addr(int_const(0x200u128)).build(), 1);
}

#[test]
fn load_only_graph_matches() {
    let mut t = Tb::empty();
    let addr = t.u64(0x100);
    let v = t.load_ram(addr, ValueType::I64);
    let function = t.ret_val(v);

    a::matches(&function, load().build(), 1);
    a::none(&function, store().build());
}

/// bit_width(n) must select the load whose value output is n bits and leave
/// the other unmatched, on two same-base loads of different widths.
#[test]
fn load_bit_width_filters_among_multiple_loads() {
    let base = reg_vn(0x40, 8);
    let mut t = Tb::with_vars(&[base]);
    let base_v = t.read_var(&base);
    let l32 = t.load_ram(base_v, ValueType::I32);
    let l64 = t.load_ram(base_v, ValueType::I64);
    // Combined so both loads are in the Return's reachable set.
    let l32_64 = t.zext_to(l32, ValueType::I64);
    let combined = t.add(l32_64, l64);
    let function = t.ret_val(combined);

    a::matches(&function, load().build(), 2);
    a::matches(&function, load().bit_width(32).build(), 1);
    a::matches(&function, load().bit_width(64).build(), 1);
}
