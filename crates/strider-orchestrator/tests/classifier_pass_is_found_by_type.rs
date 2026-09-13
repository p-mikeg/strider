//! `analyze` appends the indirect-branch classifier unless the pipeline already
//! runs it, and reports every live placeholder. A caller's post-pass that only
//! shares the classifier's name is not the classifier, and one that rewrites
//! the classification map cannot hide a live placeholder.
mod common;

use strider_orchestrator::LiftOptions;
use strider_orchestrator::opt::{EditFunction, OptCtx, OptOptions, PostOptimizer};

mod user {
    /// A caller's own post-pass that happens to be called
    /// `IndirectBranchClassify`.
    #[derive(Clone)]
    pub struct IndirectBranchClassify;

    impl strider_orchestrator::opt::PostOptimizer for IndirectBranchClassify {
        fn apply(
            &self,
            _edit: &mut strider_orchestrator::opt::EditFunction<'_>,
            _ctx: &mut strider_orchestrator::opt::OptCtx<'_>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
    }
}

/// Runs after the real classifier and drops everything it reported.
#[derive(Clone)]
struct ClearResolutions;

impl PostOptimizer for ClearResolutions {
    fn apply(&self, _edit: &mut EditFunction<'_>, ctx: &mut OptCtx<'_>) -> anyhow::Result<()> {
        ctx.indirect_resolutions.clear();
        Ok(())
    }
}

/// `jmp rax` at 0x1000, which nothing resolves on x86-64.
fn analyze_jmp_rax(pipeline: strider_orchestrator::opt::OptimizerPipeline) -> bool {
    let mut bytes = vec![0xff, 0xe0u8];
    bytes.extend(std::iter::repeat_n(0xccu8, 16));
    let (mut s, cc) = common::strider_over_bytes(common::Arch::X64, bytes, 0x1000, None);
    let r = s
        .analyze(
            0x1000,
            &cc,
            &LiftOptions::default(),
            &OptOptions::default(),
            Some(pipeline),
        )
        .expect("analyze");
    r.unresolved_indirect_branches
        .iter()
        .any(|a| a.machine_addr.addr == 0x1000)
}

#[test]
fn a_same_named_post_pass_does_not_silence_the_unresolved_report() {
    let mut pipeline = strider_orchestrator::opt::default_pipeline();
    pipeline.add_post_pass(user::IndirectBranchClassify);
    assert_eq!(
        pipeline.post_passes().last().map(|p| p.name()),
        Some(strider_orchestrator::opt::IndirectBranchClassify.name())
    );
    assert!(
        analyze_jmp_rax(pipeline),
        "a live `jmp rax` placeholder is reported"
    );
}

#[test]
fn a_live_placeholder_is_reported_whatever_the_classification_map_says() {
    let mut pipeline = strider_orchestrator::opt::default_pipeline();
    pipeline.add_post_pass(strider_orchestrator::opt::IndirectBranchClassify);
    pipeline.add_post_pass(ClearResolutions);
    assert!(
        analyze_jmp_rax(pipeline),
        "a live `jmp rax` placeholder is reported"
    );
}
