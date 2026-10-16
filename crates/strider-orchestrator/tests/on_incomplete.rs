//! `OnIncomplete` decides whether a result that is not complete is returned
//! or refused.

mod common;

use strider_orchestrator::{AnalyzeError, AnalyzeOptions, AnalyzeResult, OnIncomplete, Reason};

const BASE: u64 = 0x1000;

/// `mov rax, [rdi]; jmp rax`: one dispatch nothing resolves.
fn jmp_rax() -> Vec<u8> {
    vec![0x48, 0x8b, 0x07, 0xff, 0xe0]
}

/// `mov rax, 42; ret`: complete.
fn mov_ret() -> Vec<u8> {
    vec![0x48, 0xc7, 0xc0, 0x2a, 0x00, 0x00, 0x00, 0xc3]
}

fn analyze(bytes: Vec<u8>, on_incomplete: OnIncomplete) -> Result<AnalyzeResult, AnalyzeError> {
    let (mut strider, cc) = common::strider_over_bytes(common::Arch::X64, bytes, BASE, None);
    let opts = AnalyzeOptions {
        on_incomplete,
        ..Default::default()
    };
    strider.analyze(BASE, &cc, &opts, None)
}

#[test]
fn error_refuses_what_ignore_returns() {
    let ignored = analyze(jmp_rax(), OnIncomplete::Ignore).expect("Ignore returns the result");
    let Err(AnalyzeError::Incomplete(sites)) = analyze(jmp_rax(), OnIncomplete::Error) else {
        panic!("an unresolved dispatch must be refused");
    };
    assert_eq!(&sites, ignored.incomplete());
    let unresolved: Vec<u64> = sites
        .of(Reason::UnresolvedBranch)
        .map(|a| a.machine_addr.addr)
        .collect();
    assert_eq!(unresolved, [0x1003]);
}

#[test]
fn error_returns_a_complete_result() {
    let r = analyze(mov_ret(), OnIncomplete::Error).expect("a complete result is returned");
    assert!(r.is_complete());
}

#[test]
fn build_cfg_refuses_under_error_only() {
    let (mut strider, _cc) = common::strider_over_bytes(common::Arch::X64, jmp_rax(), BASE, None);
    let opts = strider_cfg::CfgOptions::default();
    let cfg = strider
        .build_cfg(BASE, &opts, OnIncomplete::Ignore)
        .expect("Ignore returns the cfg");
    let Err(AnalyzeError::Incomplete(sites)) = strider.build_cfg(BASE, &opts, OnIncomplete::Error)
    else {
        panic!("an unresolved dispatch must be refused");
    };
    assert_eq!(&sites, cfg.incompleteness());
}

/// A failure is not an incompleteness, whatever the policy.
#[test]
fn an_unmapped_entry_fails_under_either_policy() {
    for on_incomplete in [OnIncomplete::Ignore, OnIncomplete::Error] {
        let (mut strider, cc) =
            common::strider_over_bytes(common::Arch::X64, jmp_rax(), BASE, None);
        let opts = AnalyzeOptions {
            on_incomplete,
            ..Default::default()
        };
        let r = strider.analyze(0x9000, &cc, &opts, None);
        assert!(
            matches!(r, Err(AnalyzeError::Failed(_))),
            "{on_incomplete:?}"
        );
    }
}
