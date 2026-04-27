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

/// Build a `BuiltFunctionGraph` whose placeholder Return's
/// value-input is a `ValuePhi` whose every value slot folds to an
/// `IntConst(k_i)` taken from `per_pred`.
///
/// The fixture mirrors `crates/opt/src/stack_load_forward/tests.rs::
/// phi_both_branches_store_same_offset` — an if/else diamond where
/// each arm stores a distinct constant at `sp + 4`, and the merge
/// loads from that slot.  After the strider optimiser runs the
/// merge's `Load` is replaced by a synthesised `ValuePhi` whose
/// per-pred value inputs are the per-pred IntConsts.  We anchor
/// the load via a single-input `Return(target_value)` — exactly
/// the shape strider's R1.4 placeholder lift produces.
///
/// Bypasses the cfg builder + `Strider::analyze_cfg_with_unresolved`
/// because the only x86_64 byte sequence that compresses to this
/// shape requires a `mov [rsp+K], imm; ...; jmp *[rsp+K]` flow with
/// a real conditional branch — that's a 25+-byte fixture that adds
/// nothing the FunctionBuilder path doesn't already exercise more
/// directly.  The optimiser's `StackLoadForward` is the same code
/// path either way.
///
/// `RedundantPhis` is **deliberately omitted** from the inline
/// pipeline below — with it included, a single-target path
/// (per_pred.len() == 1) would collapse the synthesised ValuePhi
/// to its sole IntConst input via the trivial-phi rule, defeating
/// the test's purpose.  Leaving it out preserves the ValuePhi
/// shape across all `per_pred` lengths.
pub fn build_value_phi_target_scenario(
    per_pred: &[u64],
) -> (BuiltFunctionGraph, ir::Value) {
    use ir::node::NodeOutputType;
    use ir::{FunctionBuilder, IntBinaryOp};
    use opt::{ConstantFold, OptimizerPipeline, StackLoadForward, StackStoreDetect};
    use target::Endianness;

    assert!(
        !per_pred.is_empty(),
        "ValuePhi fixture needs at least one predecessor",
    );

    // 4-byte stack pointer VN — register space, offset 0x20, size 4.
    // Doesn't have to match a real arch's SP; StackStoreDetect /
    // StackLoadForward only care that it's the SP register passed
    // into the pass constructors.
    let sp = rsleigh::Vn {
        addr: rsleigh::VnAddr {
            space: rsleigh::VnSpace::REGISTER,
            off: 0x20,
        },
        size: 4,
    };
    let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)
        .expect("new_raw");
    let entry = b.create_region().expect("create entry");
    // One arm region per predecessor + a merge region.
    let arm_regions: Vec<_> = (0..per_pred.len())
        .map(|_| b.create_region().expect("create arm"))
        .collect();
    let merge = b.create_region().expect("create merge");
    b.set_entry_region(entry).expect("set_entry_region");

    // Entry: chain through nested if(true)s to dispatch to one arm
    // each.  For per_pred.len() == 1 we simply branch unconditionally
    // to the only arm.  For >1 we build a left-leaning chain of
    // if(true)/else; each else feeds the next predicate.  This
    // keeps the fixture topology arbitrary-arity friendly.
    b.set_region(entry);
    if per_pred.len() == 1 {
        b.build_branch(arm_regions[0]).expect("entry branch");
    } else {
        // First arm via if(true); else falls through to the next
        // dispatcher region we synthesize on the fly.
        let mut prev_region = entry;
        for (idx, arm) in arm_regions.iter().enumerate() {
            let last = idx == per_pred.len() - 1;
            b.set_region(prev_region);
            if last {
                b.build_branch(*arm).expect("final branch");
            } else {
                let cond = b.build_boolean_const(true);
                let dispatcher = b.create_region().expect("create dispatcher");
                b.build_if(cond, *arm, dispatcher).expect("if dispatcher");
                prev_region = dispatcher;
            }
        }
    }

    // Each arm stores its IntConst at `sp + 4` and branches to merge.
    for (arm, k) in arm_regions.iter().zip(per_pred.iter().copied()) {
        b.set_region(*arm);
        let sp_v = b.read_variable(&sp).expect("read sp in arm");
        let four = b.build_int_const(4u64, NodeOutputType::U32);
        let addr = b
            .build_int_binary_operation(sp_v, four, IntBinaryOp::Add, NodeOutputType::U32)
            .expect("addr");
        let v = b.build_int_const(k, NodeOutputType::U32);
        b.build_store(addr, v, rsleigh::VnSpace::RAM).expect("store");
        b.build_branch(merge).expect("branch to merge");
    }

    // Merge: load `*(sp+4)` and Return it as the placeholder anchor.
    b.set_region(merge);
    let sp_m = b.read_variable(&sp).expect("read sp in merge");
    let four_m = b.build_int_const(4u64, NodeOutputType::U32);
    let addr_m = b
        .build_int_binary_operation(sp_m, four_m, IntBinaryOp::Add, NodeOutputType::U32)
        .expect("merge addr");
    let loaded = b
        .build_load(addr_m, rsleigh::VnSpace::RAM, NodeOutputType::U32)
        .expect("load");
    // The placeholder Return: single-input, slot 2 = anchor.  R1.4
    // contract-shape.
    b.build_return(Some(loaded), &[])
        .expect("placeholder return");
    let mut fg = b.build().expect("build");

    // Run the stable subset that produces the ValuePhi.  We omit
    // RedundantPhis here so a single-pred fixture preserves the
    // ValuePhi shape (otherwise the trivial-phi rule collapses it).
    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(StackStoreDetect::new(sp));
    pipeline.add(StackLoadForward::new(sp, Endianness::Little));
    pipeline.run(&mut fg).expect("opt pipeline");

    // Resolve the placeholder anchor by walking the unique 3-input
    // Return.  Same contract as `current_anchor_after_opt`, copied
    // inline because that helper hard-codes the
    // analyze_cfg_with_unresolved-driven path.
    let mut found: Option<ir::Value> = None;
    for nid in fg.preorder() {
        if !matches!(fg.graph.node_kind(nid), NodeKind::Return) {
            continue;
        }
        let inputs: Vec<_> = fg.graph.node_inputs(nid).into_iter().collect();
        if inputs.len() != 3 {
            continue;
        }
        assert!(found.is_none(), "multiple 3-input Returns");
        found = Some(inputs[2]);
    }
    let anchor = found.expect("no placeholder Return");
    (fg, anchor)
}

