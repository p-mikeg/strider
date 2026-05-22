//! Fixture builders feeding the IR-level *classifier* unit / integration tests.
//!
//! Split out from the previous monolithic `indirect_resolve_helpers.rs`.  Every
//! helper here builds a `BuiltFunctionGraph` whose unique placeholder
//! Return's value-input is shaped to exercise one specific classifier arm
//! (IntConst, InitialVar(lr), ValuePhi-of-IntConsts, Load jump-table, etc.).
//!
//! Subset matrix:
//!
//! | Helper                                            | Anchor shape                                |
//! |---------------------------------------------------|---------------------------------------------|
//! | `build_int_const_target_scenario_via_stack`       | `IntConst(K)` after StackLoadForward        |
//! | `build_initial_var_target_scenario_x86_64`        | `InitialVar(rax)` (no lr on x86_64)         |
//! | `build_value_phi_target_scenario`                 | `ValuePhi(IntConst, …)` over diamond merge  |
//! | `build_pop_pc_via_stack_load_forward_scenario`    | `InitialVar(lr)` via push-lr / pop-pc       |
//! | `build_push_target_pop_pc_scenario`               | `IntConst(K)` via push-K / pop-pc           |
//! | `build_bx_lr_scenario`                            | `InitialVar(lr)` via AArch64 `mov x0,x30; br x0` |
//! | `build_jump_table_known_bits_scenario`            | `Load(base + (idx & mask)*stride)`          |
//! | `build_jump_table_predecessor_if_scenario`        | `Load(base + idx*stride)` after `If(idx<N)` |
//! | `build_jump_table_unbounded_scenario`             | `Load(base + idx*stride)` (unbounded idx)   |
//! | `build_non_jump_table_load_scenario`              | `Load(IntConst(addr))` (control case)       |

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, dead_code)]

use strider_lift::cfg::{Builder, OptionsBuilder};
use strider_ir::BuiltFunctionGraph;
use strider_ir::node::NodeKind;
use rsleigh::Sleigh;
use rsleigh::mem_readers::BufMemReader;
use strider_analyze::Strider;
use strider_target::{CallingConvention, SleighArch};

use super::orchestrator::{anchor_value_input, run_pipeline_x86_64};

// NOTE: there is no `build_int_const_target_scenario(K)` because cfg-time
// always classifies a literal constant target — the synthetic shape
// `mov rax, K; jmp *rax` resolves at cfg-build time before
// `classify_anchor` ever sees it.  Tests that want the
// IntConst-classifier arm route through a runtime-computed target that
// folds to `IntConst(K)` only after the IR-level optimiser runs; see
// [`build_int_const_target_scenario_via_stack`] below.

