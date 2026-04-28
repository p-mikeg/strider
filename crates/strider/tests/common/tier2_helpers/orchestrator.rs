//! Shared end-to-end pipeline runners + placeholder-anchor finders for the
//! tier-2 fixture builders.
//!
//! Split out from the previous monolithic `tier2_helpers.rs` (W7) so each
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

/// Walk every reachable Return node's inputs and return the value-input
/// (slot 2) of the unique placeholder Return whose pcode-address-keyed
/// anchor was registered in `unresolved_branches`.
///
/// The placeholder Return has exactly 3 inputs: `[control, memory,
/// target_value]` (R1.4's lift contract).  All other Return nodes —
/// the function's real ABI returns — have either 2 inputs or
/// `2 + ret_val_regs.len()` inputs.  Filtering by `inputs.len() == 3`
/// uniquely picks out the placeholder.
pub(super) fn current_anchor_after_opt(graph: &BuiltFunctionGraph) -> ir::Value {
    let mut found: Option<ir::Value> = None;
    for nid in graph.preorder() {
        if !matches!(graph.graph.node_kind(nid), NodeKind::Return) {
            continue;
        }
        let inputs: Vec<_> = graph.graph.node_inputs(nid).into_iter().collect();
        if inputs.len() != 3 {
            // Not a tier-2 placeholder: real ABI Returns have 2
            // (no value) or 2 + ret_val_regs.len() inputs.
            continue;
        }
        assert!(
            found.is_none(),
            "fixture must have exactly one placeholder Return; found a second",
        );
        // Slot layout: [control, memory, target_value].
        found = Some(inputs[2]);
    }
    found.expect("fixture must have one placeholder Return after optimisation")
}

/// Run `Strider::analyze_cfg` on a hand-assembled byte
/// sequence + the standard SystemV-x86_64 calling convention, then run
/// the full optimiser pipeline.  Returns the resulting graph plus the
/// (single) tier-2 placeholder anchor's `NodeOutputId` and the
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
    let cfg = Builder::with_endianness(sleigh, base, opts, arch.endianness)
        .build()
        .expect("cfg build");

    let probe = BufMemReader::new(Vec::<u8>::new(), 0);
    let regs = Sleigh::new(arch.sla_spec, arch.pspec, probe)
        .expect("probe sleigh")
        .regs()
        .expect("probe regs");
    let strider =
        Strider::new(arch, regs, CallingConvention::x86_64_systemv_abi()).expect("Strider::new");
    let lr_vn = strider.calling_convention().link_register_vn;
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
        "fixture must have exactly one tier-2 placeholder",
    );
    // Resolve the *current* anchor after the optimiser ran — the
    // original recorded NodeOutputId may be orphaned if any pass
    // `replace_all_uses`-rewrote the placeholder's input slot
    // (e.g. ConstantFold rewriting a folded IntBinaryOp into an
    // IntConst).  See module-level docs for the full contract.
    let anchor = current_anchor_after_opt(&graph);
    (graph, anchor, lr_vn)
}

/// Walk every reachable Return and return the value-input (slot 2)
/// of the unique 3-input Return — the placeholder shape strider's
/// R1.4 lift produces.  `None` when there's no such Return (caller
/// asserts).  Local copy of the helper at the top of this module
/// because the existing `current_anchor_after_opt` is private.
pub fn anchor_value_input(graph: &BuiltFunctionGraph) -> Option<ir::Value> {
    for nid in graph.preorder() {
        if !matches!(graph.graph.node_kind(nid), NodeKind::Return) {
            continue;
        }
        let inputs: Vec<_> = graph.graph.node_inputs(nid).into_iter().collect();
        if inputs.len() != 3 {
            continue;
        }
        return Some(inputs[2]);
    }
    None
}
