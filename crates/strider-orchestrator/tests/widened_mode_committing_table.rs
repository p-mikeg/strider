//! A MIPS `jr` table commits an ISA mode per arm, and a table that widens once
//! its loop closes is re-derived through its seated `Switch`, which carries
//! that mode input. Every widened arm is seated in the mode its own table word
//! commits, so none is a guess inherited from its siblings and
//! `unverified_seeded_sites` stays clear of the site.
//!
//! Only an arch with an ISA-mode var has a mode to evaluate, which is why these
//! run on MIPS and the same C source on x86-64 is the control.

mod common;

use object::{Object, ObjectSymbol};
use strider_cfg::PcodeInsnAddr;

struct Report {
    arms: Vec<u64>,
    modes: Vec<Option<bool>>,
    unresolved: Vec<u64>,
    unverified: Vec<u64>,
}

fn analyze(arch: common::Arch, case: &str, fn_name: &str) -> Report {
    let path = common::binary_path(arch, case);
    let owned = strider_reader::load_elf(&path).expect("load_elf");
    let obj = owned.checked_file().expect("the mapped file is unchanged");
    let sa = arch.sleigh();
    let mem = strider_reader::ElfFileMemReader::from_object(&obj).expect("mem");
    let sleigh = rsleigh::Sleigh::new(sa.sla_spec(), sa.pspec(), mem).expect("sleigh");
    let addr = obj.symbol_by_name(fn_name).expect("symbol").address();
    let rom: Box<dyn strider_orchestrator::opt::ReadOnlyMemory> =
        Box::new(strider_reader::ElfFileMemReader::from_object(&obj).expect("rom"));
    let cc = arch.cc().build(&sleigh.regs().expect("regs")).expect("cc");
    let mut strider = strider_orchestrator::Strider::new(sa, sleigh, Some(rom)).expect("new");
    let r = strider
        .analyze(
            addr,
            &cc,
            &strider_orchestrator::LiftOptions::default(),
            &strider_orchestrator::opt::OptOptions::default(),
            None,
        )
        .expect("analyze");
    let mut seated: Vec<(u64, Option<bool>)> = r
        .cfg
        .regions()
        .filter_map(|region| match &region.terminator {
            strider_cfg::RegionTerminator::Switch { targets, .. } => Some(targets),
            _ => None,
        })
        .flatten()
        .map(|t| (t.addr, t.isa_bit))
        .collect();
    seated.sort_unstable();
    seated.dedup();
    let (arms, modes) = seated.into_iter().unzip();
    let addrs = |v: &[PcodeInsnAddr]| v.iter().map(|a| a.machine_addr.addr).collect::<Vec<_>>();
    Report {
        arms,
        modes,
        unresolved: addrs(&r.unresolved_indirect_branches),
        unverified: addrs(&r.unverified_seeded_sites),
    }
}

/// `switch.c::main`'s dispatch. Round one classifies one arm and proves its
/// mode; round two re-derives all eight, each with its own mode.
const DISPATCH: u64 = 0x400540;

/// The eight words of `.rodata` the dispatch indexes, per `objdump`.
const TABLE: [u64; 8] = [
    0x400548, 0x400570, 0x400578, 0x400588, 0x400590, 0x400598, 0x4005a0, 0x4005a8,
];

fn a_widened_table_seats_every_arm_in_its_own_mode(arch: common::Arch) {
    let r = analyze(arch, "switch", "main");
    assert_eq!(r.arms, TABLE, "every arm of the real table must be seated");
    assert!(
        r.modes.iter().all(|&mode| mode == Some(false)),
        "every arm carries the mode its word commits; got {:?}",
        r.modes,
    );
    assert!(
        !r.unverified.contains(&DISPATCH),
        "no arm's mode was inherited; got {:x?}",
        r.unverified,
    );
}

#[test]
fn mips32le_widened_table_seats_every_arm_in_its_own_mode() {
    a_widened_table_seats_every_arm_in_its_own_mode(common::Arch::Mips32le);
}

#[test]
fn mips32be_widened_table_seats_every_arm_in_its_own_mode() {
    a_widened_table_seats_every_arm_in_its_own_mode(common::Arch::Mips32be);
}

/// `switch_masked_loop`'s six arms are proven off the back edge's bound, but the
/// arms' `Call` clobbers the table base, so the selector stops deriving once
/// the loop closes: nothing vouches for the set being whole, which is a LOSS
/// and nothing else.
fn a_selector_that_stops_deriving_is_a_loss(arch: common::Arch) {
    let r = analyze(arch, "switch_masked_loop", "masked_loop_switch");
    let site = 0x40072c;
    assert_eq!(
        r.arms,
        [0x400734, 0x400760, 0x400768, 0x400770, 0x400778, 0x400780],
        "exactly the table's six words, never a slot read past them",
    );
    assert!(
        r.unresolved.contains(&site),
        "a selector that stopped deriving is a loss; got {:x?}",
        r.unresolved,
    );
    assert!(
        !r.unverified.contains(&site),
        "no arm's mode was inherited; got {:x?}",
        r.unverified,
    );
}

#[test]
fn mips32le_selector_that_stops_deriving_is_a_loss() {
    a_selector_that_stops_deriving_is_a_loss(common::Arch::Mips32le);
}

#[test]
fn mips32be_selector_that_stops_deriving_is_a_loss() {
    a_selector_that_stops_deriving_is_a_loss(common::Arch::Mips32be);
}

/// The same C source on an arch with no ISA-mode var: no channel fires.
#[test]
fn x64_control_reports_nothing() {
    let r = analyze(common::Arch::X64, "switch", "main");
    assert!(r.unresolved.is_empty() && r.unverified.is_empty());
}