/// Build a function whose only indirect branch resolves to a
/// constant `k` *only after* the optimiser has run on the lifted IR
/// — i.e. one where cfg-time resolver's mini-graph couldn't classify it.
///
/// Approach: write `k` to a stack slot via a function-entry push,
/// then load that slot through a register-indirect load and jump
/// through the loaded value.  the cfg-time mini-graph resolver's mini-graph isn't given
/// `LoadReadOnly` for synthetic regions and doesn't track stack
/// stores / loads, so the BranchIndirect defers via
/// `UnresolvedIndirectBranch`.  After strider runs the full
/// optimiser pipeline (including `StackStoreDetect` +
/// `StackLoadForward`), the loaded value folds to `IntConst(k)` —
/// exactly the shape IR-level indirect-branch resolver's IntConst arm classifies.
pub fn build_int_const_target_scenario_via_stack(
    k: u64,
) -> (BuiltFunctionGraph, strider_ir::Value) {
    // x86_64 encoding:
    //   68 K K K K           push imm32       (sign-extended; rsp -= 8)
    //   58                   pop rax          (rax = pushed K; rsp += 8)
    //   ff e0                jmp rax
    // The `pop rax` step gives the optimiser an SP-rooted load that
    // `StackLoadForward` can simplify back to the pushed constant
    // K, while keeping cfg-time resolver's single-region mini-graph (which
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
pub fn build_initial_var_target_scenario_x86_64() -> (BuiltFunctionGraph, strider_ir::Value) {
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
/// the shape strider's the strider deferred-anchor lift placeholder lift produces.
///
/// Bypasses the cfg builder + `Strider::analyze_cfg`
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
) -> (BuiltFunctionGraph, strider_ir::Value) {
    use strider_ir::node::NodeOutputType;
    use strider_ir::{FunctionBuilder, IntBinaryOp};
    use strider_analyze::opt::{ConstantFold, OptimizerPipeline, StackLoadForward, StackStoreDetect};
    use strider_target::Endianness;

    assert!(
        !per_pred.is_empty(),
        "ValuePhi fixture needs at least one predecessor",
    );

    // 4-byte stack pointer VN — register space, offset 0x20, size 4.
    // Doesn't have to match a real arch's SP; StackStoreDetect /
    // StackLoadForward only care that it's the SP register passed
    // into the pass constructors.
    let sp = rsleigh::Vn {
        addr_off: 0x20,
        addr_space: rsleigh::VnSpace::REGISTER,
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
    b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
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
        let four = b.build_int_const(4u64, NodeOutputType::U32).unwrap();
        let addr = b
            .build_int_binary_operation(sp_v, four, IntBinaryOp::Add, NodeOutputType::U32)
            .expect("addr");
        let v = b.build_int_const(k, NodeOutputType::U32).unwrap();
        b.build_store(addr, v, rsleigh::VnSpace::RAM).expect("store");
        b.build_branch(merge).expect("branch to merge");
    }

    // Merge: load `*(sp+4)` and Return it as the placeholder anchor.
    b.set_region(merge);
    let sp_m = b.read_variable(&sp).expect("read sp in merge");
    let four_m = b.build_int_const(4u64, NodeOutputType::U32).unwrap();
    let addr_m = b
        .build_int_binary_operation(sp_m, four_m, IntBinaryOp::Add, NodeOutputType::U32)
        .expect("merge addr");
    let loaded = b
        .build_load(addr_m, rsleigh::VnSpace::RAM, NodeOutputType::U32)
        .expect("load");
    // The IndirectBranch placeholder: slot 2 = anchor.
    b.build_indirect_branch(loaded)
        .expect("placeholder IndirectBranch");
    b.set_lift_addr(None);
    let mut fg = b.build().expect("build");

    // Run the stable subset that produces the ValuePhi.  We omit
    // RedundantPhis here so a single-pred fixture preserves the
    // ValuePhi shape (otherwise the trivial-phi rule collapses it).
    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(StackStoreDetect::new(sp));
    pipeline.add(StackLoadForward::new(sp, Endianness::Little));
    let entry = fg.entry().unwrap();
    pipeline.run(fg.graph_mut(), entry).expect("opt pipeline");

    let anchor = anchor_value_input(&fg).expect("no IndirectBranch placeholder");
    (fg, anchor)
}

/// Build a `BuiltFunctionGraph` modelling gcc-ARM's standard
/// `push {lr}; ...; pop {pc}` epilogue using FunctionBuilder
/// directly (not through cfg + analyze_cfg).
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
) -> (BuiltFunctionGraph, strider_ir::Value, rsleigh::Vn) {
    use strider_ir::node::NodeOutputType;
    use strider_ir::FunctionBuilder;
    use strider_analyze::opt::{ConstantFold, OptimizerPipeline, StackLoadForward, StackStoreDetect};
    use strider_target::Endianness;

    let sp = rsleigh::Vn {
        addr_off: 0x20,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 4,
    };
    let lr = rsleigh::Vn {
        addr_off: 0x4c,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 4,
    };
    let mut b = FunctionBuilder::new_raw(vec![sp, lr], &[], &[sp], &[], None, 0)
        .expect("new_raw");
    let region = b.create_region().expect("region");
    b.set_entry_region(region).expect("set_entry_region");
    b.set_region(region);
    b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));

    // Compute `sp - 4` — the slot we'll push lr to.
    let sp_v = b.read_variable(&sp).expect("read sp");
    let four = b.build_int_const(4u64, NodeOutputType::U32).unwrap();
    let store_addr = b
        .build_int_sub(sp_v, four, NodeOutputType::U32)
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
    let four2 = b.build_int_const(4u64, NodeOutputType::U32).unwrap();
    let load_addr = b
        .build_int_sub(sp_v2, four2, NodeOutputType::U32)
        .expect("sp - 4 (load)");
    let loaded = b
        .build_load(load_addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)
        .expect("load");
    b.build_indirect_branch(loaded).expect("placeholder IndirectBranch");
    b.set_lift_addr(None);
    let mut fg = b.build().expect("build");

    // Include `RedundantPhis` so the trivial single-input
    // VarPhi(lr) at the entry region collapses back to
    // `InitialVar(lr)` — that's the shape IR-level indirect-branch resolver's LinkRegister
    // arm classifies, and it's what the production strider
    // pipeline (`default_pipeline()` includes RedundantPhis)
    // produces in real-binary integration tests.
    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(strider_analyze::opt::RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    pipeline.add(StackLoadForward::new(sp, Endianness::Little));
    // RedundantPhis again post-StackLoadForward to collapse any
    // single-input VarPhi the forward inserts (e.g. wrapping
    // the loaded InitialVar(lr) in a phi at the merge region).
    pipeline.add(strider_analyze::opt::RedundantPhis);
    let entry = fg.entry().unwrap();
    pipeline.run(fg.graph_mut(), entry).expect("opt pipeline");

    let mut found: Option<strider_ir::Value> = None;
    for nid in fg.preorder() {
        if !matches!(fg.node_kind(nid), NodeKind::IndirectBranch) {
            continue;
        }
        let inputs: Vec<_> = fg.node_inputs(nid).into_iter().collect();
        assert!(found.is_none(), "multiple IndirectBranch placeholders");
        found = Some(inputs[2]);
    }
    let anchor = found.expect("no IndirectBranch placeholder");
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
/// wrong-edge-set path.  the IR-level orchestrator resolver dodges that trap because
/// StackLoadForward folds the load to the **stored constant** K,
/// not to InitialVar(lr); the IntConst arm then classifies as
/// Single(K).
///
/// Also returns the `lr` VN we added to the tracked-vars set so
/// callers can pass it to `classify_anchor` and verify the
/// LinkRegister arm doesn't false-positive.
pub fn build_push_target_pop_pc_scenario(
    k: u64,
) -> (BuiltFunctionGraph, strider_ir::Value, rsleigh::Vn) {
    use strider_ir::node::NodeOutputType;
    use strider_ir::FunctionBuilder;
    use strider_analyze::opt::{ConstantFold, OptimizerPipeline, StackLoadForward, StackStoreDetect};
    use strider_target::Endianness;

    let sp = rsleigh::Vn {
        addr_off: 0x20,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 4,
    };
    let lr = rsleigh::Vn {
        addr_off: 0x4c,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 4,
    };
    let mut b = FunctionBuilder::new_raw(vec![sp, lr], &[], &[sp], &[], None, 0)
        .expect("new_raw");
    let region = b.create_region().expect("region");
    b.set_entry_region(region).expect("set_entry_region");
    b.set_region(region);
    b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));

    let sp_v = b.read_variable(&sp).expect("read sp");
    let four = b.build_int_const(4u64, NodeOutputType::U32).unwrap();
    let store_addr = b
        .build_int_sub(sp_v, four, NodeOutputType::U32)
        .expect("sp - 4");
    let stored_const = b.build_int_const(k, NodeOutputType::U32).unwrap();
    b.build_store(store_addr, stored_const, rsleigh::VnSpace::RAM)
        .expect("store K");

    let sp_v2 = b.read_variable(&sp).expect("read sp again");
    let four2 = b.build_int_const(4u64, NodeOutputType::U32).unwrap();
    let load_addr = b
        .build_int_sub(sp_v2, four2, NodeOutputType::U32)
        .expect("sp - 4 (load)");
    let loaded = b
        .build_load(load_addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)
        .expect("load");
    b.build_indirect_branch(loaded).expect("placeholder IndirectBranch");
    b.set_lift_addr(None);
    let mut fg = b.build().expect("build");

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(StackStoreDetect::new(sp));
    pipeline.add(StackLoadForward::new(sp, Endianness::Little));
    let entry = fg.entry().unwrap();
    pipeline.run(fg.graph_mut(), entry).expect("opt pipeline");

    let mut found: Option<strider_ir::Value> = None;
    for nid in fg.preorder() {
        if !matches!(fg.node_kind(nid), NodeKind::IndirectBranch) {
            continue;
        }
        let inputs: Vec<_> = fg.node_inputs(nid).into_iter().collect();
        assert!(found.is_none(), "multiple IndirectBranch placeholders");
        found = Some(inputs[2]);
    }
    let anchor = found.expect("no IndirectBranch placeholder");
    (fg, anchor, lr)
}

