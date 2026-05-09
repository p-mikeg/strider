//! Shared end-to-end pipeline runners + placeholder-anchor finders for the
//! IR-level fixture builders.
//!
//! Split out from the previous monolithic `indirect_resolve_helpers.rs` so each
//! sub-module imports only the helpers it actually needs.  This module owns
//! the lift-and-optimise harness used by every classify / inplace / cache
//! fixture; it does not build any specific scenario itself.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, dead_code)]

use cfg::{Builder, OptionsBuilder};
use ir::BuiltFunctionGraph;
use ir::node::NodeKind;
use rsleigh::Sleigh;
use rsleigh::mem_readers::BufMemReader;
use strider::{CallingConvention, SleighArch, Strider};

/// Walk every reachable `IndirectBranch` node and return the value-
/// input (slot 2) of the unique placeholder.  Returns `None` if none
/// exists; panics if more than one is found (every fixture in this
/// module has exactly one indirect branch).
///
/// The placeholder `IndirectBranch` has exactly 3 inputs:
/// `[control, memory, target_value]`.
pub fn anchor_value_input(graph: &BuiltFunctionGraph) -> Option<ir::Value> {
    let mut found: Option<ir::Value> = None;
    for nid in graph.preorder() {
        if !matches!(graph.graph.node_kind(nid), NodeKind::IndirectBranch) {
            continue;
        }
        let inputs: Vec<_> = graph.graph.node_inputs(nid).into_iter().collect();
        assert!(
            inputs.len() == 3,
            "IndirectBranch placeholder must have exactly 3 inputs; got {}",
            inputs.len(),
        );
        assert!(
            found.is_none(),
            "fixture must have exactly one IndirectBranch placeholder; found a second",
        );
        found = Some(inputs[2]);
    }
    found
}

/// Run `Strider::analyze_cfg` on a hand-assembled byte
/// sequence + the standard SystemV-x86_64 calling convention, then run
/// the full optimiser pipeline.  Returns the resulting graph plus the
/// (single) IR-level placeholder anchor's `NodeOutputId` and the
/// convention's link-register VN (always `None` on x86_64 — that arch
/// pushes return addresses on the stack).
///
/// Panics if the synthetic CFG produces zero or multiple
/// `UnresolvedIndirectBranch` placeholders — every fixture in this
/// module is supposed to have exactly one indirect branch.
pub fn run_pipeline_x86_64(
    bytes: Vec<u8>,
) -> (BuiltFunctionGraph, ir::Value, Option<rsleigh::Vn>) {
    let base = 0x1000u64;
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, base);
    let sleigh = Sleigh::new(arch.sla_spec, arch.pspec, reader)
        .expect("create x86_64 sleigh");
    let opts = OptionsBuilder::new().build();
    let cfg = Builder::for_arch(&arch, sleigh, base, opts)
        .build()
        .expect("cfg build");

    let regs = arch.probe_regs().expect("probe regs");
    let strider =
        Strider::new(arch, regs, CallingConvention::x86_64_systemv()).expect("Strider::new");
    let lr_vn = strider.calling_convention().link_register_vn();
    let outcome = strider
        .analyze_cfg(&cfg)
        .expect("analyze_cfg");
    let mut graph = outcome.graph;

    // Run the full optimiser pipeline so the placeholder's anchor
    // value reaches the producer-shape the classifier looks at.
    // ConstantFold collapses `mov rax, K; jmp *rax` to IntConst(K);
    // RedundantPhis simplifies the trivial Return shape we don't
    // need to walk past.
    let p = strider.build_optimizer_pipeline();
    p.run(&mut graph.graph, graph.entry).expect("optimizer pipeline");

    assert_eq!(
        outcome.unresolved_branches.len(),
        1,
        "fixture must have exactly one IR-level placeholder",
    );
    // Resolve the *current* anchor after the optimiser ran — the
    // original recorded NodeOutputId may be orphaned if any pass
    // `replace_all_uses`-rewrote the placeholder's input slot
    // (e.g. ConstantFold rewriting a folded IntBinaryOp into an
    // IntConst).  See module-level docs for the full contract.
    let anchor = anchor_value_input(&graph)
        .expect("fixture must have one IndirectBranch placeholder after optimisation");
    (graph, anchor, lr_vn)
}
