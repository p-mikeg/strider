use rustc_hash::FxHashSet;
use strider_pattern::*;

use super::support::{Tb, assertions as a};

#[test]
fn asm_fingerprint_returns_attributed_address() {
    let mut t = Tb::empty();
    t.fb_mut().set_lift_addr(Some(0x100));
    let c = t.u64(42);
    // lift_addr stays set so ret_val's Return also carries a fingerprint and
    // passes the always-on fingerprint check.
    let function = t.ret_val(c);

    let v = Capture::new();
    let m = a::first(&function, int_const(42u128).capture(v).into_pattern());
    assert_eq!(
        m.asm_fingerprint(v, &function),
        FxHashSet::from_iter([0x100])
    );
}

#[test]
fn asm_fingerprint_unbound_capture_is_empty() {
    let mut t = Tb::empty();
    t.fb_mut().set_lift_addr(Some(0x200));
    let c = t.u64(7);
    let function = t.ret_val(c);
    let bound = Capture::new();
    let unbound = Capture::new();
    let m = a::first(&function, int_const(7u128).capture(bound).into_pattern());
    // `unbound` was never declared in the pattern, so it stays unbound.
    assert!(m.asm_fingerprint(unbound, &function).is_empty());
}

#[test]
fn asm_fingerprint_captures_dedup_unioned_addresses() {
    // Two adds of (1, 2) at different addresses dedup to one node, whose
    // fingerprint is the union of both addresses.
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
    let m = a::first(
        &function,
        int_add(int_const(1u128), int_const(2u128))
            .capture(v)
            .into_pattern(),
    );
    let fp = m.asm_fingerprint(v, &function);
    assert!(
        fp.contains(&0x100) && fp.contains(&0x200),
        "expected union fingerprint [0x100, 0x200], got {fp:?}"
    );
}