// ── jump-table fixtures ──────────────────────────────────────────────────
//
// Each helper builds a `BuiltFunctionGraph` whose placeholder Return's
// value-input is shaped like a jump-table dispatch — `Load(IntAdd(
// IntConst(base), IntMul(idx, IntConst(stride))))` — and runs the
// stable optimiser subset so the structure is exactly what IR-level indirect-branch resolver's
// classifier sees in production.  Helpers parameterise over how `idx`
// is bounded:
//
//   * AND-mask (KnownBits route) — `build_jump_table_known_bits_*`.
//   * Predecessor `If(idx < N)` — `build_jump_table_predecessor_if_*`.
//   * Neither (unbounded; classifier must return None) —
//     `build_jump_table_unbounded`.
//
// All helpers go through `FunctionBuilder::new_raw` rather than the
// cfg-builder + analyze_cfg path because (a) we don't
// need the cfg builder's cfg-time resolver here, (b) constructing real
// arch bytes that lift to a jump-table-shaped IR is fixture overkill,
// and (c) the FunctionBuilder API is the same code path the cfg
// builder ultimately goes through, so we exercise the same lift
// semantics.

/// Build a placeholder `Return(load)` whose load is jump-table-shaped
/// with `idx & idx_mask` bounding the index.  After the stable
/// optimiser subset runs, `KnownBits` proves `idx <= idx_mask` and
/// the classifier's jump-table arm reads `idx_mask + 1` entries
/// from the caller's rom.
///
/// Returns the graph and the placeholder Return's value-input slot.
pub fn build_jump_table_known_bits_scenario(
    base: u64,
    stride: u64,
    idx_mask: u64,
) -> (BuiltFunctionGraph, strider_ir::Value) {
    use strider_ir::node::NodeOutputType;
    use strider_ir::{FunctionBuilder, IntBinaryOp};
    use strider_analyze::opt::{ConstantFold, OptimizerPipeline};

    // Single tracked variable — a register-shaped VN.  We seed `idx`
    // from `read_variable` so it's a non-IntConst (else the matcher
    // would mis-disambiguate stride vs idx in commuted multiplications).
    let idx_var = rsleigh::Vn {
        addr_off: 0x10,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 4,
    };
    let mut b = FunctionBuilder::new_raw(vec![idx_var], &[], &[], &[], None, 0)
        .expect("new_raw");
    let region = b.create_region().expect("region");
    b.set_entry_region(region).expect("set_entry");
    b.set_region(region);
    b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));

    let raw_idx = b.read_variable(&idx_var).expect("read idx");
    let mask_c = b.build_int_const(idx_mask, NodeOutputType::U32).unwrap();
    let masked = b
        .build_int_binary_operation(raw_idx, mask_c, IntBinaryOp::And, NodeOutputType::U32)
        .expect("idx & mask");
    let stride_c = b.build_int_const(stride, NodeOutputType::U32).unwrap();
    let mul = b
        .build_int_binary_operation(masked, stride_c, IntBinaryOp::Mul, NodeOutputType::U32)
        .expect("mul");
    let base_c = b.build_int_const(base, NodeOutputType::U32).unwrap();
    let addr = b
        .build_int_binary_operation(base_c, mul, IntBinaryOp::Add, NodeOutputType::U32)
        .expect("add");
    let loaded = b
        .build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)
        .expect("load");
    b.build_indirect_branch(loaded).expect("placeholder IndirectBranch");
    b.set_lift_addr(None);
    let mut fg = b.build().expect("build");

    // Stable optimiser subset.  We deliberately omit RedundantPhis
    // and DeadBranchElim because the spec routes the jump-table
    // classifier through the same destructive-omitted pipeline that
    // intermediate iterations of the orchestrator use, so the graph
    // shape we hand to classify_anchor here matches the orchestrator's
    // intermediate-iteration sees.
    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    let entry = fg.entry().unwrap();
    pipeline.run(fg.graph_mut(), entry).expect("opt pipeline");

    let anchor = anchor_value_input(&fg).expect("anchor");
    (fg, anchor)
}

