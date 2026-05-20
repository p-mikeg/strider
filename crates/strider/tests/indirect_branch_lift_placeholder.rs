//! strider lifts a `RegionTerminator::UnresolvedIndirectBranch`
//! region by emitting a placeholder `IndirectBranch(target_value)`
//! that anchors the dispatch varnode in the IR for the indirect-
//! branch resolver.
//!
//! The test drives a synthetic x86-64 `jmp rax` CFG (RAX is a
//! function-entry value, not constant, so the cfg-time mini-graph
//! resolver cannot classify the target).  Pre-fix, `analyze_cfg` either errored or emitted an
//! ABI Return that discarded the dispatch value.  Post-fix, it
//! succeeds and produces an IR with exactly one IndirectBranch node
//! whose single value-input is `target_vn`'s value at the
//! BranchIndirect site.
//!
//! These tests intentionally do NOT use the per-arch fixture suite —
//! that infrastructure runs the full optimizer pipeline against a real
//! ELF.  This is a per-region lifting concern; we use a direct
//! `Builder + Strider::new + analyze_cfg` call sequence so the test
//! exercises *only* the strider IR-lift step.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use strider_lift::cfg::{Builder, OptionsBuilder};
use rsleigh::mem_readers::BufMemReader;
use rsleigh::Sleigh;
use strider::SleighArch;

mod common;

/// Build a synthetic x86-64 CFG containing a single region whose
/// terminator is `UnresolvedIndirectBranch{target_vn=RAX, addr=...}`.
///
/// Bytes: `0xff 0xe0` — `jmp rax`.  RAX is the function-entry value of
/// the dispatch register; cfg-time resolver cannot classify (no LR is set, no
/// constant write to RAX), so the cfg builder defers via the the cfg-time placeholder lift
/// fall-through and we end up with the new terminator.
fn make_unresolved_indirect_branch_cfg(
) -> (strider_lift::cfg::Cfg<BufMemReader<Vec<u8>>>, SleighArch) {
    let base = 0x1000u64;
    let bytes: Vec<u8> = vec![0xff, 0xe0];
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, base);
    let sleigh = Sleigh::new(arch.sla_spec(), arch.pspec(), reader)
        .expect("create x86-64 sleigh");
    // No link-register on x86-64 (the cdecl-family conventions push the
    // return address onto the stack), so cfg-time resolver's LinkRegister arm
    // can't classify either.
    let opts = OptionsBuilder::new().build();
    let cfg = Builder::for_arch(&arch, sleigh, base, opts)
        .build()
        .expect("cfg build must succeed under the cfg-time placeholder lift deferral");
    (cfg, arch)
}

/// Placeholder contract: a region terminated with
/// `UnresolvedIndirectBranch` lifts to an IR that is well-formed
/// (no error, one IndirectBranch node).  Pre-restructure, the strider
/// lifter dispatched the `BranchIndirect` opcode to `handle_return`,
/// which produced an ABI Return whose inputs were the convention's
/// `ret_val_regs` — NOT the dispatch varnode.  Post-fix, strider
/// inspects the region's terminator and emits an
/// `IndirectBranch(target_value)` placeholder that anchors `target_vn`
/// in the IR.
///
/// Side-effect anchor expectation: the IR's unique IndirectBranch
/// must have a value-input slot wired (the placeholder anchors
/// target_value at slot 2 — slots 0/1 are control/memory).
#[test]
fn unresolvable_branch_indirect_lifts_as_return_placeholder() {
    let (cfg, arch) = make_unresolved_indirect_branch_cfg();
    let _ = arch; // arch is the SleighArch the cfg was built with; unused here
    let strider = common::strider_builders::strider_x86_64();
    let graph = strider
        .analyze_cfg(&cfg)
        .expect("strider must lift unresolved branches as IndirectBranch placeholder")
        .graph;

    // Exactly one IndirectBranch node — strider emitted the
    // placeholder, did not double-emit, and did not lift the
    // BranchIndirect via the pre-fix ABI handle_return path.
    let placeholder_count = graph
        .preorder()
        .filter(|nid| matches!(graph.graph.node_kind(*nid), strider_ir::node::NodeKind::IndirectBranch))
        .count();
    assert_eq!(
        placeholder_count, 1,
        "expected exactly one IndirectBranch placeholder, got {placeholder_count}"
    );

    // The placeholder must have a value-input slot wired — its layout
    // is [control, memory, target_value].  That's exactly 3 inputs.
    let placeholder = graph
        .preorder()
        .find(|nid| matches!(graph.graph.node_kind(*nid), strider_ir::node::NodeKind::IndirectBranch))
        .expect("must have an IndirectBranch node");
    let inputs = graph.graph.node_inputs(placeholder);
    assert_eq!(
        inputs.len(),
        3,
        "placeholder must have layout [control, memory, target_value]; got {} inputs",
        inputs.len()
    );
}

/// Anchor-tracking contract: the strider exposes a side-table
/// mapping each placeholder's pcode address to the `NodeOutputId`
/// that anchors `target_vn`.  the IR-level orchestrator resolver walks this table.
///
/// Pinning the table now keeps the API surface stable for R2.
#[test]
fn unresolved_branches_table_tracks_each_placeholder() {
    let (cfg, arch) = make_unresolved_indirect_branch_cfg();
    let _ = arch; // arch is the SleighArch the cfg was built with; unused here
    let strider = common::strider_builders::strider_x86_64();
    let outcome = strider
        .analyze_cfg(&cfg)
        .expect("analyze_cfg");
    // Single deferred branch in this synthetic fixture.
    assert_eq!(
        outcome.unresolved_branches.len(),
        1,
        "expected exactly one tracked placeholder, got {}",
        outcome.unresolved_branches.len(),
    );
    // The tracked address must point at the original BranchIndirect.
    let (addr, _value) = outcome.unresolved_branches[0];
    assert_eq!(addr.machine_addr_u64(), 0x1000);
}
