//! An arm seated on a mode nobody proved for it is an UNVERIFIED answer, not
//! an incomplete one: the arm set is whole, only the ISA mode it decodes in
//! was inherited from its siblings. It belongs in `unverified_seeded_sites`,
//! which is derived from the final cfg, so `analyze` accumulates the rounds'
//! guesses and appends the ones still seated.
//!
//! Only an arch with an ISA-mode var reaches the flag at all, which is why
//! these run on MIPS and the same C source on x86-64 is the control.

mod common;

use object::{Object, ObjectSymbol};
use strider_cfg::PcodeInsnAddr;

struct Report {
    arms: Vec<u64>,
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
    let mut arms: Vec<u64> = r
        .cfg
        .regions()
        .filter_map(|region| match &region.terminator {
            strider_cfg::RegionTerminator::Switch { targets, .. } => Some(targets),
            _ => None,
        })
        .flatten()
        .map(|t| t.addr)
        .collect();
    arms.sort_unstable();
    arms.dedup();
    let addrs = |v: &[PcodeInsnAddr]| v.iter().map(|a| a.machine_addr.addr).collect::<Vec<_>>();
    Report {
        arms,
        unresolved: addrs(&r.unresolved_indirect_branches),
        unverified: addrs(&r.unverified_seeded_sites),
    }
}

/// `switch.c::main`'s dispatch. Round one classifies one arm and proves its
/// mode; round two re-derives all eight mode-less, which the site seats on the
/// mode its one proved arm committed.
const DISPATCH: u64 = 0x400540;

/// The eight words of `.rodata` the dispatch indexes, per `objdump`.
const TABLE: [u64; 8] = [
    0x400548, 0x400570, 0x400578, 0x400588, 0x400590, 0x400598, 0x4005a0, 0x4005a8,
];

fn a_widened_table_is_reported_unverified(arch: common::Arch) {
    let r = analyze(arch, "switch", "main");
    assert_eq!(r.arms, TABLE, "every arm of the real table must be seated");
    assert!(
        r.unverified.contains(&DISPATCH),
        "an arm seated on an inherited mode is unverified; got {:x?}",
        r.unverified,
    );
}

#[test]
fn mips32le_widened_table_is_reported_unverified() {
    a_widened_table_is_reported_unverified(common::Arch::Mips32le);
}

#[test]
fn mips32be_widened_table_is_reported_unverified() {
    a_widened_table_is_reported_unverified(common::Arch::Mips32be);
}

/// `switch_masked_loop`'s dispatch reaches BOTH channels at once, which the two
/// being complementary for a seated `Switch` alone would hide. Its six arms are
/// proven off the back edge's bound, but the arms' `Call` clobbers the table
/// base, so the selector stops deriving once the loop closes: nothing vouches
/// for the set being whole (a LOSS), while the mode one arm was seated on is
/// still only inherited (UNVERIFIED).
fn a_site_can_reach_both_report_channels(arch: common::Arch) {
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
        r.unverified.contains(&site),
        "the seated mode is still a guess; got {:x?}",
        r.unverified,
    );
}

#[test]
fn mips32le_site_can_reach_both_report_channels() {
    a_site_can_reach_both_report_channels(common::Arch::Mips32le);
}

#[test]
fn mips32be_site_can_reach_both_report_channels() {
    a_site_can_reach_both_report_channels(common::Arch::Mips32be);
}

/// The same C source on an arch with no ISA-mode var: nothing can be assumed,
/// so no channel fires.
#[test]
fn x64_has_no_mode_to_assume() {
    let r = analyze(common::Arch::X64, "switch", "main");
    assert!(r.unresolved.is_empty() && r.unverified.is_empty());
}
