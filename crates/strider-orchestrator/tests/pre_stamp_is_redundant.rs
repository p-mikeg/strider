//! The stamp `run` ends with fills every memory class, so a `StackOffsetDetect`
//! ahead of the other post-passes changes none of them.

mod common;

use common::fixtures::{Fixture, fixture_arch};
use strider_ir::node::{NodeId, NodeKind, ValueId};
use strider_ir::{IRViewer, IRWalker, MemDecomp};
use strider_orchestrator::opt::{OptimizerPipeline, StackOffsetDetect, default_pipeline};
use strider_orchestrator::{AnalyzeOptions, OnIncomplete};

/// `default_pipeline()`, with `StackOffsetDetect` its first post-pass when
/// `pre_stamp`.
fn pipeline(pre_stamp: bool) -> OptimizerPipeline {
    let base = default_pipeline();
    let mut p = OptimizerPipeline::new();
    for pass in base.passes() {
        p.add_boxed(pass.clone_box());
    }
    if pre_stamp {
        p.add_post_pass(StackOffsetDetect);
    }
    for pass in base.post_passes() {
        if pass.name() != "StackOffsetDetect" {
            p.add_post_boxed(pass.clone_box());
        }
    }
    p
}

/// The region a memory class names; the `MemoryId` itself is run-local.
fn region(class: MemDecomp) -> &'static str {
    match class {
        MemDecomp::Unknown => "unknown",
        MemDecomp::NotMemory => "not memory",
        MemDecomp::Stack(_) => "stack",
        MemDecomp::Heap(_) => "heap",
    }
}

type Class = (NodeId, &'static str, Option<(ValueId, i128)>);

fn classes(f: &strider_ir::Function) -> Vec<Class> {
    f.walk_kind(|k| matches!(k, NodeKind::Load(_) | NodeKind::Store(_)))
        .map(|n| {
            let (class, slot) = f.side_tables().memory_decomp(f.node_inputs(n)[1]);
            (n, region(class), slot)
        })
        .collect()
}

#[test]
#[cfg_attr(debug_assertions, ignore)]
fn a_pre_stamp_changes_no_memory_class() {
    let opts = AnalyzeOptions {
        on_incomplete: OnIncomplete::Ignore,
        ..AnalyzeOptions::default()
    };
    let (mut compared, mut stack_slots) = (0, 0);
    for arch in ["x64", "aarch64"] {
        for fixture in Fixture::all(fixture_arch(arch)) {
            let (mut strider, cc) = fixture.strider();
            for (addr, name) in fixture.functions() {
                let mut got = Vec::new();
                for pre_stamp in [false, true] {
                    let result = strider.analyze(addr, &cc, &opts, Some(pipeline(pre_stamp)));
                    got.push(result.ok().map(|r| classes(&r.function)));
                }
                assert_eq!(got[0], got[1], "{arch}/{}::{name}", fixture.case);
                if let Some(c) = &got[0] {
                    compared += 1;
                    stack_slots += c.iter().filter(|(_, r, _)| *r == "stack").count();
                }
            }
        }
    }
    assert!(compared >= 300, "only {compared} functions analyzed");
    assert!(stack_slots > 0, "no stack slot was classified");
}
