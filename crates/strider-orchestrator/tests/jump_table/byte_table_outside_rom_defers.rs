//! An AArch64 byte-offset table from a clang arm64 kernel,
//! `ZSTD_decompressBlock_internal`, entered at the function's
//! `sub x4, x25, x28` and ending in a `ret` here:
//!
//! ```text
//! ffffffc0081d99b8: sub  x4, x25, x28
//! ffffffc0081d99c0: sub  x9, x4, #0x2
//! ffffffc0081d99c4: cmp  x9, #0x5
//! ffffffc0081d99d0: b.hi ffffffc0081d9a20
//! ffffffc0081d99d4: adrp x10, ...; add x10, x10, #0xbb0   ; cells 0a 08 06 04 02 00
//! ffffffc0081d99dc: adr  x11, ffffffc0081d99ec
//! ffffffc0081d99e0: ldrb w12, [x10, x9]
//! ffffffc0081d99e4: add  x11, x11, x12, lsl #2
//! ffffffc0081d99e8: br   x11
//! ```
//!
//! With the cells in ROM the guarded `x9` seats the six arms. Without, the
//! table address reads as a field through a pointer, and the loaded byte is
//! bounded only by its width, which bounds no table: the site defers rather
//! than seating `base + 4k` for all 256 bytes.

use strider_cfg::Reason;
use strider_ir_test_utils::MockRom;

use crate::common;

const BASE: u64 = 0xffff_ffc0_081d_99b8;
const BR: u64 = 0xffff_ffc0_081d_99e8;
const CELLS_AT: u64 = 0xffff_ffc0_083b_cbb0;
const CELLS: &[u8] = &[10, 8, 6, 4, 2, 0];

/// `sub x4, x25, x28`, `ffffffc0081d99bc..ffffffc0081d9a20`, then a `ret` at
/// the `b.hi` target.
const CODE: &[&str] = &[
    "24031ccb",
    "68004039890800d13f1500f1e34f00f9e84700f9880200540a0f00f04ac12e91",
    "8b0000104c6969386b090c8b60011fd669184039281d50b36914403908a1098b",
    "691040390881098b690c40390861098b690840390841098b690440390821098b",
    "e84700f9c0035fd6",
];

fn analyze(rom: MockRom) -> strider_orchestrator::AnalyzeResult {
    let code = common::hex(&CODE.concat());
    common::analyze_bytes(common::Arch::Aarch64, code, BASE, Some(Box::new(rom)), BASE)
}

fn arms(r: &strider_orchestrator::AnalyzeResult) -> Vec<u64> {
    let mut arms: Vec<u64> = r
        .cfg
        .regions()
        .filter_map(|reg| match &reg.terminator {
            strider_cfg::RegionTerminator::Switch { targets, .. } => Some(targets.clone()),
            _ => None,
        })
        .flatten()
        .map(|t| t.addr)
        .collect();
    arms.sort_unstable();
    arms
}

#[test]
fn rom_cells_seat_the_six_arms() {
    let r = analyze(MockRom::raw_bytes(CELLS_AT, CELLS.to_vec()));
    let want: Vec<u64> = (0..6).map(|k| 0xffff_ffc0_081d_99ec + 8 * k).collect();
    assert_eq!(arms(&r), want);
    assert!(r.incomplete().is_empty(), "{}", r.incomplete());
}

#[test]
fn cells_outside_rom_defer() {
    let r = analyze(MockRom::regions([]));
    assert_eq!(arms(&r), Vec::<u64>::new());
    let reported: Vec<(u64, Reason)> = r
        .incomplete()
        .iter()
        .map(|(at, why)| (at.machine_addr.addr, why))
        .collect();
    assert_eq!(reported, [(BR, Reason::UnresolvedBranch)]);
}