/// Build a placeholder `Return(load)` whose load is jump-table-shaped
/// with the index bounded by a *predecessor* `If(idx < bound)` —
/// the dispatch region is on the true branch.  The stable optimiser
/// subset is run, but RedundantPhis is OMITTED so the trivial-phi
/// rule doesn't strip the entry merge-region's structure.
///
/// Topology:
///   entry  ──[if idx < bound: true]── dispatch (loads + Returns)
///         └──[false]─────────────────── exit (early Return)
///
/// The dispatch's placeholder Return is the anchor we return.
pub fn build_jump_table_predecessor_if_scenario(
    base: u64,
    stride: u64,
    bound: u64,
) -> (BuiltFunctionGraph, strider_ir::Value) {
    use strider_ir::node::NodeOutputType;
    use strider_ir::{FunctionBuilder, IntBinaryOp, IntCmpOp};
    use strider_analyze::opt::{ConstantFold, OptimizerPipeline};

    let idx_var = rsleigh::Vn {
        addr_off: 0x10,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 4,
    };
    let mut b = FunctionBuilder::new_raw(vec![idx_var], &[], &[], &[], None, 0)
        .expect("new_raw");
    let entry = b.create_region().expect("entry");
    let dispatch = b.create_region().expect("dispatch");
    let exit = b.create_region().expect("exit");
    b.set_entry_region(entry).expect("set_entry");

    // Entry: build `idx < bound`, branch to dispatch on true / exit on false.
    b.set_region(entry);
    b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
    let raw_idx_at_entry = b.read_variable(&idx_var).expect("read idx (entry)");
    let bound_c = b.build_int_const(bound, NodeOutputType::U32).unwrap();
    let cond = b
        .build_int_cmp_operation(raw_idx_at_entry, bound_c, IntCmpOp::Less, NodeOutputType::U32)
        .expect("idx < bound");
    b.build_if(cond, dispatch, exit).expect("if dispatch");

    // Dispatch: build the jump-table-shaped load.
    b.set_region(dispatch);
    let idx = b.read_variable(&idx_var).expect("read idx (dispatch)");
    let stride_c = b.build_int_const(stride, NodeOutputType::U32).unwrap();
    let mul = b
        .build_int_binary_operation(idx, stride_c, IntBinaryOp::Mul, NodeOutputType::U32)
        .expect("mul");
    let base_c = b.build_int_const(base, NodeOutputType::U32).unwrap();
    let addr = b
        .build_int_binary_operation(base_c, mul, IntBinaryOp::Add, NodeOutputType::U32)
        .expect("add");
    let loaded = b
        .build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)
        .expect("load");
    b.build_indirect_branch(loaded).expect("placeholder IndirectBranch");

    // Exit: a real, non-placeholder Return.  Now that the placeholder
    // is its own `NodeKind::IndirectBranch`, distinguishing real vs
    // placeholder is by NodeKind, not input count — but we still emit
    // a 2-input Return (just control + memory) as a clean exit shape.
    b.set_region(exit);
    b.build_return(None, &[]).expect("exit return");
    b.set_lift_addr(None);

    let mut fg = b.build().expect("build");

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    let entry = fg.entry().unwrap();
    pipeline.run(fg.graph_mut(), entry).expect("opt pipeline");

    let anchor = anchor_value_input(&fg).expect("anchor");
    (fg, anchor)
}

