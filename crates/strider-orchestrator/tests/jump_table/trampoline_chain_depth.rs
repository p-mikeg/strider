//! One resolve round seats one LEVEL of discovery, so a chain of trampolines
//! needs one round per link and the iteration cap is the depth limit.
//!
//! Pins both halves of that: a chain within the cap converges on every link,
//! and one past it comes back reported rather than failed. Each test also
//! pins how many rounds `analyze` built to get there.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use strider_orchestrator::opt::{EditFunction, OptCtx, Optimizer, default_pipeline};

use crate::common;

const BASE: u64 = 0x40_0000;
/// `mov eax, <next>` (5) + `jmp rax` (2).
const LINK: u64 = 7;

/// `links` trampolines, each jumping to the next, ending on a `ret`.
fn chain(links: u64) -> Vec<u8> {
    let mut bytes = Vec::with_capacity((links * LINK + 1) as usize);
    for i in 0..links {
        let next = u32::try_from(BASE + (i + 1) * LINK).expect("32-bit immediate");
        bytes.push(0xb8);
        bytes.extend_from_slice(&next.to_le_bytes());
        bytes.extend_from_slice(&[0xff, 0xe0]);
    }
    bytes.push(0xc3);
    bytes
}

/// Counts pipeline runs: one per round whose pipeline converges.
#[derive(Clone)]
struct CountRounds(Arc<AtomicUsize>);

