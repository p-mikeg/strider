//! A jump table indexed by a byte the function reads through a pointer, from
//! Linux 6.1's `psmouse_process_byte` (x86-64, clang):
//!
//! ```text
//! ffffffff811cfdcc: movzbl 0xcb(%rbx),%ecx
//! ffffffff811cfdd3: mov    %rcx,%rax
//! ffffffff811cfdd6: shr    $0x6,%rax
//! ffffffff811cfdda: jmp    *-0x7e7db240(,%rax,8)
//! ```
//!
//! The byte is a field of the struct `%rbx` points at, not a cell of a table,
//! so its width bounds the index: `>> 6` leaves four slots.

use strider_cfg::{CfgOptions, FunctionBound, RegionTerminator};
use strider_ir_test_utils::MockRom;
use strider_orchestrator::{AnalyzeOptions, Reason};

use crate::common;

const BASE: u64 = 0xffff_ffff_811c_fcee;
const DISPATCH: u64 = 0xffff_ffff_811c_fdda;
/// The function's two tables, adjacent in `.rodata`: the guarded state
/// switch's ten slots, then the field-indexed one's four.
const RODATA: u64 = 0xffff_ffff_8182_4d70;
const STATE_SLOTS: [u64; 10] = [
    0xffff_ffff_811c_fd34,
    0xffff_ffff_811c_fd68,
    0xffff_ffff_811c_fdb1,
    0xffff_ffff_811c_fdcc,
    0xffff_ffff_811c_febe,
    0xffff_ffff_811c_febe,
    0xffff_ffff_811c_febe,
    0xffff_ffff_811c_febe,
    0xffff_ffff_811c_febe,
    0xffff_ffff_811c_fe19,
];
const SLOTS: [u64; 4] = [
    0xffff_ffff_811c_fde1,
    0xffff_ffff_811c_fe3e,
    0xffff_ffff_811c_fe55,
    0xffff_ffff_811c_fde1,
];

/// The function's 0x211 bytes.
const CODE: &[&str] = &[
    "41574156538a8fd1000000b8010000003a8fd20000000f82ef0100004889fb4c",
    "8b7f084c8db7c8000000488b87c00000008b0083c0fd83f8090f8791010000ff",
    "24c5704d82810fb68bc8000000c1e90383e1014c89ffbe01000000ba14010000",
    "e8dda2ffff8a83c800000000c024800883c9000000e9560100000fbe8bcb0000",
    "00f7d94c89ffbe02000000ba08000000e8ada2ffff0fb68bc8000000c1e90683",
    "e1014c89ffbe01000000ba13010000e88ea2ffff0fb68bc8000000c1e907e9fb",
    "0000000fbe8bcb000000f7d94c89ffbe02000000ba08000000e9ed0000000fb6",
    "8bcb0000004889c848c1e806ff24c5c04d8281c1e11cc1f91c89c8f7d889ca0f",
    "49d0803d39499b0000747383fa02726e89c8c0e80700c8d0f80fbec84c89ffbe",
    "02000000ba06000000eb62410fb60ec1e90383e1014c89ffbe01000000ba1301",
    "0000e8fba1ffff41800e08e980000000c1e11ac1f91af7d94c89ffbe02000000",
    "ba06000000eb15c1e11ac1f91af7d94c89ffbe02000000ba08000000eb4d4c89",
    "ffbe02000000ba0800000089c1e8b0a1ffff0fb68bcb000000c1e90483e1014c",
    "89ffbe01000000ba13010000e891a1ffff0fb68bcb000000c1e90583e1014c89",
    "ffbe01000000ba14010000e872a1ffff8a83c80000000a83d40000008883c800",
    "00000fb6f04c89ffe845fdffff4c89ff4c89f6e890fdffff4c89ff31f631d231",
    "c9e83ca1ffffb8020000005b415e415fc3",
];

/// Mutation: count a field load's width-only range as a table cell's.
#[test]
fn a_table_indexed_by_a_loaded_field_resolves_to_its_slots() {
    let bytes = common::hex(&CODE.concat());
    assert_eq!(bytes.len(), 0x211);
    let rodata = [STATE_SLOTS.as_slice(), SLOTS.as_slice()].concat();
    let rom = Box::new(MockRom::strided(RODATA, 8, rodata, 8))
        as Box<dyn strider_orchestrator::opt::ReadOnlyMemory>;
    let (mut strider, cc) = common::strider_over_bytes(common::Arch::X64, bytes, BASE, Some(rom));
    let opts = AnalyzeOptions {
        cfg: CfgOptions {
            bound: FunctionBound::size(0x211),
            ..CfgOptions::default()
        },
        ..Default::default()
    };
    let result = strider.analyze(BASE, &cc, &opts, None).expect("analyze");
    assert_eq!(
        result.incomplete().of(Reason::UnresolvedBranch).count(),
        0,
        "{:?}",
        result.incomplete().iter().collect::<Vec<_>>()
    );
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
    want.dedup();
    assert_eq!(arms, want);
}
