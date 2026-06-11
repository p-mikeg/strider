#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Type-level tests for `MachineInsnAddr`, `PcodeInsnAddr`, `Region`,
//! and `CfgOptions`.  Ported from the pre-rewrite
//! `crates/cfg/tests/{addr_types,region,options}.rs`.
//!
//! These tests exercise pure data-type behaviour (ordering, conversions,
//! containment, distinctness of variants) and need no internal CFG state.

use strider_cfg::{
    CfgOptions, MachineInsnAddr, PcodeInsnAddr, Region, RegionInstruction,
    RegionTerminator,
};

// ── helpers ──────────────────────────────────────────────────────────────

fn addr(machine: u64, insn: u64) -> PcodeInsnAddr {
    PcodeInsnAddr {
        machine_addr: MachineInsnAddr { addr: machine },
        insn_index: insn,
    }
}

fn fake_insn() -> rsleigh::Insn {
    rsleigh::Insn {
        opcode: rsleigh::Opcode::Copy,
        output: None,
        inputs: vec![].into(),
    }
}

fn make_region(addrs: &[(u64, u64)]) -> Region {
    assert!(!addrs.is_empty(), "make_region requires at least one address");
    let start = addr(addrs[0].0, addrs[0].1);
    let insns: Vec<_> = addrs
        .iter()
        .map(|&(m, i)| RegionInstruction {
            addr: addr(m, i),
            insn: fake_insn(),
        })
        .collect();
    Region {
        start_addr: start,
        insns,
        terminator: RegionTerminator::Unconditional,
    }
}

// ── MachineInsnAddr / PcodeInsnAddr ──────────────────────────────────────

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
    // Machine-addr dominance both directions: a larger insn_index never
    // outranks a smaller machine address.
    assert!(addr(200, 0) > addr(100, 99));
    assert!(addr(100, 99) < addr(200, 0));
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

#[test]
fn pcode_addr_at_machine_start_zero_index() {
    let a = PcodeInsnAddr::at_machine_start(0x2000);
    assert_eq!(a.machine_addr.addr, 0x2000);
    assert_eq!(a.insn_index, 0);
}

// ── Region::contains_addr ────────────────────────────────────────────────

#[test]
fn contains_addr_at_start() {
    let r = make_region(&[(0x1000, 0), (0x1010, 0)]);
    assert!(r.contains_addr(addr(0x1000, 0)));
}

#[test]
fn contains_addr_at_end() {
    let r = make_region(&[(0x1000, 0), (0x1010, 0)]);
    assert!(r.contains_addr(addr(0x1010, 0)));
}

#[test]
fn contains_addr_in_interior() {
    let r = make_region(&[(0x1000, 0), (0x1010, 0)]);
    assert!(r.contains_addr(addr(0x1008, 0)));
}

#[test]
fn contains_addr_pcode_interior() {
    let r = make_region(&[(0x1000, 0), (0x1000, 3)]);
    assert!(r.contains_addr(addr(0x1000, 1)));
}

#[test]
fn contains_addr_before_start_returns_false() {
    let r = make_region(&[(0x1000, 0), (0x1010, 0)]);
    assert!(!r.contains_addr(addr(0x0ff8, 0)));
}

#[test]
fn contains_addr_after_end_returns_false() {
    let r = make_region(&[(0x1000, 0), (0x1010, 0)]);
    assert!(!r.contains_addr(addr(0x1014, 0)));
}

#[test]
fn contains_addr_returns_true_for_empty_region_at_start_addr() {
    // Empty regions arise from a popped trailing branch (Unconditional)
    // or a synthetic tail-call stub (TailCall) — see `add_region`; they
    // own exactly their `start_addr`.  Documented contract in
    // `Region::contains_addr`'s docstring.
    let r = Region {
        start_addr: addr(0x1000, 0),
        insns: Vec::new(),
        terminator: RegionTerminator::Unconditional,
    };
    assert!(r.contains_addr(addr(0x1000, 0)));
    assert!(!r.contains_addr(addr(0x1000, 1)));
}

// ── CfgOptions ─────────────────────────────────────────────────────────

#[test]
fn cfg_options_default_knobs() {
    let d = CfgOptions::default();
    assert_eq!(d.fn_max_size, None);
    assert!(!d.allow_code_before_start_addr);
    assert!(d.known_targets.is_empty());
}

#[test]
fn cfg_options_set_fn_max_size() {
    let sized = CfgOptions {
        fn_max_size: Some(0x1000),
        ..CfgOptions::default()
    };
    assert_eq!(sized.fn_max_size, Some(0x1000));
}

#[test]
fn cfg_options_allow_code_before_start_addr() {
    let allow = CfgOptions {
        allow_code_before_start_addr: true,
        ..CfgOptions::default()
    };
    assert!(allow.allow_code_before_start_addr);
}

#[test]
fn cfg_options_both_set() {
    let both = CfgOptions {
        fn_max_size: Some(0x1000),
        allow_code_before_start_addr: true,
        ..CfgOptions::default()
    };
    assert_eq!(both.fn_max_size, Some(0x1000));
    assert!(both.allow_code_before_start_addr);
}

// ── RegionTerminator: Switch + UnresolvedIndirectBranch shape ────────────

#[test]
fn switch_variant_round_trips_target_vn_and_targets() {
    let target_vn = rsleigh::Vn {
        addr_off: 0x20,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 8,
    };
    let targets = vec![0x1100u64, 0x1200, 0x1300, 0x1400];
    let term = RegionTerminator::Switch {
        target_vn,
        targets: targets.clone(),
    };
    let cloned = term.clone();
    assert_eq!(term, cloned, "Clone + Eq round-trip");
    match cloned {
        RegionTerminator::Switch {
            target_vn: tvn,
            targets: tts,
        } => {
            assert_eq!(tvn, target_vn);
            assert_eq!(tts, targets);
        }
        other => panic!("clone changed variant: {other:?}"),
    }
}

#[test]
fn unresolved_indirect_branch_variant_is_constructible() {
    let target_vn = rsleigh::Vn {
        size: 8,
        addr_off: 0x100,
        addr_space: rsleigh::VnSpace::REGISTER,
    };
    let pcode_addr = PcodeInsnAddr {
        machine_addr: MachineInsnAddr { addr: 0x1000 },
        insn_index: 3,
    };
    let term = RegionTerminator::UnresolvedIndirectBranch {
        target_vn,
        addr: pcode_addr,
    };
    let cloned = term.clone();
    assert_eq!(term, cloned);
    match term {
        RegionTerminator::UnresolvedIndirectBranch {
            target_vn: got_vn,
            addr: got_addr,
        } => {
            assert_eq!(got_vn, target_vn);
            assert_eq!(got_addr, pcode_addr);
        }
        other => panic!("unexpected variant: {other:?}"),
    }
}
