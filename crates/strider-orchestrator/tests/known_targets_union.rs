//! `analyze` re-unions a caller's `known_targets` into the working set every
//! round, so the FOLD never drops what the classifier proves. Seating does: a
//! seed changes the CFG the classifier reads, and a wrong one can stop the
//! selector deriving and take the site's real arms with it, which
//! `unverified_seeded_sites` is the report for.
//!
//! Seating is also all-or-nothing: the seed asserts the site's successor set
//! is complete, so one member the cfg cannot seat leaves the whole site
//! unseated and reported unresolved.

mod common;

use object::{Object, ObjectSymbol};
use rustc_hash::FxHashMap;
use strider_cfg::{PcodeInsnAddr, ResolvedTarget, ResolvedTargets};

/// Runs `analyze` on `x86/switch.elf::dispatch_value` with `seed` seated at the
/// dispatch site, returning the switch arm addresses it produced.
fn arms_with(
    seed: Option<Vec<u64>>,
) -> (Vec<u64>, Vec<PcodeInsnAddr>, Vec<u64>, Vec<PcodeInsnAddr>) {
    let path = common::binary_path(common::Arch::X86, "switch");
    let owned = strider_reader::load_elf(&path).expect("load_elf");
    let obj = owned.file();
    let sa = common::Arch::X86.sleigh();
    let mem = strider_reader::ElfFileMemReader::from_object(&obj).expect("mem");
    let sleigh = rsleigh::Sleigh::new(sa.sla_spec(), sa.pspec(), mem).expect("sleigh");
    let addr = obj
        .symbol_by_name("dispatch_value")
        .expect("symbol")
        .address();
    let rom: Box<dyn strider_orchestrator::opt::ReadOnlyMemory> =
        Box::new(strider_reader::ElfFileMemReader::from_object(&obj).expect("rom"));
    let regs = sleigh.regs().expect("regs");
    let cc = common::Arch::X86.cc().build(&regs).expect("cc");

    let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
    if let Some(targets) = seed {
        // The dispatch is not the first p-code op of its instruction, so the
        // machine-address key is what a caller can spell.
        known.insert(
            PcodeInsnAddr::at_machine_start(DISPATCH),
            ResolvedTargets::Multiple(
                targets
                    .into_iter()
                    .map(|t| ResolvedTarget::new(t, None))
                    .collect(),
            ),
        );
    }
    let opts = strider_orchestrator::LiftOptions {
        cfg: strider_cfg::CfgOptions {
            known_targets: known,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut strider = strider_orchestrator::Strider::new(sa, sleigh, Some(rom)).expect("new");
    let r = strider
        .analyze(
            addr,
            &cc,
            &opts,
            &strider_orchestrator::opt::OptOptions::default(),
            None,
        )
        .expect("analyze");
    let mut arms = vec![];
    let mut starts = vec![];
    for n in r.cfg.region_graph().node_indices() {
        let region = r.cfg.region_graph().node_weight(n).unwrap();
        starts.push(region.start_addr.machine_addr.addr);
        if let strider_cfg::RegionTerminator::Switch { targets, .. } = &region.terminator {
            arms.extend(targets.iter().map(|t| t.addr));
        }
    }
    arms.sort_unstable();
    arms.dedup();
    starts.sort_unstable();
    starts.dedup();
    (
        arms,
        r.unresolved_indirect_branches,
        starts,
        r.unverified_seeded_sites,
    )
}

/// `jmp *0x4000ec(,%ecx,4)` at 0x40113e.
const DISPATCH: u64 = 0x40113E;
/// Outside the function, so the CFG refuses to seat the set containing it.
const OUT_OF_RANGE: u64 = 0x400000;

#[test]
fn a_seed_the_cfg_declines_is_reported_unresolved_not_partially_seated() {
    let (baseline, _, starts, _) = arms_with(None);
    // An in-range instruction boundary the classifier does NOT find on its own,
    // so the seed's contribution is observable.
    let extra = starts
        .iter()
        .copied()
        .find(|a| !baseline.contains(a))
        .expect("a region start that is not already an arm");

    let (good_seed, _, _, _) = arms_with(Some(vec![extra]));
    assert!(
        good_seed.contains(&extra),
        "a seatable seed must add {extra:#x}; arms {good_seed:#x?}",
    );

    // A seed asserts the site is COMPLETE, so one unseatable member invalidates
    // the whole answer: seating the rest would present an arm set missing that
    // member as complete. The site must come back unresolved instead, and no
    // subset of the seed may be seated.
    let (mixed, unresolved, _, _) = arms_with(Some(vec![extra, OUT_OF_RANGE]));
    assert_eq!(
        unresolved
            .iter()
            .map(|a| a.machine_addr.addr)
            .collect::<Vec<_>>(),
        vec![DISPATCH],
        "the unseatable seed member {OUT_OF_RANGE:#x} vanished silently: the \
         site was reported fully resolved, or a different site was named",
    );
    assert!(
        !mixed.contains(&extra),
        "a declined seed was partially seated: {extra:#x} is presented as a \
         complete arm set without {OUT_OF_RANGE:#x}; arms {mixed:#x?}",
    );
}

/// Seating a seed changes the CFG the classifier reads, so a stale or wrong
/// seed can stop the selector deriving and take the site's real arms with it.
/// The site is not "unresolved" -- the caller asserted the answer -- but
/// nothing verified it, so it must be named in `unverified_seeded_sites`.
/// Without that channel the loss is completely silent.
#[test]
fn a_site_seated_only_from_a_seed_is_named_as_unverified() {
    // x64 `main`'s dispatch, seeded with only itself. Seating that seed
    // changes the CFG the classifier reads: the selector stops deriving, the
    // site converges holding just the seed, and every real arm is gone.
    const X64_MAIN_DISPATCH: u64 = 0x401042;
    let (arms, unresolved, unverified) = x64_main_with_seed(vec![X64_MAIN_DISPATCH]);
    assert_eq!(
        arms,
        vec![X64_MAIN_DISPATCH],
        "the site must hold exactly the seed and nothing the classifier derived",
    );
    assert!(
        unresolved.is_empty(),
        "the caller asserted the answer, so the site is not unresolved",
    );
    assert_eq!(
        unverified
            .iter()
            .map(|a| a.machine_addr.addr)
            .collect::<Vec<_>>(),
        vec![X64_MAIN_DISPATCH],
        "the report must name the site whose arms the seed cost",
    );
}

/// `analyze` of x64 `main` with `known_targets` seeded at `seed[0]`'s site,
/// returning its switch arms, the unresolved set and the unverified-seed set.
fn x64_main_with_seed(seed: Vec<u64>) -> (Vec<u64>, Vec<PcodeInsnAddr>, Vec<PcodeInsnAddr>) {
    let path = common::binary_path(common::Arch::X64, "switch");
    let owned = strider_reader::load_elf(&path).expect("load_elf");
    let obj = owned.file();
    let sa = common::Arch::X64.sleigh();
    let mem = strider_reader::ElfFileMemReader::from_object(&obj).expect("mem");
    let sleigh = rsleigh::Sleigh::new(sa.sla_spec(), sa.pspec(), mem).expect("sleigh");
    let addr = obj.symbol_by_name("main").expect("symbol").address();
    let rom: Box<dyn strider_orchestrator::opt::ReadOnlyMemory> =
        Box::new(strider_reader::ElfFileMemReader::from_object(&obj).expect("rom"));
    let regs = sleigh.regs().expect("regs");
    let cc = common::Arch::X64.cc().build(&regs).expect("cc");

    let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
    known.insert(
        PcodeInsnAddr::at_machine_start(seed[0]),
        ResolvedTargets::Multiple(seed.iter().map(|&t| ResolvedTarget::new(t, None)).collect()),
    );
    let opts = strider_orchestrator::LiftOptions {
        cfg: strider_cfg::CfgOptions {
            known_targets: known,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut strider = strider_orchestrator::Strider::new(sa, sleigh, Some(rom)).expect("new");
    let r = strider
        .analyze(
            addr,
            &cc,
            &opts,
            &strider_orchestrator::opt::OptOptions::default(),
            None,
        )
        .expect("analyze");
    let mut arms = vec![];
    for n in r.cfg.region_graph().node_indices() {
        let region = r.cfg.region_graph().node_weight(n).unwrap();
        if let strider_cfg::RegionTerminator::Switch { targets, .. } = &region.terminator {
            arms.extend(targets.iter().map(|t| t.addr));
        }
    }
    arms.sort_unstable();
    arms.dedup();
    (
        arms,
        r.unresolved_indirect_branches,
        r.unverified_seeded_sites,
    )
}

/// With resolution off nothing derives, so a seated site's arms are exactly the
/// caller's answer and nothing verified them. That is what
/// `unverified_seeded_sites` reports, and turning the classifier off must not
/// silence it: the CFG then presents a fabricated successor set as complete.
#[test]
fn a_seeded_site_is_still_reported_unverified_with_resolution_off() {
    let path = common::binary_path(common::Arch::X86, "switch");
    let owned = strider_reader::load_elf(&path).expect("load_elf");
    let obj = owned.file();
    let sa = common::Arch::X86.sleigh();
    let mem = strider_reader::ElfFileMemReader::from_object(&obj).expect("mem");
    let sleigh = rsleigh::Sleigh::new(sa.sla_spec(), sa.pspec(), mem).expect("sleigh");
    let addr = obj
        .symbol_by_name("dispatch_value")
        .expect("symbol")
        .address();
    let rom: Box<dyn strider_orchestrator::opt::ReadOnlyMemory> =
        Box::new(strider_reader::ElfFileMemReader::from_object(&obj).expect("rom"));
    let regs = sleigh.regs().expect("regs");
    let cc = common::Arch::X86.cc().build(&regs).expect("cc");

    let mut known: FxHashMap<PcodeInsnAddr, ResolvedTargets> = FxHashMap::default();
    known.insert(
        PcodeInsnAddr::at_machine_start(DISPATCH),
        ResolvedTargets::Multiple(vec![ResolvedTarget::new(addr, None)]),
    );
    let opts = strider_orchestrator::LiftOptions {
        cfg: strider_cfg::CfgOptions {
            known_targets: known,
            ..Default::default()
        },
        ..Default::default()
    };
    let opt_opts = strider_orchestrator::opt::OptOptions {
        resolve_indirect_branches: false,
        ..Default::default()
    };
    let mut strider = strider_orchestrator::Strider::new(sa, sleigh, Some(rom)).expect("new");
    let r = strider
        .analyze(addr, &cc, &opts, &opt_opts, None)
        .expect("analyze");

    assert_eq!(
        r.unverified_seeded_sites
            .iter()
            .map(|a| a.machine_addr.addr)
            .collect::<Vec<_>>(),
        vec![DISPATCH],
        "the seed is the only answer in the CFG and nothing checked it, so the \
         dispatch must be named",
    );
}