/// Build a placeholder `Return(load)` whose load is jump-table-shaped
/// but whose `idx` is NOT bounded by either KnownBits-visible bits
/// or a predecessor If.  Used to verify the classifier returns None
/// rather than guessing a bound.
pub fn build_jump_table_unbounded_scenario(
    base: u64,
    stride: u64,
) -> (BuiltFunctionGraph, strider_ir::Value) {
    use strider_ir::node::NodeOutputType;
    use strider_ir::{FunctionBuilder, IntBinaryOp};
    use strider_analyze::opt::{ConstantFold, OptimizerPipeline};

    let idx_var = rsleigh::Vn {
        addr_off: 0x10,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 4,
    };
    let mut b = FunctionBuilder::new_raw(vec![idx_var], &[], &[], &[], None, 0)
        .expect("new_raw");
    let region = b.create_region().expect("region");
    b.set_entry_region(region).expect("set_entry");
    b.set_region(region);
    b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));

    let idx = b.read_variable(&idx_var).expect("read idx");
    let stride_c = b.build_int_const(stride, NodeOutputType::U32).unwrap();
    let mul = b
        .build_int_binary_operation(idx, stride_c, IntBinaryOp::Mul, NodeOutputType::U32)
        .expect("mul");
    let base_c = b.build_int_const(base, NodeOutputType::U32).unwrap();
    let addr = b
        .build_int_binary_operation(base_c, mul, IntBinaryOp::Add, NodeOutputType::U32)
        .expect("add");
    let loaded = b
        .build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)
        .expect("load");
    b.build_indirect_branch(loaded).expect("placeholder IndirectBranch");
    b.set_lift_addr(None);
    let mut fg = b.build().expect("build");

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    let entry = fg.entry().unwrap();
    pipeline.run(fg.graph_mut(), entry).expect("opt pipeline");

    let anchor = anchor_value_input(&fg).expect("anchor");
    (fg, anchor)
}

