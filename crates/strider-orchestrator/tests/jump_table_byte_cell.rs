//! A jump through a byte table indexed by a guarded index, from Linux
//! 7.2-rc2's `tcp_accecn_third_ack` (AArch64, clang):
//!
//! ```text
//! ffff800080349a74: cmp  w8, #0x7
//! ffff800080349a78: b.hi 0xffff800080349ab4
//! ffff800080349a7c: adrp x9, 0xffff800080460000
//! ffff800080349a80: add  x9, x9, #0xfb8
//! ffff800080349a84: adr  x10, 0xffff800080349a94
//! ffff800080349a88: ldrb w11, [x9, x8]
//! ffff800080349a8c: add  x10, x10, x11, lsl #2
//! ffff800080349a90: br   x10
//! ```
//!
//! The byte is a cell of the table at `x9`, picked by `x8 <= 7`, so its width
//! bounds nothing. The kernel maps that `.rodata` writable, where its ROM,
//! serving the text alone, does not reach. Only the cells' own values are
//! targets: the byte's 256 values put arms in the next functions.

use strider_cfg::ResolvedTargets;
use strider_ir::node::NodeKind;
use strider_ir::{IRViewer, IRWalker};
use strider_ir_test_utils::MockRom;
use strider_orchestrator::AnalyzeOptions;
use strider_orchestrator::opt::{ReadOnlyMemory, classify_target};

mod common;

const BASE: u64 = 0xffff_8000_8034_9a5c;
const BYTE_TABLE: u64 = 0xffff_8000_8046_0fb8;
/// The byte table, as the image holds it.
const CELLS: [u8; 8] = [1, 0, 8, 8, 8, 0, 8, 0];
/// `0x...9a94 + 4 * cell` for each distinct cell.
const ARMS: [u64; 3] = [
    0xffff_8000_8034_9a94,
    0xffff_8000_8034_9a98,
    0xffff_8000_8034_9ab4,
];

/// The function's 0xcc bytes.
const CODE: &[&str] = &[
    "285c40f9294c41790801098b0919407928fd4ed328017eb31f1d0071e8010054",
    "a90800f029e13e918a0000102b6968384a090b8b40011fd6c0035fd628d84039",
    "c8ffff3508185e39490180520801092a08181e39c0035fd6292845293f010a6b",
    "c1feff5429d8403989feff35e91600d06a0a00904a410791487968b8293d7339",
    "a9000034491c00120a1d00125f01096b010100541f0d007101fdff54080446b9",
    "c8fcff3528008052080406b9c0035fd65f0100712419437a2419407ac1feff54",
    "08185e3908011f32e2ffff17",
];

fn code() -> Vec<u8> {
    let hex: String = CODE.concat();
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex"))
        .collect()
}

/// The lifted and optimised function, its dispatch left unresolved, and what
/// the classifier answers for it over `rom`.
fn classify(rom: &dyn ReadOnlyMemory) -> Option<ResolvedTargets> {
    let bytes = code();
    assert_eq!(bytes.len(), 0xcc);
    let (mut strider, cc) = common::strider_over_bytes(common::Arch::Aarch64, bytes, BASE, None);
    let opts = AnalyzeOptions {
        resolve_indirect_branches: false,
        ..Default::default()
    };
    let function = strider
        .analyze(BASE, &cc, &opts, None)
        .expect("analyze")
        .function;
    let branch = function
        .walk()
        .find(|&n| matches!(function.node_kind(n), NodeKind::IndirectBranch))
        .expect("the dispatch");
    classify_target(&function, branch, Some(rom)).expect("classify")
}

/// The arms of `targets`, sorted.
fn arms(targets: &ResolvedTargets) -> Vec<u64> {
    let mut arms: Vec<u64> = targets.arms().iter().map(|t| t.addr).collect();
    arms.sort_unstable();
    arms
}

/// Unserved, as the kernel's ROM leaves it, the cells are unknown, so the
/// site defers: it never takes the byte's 256 values for arms.
/// Mutation: take no term of the address for a bounded index.
#[test]
fn a_byte_table_no_rom_serves_defers() {
    let text = MockRom::raw_bytes(BASE, code());
    let got = classify(&text);
    assert!(got.is_none(), "{:x?}", got.as_ref().map(arms));
}

/// Served, an answer names only the cells' arms.
#[test]
fn a_byte_table_the_rom_serves_names_no_arm_but_its_cells() {
    let table = MockRom::raw_bytes(BYTE_TABLE, CELLS.to_vec());
    let got = classify(&table);
    assert!(
        got.as_ref().is_none_or(|t| arms(t) == ARMS),
        "{:x?}",
        got.as_ref().map(arms)
    );
}
