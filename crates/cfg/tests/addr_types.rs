#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Ordering / conversion tests for `MachineInsnAddr` and `PcodeInsnAddr`.

mod common;
use common::addr;

use cfg::test_api::MachineInsnAddr;

#[test]
fn machine_insn_addr_from_u64() {
    let a: MachineInsnAddr = 0x1000u64.into();
    assert_eq!(a.addr, 0x1000);
}

#[test]
fn machine_insn_addr_ordering() {
    let lo: MachineInsnAddr = 0x100u64.into();
    let hi: MachineInsnAddr = 0x200u64.into();
    assert!(lo < hi);
    assert!(hi > lo);
    assert_eq!(lo, lo);
}

#[test]
fn pcode_addr_orders_by_machine_addr_first() {
    assert!(addr(200, 0) > addr(100, 99));
}

#[test]
fn pcode_addr_orders_by_insn_index_when_machine_addr_equal() {
    assert!(addr(100, 1) > addr(100, 0));
    assert!(addr(100, 5) > addr(100, 4));
    assert_eq!(addr(100, 3), addr(100, 3));
}

#[test]
fn pcode_addr_ordering_is_antisymmetric() {
    let a = addr(0x400, 2);
    let b = addr(0x400, 5);
    assert!(a < b);
    assert!(b > a);
}

#[test]
fn pcode_addr_equality() {
    let a = addr(0x1000, 7);
    let b = addr(0x1000, 7);
    assert_eq!(a, b);
    assert!(a >= b);
    assert!(a <= b);
}
