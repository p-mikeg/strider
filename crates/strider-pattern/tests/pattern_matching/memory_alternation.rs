//! Alternation in a memory slot.

use strider_ir::node::ValueType;
use strider_ir_test_utils::Tb;
use strider_pattern::{Capture, Matcher, call, first_of, int_const, load, one_of, store, var};

/// `store(0x100, 42); load(0x200); call(0x1234); load(0x300)`, so the two
/// loads take their memory token from the store and from the call.
fn store_load_call_load() -> strider_ir::Function {
    let mut t = Tb::empty();
    let addr1 = t.u64(0x100);
    let val = t.u64(42);
    t.store_ram(addr1, val);
    let addr2 = t.u64(0x200);
    let after_store = t.load_ram(addr2, ValueType::I64);
    t.call_at(0x1234);
    let addr3 = t.u64(0x300);
    let after_call = t.load_ram(addr3, ValueType::I64);
    let sum = t.add(after_store, after_call);
    t.ret_val(sum)
}

#[test]
fn one_of_matches_either_memory_producer() {
    let f = store_load_call_load();
    let m = Matcher::new(&f);
    let pat = load().mem(one_of![store(), call()]).build();
    assert_eq!(m.find_all(&pat).unwrap().len(), 2);
}

#[test]
fn one_of_arms_discriminate() {
    let f = store_load_call_load();
    let m = Matcher::new(&f);
    let from_store = load()
        .addr(int_const(0x200u128))
        .mem(one_of![store(), call()])
        .build();
    assert_eq!(m.find_all(&from_store).unwrap().len(), 1);

    let no_arm_fits = load()
        .addr(int_const(0x200u128))
        .mem(one_of![call()])
        .build();
    assert_eq!(m.find_all(&no_arm_fits).unwrap().len(), 0);
}

/// A wildcard arm binds the memory edge.
#[test]
fn wildcard_arm_binds_the_memory_edge() {
    let f = store_load_call_load();
    let m = Matcher::new(&f);
    let y = Capture::new();
    let pat = load()
        .addr(int_const(0x200u128))
        .mem(one_of![var(y)])
        .build();
    let hits = m.find_all(&pat).unwrap();
    assert_eq!(hits.len(), 1);
    assert!(hits[0].value(y).is_some(), "memory edge bound");
}

#[test]
fn one_of_mixes_a_wildcard_with_a_producer_arm() {
    let f = store_load_call_load();
    let m = Matcher::new(&f);
    let pat = load()
        .addr(int_const(0x300u128))
        .mem(one_of![store(), var(Capture::new())])
        .build();
    assert_eq!(m.find_all(&pat).unwrap().len(), 1);
}

/// `first_of` cuts to the first matching arm in a memory slot too: the store
/// arm wins, and the two loads still take one match each.
#[test]
fn first_of_cuts_to_the_first_memory_arm() {
    let f = store_load_call_load();
    let m = Matcher::new(&f);
    let pat = load()
        .addr(int_const(0x200u128))
        .mem(first_of![store(), var(Capture::new())])
        .build();
    assert_eq!(m.find_all(&pat).unwrap().len(), 1);
}
