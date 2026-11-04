//! A jump table indexed by a byte read through a pointer at a small offset
//! the image also maps, as in Linux 3.16 ARM `__sk_run_filter` (`.vectors`
//! sit at 0) or a PIE (its headers sit at 0):
//!
//! ```text
//! 0x00: ldrb r3, [r1, #8]
//! 0x04: movw r6, #0x100
//! 0x08: add  r3, r6, r3, lsl #2
//! 0x0c: ldr  pc, [r3]
//! 0x10: bx   lr
//! 0x14: mov  r0, #1
//! 0x18: bx   lr
//! 0x100: 256 slots, alternately 0x10 and 0x14
//! ```
//!
//! The offset 8 is a field offset, not a table base, so the byte's width
//! bounds the index.

use strider_cfg::RegionTerminator;
use strider_ir_test_utils::MockRom;
use strider_orchestrator::AnalyzeOptions;

use crate::common;

fn image() -> Vec<u8> {
    let mut bytes = Vec::new();
    for word in [
        0xe5d1_3008u32,
        0xe300_6100,
        0xe086_3103,
        0xe593_f000,
        0xe12f_ff1e,
        0xe3a0_0001,
        0xe12f_ff1e,
    ] {
        bytes.extend(word.to_le_bytes());
    }
    bytes.resize(0x100, 0);
    for slot in 0..256u32 {
        bytes.extend((0x10 + 4 * (slot & 1)).to_le_bytes());
    }
    bytes
}

/// Mutation: read every ROM-served constant term as a table base in
/// `loads_a_field`.
#[test]
fn a_field_offset_the_image_maps_is_not_a_table_base() {
    let rom = MockRom::raw_bytes(0, image());
    let (mut strider, cc) =
        common::strider_over_bytes(common::Arch::Arm, image(), 0, Some(Box::new(rom)));
    let cfg = strider
        .analyze(0, &cc, &AnalyzeOptions::default(), None)
        .expect("analyze")
        .cfg;
    assert!(cfg.incomplete().is_empty(), "{}", cfg.incomplete());
    let arms: Vec<u64> = cfg
        .regions()
        .find_map(|r| match &r.terminator {
            RegionTerminator::Switch { targets, .. } => {
                Some(targets.iter().map(|t| t.addr).collect())
            }
            _ => None,
        })
        .expect("a switch");
    assert_eq!(arms, [0x10, 0x14]);
}
