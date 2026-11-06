//! `analyze` and `build_cfg` return their result whatever its
//! `Incompleteness` names; only a failure is an `Err`.

use crate::common;

use strider_cfg::PcodeInsnAddr;
use strider_orchestrator::{AnalyzeOptions, Reason};

const BASE: u64 = 0x1000;

/// `mov rax, [rdi]; jmp rax`: one dispatch nothing resolves.
fn jmp_rax() -> Vec<u8> {
    vec![0x48, 0x8b, 0x07, 0xff, 0xe0]
}

/// Everything `jmp_rax` leaves incomplete.
fn jmp_rax_incomplete() -> Vec<(PcodeInsnAddr, Reason)> {
    vec![(
        PcodeInsnAddr::at_machine_start(0x1003),
        Reason::UnresolvedBranch,
    )]
}

/// `mov rax, 42; ret`: complete.
fn mov_ret() -> Vec<u8> {
    vec![0x48, 0xc7, 0xc0, 0x2a, 0x00, 0x00, 0x00, 0xc3]
}

#[test]
fn the_default_returns_an_incomplete_result() {
    let r = common::analyze_bytes(common::Arch::X64, jmp_rax(), BASE, None, BASE);
    assert_eq!(
        r.incomplete().iter().collect::<Vec<_>>(),
        jmp_rax_incomplete()
    );
}

#[test]
fn a_complete_result_names_nothing() {
    let r = common::analyze_bytes(common::Arch::X64, mov_ret(), BASE, None, BASE);
    assert!(r.incomplete().is_empty(), "{}", r.incomplete());
}

#[test]
fn build_cfg_returns_an_incomplete_cfg() {
    let (mut strider, _cc) = common::strider_over_bytes(common::Arch::X64, jmp_rax(), BASE, None);
    let cfg = strider
        .build_cfg(BASE, &strider_cfg::CfgOptions::default())
        .expect("an incomplete cfg is returned");
    assert_eq!(
        cfg.incomplete().iter().collect::<Vec<_>>(),
        jmp_rax_incomplete()
    );
}

#[test]
fn an_unmapped_entry_fails() {
    let (mut strider, cc) = common::strider_over_bytes(common::Arch::X64, jmp_rax(), BASE, None);
    assert!(
        strider
            .analyze(0x9000, &cc, &AnalyzeOptions::default(), None)
            .is_err()
    );
}

/// `derived_targets` is the loop's own layer, so a caller's would be dropped
/// without a word: `analyze` refuses it, while `build_cfg` seats it.
#[test]
fn analyze_refuses_a_callers_derived_targets() {
    let (mut strider, cc) = common::strider_over_bytes(common::Arch::X64, jmp_rax(), BASE, None);
    let mut derived = rustc_hash::FxHashMap::default();
    derived.insert(
        PcodeInsnAddr::at_machine_start(0x1003),
        strider_cfg::ResolvedTargets::Withdrawn,
    );
    let opts = AnalyzeOptions {
        cfg: strider_cfg::CfgOptions {
            derived_targets: &derived,
            ..strider_cfg::CfgOptions::default()
        },
        ..AnalyzeOptions::default()
    };
    let err = strider
        .analyze(BASE, &cc, &opts, None)
        .err()
        .expect("a caller's derived_targets is refused");
    assert!(err.to_string().contains("derived_targets"), "{err}");
    strider
        .build_cfg(BASE, &opts.cfg)
        .expect("build_cfg seats derived_targets");
}