/// Build a `BuiltFunctionGraph` modelling gcc-ARM's standard
/// `push {lr}; ...; pop {pc}` epilogue using FunctionBuilder
/// directly (not through cfg + analyze_cfg_with_unresolved).
///
/// Steps in the IR:
///   1. Single region.  Tracked vars: `sp`, `lr`.
///   2. Store `InitialVar(lr)` at `sp - 4` — this is the "push lr".
///   3. Load `*(sp - 4)` — this is the "pop into pc".
///   4. Placeholder `Return(loaded)` — anchors the dispatch value.
///
/// `StackStoreDetect + StackLoadForward` then collapse the load
/// directly to `InitialVar(lr)` (same offset, no aliasing stores
/// in between).  The classifier's LinkRegister arm matches the
/// resulting shape.
///
/// This is the headline soundness test — it pins the design's
/// claim that the natural pop-pc shape resolves to LinkRegister
/// via StackLoadForward without any special-cased heuristic.
pub fn build_pop_pc_via_stack_load_forward_scenario(
) -> (BuiltFunctionGraph, ir::Value, rsleigh::Vn) {
    use ir::node::NodeOutputType;
    use ir::{FunctionBuilder, IntBinaryOp};
    use opt::{ConstantFold, OptimizerPipeline, StackLoadForward, StackStoreDetect};
    use target::Endianness;

    let sp = rsleigh::Vn {
        addr: rsleigh::VnAddr {
            space: rsleigh::VnSpace::REGISTER,
            off: 0x20,
        },
        size: 4,
    };
    let lr = rsleigh::Vn {
        addr: rsleigh::VnAddr {
            space: rsleigh::VnSpace::REGISTER,
            off: 0x4c,
        },
        size: 4,
    };
    let mut b = FunctionBuilder::new_raw(vec![sp, lr], &[], &[sp], &[], None, 0)
        .expect("new_raw");
    let region = b.create_region().expect("region");
    b.set_entry_region(region).expect("set_entry_region");
    b.set_region(region);

    // Compute `sp - 4` — the slot we'll push lr to.
    let sp_v = b.read_variable(&sp).expect("read sp");
    let four = b.build_int_const(4u64, NodeOutputType::U32);
    let store_addr = b
        .build_int_binary_operation(sp_v, four, IntBinaryOp::Sub, NodeOutputType::U32)
        .expect("sp - 4");
    // Store the function-entry lr value there.
    let lr_v = b.read_variable(&lr).expect("read lr");
    b.build_store(store_addr, lr_v, rsleigh::VnSpace::RAM)
        .expect("store lr");

    // Load from the same slot and use as the placeholder anchor.
    // The address is structurally identical (sp - 4), so
    // StackLoadForward will fold the load directly to lr_v after
    // StackStoreDetect rewrites the store.
    let sp_v2 = b.read_variable(&sp).expect("read sp again");
    let four2 = b.build_int_const(4u64, NodeOutputType::U32);
    let load_addr = b
        .build_int_binary_operation(sp_v2, four2, IntBinaryOp::Sub, NodeOutputType::U32)
        .expect("sp - 4 (load)");
    let loaded = b
        .build_load(load_addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)
        .expect("load");
    b.build_return(Some(loaded), &[]).expect("placeholder return");
    let mut fg = b.build().expect("build");

    // Include `RedundantPhis` so the trivial single-input
    // ControlPhi(lr) at the entry region collapses back to
    // `InitialVar(lr)` — that's the shape tier 2's LinkRegister
    // arm classifies, and it's what the production strider
    // pipeline (`default_pipeline()` includes RedundantPhis)
    // produces in real-binary integration tests.
    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(opt::RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    pipeline.add(StackLoadForward::new(sp, Endianness::Little));
    // RedundantPhis again post-StackLoadForward to collapse any
    // single-input ControlPhi the forward inserts (e.g. wrapping
    // the loaded InitialVar(lr) in a phi at the merge region).
    pipeline.add(opt::RedundantPhis);
    pipeline.run(&mut fg).expect("opt pipeline");

    let mut found: Option<ir::Value> = None;
    for nid in fg.preorder() {
        if !matches!(fg.graph.node_kind(nid), NodeKind::Return) {
            continue;
        }
        let inputs: Vec<_> = fg.graph.node_inputs(nid).into_iter().collect();
        if inputs.len() != 3 {
            continue;
        }
        assert!(found.is_none(), "multiple 3-input Returns");
        found = Some(inputs[2]);
    }
    let anchor = found.expect("no placeholder Return");
    (fg, anchor, lr)
}

/// Build a `BuiltFunctionGraph` modelling the soundness-critical
/// `push 0xK; pop pc` tail-call shape.  Same SP slot manipulation
/// as `build_pop_pc_via_stack_load_forward_scenario`, but the
/// stored value is an IntConst rather than `InitialVar(lr)`.
///
/// Distinguishing this case from a real pop-pc is the soundness
/// gate that killed the prior in-place heuristic: a naïve
/// "Load(InitialVar(sp)+K) means return" classifier would mark
/// this as LinkRegister, sending the analyser down the
/// wrong-edge-set path.  Tier 2 dodges that trap because
/// StackLoadForward folds the load to the **stored constant** K,
/// not to InitialVar(lr); the IntConst arm then classifies as
/// Single(K).
///
/// Also returns the `lr` VN we added to the tracked-vars set so
/// callers can pass it to `classify_anchor` and verify the
/// LinkRegister arm doesn't false-positive.
pub fn build_push_target_pop_pc_scenario(
    k: u64,
) -> (BuiltFunctionGraph, ir::Value, rsleigh::Vn) {
    use ir::node::NodeOutputType;
    use ir::{FunctionBuilder, IntBinaryOp};
    use opt::{ConstantFold, OptimizerPipeline, StackLoadForward, StackStoreDetect};
    use target::Endianness;

    let sp = rsleigh::Vn {
        addr: rsleigh::VnAddr {
            space: rsleigh::VnSpace::REGISTER,
            off: 0x20,
        },
        size: 4,
    };
    let lr = rsleigh::Vn {
        addr: rsleigh::VnAddr {
            space: rsleigh::VnSpace::REGISTER,
            off: 0x4c,
        },
        size: 4,
    };
    let mut b = FunctionBuilder::new_raw(vec![sp, lr], &[], &[sp], &[], None, 0)
        .expect("new_raw");
    let region = b.create_region().expect("region");
    b.set_entry_region(region).expect("set_entry_region");
    b.set_region(region);

    let sp_v = b.read_variable(&sp).expect("read sp");
    let four = b.build_int_const(4u64, NodeOutputType::U32);
    let store_addr = b
        .build_int_binary_operation(sp_v, four, IntBinaryOp::Sub, NodeOutputType::U32)
        .expect("sp - 4");
    let stored_const = b.build_int_const(k, NodeOutputType::U32);
    b.build_store(store_addr, stored_const, rsleigh::VnSpace::RAM)
        .expect("store K");

    let sp_v2 = b.read_variable(&sp).expect("read sp again");
    let four2 = b.build_int_const(4u64, NodeOutputType::U32);
    let load_addr = b
        .build_int_binary_operation(sp_v2, four2, IntBinaryOp::Sub, NodeOutputType::U32)
        .expect("sp - 4 (load)");
    let loaded = b
        .build_load(load_addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)
        .expect("load");
    b.build_return(Some(loaded), &[]).expect("placeholder return");
    let mut fg = b.build().expect("build");

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(StackStoreDetect::new(sp));
    pipeline.add(StackLoadForward::new(sp, Endianness::Little));
    pipeline.run(&mut fg).expect("opt pipeline");

    let mut found: Option<ir::Value> = None;
    for nid in fg.preorder() {
        if !matches!(fg.graph.node_kind(nid), NodeKind::Return) {
            continue;
        }
        let inputs: Vec<_> = fg.graph.node_inputs(nid).into_iter().collect();
        if inputs.len() != 3 {
            continue;
        }
        assert!(found.is_none(), "multiple 3-input Returns");
        found = Some(inputs[2]);
    }
    let anchor = found.expect("no placeholder Return");
    (fg, anchor, lr)
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
