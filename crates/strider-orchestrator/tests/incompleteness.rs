//! What `analyze` and `build_cfg` report through `Incompleteness`.

mod common;

use strider_orchestrator::Reason;

const BASE: u64 = 0x1000;

/// `mov rax, [rdi]; jmp rax`: one dispatch nothing resolves.
fn jmp_rax() -> Vec<u8> {
    vec![0x48, 0x8b, 0x07, 0xff, 0xe0]
}

fn machine(set: &strider_orchestrator::Incompleteness) -> Vec<(u64, Reason)> {
    set.iter().map(|(a, r)| (a.machine_addr.addr, r)).collect()
}

#[test]
fn analyze_reports_an_unresolved_dispatch_through_its_cfg() {
    let (mut strider, cc) = common::strider_over_bytes(common::Arch::X64, jmp_rax(), BASE, None);
    let r = strider
        .analyze(
            BASE,
            &cc,
            &strider_orchestrator::AnalyzeOptions::default(),
            None,
        )
        .expect("analyze");
    assert_eq!(
        machine(r.incomplete()),
        [(0x1003, Reason::UnresolvedBranch)]
    );
    assert!(std::ptr::eq(r.incomplete(), r.cfg.incomplete()));
    assert!(!r.incomplete().is_empty());
}

/// The final round's own build calls every classifier-derived seat an
/// unverified seed; the returned CFG must carry the analysis's verdict.
#[test]
fn a_resolved_table_leaves_the_returned_cfg_complete() {
    let (mut strider, cc, addr) =
        common::fixture_strider(common::Arch::X64, "switch", "dispatch_value");
    let r = strider
        .analyze(
            addr,
            &cc,
            &strider_orchestrator::AnalyzeOptions::default(),
            None,
        )
        .expect("analyze");
    assert!(
        r.cfg.regions().any(|region| matches!(
            region.terminator,
            strider_cfg::RegionTerminator::Switch { .. }
        )),
        "precondition: the table is seated"
    );
    assert!(r.cfg.incomplete().is_empty(), "{}", r.cfg.incomplete());
}

#[test]
fn build_cfg_names_each_dispatch_it_leaves_and_each_seed_it_seats() {
    let (mut strider, _cc) = common::strider_over_bytes(common::Arch::X64, jmp_rax(), BASE, None);
    let cfg = strider
        .build_cfg(BASE, &strider_cfg::CfgOptions::default())
        .expect("build_cfg");
    assert_eq!(
        machine(cfg.incomplete()),
        [(0x1003, Reason::UnresolvedBranch)]
    );

    let mut known = rustc_hash::FxHashMap::default();
    known.insert(
        strider_cfg::PcodeInsnAddr::at_machine_start(0x1003),
        strider_cfg::ResolvedTargets::LinkRegister,
    );
    let seeded = strider
        .build_cfg(
            BASE,
            &strider_cfg::CfgOptions {
                known_targets: &known,
                ..Default::default()
            },
        )
        .expect("build_cfg");
    assert_eq!(
        machine(seeded.incomplete()),
        [(0x1003, Reason::UnverifiedSeed)]
    );
}
