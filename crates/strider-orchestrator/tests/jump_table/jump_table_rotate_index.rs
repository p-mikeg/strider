//! PowerPC jump tables whose guard or index is an `rlwinm`, which lifts as a
//! rotate, `(x << r) | (x >> (32 - r))`, under a mask. The shapes are Linux
//! 6.6's on ppc32 clang: `mas_find`'s rotate guard, `scsi_decide_disposition`'s
//! extracted field, `sd_init_command`'s `clrlwi` guard and `may_open`'s
//! `srwi`. Each table is followed by cells naming the function's entry, which
//! a read past the guard would seat.

use strider_cfg::RegionTerminator;
use strider_ir_test_utils::MockRom;
use strider_orchestrator::{AnalyzeOptions, Reason};

use crate::common;

const BASE: u64 = 0x1000_0000;
const TABLE: u64 = 0x1000_2000;

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

/// The arms of every seated `Switch` and the unresolved sites, for `code` at
/// `BASE` over a ROM holding `arms` at `TABLE`, then cells naming `BASE`.
fn analyze(code: &str, arms: &[u64]) -> (Vec<Vec<u64>>, Vec<u64>) {
    let cells: Vec<u8> = arms
        .iter()
        .copied()
        .chain([BASE; 8])
        .flat_map(|a| u32::try_from(a).unwrap().to_be_bytes())
        .collect();
    let (mut strider, cc) = common::strider_over_bytes(
        common::Arch::Ppc32be,
        hex(code),
        BASE,
        Some(Box::new(MockRom::raw_bytes(TABLE, cells))),
    );
    let result = strider
        .analyze(BASE, &cc, &AnalyzeOptions::default(), None)
        .expect("analyze");
    let unresolved = result
        .incomplete()
        .of(Reason::UnresolvedBranch)
        .map(|a| a.machine_addr.addr)
        .collect();
    let switches = result
        .cfg
        .regions()
        .filter_map(|region| match &region.terminator {
            RegionTerminator::Switch { targets, .. } => {
                let mut arms: Vec<u64> = targets.iter().map(|t| t.addr).collect();
                arms.sort_unstable();
                Some(arms)
            }
            _ => None,
        })
        .collect();
    (switches, unresolved)
}

/// The `li`s and `blr`s after the dispatch, each one an arm, from `first`.
fn stubs(first: u64, n: u64) -> Vec<u64> {
    (0..n).map(|i| first + 4 * i).collect()
}

/// ```text
/// 00: addi   r3,r5,-9
/// 04: rotlwi r4,r3,29
/// 08: cmplwi r4,7
/// 0c: bgt    40
/// 10: li     r4,0x2000
/// 14: rlwinm r3,r3,31,0,29
/// 18: addis  r4,r4,0x1000
/// 1c: lwzx   r3,r3,r4
/// 20: mtctr  r3
/// 24: bctr
/// ```
///
/// `rotl(r3, 29) <= 7` holds only where `r3`'s low 3 bits are zero and its
/// top 3 are at most 7: `r3` is one of `0, 8, .., 56`.
/// Mutation: drop the rotate step from the guard's back-propagation.
#[test]
fn a_rotate_guard_bounds_the_value_it_rotates() {
    let arms = stubs(BASE + 0x28, 8);
    let (switches, unresolved) = analyze(
        "3865fff75464e83e2804000741810034388020005463f83a3c8410007c63202e\
         7c6903a64e800420386000014e800020386000024e800020386000034e800020\
         386000004e800020",
        &arms,
    );
    assert!(unresolved.is_empty(), "{unresolved:#x?}");
    assert_eq!(switches, [arms]);
}

/// ```text
/// 00: rlwinm r5,r3,16,24,31   (r3 >> 16) & 0xff
/// 04: cmplwi r5,4
/// 08: bgt    3c
/// 0c: li     r5,0x2000
/// 10: rlwinm r6,r3,18,22,29   ((r3 >> 16) & 0xff) << 2
/// 14: addis  r5,r5,0x1000
/// 18: lwzx   r5,r6,r5
/// 1c: mtctr  r5
/// 20: bctr
/// ```
/// Mutation: keep the rotate's masked-off operand in `field_step`.
#[test]
fn a_guard_on_a_rotated_field_bounds_the_same_field_scaled() {
    let arms = stubs(BASE + 0x24, 5);
    let (switches, unresolved) = analyze(
        "5465863e280500044181003438a02000546695ba3ca510007ca6282e7ca903a6\
         4e800420386000014e800020386000024e800020386000034e800020386000004e800020",
        &arms,
    );
    assert!(unresolved.is_empty(), "{unresolved:#x?}");
    assert_eq!(switches, [arms]);
}

/// ```text
/// 00: clrlwi r4,r3,24         r3 & 0xff
/// 04: cmplwi r4,4
/// 08: bgt    3c
/// 0c: li     r5,0x2000
/// 10: rlwinm r4,r3,2,22,29    (r3 & 0xff) << 2
/// 14: addis  r5,r5,0x1000
/// 18: lwzx   r5,r4,r5
/// 1c: mtctr  r5
/// 20: bctr
/// ```
/// Mutation: read a mask of bits `s..s + m`, `s > 0`, as no field in
/// `field_step`.
#[test]
fn a_guard_on_low_bits_bounds_them_rotated_into_place() {
    let arms = stubs(BASE + 0x24, 5);
    let (switches, unresolved) = analyze(
        "5464063e280400044181003438a02000546415ba3ca510007ca4282e7ca903a6\
         4e800420386000014e800020386000024e800020386000034e800020386000004e800020",
        &arms,
    );
    assert!(unresolved.is_empty(), "{unresolved:#x?}");
    assert_eq!(switches, [arms]);
}

/// ```text
/// 00: addi   r5,r3,-4096
/// 04: rlwinm r3,r5,20,12,31   r5 >> 12
/// 08: cmplwi r3,4
/// 0c: bgt    40
/// 10: li     r6,0x2000
/// 14: rlwinm r5,r5,22,10,29   (r5 >> 12) << 2
/// 18: addis  r6,r6,0x1000
/// 1c: lwzx   r5,r5,r6
/// 20: mtctr  r5
/// 24: bctr
/// ```
/// Mutation: keep the rotate's masked-off operand in `field_step`.
#[test]
fn a_guard_on_a_rotated_shift_bounds_the_shift_scaled() {
    let arms = stubs(BASE + 0x28, 5);
    let (switches, unresolved) = analyze(
        "38a3f00054a3a33e280300044181003438c0200054a5b2ba3cc610007ca5302e\
         7ca903a64e800420386000014e800020386000024e800020386000034e800020\
         386000004e800020",
        &arms,
    );
    assert!(unresolved.is_empty(), "{unresolved:#x?}");
    assert_eq!(switches, [arms]);
}
