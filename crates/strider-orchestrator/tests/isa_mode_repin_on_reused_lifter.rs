//! A function-entry ISA-mode pin must reach the decoder on a reused
//! `Lifter`, even when the address it pins is still in Sleigh's parse-tree
//! cache from a neighbouring function's over-decode.
//!
//! Two arches, because Sleigh sizes that cache at 2 entries normally and 8 on
//! a delay-slot arch such as MIPS.

use rsleigh::mem_readers::BufMemReader;
use strider_cfg::{Cfg, CfgOptions, MachineInsnAddr};
use strider_orchestrator::Lifter;

mod common;

/// `0x1000` `bl 0x2000`; `0x1004` the two Thumb halfwords `202a 4770`, which
/// as ARM is the single word `4770202a` (`ldrbmi`); `0x1008` ARM `bx lr`.
/// The ARM function falls through its call and over-decodes `0x1004` in ARM
/// state, seating a 4-byte parse tree at an address the Thumb function then
/// re-pins to `TMode = 1`.
fn arm_bytes() -> Vec<u8> {
    let mut bytes = vec![
        0xfe, 0x03, 0x00, 0xeb, // bl 0x2000
        0x2a, 0x20, 0x70, 0x47, // thumb: movs r0, #42 ; bx lr
        0x1e, 0xff, 0x2f, 0xe1, // bx lr
    ];
    bytes.extend(std::iter::repeat_n(0x00u8, 32));
    bytes
}

/// `0xff8` `addiu`; `0xffc` `jr ra`; `0x1000` its delay slot, whose low
/// halfword is the MIPS16 `jrc ra`.  `0x1000` is the LAST address the MIPS32
/// function decodes, so it is the entry the MIPS16 function then re-pins to
/// `ISA_MODE = 1`.
fn mips_bytes() -> Vec<u8> {
    let mut bytes = vec![
        0x01, 0x00, 0x42, 0x24, // addiu v0,v0,1
        0x08, 0x00, 0xe0, 0x03, // jr ra
        0xa0, 0xe8, 0x42, 0x24, // addiu v0,v0,-5984 / mips16 jrc ra
    ];
    bytes.extend(std::iter::repeat_n(0x00u8, 64));
    bytes
}

fn lifter(arch: common::Arch, bytes: Vec<u8>, base: u64) -> Lifter<BufMemReader<Vec<u8>>> {
    common::driver_for_reader(arch, BufMemReader::new(bytes, base)).0
}

fn build(lifter: &mut Lifter<BufMemReader<Vec<u8>>>, entry: u64) -> Cfg {
    lifter
        .build_cfg(
            MachineInsnAddr::from(entry),
            &CfgOptions::default(),
            &Default::default(),
        )
        .expect("build_cfg on the probe bytes")
}

/// Machine instructions the cfg decoded, as `(addr, byte length)`.
fn decoded(cfg: &Cfg) -> Vec<(u64, u32)> {
    let mut out: Vec<(u64, u32)> = cfg
        .regions()
        .flat_map(|region| region.insns.iter())
        .map(|insn| (insn.addr.machine_addr.addr, insn.len))
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

#[test]
fn thumb_entry_decodes_as_thumb_after_an_arm_neighbour_over_decoded_it() {
    let mut fresh = lifter(common::Arch::Arm, arm_bytes(), 0x1000);
    let alone = decoded(&build(&mut fresh, 0x1005));
    assert_eq!(
        alone,
        vec![(0x1004, 2), (0x1006, 2)],
        "fresh engine must decode the Thumb entry as two halfwords"
    );

    let mut reused = lifter(common::Arch::Arm, arm_bytes(), 0x1000);
    let arm = decoded(&build(&mut reused, 0x1000));
    assert_eq!(
        arm,
        vec![(0x1000, 4), (0x1004, 4), (0x1008, 4)],
        "the ARM neighbour must over-decode 0x1004 as one ARM word"
    );

    let after = decoded(&build(&mut reused, 0x1005));
    assert_eq!(
        after, alone,
        "the re-pinned Thumb mode must survive the cached ARM parse tree"
    );
}

#[test]
fn mips16_entry_decodes_as_mips16_after_a_mips32_neighbour_over_decoded_it() {
    let mut fresh = lifter(common::Arch::Mips32le, mips_bytes(), 0x0ff8);
    let alone = decoded(&build(&mut fresh, 0x1001));
    assert_eq!(
        alone,
        vec![(0x1000, 2)],
        "fresh engine must decode the MIPS16 entry as one halfword"
    );

    let mut reused = lifter(common::Arch::Mips32le, mips_bytes(), 0x0ff8);
    build(&mut reused, 0x0ff8);

    let after = decoded(&build(&mut reused, 0x1001));
    assert_eq!(
        after, alone,
        "the re-pinned MIPS16 mode must survive the cached MIPS32 parse tree"
    );
}
