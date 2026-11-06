//! A `switch (len)` whose guard and index are two zero-extends of one byte,
//! from Linux 3.8's `cipso_v4_cache_add` (x86-64, gcc), the `jhash` tail:
//!
//! ```text
//! movzbl 0x1(%rdi),%ecx     ; len, then a loop over it
//! ...
//! ffffffff8151be12: cmp $0xc,%ecx
//! ffffffff8151be15: ja  ffffffff8151bdc0
//! ffffffff8151be17: jmp *-0x7e778908(,%rcx,8)
//! ```
//!
//! The index is a phi of the byte's 64-bit extend and the loop's remainder;
//! the guard on the first arm reads the byte's 32-bit extend. Both are the
//! byte's value, so the guard bounds the arm.

use strider_cfg::{CfgOptions, FunctionBound, RegionTerminator};
use strider_ir_test_utils::MockRom;
use strider_orchestrator::{AnalyzeOptions, Reason};

use crate::common;

const BASE: u64 = 0xffff_ffff_8151_bd10;
const DISPATCH: u64 = 0xffff_ffff_8151_be17;
const TABLE: u64 = 0xffff_ffff_8188_76f8;
/// The table's 13 slots, as the image holds them.
const SLOTS: [u64; 13] = [
    0xffff_ffff_8151_bebb,
    0xffff_ffff_8151_be7b,
    0xffff_ffff_8151_be72,
    0xffff_ffff_8151_be69,
    0xffff_ffff_8151_be60,
    0xffff_ffff_8151_be5a,
    0xffff_ffff_8151_be51,
    0xffff_ffff_8151_be48,
    0xffff_ffff_8151_be3f,
    0xffff_ffff_8151_be39,
    0xffff_ffff_8151_be30,
    0xffff_ffff_8151_be27,
    0xffff_ffff_8151_be1e,
];

/// The function's 0x2c4 bytes.
const CODE: &[&str] = &[
    "8b05c2ab550085c00f841c0200008b15b0ab550031c085d20f8e0c0200005548",
    "89e541574156415541544989f5538b9fc00000000fb647364801c348039fd000",
    "0000488b3d579c6e004885ff440fb673010f8441020000ba38000000be208000",
    "00e88aaec0ff4885c04989c40f8448020000450fb6feba200000004889df4c89",
    "fee87ae8bdff4885c049894424080f8414020000410fb6ce4d897c24108db1ef",
    "beadde83f90c89f089f2765b0f1f400003430803530483e90c4883c30c29c603",
    "73f489c7c1c70401d031fe89f729f201c6c1c70631fa89d729d001f2c1c70831",
    "f889c729c601d0c1c71031fe89f729f201c6c1cf0d31fa89d729d001f2c1c704",
    "31f883f90c77a9ff24cdf87688810fb64b0bc1e11801c80fb64b0ac1e11001c8",
    "0fb64b09c1e10801c80fb64b0801c80fb64b07c1e11801ca0fb64b06c1e11001",
    "ca0fb64b05c1e10801ca0fb64b0401ca0fb64b03c1e11801ce0fb64b02c1e110",
    "01ce0fb64b01c1e10801ce0fb60b31d001ce89d1c1c10e29c889c131c6c1c10b",
    "29ce89f131f2c1c90729ca89d131d0c1c11029c889c131c6c1c10429ce31f2c1",
    "c60e29f231d0c1ca0829d041890424498b4510f0ff00498b4510498944241841",
    "8b042483e07f488d1c4048c1e3034889df48033da0367000e8235611004889df",
    "48033d913670008b05d7a955003947040f833a000000488b5708498d44242848",
    "8942084989542428488d570849895424308347040148894708e8b256110031c0",
    "5b415c415d415e415f5df3c30f1f4000488b4710488b08488b50084889510848",
    "890a48b9000110000000adde48890848b9000220000000adde498d5424284889",
    "4808488b4f084889510849894c2428488d4f0849894c243048895708488d78d8",
    "e8dbeeffff48031dec3570004889dfeb880f1f800000000041bc10000000e9cf",
    "fdffff0f1f4400004c89e7e8b0eeffffb8f4ffffffe966ffffffb8f4ffffffe9",
    "5cffffff",
];

/// Mutation: bound an arm only by a guard on the arm's own value chain.
#[test]
fn a_guard_on_a_sibling_extend_bounds_the_switch() {
    let bytes = common::hex(&CODE.concat());
    assert_eq!(bytes.len(), 0x2c4);
    let rom = Box::new(MockRom::strided(TABLE, 8, SLOTS.to_vec(), 8))
        as Box<dyn strider_orchestrator::opt::ReadOnlyMemory>;
    let (mut strider, cc) = common::strider_over_bytes(common::Arch::X64, bytes, BASE, Some(rom));
    let opts = AnalyzeOptions {
        cfg: CfgOptions {
            bound: FunctionBound::size(0x2c4),
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
    assert!(!unresolved.contains(&DISPATCH), "{unresolved:x?}");
    let mut arms: Vec<u64> = result
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
    arms.sort_unstable();
    let mut want = SLOTS.to_vec();
    want.sort_unstable();
    assert_eq!(arms, want);
}
