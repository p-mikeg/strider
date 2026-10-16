//! One resolve round seats one LEVEL of discovery, so a chain of trampolines
//! needs one round per link and the iteration cap is the depth limit.
//!
//! Pins both halves of that: a chain within the cap converges on every link,
//! and one past it comes back as a report rather than an error. Each test also
//! pins how many rounds `analyze` built to get there.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use strider_orchestrator::opt::{EditFunction, OptCtx, PostOptimizer, default_pipeline};

mod common;

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

/// The six report channels, so a failure says which one fired.
fn caveats(result: &strider_orchestrator::AnalyzeResult) -> String {
    format!(
        "unresolved {:?}, unverified {:?}, isa {:?}, interior {:?}, unmapped {:?}, undecodable {:?}",
        result
            .incomplete()
            .of(strider_orchestrator::Reason::UnresolvedBranch)
            .collect::<Vec<_>>(),
        result
            .incomplete()
            .of(strider_orchestrator::Reason::UnverifiedSeed)
            .collect::<Vec<_>>(),
        result
            .incomplete()
            .of(strider_orchestrator::Reason::IsaModeConflict)
            .collect::<Vec<_>>(),
        result
            .incomplete()
            .of(strider_orchestrator::Reason::InteriorTarget)
            .collect::<Vec<_>>(),
        result
            .incomplete()
            .of(strider_orchestrator::Reason::UnmappedTarget)
            .collect::<Vec<_>>(),
        result
            .incomplete()
            .of(strider_orchestrator::Reason::UndecodableTarget)
            .collect::<Vec<_>>(),
    )
}

/// Counts pipeline runs: one per round whose pipeline converges.
#[derive(Clone)]
struct CountRounds(Arc<AtomicUsize>);

impl PostOptimizer for CountRounds {
    fn apply(&self, _edit: &mut EditFunction<'_>, _ctx: &mut OptCtx<'_>) -> anyhow::Result<()> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

/// `analyze` over `bytes` at `BASE`, and the rounds it built.
fn analyze(bytes: Vec<u8>) -> (strider_orchestrator::AnalyzeResult, usize) {
    let (mut strider, cc) = common::strider_over_bytes(common::Arch::X64, bytes, BASE, None);
    let rounds = Arc::new(AtomicUsize::new(0));
    let mut pipeline = default_pipeline();
    pipeline.add_post_pass(CountRounds(Arc::clone(&rounds)));
    let result = strider
        .analyze(BASE, &cc, &common::ignoring(), Some(pipeline))
        .expect("a chain deeper than the cap is a result, not an error");
    (result, rounds.load(Ordering::Relaxed))
}

/// Nothing to fold: the first round is the answer.
#[test]
fn a_function_with_no_dispatch_is_built_once() {
    let (result, rounds) = analyze(chain(0));
    assert!(result.is_complete(), "{}", caveats(&result));
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
    let unverified: Vec<u64> = result
        .incomplete()
        .of(strider_orchestrator::Reason::UnverifiedSeed)
        .collect::<Vec<_>>()
        .iter()
        .map(|site| site.machine_addr.addr)
        .collect();
    assert_eq!(unverified, [BASE + 5], "{}", caveats(&result));
    assert!(
        result
            .incomplete()
            .of(strider_orchestrator::Reason::UnresolvedBranch)
            .collect::<Vec<_>>()
            .is_empty()
            && result
                .incomplete()
                .of(strider_orchestrator::Reason::IsaModeConflict)
                .collect::<Vec<_>>()
                .is_empty()
            && result
                .incomplete()
                .of(strider_orchestrator::Reason::InteriorTarget)
                .collect::<Vec<_>>()
                .is_empty()
            && result
                .incomplete()
                .of(strider_orchestrator::Reason::UnmappedTarget)
                .collect::<Vec<_>>()
                .is_empty()
            && result
                .incomplete()
                .of(strider_orchestrator::Reason::UndecodableTarget)
                .collect::<Vec<_>>()
                .is_empty(),
        "{}",
        caveats(&result),
    );
    assert!(
        result
            .cfg
            .regions()
            .any(|r| matches!(r.terminator, strider_cfg::RegionTerminator::TailCall { .. })),
        "{}",
        caveats(&result),
    );
    assert_eq!(rounds, 2);
}

#[test]
fn every_link_of_a_chain_within_the_cap_is_seated() {
    let links = 8;
    let (result, rounds) = analyze(chain(links));
    assert!(result.is_complete(), "{}", caveats(&result));
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
    assert!(fits.is_complete(), "{}", caveats(&fits));
    assert_eq!(fits_rounds, 257);

    let (over, over_rounds) = analyze(chain(257));
    assert!(!over.is_complete());
    assert_eq!(
        over.incomplete()
            .of(strider_orchestrator::Reason::UnresolvedBranch)
            .collect::<Vec<_>>()
            .iter()
            .map(|a| a.machine_addr.addr)
            .collect::<Vec<_>>(),
        vec![BASE + 256 * LINK + 5],
        "the frontier `jmp rax` is the one still growing when the cap ran out",
    );
    assert_eq!(over_rounds, 257);
}
