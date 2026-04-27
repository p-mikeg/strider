//! Shared fixture builders for the tier-2 classifier integration tests.
//!
//! Each helper drives `Strider::analyze_cfg_with_unresolved` against a
//! synthetic byte sequence + arch + calling-convention triple, runs
//! the full strider optimiser pipeline, then returns a
//! `(BuiltFunctionGraph, anchor_NodeOutputId, link_register_vn)`
//! tuple ready for `classify_anchor` to consume.
//!
//! IMPORTANT: the anchor returned is **NOT** the original
//! `NodeOutputId` recorded at lift time — that id can be invalidated
//! by `ConstantFold`'s `replace_all_uses` rewires.  Instead, helpers
//! resolve the placeholder Return's current value-input (slot 2) on
//! the post-optimisation graph and return that.  This mirrors what
//! the R3 orchestrator will do at each tier-2 invocation: walk to
//! the Return's input slot to find the live producer.
//!
//! The fixtures intentionally use small hand-assembled byte sequences
//! so the failure modes are attributable to the classifier under test
//! (or to the optimiser passes the helper runs), not to a build
//! pipeline whose contents the caller has to reason about.

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
fn current_anchor_after_opt(
    graph: &BuiltFunctionGraph,
) -> ir::Value {
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

/// Run `Strider::analyze_cfg_with_unresolved` on a hand-assembled byte
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
        .analyze_cfg_with_unresolved(&cfg)
        .expect("analyze_cfg_with_unresolved");
    let mut graph = outcome.graph;

    // Run the full optimiser pipeline so the placeholder's anchor
    // value reaches the producer-shape the classifier looks at.
    // ConstantFold collapses `mov rax, K; jmp *rax` to IntConst(K);
    // RedundantPhis simplifies the trivial Return shape we don't
    // need to walk past.
    let p = strider.build_optimizer_pipeline();
    p.run(&mut graph).expect("optimizer pipeline");

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

/// Build a function whose only indirect branch is `mov rax, K; jmp *rax`.
/// After `ConstantFold` runs, the placeholder Return's value-input
/// folds to `IntConst(K)`.
///
/// **K must be < the function start address (0x1000)** so the cfg
/// builder's tier-1 resolver classifies the branch as a tail call —
/// otherwise it enqueues exploration of `K`, the buffered memory
/// reader's range doesn't cover that, and Sleigh's `lift_one(K)`
/// trips DataUnavailErr.  When `K` is a tail call we get
/// `RegionTerminator::TailCall { target: K }`, NOT
/// `UnresolvedIndirectBranch` — i.e. tier 1 resolves it before
/// tier 2 ever sees it.  That defeats this fixture's purpose.
///
/// To force the branch into the tier-2 path we therefore use a
/// **runtime-computed** target: `mov rax, [rsp+8]; jmp rax`.  The
/// rsp-relative read prevents tier 1's mini-graph from folding the
/// target to a constant (no constant write to rax in the region),
/// so the branch defers to `UnresolvedIndirectBranch`.  Then
/// classify_anchor only sees an InitialVar / Load shape — NOT
/// IntConst — so this helper is no longer suitable for the
/// `IntConst → Single` test.  Use
/// [`build_int_const_target_scenario_via_lr`] instead.
#[allow(dead_code)]
pub fn build_int_const_target_scenario(_k: u64) -> (BuiltFunctionGraph, ir::Value) {
    unimplemented!(
        "tier-1 always classifies a constant target — use \
         build_int_const_target_scenario_phi_merge for the IntConst arm"
    )
}

/// Build a function whose only indirect branch resolves to a
/// constant `k` *only after* the optimiser has run on the lifted IR
/// — i.e. one where tier 1's mini-graph couldn't classify it.
///
/// Approach: write `k` to a stack slot via a function-entry push,
/// then load that slot through a register-indirect load and jump
/// through the loaded value.  Tier 1's mini-graph isn't given
/// `LoadReadOnly` for synthetic regions and doesn't track stack
/// stores / loads, so the BranchIndirect defers via
/// `UnresolvedIndirectBranch`.  After strider runs the full
/// optimiser pipeline (including `StackStoreDetect` +
/// `StackLoadForward`), the loaded value folds to `IntConst(k)` —
/// exactly the shape tier 2's IntConst arm classifies.
pub fn build_int_const_target_scenario_via_stack(
    k: u64,
) -> (BuiltFunctionGraph, ir::Value) {
    // x86_64 encoding:
    //   68 K K K K           push imm32       (sign-extended; rsp -= 8)
    //   58                   pop rax          (rax = pushed K; rsp += 8)
    //   ff e0                jmp rax
    // The `pop rax` step gives the optimiser an SP-rooted load that
    // `StackLoadForward` can simplify back to the pushed constant
    // K, while keeping tier 1's single-region mini-graph (which
    // lacks StackLoadForward) unable to classify the target.
    let k_le = (k as u32).to_le_bytes();
    let mut bytes: Vec<u8> = vec![
        0x68, k_le[0], k_le[1], k_le[2], k_le[3], 0x58, 0xff, 0xe0,
    ];
    // Pad with `int3` (0xcc) so any stray look-ahead the Sleigh
    // lifter performs past the BranchIndirect doesn't trip
    // `DataUnavailErr` on the buffered memory reader.  The
    // BranchIndirect is the region terminator, so these bytes are
    // never reachable from the analysed function.
    bytes.extend(std::iter::repeat_n(0xccu8, 64));
    let (graph, anchor, _lr) = run_pipeline_x86_64(bytes);
    (graph, anchor)
}

/// Build a function whose only indirect branch is `jmp *rax` with no
/// prior write to `rax` — the placeholder's value-input is therefore
/// `InitialVar(rax)` after the optimiser runs.
///
/// On x86_64 there is no architectural link register, so the
/// "InitialVar(target_vn) == InitialVar(lr_vn)" arm in the classifier
/// returns `None` here regardless of caller-supplied lr.
pub fn build_initial_var_target_scenario_x86_64() -> (BuiltFunctionGraph, ir::Value) {
    // Just `jmp rax`.  RAX is a function-entry value with no constant
    // write; the placeholder's input is `InitialVar(rax)`.
    let bytes: Vec<u8> = vec![0xff, 0xe0];
    let (graph, anchor, _lr) = run_pipeline_x86_64(bytes);
    (graph, anchor)
}

/// Build a function whose only indirect branch resolves to
/// `InitialVar(lr_vn)` after the optimiser runs.  Returns the
/// link-register VN as the third tuple element so the caller can
/// pass it to `classify_anchor`.
///
/// We use AArch64 rather than 32-bit ARM because Sleigh's ARM
/// (LE/BE-32) lifter wraps every register-indirect dispatch in a
/// thumb-interworking AND-mask (`reg & 0xfffffffe`), which leaves
/// the optimised IR's producer as `IntBinaryOp(And)` instead of
/// `InitialVar(lr_vn)` — that doesn't match this round's
/// classifier arms.  AArch64 has no thumb interworking, so
/// `mov x0, x30; br x0` lifts cleanly to `Copy + BranchIndirect`
/// and the optimiser folds `r0 = x30 = InitialVar(lr_vn)` directly.
///
/// Tier 1 cannot classify this (its mini-graph isn't given a
/// link-register VN since we don't pass `set_link_register` on
/// `OptionsBuilder`), so the cfg builder defers via
/// `UnresolvedIndirectBranch` and tier 2 sees the cleaned-up
/// shape.
pub fn build_bx_lr_scenario() -> (BuiltFunctionGraph, ir::Value, rsleigh::Vn) {
    // AArch64 (little-endian) encoding:
    //   mov x0, x30  →  e0 03 1e aa   (alias for `orr x0, xzr, x30`)
    //   br  x0       →  00 00 1f d6
    let base = 0x1000u64;
    let mut bytes: Vec<u8> = vec![
        0xe0, 0x03, 0x1e, 0xaa,
        0x00, 0x00, 0x1f, 0xd6,
    ];
    bytes.extend(std::iter::repeat_n(0xccu8, 64));
    let arch = SleighArch::aarch64();
    let reader = BufMemReader::new(bytes, base);
    let sleigh = Sleigh::new(arch.sla_spec, arch.pspec, reader)
        .expect("create aarch64 sleigh");

    let probe = BufMemReader::new(Vec::<u8>::new(), 0);
    let regs = Sleigh::new(arch.sla_spec, arch.pspec, probe)
        .expect("probe sleigh")
        .regs()
        .expect("probe regs");
    let strider = Strider::new(arch, regs, CallingConvention::aarch64_aapcs64())
        .expect("Strider::new");
    let lr_vn = strider
        .calling_convention()
        .link_register_vn
        .expect("AArch64 AAPCS has a link register");

    // Note: we deliberately omit `set_link_register` on the cfg
    // builder's options.  With it set, tier 1's mini-graph would
    // already classify the branch as LinkRegister and short-circuit
    // before tier 2 ever sees it — i.e. no
    // `UnresolvedIndirectBranch` placeholder would be emitted, and
    // the integration test would have nothing to assert against.
    let opts = OptionsBuilder::new().build();
    let cfg = Builder::with_endianness(sleigh, base, opts, arch.endianness)
        .build()
        .expect("cfg build");
    let outcome = strider
        .analyze_cfg_with_unresolved(&cfg)
        .expect("analyze_cfg_with_unresolved");
    let mut graph = outcome.graph;
    let p = strider.build_optimizer_pipeline();
    p.run(&mut graph).expect("optimizer pipeline");

    assert_eq!(
        outcome.unresolved_branches.len(),
        1,
        "bx lr fixture must have exactly one tier-2 placeholder",
    );
    let anchor = current_anchor_after_opt(&graph);
    (graph, anchor, lr_vn)
}
