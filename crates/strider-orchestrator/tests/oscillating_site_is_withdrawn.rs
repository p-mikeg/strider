//! A site whose answer never settles is withdrawn and reported, not a failure.
//!
//! Every narrowing is reported; a second means the answer depends on what the
//! previous round seated, so the resolve loop stops resolving the site.
//! That counter is the loop's only oscillation guard: without it an unstable
//! site burns the whole iteration budget and comes back unresolved.
//!
//! Oscillation needs a classifier that contradicts itself, which no real
//! binary is known to produce here, so a stand-in answers through
//! `Strider::analyze_with_classifier`: it lists the real sites and overwrites
//! every answer.

use std::cell::Cell;

use strider_cfg::{CfgOptions, PcodeInsnAddr, RegionTerminator, ResolvedTarget, ResolvedTargets};
use strider_orchestrator::opt::{ClassifiedSites, ReadOnlyMemory, classify_sites};
use strider_orchestrator::{AnalyzeOptions, AnalyzeResult, Reason};

mod common;

const BASE: u64 = 0x1000;
const STABLE_ARM: u64 = 0x1010;
const FLAPPING_ARM: u64 = 0x1020;

/// Analyses `bytes` under a classifier answering `{STABLE_ARM, FLAPPING_ARM}`
/// on even rounds and `{STABLE_ARM}` on odd ones, so every second round loses
/// an address. Returns the result and the rounds classified.
fn analyze_flapping(bytes: Vec<u8>, opts: &AnalyzeOptions<'_>) -> (AnalyzeResult, usize) {
    let round = Cell::new(0usize);
    let flapping = |f: &strider_ir::Function,
                    rom: Option<&dyn ReadOnlyMemory>|
     -> anyhow::Result<ClassifiedSites> {
        let mut found = classify_sites(f, rom)?;
        let n = round.get();
        round.set(n + 1);
        let targets = if n.is_multiple_of(2) {
            ResolvedTargets::table(vec![
                ResolvedTarget::new(STABLE_ARM, None),
                ResolvedTarget::new(FLAPPING_ARM, None),
            ])
        } else {
            ResolvedTargets::Single(ResolvedTarget::new(STABLE_ARM, None))
        };
        found.resolved = found
            .sites
            .iter()
            .map(|&site| (site, targets.clone()))
            .collect();
        Ok(found)
    };
    let (mut strider, cc) = common::strider_over_bytes(common::Arch::X64, bytes, BASE, None);
    let result = strider
        .analyze_with_classifier(BASE, &cc, opts, None, &flapping)
        .expect("an unstable site is a result, not an error");
    (result, round.get())
}

fn seats_a_switch(result: &AnalyzeResult) -> bool {
    result
        .cfg
        .regions()
        .any(|r| matches!(r.terminator, RegionTerminator::Switch { .. }))
}

#[test]
fn a_site_that_narrows_twice_is_withdrawn_and_reported() {
    let mut bytes = vec![0xc3u8; 0x40];
    bytes[..2].copy_from_slice(&[0xff, 0xe0]); // 0x1000: jmp rax
    let (result, rounds) = analyze_flapping(bytes, &common::ignoring());

    assert!(
        result
            .incomplete()
            .of(Reason::UnresolvedBranch)
            .any(|a| a.machine_addr.addr == BASE),
        "the withdrawn site is reported unresolved; got {}",
        result.incomplete(),
    );
    assert!(
        !seats_a_switch(&result),
        "a withdrawn site keeps no seat in the published CFG",
    );
    // Four rounds reach the second narrowing; one more sees the site skipped
    // and converges. Anything near the 256-round cap means the counter stopped
    // gating and the loop ran on the budget instead.
    assert!(
        rounds < 10,
        "the loop must stop on the second narrowing, took {rounds} rounds"
    );
}

/// The withdrawn seat outranks the caller's seed under the instruction's
/// machine start, so a seeded site that narrows twice defers as well.
/// Mutation: in `CfgOptions::seated`, read `known_targets` before
/// `derived_targets`.
#[test]
fn a_seeded_site_that_narrows_twice_is_withdrawn_over_its_seed() {
    let mut bytes = vec![0xc3u8; 0x40];
    bytes[..2].copy_from_slice(&[0xff, 0x20]); // 0x1000: jmp [rax]
    let known: rustc_hash::FxHashMap<_, _> = std::iter::once((
        PcodeInsnAddr::at_machine_start(BASE),
        ResolvedTargets::table(vec![ResolvedTarget::new(STABLE_ARM, None)]),
    ))
    .collect();
    let opts = AnalyzeOptions {
        cfg: CfgOptions {
            known_targets: &known,
            ..CfgOptions::default()
        },
        ..common::ignoring()
    };
    let (result, rounds) = analyze_flapping(bytes, &opts);

    let site = result
        .cfg
        .regions()
        .find_map(|r| match &r.terminator {
            RegionTerminator::UnresolvedIndirectBranch { addr, .. } => Some(*addr),
            _ => None,
        })
        .expect("the withdrawn site defers");
    assert_ne!(
        site.insn_index, 0,
        "precondition: the seed sits under the machine start, not the anchor"
    );
    assert!(
        !seats_a_switch(&result),
        "the caller's seed must not re-seat it"
    );
    assert_eq!(
        result.incomplete().iter().collect::<Vec<_>>(),
        vec![(site, Reason::UnresolvedBranch)],
    );
    assert!(rounds < 10, "took {rounds} rounds");
}
