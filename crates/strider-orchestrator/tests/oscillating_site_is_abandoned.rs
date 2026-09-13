//! A site whose answer never settles is abandoned and reported, not an error.
//!
//! One narrowing is a refinement; a second means the answer depends on what
//! the previous round seated, so the resolve loop stops resolving the site.
//! That counter is the loop's only oscillation guard: without it an unstable
//! site burns the whole iteration budget and comes back as `still_growing`.
//!
//! Oscillation needs a classifier that contradicts itself, which no real
//! binary is known to produce here, so the pass that answers is a double. It
//! takes [`strider_opt::IndirectBranchClassify`]'s name, which is what keeps
//! `Strider::analyze` from appending the real one alongside it.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use strider_ir::node::NodeKind;
use strider_ir::{IRViewer, IRWalker};
use strider_orchestrator::LiftOptions;
use strider_orchestrator::opt::{
    EditFunction, OptCtx, OptOptions, PostOptimizer, default_pipeline,
};

mod common;

const BASE: u64 = 0x1000;
/// `jmp rax`, with both arms a `ret` inside the same buffer.
const SITE: u64 = 0x1000;
const STABLE_ARM: u64 = 0x1010;
const FLAPPING_ARM: u64 = 0x1020;

/// Answers `{STABLE_ARM, FLAPPING_ARM}` on even rounds and `{STABLE_ARM}` on
/// odd ones, so every second round loses an address.
#[derive(Clone)]
struct FlappingClassify {
    round: Arc<AtomicUsize>,
}

impl PostOptimizer for FlappingClassify {
    fn apply(&self, edit: &mut EditFunction<'_>, ctx: &mut OptCtx<'_>) -> anyhow::Result<()> {
        let targets = if self.round.fetch_add(1, Ordering::Relaxed).is_multiple_of(2) {
            strider_cfg::ResolvedTargets::Multiple(vec![
                strider_cfg::ResolvedTarget::new(STABLE_ARM, None),
                strider_cfg::ResolvedTarget::new(FLAPPING_ARM, None),
            ])
        } else {
            strider_cfg::ResolvedTargets::Single(strider_cfg::ResolvedTarget::new(STABLE_ARM, None))
        };
        // Both kinds, the way the real classifier reports: the site is a
        // placeholder until a round seats it and a `Switch` after.
        let sites: Vec<_> = edit
            .function()
            .walk()
            .filter(|&n| {
                matches!(
                    edit.function().node_kind(n),
                    NodeKind::IndirectBranch | NodeKind::Switch
                )
            })
            .collect();
        for node in sites {
            ctx.indirect_resolutions.insert(node, Some(targets.clone()));
        }
        Ok(())
    }

    fn name(&self) -> &'static str {
        PostOptimizer::name(&strider_opt::IndirectBranchClassify)
    }
}

#[test]
fn a_site_that_narrows_twice_is_abandoned_and_reported() {
    let mut bytes = vec![0xc3u8; 0x40];
    bytes[0] = 0xff; // 0x1000: jmp rax
    bytes[1] = 0xe0;
    let (mut strider, cc) = common::strider_over_bytes(common::Arch::X64, bytes, BASE, None);

    let round = Arc::new(AtomicUsize::new(0));
    let mut pipeline = default_pipeline();
    pipeline.add_post_pass(FlappingClassify {
        round: Arc::clone(&round),
    });

    let result = strider
        .analyze(
            BASE,
            &cc,
            &LiftOptions::default(),
            &OptOptions::default(),
            Some(pipeline),
        )
        .expect("an unstable site is a result, not an error");

    assert!(
        result
            .unresolved_indirect_branches
            .iter()
            .any(|a| a.machine_addr.addr == SITE),
        "the abandoned site is reported unresolved; got {:?}",
        result.unresolved_indirect_branches,
    );
    assert!(
        !result
            .cfg
            .regions()
            .any(|r| matches!(r.terminator, strider_cfg::RegionTerminator::Switch { .. })),
        "an abandoned site keeps no seat in the published CFG",
    );
    // Four rounds reach the second narrowing; one more sees the site skipped
    // and converges. Anything near the 256-round cap means the counter stopped
    // gating and the loop ran on the budget instead.
    assert!(
        round.load(Ordering::Relaxed) < 10,
        "the loop must stop on the second narrowing, took {} rounds",
        round.load(Ordering::Relaxed),
    );
}
