//! `Match::asm_fingerprint` tests.
//!
//! Builds tiny mock graphs with explicit asm-addresses set on the
//! `FunctionBuilder`, then matches a pattern with a capture and
//! verifies the captured node's fingerprint is what we set.

use strider_analyze::pattern::*;

use super::support::{Tb, assertions as a};

#[test]
fn asm_fingerprint_returns_attributed_address() {
    let mut t = Tb::empty();
    t.fb_mut().set_lift_addr(Some(0x100));
    let c = t.u64(42);
    // Keep a non-None lift_addr active so the trailing Return node
    // (emitted by ret_val) carries a fingerprint and satisfies the
    // post-rewrite always-on asm-fingerprint check.
    let function = t.ret_val(c);

    let v = Capture::new();
    let m = a::first(&function, int_const(42u128).capture(v));
    assert_eq!(m.asm_fingerprint(v, &function), &[0x100]);
}

#[test]
fn asm_fingerprint_unbound_capture_is_empty() {
    let mut t = Tb::empty();
    t.fb_mut().set_lift_addr(Some(0x200));
    let c = t.u64(7);
    let function = t.ret_val(c);
    let bound = Capture::new();
    let unbound = Capture::new();
    let m = a::first(&function, int_const(7u128).capture(bound));
    // The match was for `int_const(7).capture(bound)`; `unbound` was
    // never declared in the pattern so the matcher has no binding for it.
    assert_eq!(m.asm_fingerprint(unbound, &function), &[] as &[u64]);
}

#[test]
fn asm_fingerprint_captures_dedup_unioned_addresses() {
    // Two adds of (1, 2) at different addresses dedup to a single Add
    // node in the graph; its fingerprint contains both addresses.
    let mut t = Tb::empty();
    t.fb_mut().set_lift_addr(Some(0x100));
    let l1 = t.u64(1);
    let r1 = t.u64(2);
    let _add1 = t.add(l1, r1);
    t.fb_mut().set_lift_addr(Some(0x200));
    let l2 = t.u64(1);
    let r2 = t.u64(2);
    let add2 = t.add(l2, r2);
    let function = t.ret_val(add2);

    let v = Capture::new();
    let m = a::first(&function, add(int_const(1u128), int_const(2u128)).capture(v));
    let fp = m.asm_fingerprint(v, &function);
    assert!(
        fp.contains(&0x100) && fp.contains(&0x200),
        "expected union fingerprint [0x100, 0x200], got {fp:?}"
    );
}