/// Build a placeholder `Return(load)` whose load is NOT jump-table-
/// shaped — used to verify the classifier's Load arm falls through
/// to None on unrelated load shapes (e.g. `Load(IntConst(addr))` for
/// a simple global read).
pub fn build_non_jump_table_load_scenario() -> (BuiltFunctionGraph, strider_ir::Value) {
    use strider_ir::node::NodeOutputType;
    use strider_ir::FunctionBuilder;
    use strider_analyze::opt::{ConstantFold, OptimizerPipeline};

    let mut b = FunctionBuilder::empty()
        .expect("new_raw");
    let region = b.create_region().expect("region");
    b.set_entry_region(region).expect("set_entry");
    b.set_region(region);
    b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));

    let addr = b.build_int_const(0x1234_u64, NodeOutputType::U32).unwrap();
    let loaded = b
        .build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)
        .expect("load");
    b.build_indirect_branch(loaded).expect("placeholder IndirectBranch");
    b.set_lift_addr(None);
    let mut fg = b.build().expect("build");

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    let entry = fg.entry().unwrap();
    pipeline.run(fg.graph_mut(), entry).expect("opt pipeline");

    let anchor = anchor_value_input(&fg).expect("anchor");
    (fg, anchor)
}

/// Build a placeholder `IndirectBranch(load)` whose load is a
/// **stack-array dispatch**: at function entry, `N` constants are
/// stored at contiguous SP-relative offsets (`sp + base_offset +
/// i*stride` for `i in 0..N`), and the dispatch loads from
/// `sp + base_offset + (idx & MASK) * stride` where `MASK` is the
/// power-of-two-minus-one bound that lets `KnownBits` derive the
/// per-arm `bound = MASK + 1`.
///
/// `N` must be `> 0` and `< MAX_TABLE_ENTRIES` (currently 256), and
/// must be a power of 2 so the `idx & (N - 1)` mask matches the
/// stack-array classifier's `bound_via_known_bits` path.  Returns
/// the graph, the anchor (load output), and the SP varnode the
/// caller passes to `classify_anchor`.
///
/// The fixture mirrors the existing `build_two_target_array`
/// fixture in `crates/opt/src/indirect_branch_resolve/stack_array.rs`,
/// generalised to N targets.  The stack pointer is a fake 8-byte
/// register at offset `0x40`; the index argument is a fake 8-byte
/// register at offset `0x38` (matches sysv argument register
/// width); the dispatch is `Load[(sp + base_offset) + ((arg & N-1)
/// * stride)]`.
///
/// Pipeline run: `ConstantFold + KnownBits + RedundantPhis +
/// StackStoreDetect`.  `StackLoadForward` is **deliberately
/// omitted** — including it would forward the Load to the matching
/// IntConst directly, eliminating the Load entirely and turning the
/// anchor into an IntConst (the Single-target arm), defeating the
/// stack-array classifier exercise.
pub fn build_stack_array_dispatch_scenario(
    targets: &[u64],
    base_offset: i64,
    stride: u64,
) -> (BuiltFunctionGraph, strider_ir::node::NodeOutputId, rsleigh::Vn) {
    use strider_ir::node::{NodeOutputId, NodeOutputKind, NodeOutputType};
    use strider_ir::{ExtendOp, FunctionBuilder, IntBinaryOp};
    use strider_analyze::opt::{ConstantFold, KnownBits, OptimizerPipeline, RedundantPhis, StackStoreDetect};

    let n = u64::try_from(targets.len()).expect("targets.len fits in u64");
    assert!(n > 0, "stack-array fixture needs at least one target");
    assert!(
        n.is_power_of_two(),
        "stack-array fixture requires N = power of 2 so KnownBits derives bound = N \
         (idx & (N-1) leaves N candidate values)",
    );
    let mask = n - 1;

    let sp = rsleigh::Vn {
        addr_off: 0x40,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 8,
    };
    let arg_vn = rsleigh::Vn {
        addr_off: 0x38,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 8,
    };
    let mut b = FunctionBuilder::new_raw(vec![sp, arg_vn], &[], &[sp], &[], None, 0)
        .expect("FunctionBuilder::new_raw");
    let region = b.create_region().expect("create_region");
    b.set_entry_region(region).expect("set_entry_region");
    b.set_region(region);
    b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
    let sp_val = b.read_variable(&sp).expect("read sp");

    // N entry stores: target[i] → *(sp + base_offset + i*stride).
    for (i, &target_addr) in targets.iter().enumerate() {
        let off =
            base_offset + i64::try_from(i).expect("i fits") * i64::try_from(stride).expect("stride fits");
        let off_const = b.build_int_const(off as u64, NodeOutputType::U64).unwrap();
        let addr = b
            .build_int_binary_operation(sp_val, off_const, IntBinaryOp::Add, NodeOutputType::U64)
            .expect("addr");
        let target_v = b
            .build_int_const(target_addr, NodeOutputType::U64)
            .unwrap();
        b.build_store(addr, target_v, rsleigh::VnSpace::RAM)
            .expect("store target");
    }

    // Index = (arg as u32) & MASK, zero-extended to u64.
    let arg_val = b.read_variable(&arg_vn).expect("read arg");
    let arg_u32_node = b.graph_mut().create_node(
        strider_ir::node::NodeKind::Truncate,
        [arg_val],
        [NodeOutputKind::OutputType(NodeOutputType::U32)],
    );
    // Direct `graph_mut().create_node` bypasses FunctionBuilder's
    // auto-stamping; manually attribute these nodes to the sentinel
    // lift address so Layer-C asm-fingerprint validation accepts them.
    b.graph_mut().set_asm_fingerprint(arg_u32_node, vec![strider_ir_test_utils::SENTINEL_LIFT_ADDR]);
    let arg_u32_out = b.graph().node_outputs_exact::<1>(arg_u32_node).unwrap()[0];
    let mask_c = b
        .build_int_const(mask, NodeOutputType::U32)
        .unwrap();
    let masked = b
        .build_int_binary_operation(arg_u32_out, mask_c, IntBinaryOp::And, NodeOutputType::U32)
        .expect("idx & mask");
    let idx_u64_node = b.graph_mut().create_node(
        strider_ir::node::NodeKind::Extend(ExtendOp::ZeroExtend),
        [masked],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    b.graph_mut().set_asm_fingerprint(idx_u64_node, vec![strider_ir_test_utils::SENTINEL_LIFT_ADDR]);
    let idx_u64_out = b.graph().node_outputs_exact::<1>(idx_u64_node).unwrap()[0];
    let stride_const = b.build_int_const(stride, NodeOutputType::U64).unwrap();
    let idx_scaled = b
        .build_int_binary_operation(
            idx_u64_out,
            stride_const,
            IntBinaryOp::Mul,
            NodeOutputType::U64,
        )
        .expect("idx*stride");

    // Address = (sp + base_offset) + idx*stride.  Two-Add shape so the
    // classifier's flatten_add_tree exercises both terms.
    let base_const = b
        .build_int_const(base_offset as u64, NodeOutputType::U64)
        .unwrap();
    let sp_plus_base = b
        .build_int_binary_operation(sp_val, base_const, IntBinaryOp::Add, NodeOutputType::U64)
        .expect("sp + base");
    let load_addr = b
        .build_int_binary_operation(sp_plus_base, idx_scaled, IntBinaryOp::Add, NodeOutputType::U64)
        .expect("addr");
    let loaded = b
        .build_load(load_addr, rsleigh::VnSpace::RAM, NodeOutputType::U64)
        .expect("load");
    b.build_indirect_branch(loaded)
        .expect("placeholder IndirectBranch");
    b.set_lift_addr(None);
    let mut fg = b.build().expect("build");

    let mut p = OptimizerPipeline::new();
    p.add(ConstantFold);
    p.add(KnownBits);
    p.add(RedundantPhis);
    p.add(StackStoreDetect::new(sp));
    // NOTE: StackLoadForward is intentionally NOT in this pipeline;
    // see the doc-comment above.
    let entry = fg.entry().unwrap();
    p.run(fg.graph_mut(), entry).expect("opt pipeline");

    // Locate the surviving Load from the IndirectBranch's value-input.
    // After the partial pipeline, the placeholder's anchor IS the Load
    // — `anchor_value_input` returns inputs[2], which is the Load output.
    let anchor: NodeOutputId = anchor_value_input(&fg).expect("placeholder anchor");
    (fg, anchor, sp)
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
/// the cfg-time mini-graph resolver cannot classify this (its mini-graph isn't given a
/// link-register VN since we don't pass `set_link_register` on
/// `OptionsBuilder`), so the cfg builder defers via
/// `UnresolvedIndirectBranch` and IR-level indirect-branch resolver sees the cleaned-up
/// shape.
pub fn build_bx_lr_scenario() -> (BuiltFunctionGraph, strider_ir::Value, rsleigh::Vn) {
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
    let sleigh = Sleigh::new(arch.sla_spec(), arch.pspec(), reader)
        .expect("create aarch64 sleigh");

    let regs = arch.probe_regs().expect("probe regs");
    let strider = Strider::new(arch, regs, CallingConvention::aarch64_aapcs64().unwrap())
        .expect("Strider::new");
    let lr_vn = strider
        .calling_convention()
        .link_register_vn
        .expect("AArch64 AAPCS has a link register");

    // Note: we deliberately omit `set_link_register` on the cfg
    // builder's options.  With it set, cfg-time resolver's mini-graph would
    // already classify the branch as LinkRegister and short-circuit
    // before IR-level indirect-branch resolver ever sees it — i.e. no
    // `UnresolvedIndirectBranch` placeholder would be emitted, and
    // the integration test would have nothing to assert against.
    let opts = OptionsBuilder::new().build();
    let cfg = Builder::for_arch(&arch, sleigh, base, opts)
        .build()
        .expect("cfg build");
    let outcome = strider
        .analyze_cfg(&cfg)
        .expect("analyze_cfg");
    let mut graph = outcome.graph;
    let p = strider.build_optimizer_pipeline();
    let entry = graph.entry().unwrap();
    p.run(graph.graph_mut(), entry).expect("optimizer pipeline");

    assert_eq!(
        outcome.unresolved_branches.len(),
        1,
        "bx lr fixture must have exactly one IR-level placeholder",
    );
    let anchor = anchor_value_input(&graph)
        .expect("bx lr fixture must have one IndirectBranch placeholder after optimisation");
    (graph, anchor, lr_vn)
}
