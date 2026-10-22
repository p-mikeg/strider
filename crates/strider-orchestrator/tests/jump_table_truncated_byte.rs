//! A jump table indexed by the low byte of a 32-bit load, which x86-64 lifts
//! as `zext64(trunc8(load32))`:
//!
//! ```text
//! 401000: 8b 47 10              mov    0x10(%rdi),%eax
//! 401003: 0f b6 c0              movzbl %al,%eax
//! 401006: ff 24 c5 00 20 40 00  jmp    *0x402000(,%rax,8)
//! 40100d: b8 01 00 00 00        mov    $0x1,%eax
//! 401012: c3                    ret
//! 401013: b8 02 00 00 00        mov    $0x2,%eax
//! 401018: c3                    ret
//! 401019: b8 03 00 00 00        mov    $0x3,%eax
//! 40101e: c3                    ret
//! ```
//!
//! The byte is a field read through `%rdi`, so its width bounds the index:
//! 256 cells, as with a `movzbl 0x10(%rdi)`. The truncate is the only
//! difference, and the width count must see through it.

use strider_cfg::RegionTerminator;
use strider_ir_test_utils::MockRom;
use strider_orchestrator::{AnalyzeOptions, Reason};

mod common;

const BASE: u64 = 0x40_1000;
const TABLE: u64 = 0x40_2000;
const ARMS: [u64; 3] = [0x40_100d, 0x40_1013, 0x40_1019];

/// The table's 256 cells, `ARMS` in turn.
fn table() -> Vec<u8> {
    (0..256)
        .flat_map(|i| ARMS[i % ARMS.len()].to_le_bytes())
        .collect()
}

/// The arms of every seated `Switch` and the unresolved sites, for `code`
/// at `BASE` over a ROM serving `rom`.
fn analyze(code: Vec<u8>, rom: MockRom) -> (Vec<Vec<u64>>, Vec<u64>) {
    let (mut strider, cc) =
        common::strider_over_bytes(common::Arch::X64, code, BASE, Some(Box::new(rom)));
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
                arms.dedup();
                Some(arms)
            }
            _ => None,
        })
        .collect();
    (switches, unresolved)
}

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

/// Mutation: stop the width count's strip at a truncate.
#[test]
fn the_low_byte_of_a_loaded_field_indexes_its_256_cells() {
    let code = hex("8b47100fb6c0ff24c500204000b801000000c3b802000000c3b803000000c3");
    let (switches, unresolved) = analyze(code, MockRom::raw_bytes(TABLE, table()));
    assert!(unresolved.is_empty(), "{unresolved:#x?}");
    assert_eq!(switches, [ARMS.to_vec()]);
}