impl Optimizer for CountRounds {
    fn apply(&self, _edit: &mut EditFunction<'_>, _ctx: &OptCtx<'_>) -> anyhow::Result<()> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

/// `analyze` over `bytes` at `BASE`, and the rounds it built.
fn analyze(bytes: Vec<u8>) -> (strider_orchestrator::AnalyzeResult, usize) {
    let (mut strider, cc) = common::strider_over_bytes(common::Arch::X64, bytes, BASE, None);
    let rounds = Arc::new(AtomicUsize::new(0));
    let mut pipeline = default_pipeline();
    // A pass is held `&'static`; one counter per analysis leaks one box.
    pipeline.add_post_pass(Box::leak(Box::new(CountRounds(Arc::clone(&rounds)))));
    let result = strider
        .analyze(
            BASE,
            &cc,
            &strider_orchestrator::AnalyzeOptions::default(),
            Some(pipeline),
        )
        .expect("a chain deeper than the cap is a result, not an error");
    (result, rounds.load(Ordering::Relaxed))
}

/// Nothing to fold: the first round is the answer.
#[test]
fn a_function_with_no_dispatch_is_built_once() {
    let (result, rounds) = analyze(chain(0));
    assert!(result.incomplete().is_empty(), "{}", result.incomplete());
    assert_eq!(rounds, 1);
}

/// `mov eax, <below the entry> ; jmp rax`: the first fold seats a tail call,
/// which consumes the site, so the next round has nothing left to fold.
#[test]
fn a_site_its_first_fold_consumes_settles_on_the_next_round() {
    let below = u32::try_from(BASE - 0x100).expect("32-bit immediate");
    let mut bytes = vec![0xb8];
    bytes.extend_from_slice(&below.to_le_bytes());
    bytes.extend_from_slice(&[0xff, 0xe0]);
    let (result, rounds) = analyze(bytes);
    // A tail call consumed at cfg build leaves no anchor to verify, so the
    // site is reported unverified, and nothing else is.
    assert_eq!(
        result.incomplete().iter().collect::<Vec<_>>(),
        [(
            strider_cfg::PcodeInsnAddr::at_machine_start(BASE + 5),
            strider_orchestrator::Reason::UnverifiedSeed,
        )],
    );
    assert!(
        result
            .cfg
            .regions()
            .any(|r| matches!(r.terminator, strider_cfg::RegionTerminator::TailCall { .. })),
        "{}",
        result.incomplete(),
    );
    assert_eq!(rounds, 2);
}

#[test]
fn every_link_of_a_chain_within_the_cap_is_seated() {
    let links = 8;
    let (result, rounds) = analyze(chain(links));
    assert!(result.incomplete().is_empty(), "{}", result.incomplete());
    for i in 0..=links {
        assert!(
            result
                .cfg
                .regions()
                .any(|r| r.start_addr.machine_addr.addr == BASE + i * LINK),
            "link {i} never got a region",
        );
    }
    assert_eq!(
        rounds, 9,
        "one round per link, and one whose fold changes nothing"
    );
}

/// The cap is 256 seated levels: the last chain that fits converges, and the
/// first that does not reports its frontier instead of failing the function.
/// Both spend the whole cap: 256 folds, so 257 builds.
#[test]
fn the_iteration_cap_is_the_chain_depth_it_seats() {
    let (fits, fits_rounds) = analyze(chain(256));
    assert!(fits.incomplete().is_empty(), "{}", fits.incomplete());
    assert_eq!(fits_rounds, 257);

    let (over, over_rounds) = analyze(chain(257));
    assert!(!over.incomplete().is_empty());
    assert_eq!(
        over.incomplete()
            .of(strider_orchestrator::Reason::UnresolvedBranch)
            .map(|a| a.machine_addr.addr)
            .collect::<Vec<_>>(),
        vec![BASE + 256 * LINK + 5],
        "the unfolded frontier `jmp rax` stays a live placeholder",
    );
    assert_eq!(over_rounds, 257);
}

/// A seat the cap round refuses is not published: the loop builds once more
/// from the frozen seats instead of returning the stale cfg.
#[test]
fn a_seat_refused_on_the_cap_round_is_not_published() {
    use strider_cfg::{RegionTerminator, ResolvedTarget, ResolvedTargets};
    use strider_orchestrator::opt::{ClassifiedSites, ReadOnlyMemory, classify_sites};

    const UNMAPPED: u64 = 0x4000_1000;
    let last = BASE + 256 * LINK;
    let calls = std::cell::Cell::new(0usize);
    // The 256th classification, the last one folded, also names an arm the
    // cap round cannot decode.
    let classify = |f: &strider_ir::Function,
                    rom: Option<&dyn ReadOnlyMemory>|
     -> anyhow::Result<ClassifiedSites> {
        let mut found = classify_sites(f, rom)?;
        calls.set(calls.get() + 1);
        if calls.get() == 256 {
            for targets in found.resolved.values_mut() {
                if targets.arms().iter().any(|t| t.addr == last) {
                    *targets = ResolvedTargets::table(vec![
                        ResolvedTarget::new(last, None),
                        ResolvedTarget::new(UNMAPPED, None),
                    ]);
                }
            }
        }
        Ok(found)
    };
    let (mut strider, cc) = common::strider_over_bytes(common::Arch::X64, chain(256), BASE, None);
    let result = strider
        .analyze_with_classifier(
            BASE,
            &cc,
            &strider_orchestrator::AnalyzeOptions::default(),
            None,
            &classify,
        )
        .expect("a refused seat is a result, not an error");
    assert!(calls.get() >= 257, "precondition: the cap was reached");
    let frontier = last - 2;
    assert!(
        !result.cfg.regions().any(|r| matches!(
            r.terminator,
            RegionTerminator::Switch { addr, .. } if addr.machine_addr.addr == frontier
        )),
        "the cap round's refused seat must not be published",
    );
    assert!(
        result
            .incomplete()
            .of(strider_orchestrator::Reason::UnresolvedBranch)
            .any(|a| a.machine_addr.addr == frontier),
        "{}",
        result.incomplete(),
    );
}

/// A seated table that gains an arm every round is still growing when the cap
/// runs out: classified and seated, so only the cap reports it.
#[test]
fn a_table_still_growing_at_the_cap_is_reported() {
    use strider_cfg::{RegionTerminator, ResolvedTarget, ResolvedTargets};
    use strider_orchestrator::opt::{ClassifiedSites, ReadOnlyMemory, classify_sites};

    // `jmp rax`, then a `ret` at every arm.
    let mut bytes = vec![0xc3u8; 0x200];
    bytes[..2].copy_from_slice(&[0xff, 0xe0]);
    let calls = std::cell::Cell::new(0u64);
    let classify = |f: &strider_ir::Function,
                    rom: Option<&dyn ReadOnlyMemory>|
     -> anyhow::Result<ClassifiedSites> {
        let mut found = classify_sites(f, rom)?;
        calls.set(calls.get() + 1);
        let arms: Vec<_> = (0..calls.get())
            .map(|k| ResolvedTarget::new(BASE + 2 + k, None))
            .collect();
        let targets = ResolvedTargets::table(arms);
        found.resolved = found
            .sites
            .iter()
            .map(|&site| (site, targets.clone()))
            .collect();
        Ok(found)
    };
    let (mut strider, cc) = common::strider_over_bytes(common::Arch::X64, bytes, BASE, None);
    let result = strider
        .analyze_with_classifier(
            BASE,
            &cc,
            &strider_orchestrator::AnalyzeOptions::default(),
            None,
            &classify,
        )
        .expect("a site growing at the cap is a result, not an error");
    assert_eq!(calls.get(), 257, "precondition: the cap was reached");
    assert!(
        result.cfg.regions().any(|r| matches!(
            r.terminator,
            RegionTerminator::Switch { addr, .. } if addr.machine_addr.addr == BASE
        )),
        "precondition: the site is a seated switch, not a placeholder",
    );
    assert_eq!(
        result
            .incomplete()
            .iter()
            .map(|(a, r)| (a.machine_addr.addr, r))
            .collect::<Vec<_>>(),
        vec![(BASE, strider_orchestrator::Reason::UnresolvedBranch)],
    );
}
