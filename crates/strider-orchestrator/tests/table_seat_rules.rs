//! How a table whose only arm lies past the function seats, against a
//! constant dispatch to the same address.
//!
//! A one-arm table out of range is more likely an over-read bound than a tail
//! dispatch, so it defers and is reported unresolved; a constant (`Single`)
//! leaves the function as a tail call, unverified.

use strider_cfg::{
    CfgOptions, FunctionBound, PcodeInsnAddr, RegionTerminator, ResolvedTarget, ResolvedTargets,
};
use strider_ir_test_utils::MockRom;
use strider_orchestrator::{AnalyzeOptions, AnalyzeResult, Reason};

mod common;

const BASE: u64 = 0x40_1000;
const TABLE: u64 = 0x50_0000;
/// The dispatch instruction in both buffers.
const DISPATCH: u64 = 0x40_1005;
/// Past the function's 0x20-byte bound.
const OUTSIDE: u64 = 0x40_2000;

/// `mov eax, edi ; and eax, 1 ; jmp [rax*8 + TABLE]`, then `ret`s.
fn table_dispatch() -> Vec<u8> {
    let mut bytes = vec![0xc3u8; 0x20];
    bytes[..12].copy_from_slice(&[
        0x89, 0xf8, 0x83, 0xe0, 0x01, 0xff, 0x24, 0xc5, 0x00, 0x00, 0x50, 0x00,
    ]);
    bytes
}

/// `mov eax, OUTSIDE ; jmp rax`, then `ret`s.
fn constant_dispatch() -> Vec<u8> {
    let mut bytes = vec![0xc3u8; 0x20];
    let at = u32::try_from(OUTSIDE).expect("32-bit").to_le_bytes();
    bytes[..7].copy_from_slice(&[0xb8, at[0], at[1], at[2], at[3], 0xff, 0xe0]);
    bytes
}

fn analyze(
    bytes: Vec<u8>,
    seed: Option<ResolvedTargets>,
    table: Option<[u64; 2]>,
) -> AnalyzeResult {
    let known: rustc_hash::FxHashMap<_, _> = seed
        .map(|s| (PcodeInsnAddr::at_machine_start(DISPATCH), s))
        .into_iter()
        .collect();
    let opts = AnalyzeOptions {
        cfg: CfgOptions {
            bound: FunctionBound::size(0x20),
            known_targets: &known,
            ..CfgOptions::default()
        },
        ..common::ignoring()
    };
    let rom = table.map(|t| {
        Box::new(MockRom::strided(TABLE, 8, t.to_vec(), 8))
            as Box<dyn strider_orchestrator::opt::ReadOnlyMemory>
    });
    let (mut strider, cc) = common::strider_over_bytes(common::Arch::X64, bytes, BASE, rom);
    strider.analyze(BASE, &cc, &opts, None).expect("analyze")
}

#[test]
fn a_one_arm_table_past_the_function_defers() {
    for (case, table) in [
        ("the seed alone", None),
        ("the seed and a derived table", Some([OUTSIDE, OUTSIDE])),
    ] {
        let seed = ResolvedTargets::Multiple(vec![ResolvedTarget::new(OUTSIDE, None)]);
        let result = analyze(table_dispatch(), Some(seed), table);
        let site = result
            .cfg
            .regions()
            .find_map(|r| match &r.terminator {
                RegionTerminator::UnresolvedIndirectBranch { addr, .. } => Some(*addr),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{case}: the site defers"));
        assert_eq!(site.machine_addr.addr, DISPATCH, "{case}");
        assert_eq!(
            result.incomplete().iter().collect::<Vec<_>>(),
            vec![(site, Reason::UnresolvedBranch)],
            "{case}",
        );
    }
}

#[test]
fn a_constant_dispatch_past_the_function_is_an_unverified_tail_call() {
    let result = analyze(constant_dispatch(), None, None);
    assert!(
        result.cfg.regions().any(|r| matches!(
            &r.terminator,
            RegionTerminator::TailCall { target } if target.addr == OUTSIDE
        )),
        "the constant leaves the function",
    );
    assert_eq!(
        result.incomplete().iter().collect::<Vec<_>>(),
        vec![(
            PcodeInsnAddr::at_machine_start(DISPATCH),
            Reason::UnverifiedSeed
        )],
    );
}
