//! A Thumb-2 `tbb` whose index bound the range analysis misses, from clang
//! -O2 -mthumb (armv7a):
//!
//! ```text
//! 1050e: cmp   r0, #5
//! 10510: itt   gt
//! 10512: movgt r0, #0
//! 10514: popgt {r7, pc}
//! 10516: bhi   0x1052c
//! 10518: tbb   [pc, r0]      ; cells at 0x1051c: 03 0d 12 17 1d 22
//! ```
//!
//! The IT block makes the index a phi the `bhi` does not bound. The cell is a
//! byte at the ROM-served table base plus that index, so its width bounds
//! nothing: the site defers rather than taking the byte's 256 values for arms,
//! which reach into the table and past the function.

use strider_cfg::Reason;
use strider_ir_test_utils::MockRom;

use crate::common;

const BASE: u64 = 0x1050c;
const TBB: u64 = 0x10518;

/// The function's 0x60 bytes.
const CODE: &[&str] = &[
    "80b50528c4bf002080bd09d8dfe800f0030d12171d220120bde88040fff78cbe",
    "4d20bde88040fff787be0220fff784fe013080bd0320fff77ffe400080bd0420",
    "fff77afe80f0050080bd0520fff774fe013880bd0620fff76ffe40f0090080bd",
];

/// Mutation: read no constant term as ROM in `loads_a_field`.
#[test]
fn a_tbb_over_an_unbounded_index_defers() {
    let rom = MockRom::raw_bytes(BASE, common::hex(&CODE.concat()));
    let cfg = common::analyze_bytes(
        common::Arch::ArmThumb,
        common::hex(&CODE.concat()),
        BASE,
        Some(Box::new(rom)),
        BASE | 1,
    )
    .cfg;
    let reported: Vec<(u64, Reason)> = cfg
        .incomplete()
        .iter()
        .map(|(at, why)| (at.machine_addr.addr, why))
        .collect();
    assert_eq!(reported.first(), Some(&(TBB, Reason::UnresolvedBranch)));
    assert_eq!(reported.len(), 1, "{} reports", reported.len());
}
