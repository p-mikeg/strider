//! A state machine `switch (s)` in a loop, gcc -Os x86-64:
//!
//! ```text
//! 40108c: cmp $0x6,%eax
//! 40108f: ja  401135
//! 401095: notrack jmp *0x402000(,%rax,8)
//! ```
//!
//! The index is a phi of `zext(trunc rdi)` and each case's next state; the
//! guard reads its low 32 bits. Every arm fits 32 bits, so the guard bounds
//! the phi on every round, back edges included.

use strider_cfg::{CfgOptions, FunctionBound, RegionTerminator};
use strider_ir_test_utils::MockRom;
use strider_orchestrator::{AnalyzeOptions, Reason};

use crate::common;

const BASE: u64 = 0x40_1077;
const DISPATCH: u64 = 0x40_1095;
const TABLE: u64 = 0x40_2000;
const SLOTS: [u64; 7] = [
    0x40_109d, 0x40_10b0, 0x40_10c4, 0x40_10d7, 0x40_10ea, 0x40_1103, 0x40_1119,
];

/// `fsm`'s 0xc5 bytes.
const CODE: &[&str] = &[
    "f30f1efa5589f589f85331db5139dd0f8eac00000083f8060f87a00000003eff",
    "24c5002040004889dfe8cd02000085c07569b801000000eb7d4889dfe8c10200",
    "0085c00f94c00fb6c001c0eb694889dfe8b502000085c07558b805000000eb56",
    "4889dfe8aa02000085c075c6b802000000eb434889dfe89f0200004189c031c0",
    "4585c07531b806000000eb2a8d7b014863ffe86402000085c075eab803000000",
    "eb148d7b014863ffe85502000085c075bbb80400000048ffc3e94fffffff83c8",
    "ff5a5b5dc3",
];

fn code() -> Vec<u8> {
    let hex: String = CODE.concat();
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex"))
        .collect()
}

/// Mutation: drop the phi's truncated guard, or count it as unconnected.
#[test]
fn a_loop_carried_switch_under_a_truncated_guard_resolves() {
    let bytes = code();
    assert_eq!(bytes.len(), 0xc5);
    let rom = Box::new(MockRom::strided(TABLE, 8, SLOTS.to_vec(), 8))
        as Box<dyn strider_orchestrator::opt::ReadOnlyMemory>;
    let (mut strider, cc) = common::strider_over_bytes(common::Arch::X64, bytes, BASE, Some(rom));
    let opts = AnalyzeOptions {
        cfg: CfgOptions {
            bound: FunctionBound::size(0xc5),
            ..CfgOptions::default()
        },
        ..Default::default()
    };
    let result = strider.analyze(BASE, &cc, &opts, None).expect("analyze");
    let unresolved: Vec<u64> = result
        .incomplete()
        .of(Reason::UnresolvedBranch)
        .map(|a| a.machine_addr.addr)
        .collect();
    assert!(unresolved.is_empty(), "{unresolved:x?}");
    let arms: Vec<u64> = result
        .cfg
        .regions()
        .find_map(|r| match &r.terminator {
            RegionTerminator::Switch { targets, addr, .. }
                if addr.machine_addr.addr == DISPATCH =>
            {
                Some(targets.iter().map(|t| t.addr).collect())
            }
            _ => None,
        })
        .expect("the dispatch is a switch");
    assert_eq!(arms, SLOTS);
}
