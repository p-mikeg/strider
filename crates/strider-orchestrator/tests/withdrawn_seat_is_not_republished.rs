//! A seat the loop withdrew must not survive in the CFG it publishes.
//!
//! The loop withdraws the whole seat of a site that named a target the CFG
//! could not decode, because a bad arm is evidence the bound is wrong. The
//! build that raised it had only dropped the ONE arm, so its CFG still seats the
//! rest; converging on that publishes a `Switch` at a site the analysis
//! reports unresolved.

use strider_cfg::{CfgOptions, PcodeInsnAddr, RegionTerminator, ResolvedTarget, ResolvedTargets};
use strider_orchestrator::AnalyzeOptions;

mod common;

const BASE: u64 = 0x1000;
/// `jmp rax`, seeded with one arm that decodes and one that cannot.
const SITE: u64 = 0x1000;
const GOOD_ARM: u64 = 0x1010;
const UNMAPPED_ARM: u64 = 0x4000_1000;

#[test]
fn a_site_the_loop_withdrew_is_republished_without_its_seat() {
    let mut bytes = vec![0xc3u8; 0x20];
    bytes[0] = 0xff; // 0x1000: jmp rax
    bytes[1] = 0xe0;

    let mut known = rustc_hash::FxHashMap::default();
    known.insert(
        PcodeInsnAddr::at_machine_start(SITE),
        ResolvedTargets::table(vec![
            ResolvedTarget::new(GOOD_ARM, None),
            ResolvedTarget::new(UNMAPPED_ARM, None),
        ]),
    );
    let opts = AnalyzeOptions {
        cfg: CfgOptions {
            known_targets: &known,
            ..CfgOptions::default()
        },
        ..Default::default()
    };

    let (mut strider, cc) = common::strider_over_bytes(common::Arch::X64, bytes, BASE, None);

    // The build the withdrawal is decided from still seats the arm that decoded.
    let seeded = strider.build_cfg(BASE, &opts.cfg).expect("build_cfg");
    assert!(
        seeded
            .regions()
            .any(|r| matches!(r.terminator, RegionTerminator::Switch { .. })),
        "precondition: one bad arm must leave the rest of the seat standing, \
         or there is nothing for a later round to republish",
    );

    let result = strider
        .analyze(BASE, &cc, &opts, None)
        .expect("an undecodable seeded arm is a result, not an error");

    assert!(
        !result
            .cfg
            .regions()
            .any(|r| matches!(r.terminator, RegionTerminator::Switch { .. })),
        "the published CFG must carry no seat for a withdrawn site",
    );
    assert!(
        result
            .incomplete()
            .of(strider_orchestrator::Reason::UnresolvedBranch)
            .any(|a| a.machine_addr.addr == SITE),
        "the withdrawn site is reported unresolved; got {}",
        result.incomplete(),
    );
    // The complement, which an emptiness check cannot see: the unresolved
    // entry alone already makes the set non-empty, so only this pins that a
    // withdrawn site holding no arms never also claims its answer is whole.
    assert!(
        !result
            .incomplete()
            .of(strider_orchestrator::Reason::UnverifiedSeed)
            .any(|a| a.machine_addr.addr == SITE),
        "a withdrawn site holds no arms to be unverified; got {}",
        result.incomplete(),
    );
    assert!(!result.incomplete().is_empty());
}
