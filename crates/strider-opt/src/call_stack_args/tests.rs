use strider_ir::IRBuilderExt;
use strider_ir::IRViewer;
use super::*;
use crate::error::Result;
use crate::pipeline::OptimizerTestExt;
use crate::test_support::cf_rp_pipeline;
use anyhow::anyhow;
use strider_ir::node::{NodeId, NodeKind, ValueId, ValueType};
use strider_ir::{Graph, IntBinaryOp};
use strider_ir_test_utils::{RegisterSet, stack_vn_x86 as stack_vn};

/// A prologue local-variable zero-init writes to offsets that happen to
/// land in the arg-slot range for a later call, but *chronologically*
/// before the real arg pushes.  In memory-chain order, the walker sees:
///   ret-push, arg 0 push, arg 1 push, buf-init stores, prologue saves, …
/// The buf-init stores break chain-order contiguity (after arg 1 at
/// `ret + 8` the next chain entry jumps to some much higher offset), so
/// collection must stop after arg 1 rather than scoop up the zero-init
/// writes as spurious args.  Reproduces the `hard_func` case where Call
/// nodes ended up with 4× `const 0` + an `init EBX` tacked on.
#[test]
fn buf_init_does_not_leak_into_args() -> Result<()> {
    let sp = stack_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_arg_offsets(vec![4, 8, 12, 16, 20, 24, 28, 32])
        .build_fn_single_region()?;
    let sp0 = b.read_variable(&sp)?;
    // Simulate: `push ebx` + `sub esp, 16` + 4× zero-init + push arg1 +
    // push arg0 + implicit-call ret-push.
    let four = b.build_int_const(4u64, ValueType::I32)?;
    let sixteen = b.build_int_const(16u64, ValueType::I32)?;

    // push ebx → [sp - 4] = init_ebx.
    let sp_after_push_ebx = b.build_sub_as_add_neg(sp0, four, ValueType::I32)?;
    b.write_variable(&sp, sp_after_push_ebx)?;
    let init_ebx = b.build_int_const(0xEBu64, ValueType::I32)?;
    b.build_store(sp_after_push_ebx, init_ebx, rsleigh::VnSpace::RAM)?;

    // sub esp, 16 → reserve buf.
    let sp_after_sub = b.build_sub_as_add_neg(sp_after_push_ebx, sixteen, ValueType::I32)?;
    b.write_variable(&sp, sp_after_sub)?;

    // 4× zero-init at buf[0..16] (esp+0, +4, +8, +12) = [-20, -16, -12, -8].
    let zero = b.build_int_const(0u64, ValueType::I32)?;
    for k in 0..4 {
        let off = b.build_int_const((k * 4) as u64, ValueType::I32)?;
        let addr =
            b.build_int_binary_operation(sp_after_sub, off, IntBinaryOp::Add, ValueType::I32)?;
        b.build_store(addr, zero, rsleigh::VnSpace::RAM)?;
    }

    // push arg1 = 1 → [sp - 24].
    let sp_push_arg1 = b.build_sub_as_add_neg(sp_after_sub, four, ValueType::I32)?;
    b.write_variable(&sp, sp_push_arg1)?;
    let arg1 = b.build_int_const(1u64, ValueType::I32)?;
    b.build_store(sp_push_arg1, arg1, rsleigh::VnSpace::RAM)?;

    // push arg0 = 42 → [sp - 28].
    let sp_push_arg0 = b.build_sub_as_add_neg(sp_push_arg1, four, ValueType::I32)?;
    b.write_variable(&sp, sp_push_arg0)?;
    let arg0 = b.build_int_const(42u64, ValueType::I32)?;
    b.build_store(sp_push_arg0, arg0, rsleigh::VnSpace::RAM)?;

    // implicit call ret-addr push at [sp - 32] — mimics x86 `call`.
    let sp_call = b.build_sub_as_add_neg(sp_push_arg0, four, ValueType::I32)?;
    b.write_variable(&sp, sp_call)?;
    let retaddr = b.build_int_const(0x1234u64, ValueType::I32)?;
    b.build_store(sp_call, retaddr, rsleigh::VnSpace::RAM)?;

    // call target.
    let target = b.build_int_const(0x1000u64, ValueType::I32)?;
    b.build_call(target, None)?;
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    // x86 cdecl: ret addr at offset 0, args at +4, +8, +12, …
    pipeline.add_post_pass(CallStackArgCollect::new());
    pipeline.run(&mut fg, &mut crate::OptCtx::empty())?;

    let call_id = find_call(fg.graph())?;
    let inputs: Vec<ValueId> = fg.node_inputs(call_id).into_iter().collect();
    // ctrl + mem + target + sp + exactly 2 args = 6 inputs.
    assert_eq!(
        inputs.len(),
        6,
        "buf-init and callee-save writes must not be mis-collected as args; got inputs={inputs:?}"
    );
    let arg0_kind = *fg.kind_of_value(inputs[4]);
    let arg1_kind = *fg.kind_of_value(inputs[5]);
    assert!(
        matches!(arg0_kind, NodeKind::IntConst(42)),
        "arg0 should be 42, got {arg0_kind:?}"
    );
    assert!(
        matches!(arg1_kind, NodeKind::IntConst(1)),
        "arg1 should be 1, got {arg1_kind:?}"
    );
    Ok(())
}

// ── CallStackArgCollect tests ────────────────────────────────────────────

/// Finds the unique Call node in `graph`.
fn find_call(graph: &Graph) -> Result<NodeId> {
    graph
        .all_node_ids()
        .find(|&n| matches!(graph.node_kind(n), NodeKind::Call))
        .ok_or_else(|| anyhow!("expected Call node, got {:?}", NodeKind::Call))
}

/// cdecl-style: `push arg1=22; push arg0=11; call target(0x1000)`.
/// After optimization the Call's inputs should be extended with
/// `[arg0, arg1]` in positional order.
#[test]
fn cdecl_two_stack_args_collected_in_order() -> Result<()> {
    let sp = stack_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_arg_offsets(vec![0, 4, 8, 12])
        .build_fn_single_region()?;
    let sp_v0 = b.read_variable(&sp)?;
    // push arg1 (= 22) at sp - 4
    let four = b.build_int_const(4u64, ValueType::I32)?;
    let sp_v1 = b.build_sub_as_add_neg(sp_v0, four, ValueType::I32)?;
    b.write_variable(&sp, sp_v1)?;
    let arg1 = b.build_int_const(22u64, ValueType::I32)?;
    b.build_store(sp_v1, arg1, rsleigh::VnSpace::RAM)?;

    // push arg0 (= 11) at sp - 8
    let sp_v2 = b.build_sub_as_add_neg(sp_v1, four, ValueType::I32)?;
    b.write_variable(&sp, sp_v2)?;
    let arg0 = b.build_int_const(11u64, ValueType::I32)?;
    b.build_store(sp_v2, arg0, rsleigh::VnSpace::RAM)?;

    // call 0x1000
    let target = b.build_int_const(0x1000u64, ValueType::I32)?;
    b.build_call(target, None)?;
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    pipeline.add_post_pass(CallStackArgCollect::new());
    pipeline.run(&mut fg, &mut crate::OptCtx::empty())?;

    let call_id = find_call(fg.graph())?;
    let inputs: Vec<ValueId> = fg.node_inputs(call_id).into_iter().collect();
    // inputs = [ctrl, memory, target, sp, stack_arg_0, stack_arg_1] — no
    // arg-passing registers on cdecl, so indices 4 and 5 are the stack args.
    assert_eq!(
        inputs.len(),
        6,
        "expected ctrl+mem+target+sp+2 stack args; got {inputs:?}"
    );

    let arg0_val = inputs[4];
    let arg1_val = inputs[5];
    let arg0_kind = *fg.kind_of_value(arg0_val);
    let arg1_kind = *fg.kind_of_value(arg1_val);
    assert!(
        matches!(arg0_kind, NodeKind::IntConst(11)),
        "arg0 should be 11, got {arg0_kind:?}"
    );
    assert!(
        matches!(arg1_kind, NodeKind::IntConst(22)),
        "arg1 should be 22, got {arg1_kind:?}"
    );
    Ok(())
}

/// One store at the anchor offset (= slot 0 under an AArch64-style table
/// `[0, 4]`) — the dense prefix is `[arg]`, so exactly one positional
/// arg gets appended.  Pins the "single arg collected when higher slots
/// missing" path.
#[test]
fn single_arg_collected_when_higher_slot_missing() -> Result<()> {
    let sp = stack_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_arg_offsets(vec![0, 4])
        .build_fn_single_region()?;
    let sp_v0 = b.read_variable(&sp)?;
    let four = b.build_int_const(4u64, ValueType::I32)?;
    let sp_v1 = b.build_sub_as_add_neg(sp_v0, four, ValueType::I32)?;
    b.write_variable(&sp, sp_v1)?;
    let only_arg = b.build_int_const(99u64, ValueType::I32)?;
    b.build_store(sp_v1, only_arg, rsleigh::VnSpace::RAM)?;

    let target = b.build_int_const(0x1000u64, ValueType::I32)?;
    b.build_call(target, None)?;
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    pipeline.add_post_pass(CallStackArgCollect::new());
    pipeline.run(&mut fg, &mut crate::OptCtx::empty())?;

    let call_id = find_call(fg.graph())?;
    let inputs: Vec<ValueId> = fg.node_inputs(call_id).into_iter().collect();
    // ctrl + memory + target + sp + stack_arg_0 — only the one we have.
    assert_eq!(inputs.len(), 5, "only one stack arg could be collected");
    Ok(())
}

/// When slot 0 is never filled the collection must remain empty.
///
/// Uses a ret-addr-push pattern: the chain anchor is a store at
/// sp-4 (the implicit ret-addr push), which is NOT in the slot table
/// `[4, 8]` (cdecl-style: args at sp+0 and sp+4 relative to the
/// pre-call SP, i.e. at rel=+4 and rel=+8 from the post-push anchor).
/// Only slot 1 (rel=8, arg1 at sp+4) is filled; slot 0 (rel=4) has
/// no store — dense prefix is empty → no args appended.
#[test]
fn missing_slot_zero_skips_collection() -> Result<()> {
    let sp = stack_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_arg_offsets(vec![4, 8])
        .build_fn_single_region()?;
    let sp_v0 = b.read_variable(&sp)?;
    let four = b.build_int_const(4u64, ValueType::I32)?;

    // Store arg1 at sp+4 (rel = 4+4=8 from anchor at sp-4 below).
    // This fills slot 1 of the [4, 8] table (value 8, index 1).
    let sp_plus_4 = b.build_int_binary_operation(sp_v0, four, IntBinaryOp::Add, ValueType::I32)?;
    let arg1 = b.build_int_const(22u64, ValueType::I32)?;
    b.build_store(sp_plus_4, arg1, rsleigh::VnSpace::RAM)?;

    // Implicit `call` ret-addr push at sp-4 — chain anchor.
    // rel = 0 is NOT in the slot table [4, 8], so the
    // `is_first_store` exception lets the walk continue.
    let sp_minus_4 = b.build_sub_as_add_neg(sp_v0, four, ValueType::I32)?;
    b.write_variable(&sp, sp_minus_4)?;
    let retaddr = b.build_int_const(0x1234u64, ValueType::I32)?;
    b.build_store(sp_minus_4, retaddr, rsleigh::VnSpace::RAM)?;

    let target = b.build_int_const(0x1000u64, ValueType::I32)?;
    b.build_call(target, None)?;
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let before_inputs = fg.node_inputs(find_call(fg.graph())?).into_iter().count();

    let mut pipeline = cf_rp_pipeline();
    // x86 cdecl-style: ret addr at offset 0 from anchor, args at +4 and +8.
    // Only slot 1 (rel=8) is filled; slot 0 (rel=4) is absent →
    // dense prefix is empty → no args appended.
    pipeline.add_post_pass(CallStackArgCollect::new());
    pipeline.run(&mut fg, &mut crate::OptCtx::empty())?;

    let after_inputs = fg.node_inputs(find_call(fg.graph())?).into_iter().count();
    assert_eq!(
        before_inputs, after_inputs,
        "no args should have been collected when slot 0 is missing"
    );
    Ok(())
}

/// A call with no stack stores at all — the walker should not add any
/// extra inputs.
#[test]
fn call_with_no_stack_stores_unchanged() -> Result<()> {
    let sp = stack_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_arg_offsets(vec![0, 4, 8])
        .build_fn_single_region()?;
    let _sp_val = b.read_variable(&sp)?;
    let target = b.build_int_const(0x1000u64, ValueType::I32)?;
    b.build_call(target, None)?;
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let before_inputs = fg.node_inputs(find_call(fg.graph())?).into_iter().count();

    let mut pipeline = cf_rp_pipeline();
    pipeline.add_post_pass(CallStackArgCollect::new());
    pipeline.run(&mut fg, &mut crate::OptCtx::empty())?;

    let after_inputs = fg.node_inputs(find_call(fg.graph())?).into_iter().count();
    assert_eq!(
        before_inputs, after_inputs,
        "no args should have been collected"
    );
    Ok(())
}

// ── Walker: non-aliasing Store passthrough ──────────────

/// Existing-behaviour pin: an in-frame stack-aliasing store (one that lands
/// at an offset INSIDE the convention's stack-arg range) interleaved between
/// two stack-arg pushes must NOT silently let both args through to the Call.
/// The walker must terminate (or the chain-order check must reject the
/// trash) — after the fix, the walker still recognises SP-rooted stores as
/// chain-terminating.
#[test]
fn walker_terminates_at_aliasing_stack_store() -> Result<()> {
    let sp = stack_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_arg_offsets(vec![0, 4, 8, 12])
        .build_fn_single_region()?;
    let sp_v0 = b.read_variable(&sp)?;
    // push arg1 (= 22) at sp - 4.
    let four = b.build_int_const(4u64, ValueType::I32)?;
    let sp_v1 = b.build_sub_as_add_neg(sp_v0, four, ValueType::I32)?;
    b.write_variable(&sp, sp_v1)?;
    let arg1 = b.build_int_const(22u64, ValueType::I32)?;
    b.build_store(sp_v1, arg1, rsleigh::VnSpace::RAM)?;

    // Trash store at sp + 0 (in stack-arg-offset range for the cdecl
    // table {0, 4, 8, 12}).  This addresses the same memory class as
    // arg slots; the walker must not silently pass through it.
    let trash = b.build_int_const(0xAAAAu64, ValueType::I32)?;
    b.build_store(sp_v0, trash, rsleigh::VnSpace::RAM)?;

    // push arg0 (= 11) at sp - 8.
    let sp_v2 = b.build_sub_as_add_neg(sp_v1, four, ValueType::I32)?;
    b.write_variable(&sp, sp_v2)?;
    let arg0 = b.build_int_const(11u64, ValueType::I32)?;
    b.build_store(sp_v2, arg0, rsleigh::VnSpace::RAM)?;

    // call.
    let target = b.build_int_const(0x1000u64, ValueType::I32)?;
    b.build_call(target, None)?;
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    pipeline.add_post_pass(CallStackArgCollect::new());
    pipeline.run(&mut fg, &mut crate::OptCtx::empty())?;

    let call_id = find_call(fg.graph())?;
    let inputs: Vec<ValueId> = fg.node_inputs(call_id).into_iter().collect();
    let collected_arg_consts: Vec<u128> = inputs[3..]
        .iter()
        .filter_map(|&out| {
            if let NodeKind::IntConst(v) = *fg.kind_of_value(out) {
                Some(v)
            } else {
                None
            }
        })
        .collect();
    // The trash 0xAAAA must NOT appear as a collected arg, AND arg1 (=22)
    // must NOT slip through past the in-arg-range trash store.
    assert!(
        !collected_arg_consts.contains(&0xAAAA_u128),
        "trash store must not be misclassified as an arg, got {collected_arg_consts:?}"
    );
    assert!(
        !collected_arg_consts.contains(&22_u128),
        "arg1 (=22) must not be collected: walker must stop at the in-frame stack-aliasing store, got args = {collected_arg_consts:?}"
    );
    Ok(())
}

/// Soundness floor under `AliasMode::Strict`: a non-SP-rooted Store on
/// the memory chain between stack-arg pushes and a Call terminates the
/// walk.  The most-recent push (closest to the Call) is collected before
/// the walker reaches the global write; everything upstream is dropped.
///
/// Models the `volatile int g = …;` barrier-pattern interleaved with
/// stack-arg pushes that `gcc -O2` emits.  Permissive recovery of the
/// upstream args lands with `AliasMode::StackGlobalDisjoint`.
#[test]
fn strict_walker_terminates_at_non_aliasing_global_store() -> Result<()> {
    let sp = stack_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_arg_offsets(vec![0, 4, 8, 12])
        .build_fn_single_region()?;
    let sp_v0 = b.read_variable(&sp)?;
    // push arg1 = 22 at sp - 4.
    let four = b.build_int_const(4u64, ValueType::I32)?;
    let sp_v1 = b.build_sub_as_add_neg(sp_v0, four, ValueType::I32)?;
    b.write_variable(&sp, sp_v1)?;
    let arg1 = b.build_int_const(22u64, ValueType::I32)?;
    b.build_store(sp_v1, arg1, rsleigh::VnSpace::RAM)?;

    // Volatile global write — cross-class against the stack-arg slots.
    let global_addr = b.build_int_const(0xDEAD_BEEFu64, ValueType::I32)?;
    let global_data = b.build_int_const(0x1234u64, ValueType::I32)?;
    b.build_store(global_addr, global_data, rsleigh::VnSpace::RAM)?;

    // push arg0 = 11 at sp - 8.
    let sp_v2 = b.build_sub_as_add_neg(sp_v1, four, ValueType::I32)?;
    b.write_variable(&sp, sp_v2)?;
    let arg0 = b.build_int_const(11u64, ValueType::I32)?;
    b.build_store(sp_v2, arg0, rsleigh::VnSpace::RAM)?;

    // call 0x1000.
    let target = b.build_int_const(0x1000u64, ValueType::I32)?;
    b.build_call(target, None)?;
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    // Pin Strict explicitly: this test exercises the conservative floor
    // (a non-SP store terminates the walk).  The default flipped to
    // `StackGlobalDisjoint`, which would step through the global
    // write — that aggressive behaviour is covered by the permissive
    // tests below.
    pipeline.add_post_pass(CallStackArgCollect::new());
    pipeline.run(&mut fg, &mut crate::test_support::octx_strict())?;

    let call_id = find_call(fg.graph())?;
    let inputs: Vec<ValueId> = fg.node_inputs(call_id).into_iter().collect();
    // Strict: only the push closest to the Call gets collected; the
    // global write terminates the walk.  ctrl + memory + target + sp + arg0 = 5.
    assert_eq!(
        inputs.len(),
        5,
        "strict walker collects only the most-recent push before the global \
         terminator; got inputs={inputs:?}"
    );
    let arg0_kind = *fg.kind_of_value(inputs[4]);
    assert!(
        matches!(arg0_kind, NodeKind::IntConst(11)),
        "arg0 should be 11, got {arg0_kind:?}"
    );
    Ok(())
}

/// Soundness floor under `AliasMode::Strict` (multi-store stress): the
/// first (most-recent-to-Call) non-SP store terminates the walk; no
/// stack args reach the Call.  Permissive mode recovery of all four
/// args lands with `AliasMode::StackGlobalDisjoint`.
#[test]
fn strict_walker_collects_no_args_when_first_chain_node_is_global_store() -> Result<()> {
    let sp = stack_vn();
    let arg_vals: [u64; 4] = [11, 22, 33, 44];
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_arg_offsets(vec![0, 4, 8, 12])
        .build_fn_single_region()?;
    let sp_initial = b.read_variable(&sp)?;
    let four = b.build_int_const(4u64, ValueType::I32)?;

    let mut sp_cur = sp_initial;
    let global_data = b.build_int_const(0x1234u64, ValueType::I32)?;
    // Push args in reverse order (arg3 first), with a global write
    // right after each push.  Memory chain backward from Call begins
    // with the *final* global write — the walker terminates there
    // before collecting any arg.
    for (i, base_global_addr) in [0xCAFE0000u64, 0xCAFE0010, 0xCAFE0020, 0xCAFE0030]
        .into_iter()
        .enumerate()
    {
        let arg_idx = 3 - i;
        sp_cur = b.build_sub_as_add_neg(sp_cur, four, ValueType::I32)?;
        b.write_variable(&sp, sp_cur)?;
        let arg = b.build_int_const(arg_vals[arg_idx], ValueType::I32)?;
        b.build_store(sp_cur, arg, rsleigh::VnSpace::RAM)?;

        let g_addr = b.build_int_const(base_global_addr, ValueType::I32)?;
        b.build_store(g_addr, global_data, rsleigh::VnSpace::RAM)?;
    }

    let target = b.build_int_const(0x1000u64, ValueType::I32)?;
    b.build_call(target, None)?;
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    // Pin Strict explicitly — see the note on
    // `strict_walker_terminates_at_non_aliasing_global_store`.  The
    // default is now `StackGlobalDisjoint`.
    pipeline.add_post_pass(CallStackArgCollect::new());
    pipeline.run(&mut fg, &mut crate::test_support::octx_strict())?;

    let call_id = find_call(fg.graph())?;
    let inputs: Vec<ValueId> = fg.node_inputs(call_id).into_iter().collect();
    // Strict: the most-recent chain node is the trailing global write,
    // walker terminates immediately.  ctrl + memory + target + sp = 4.
    assert_eq!(
        inputs.len(),
        4,
        "strict walker terminates at the leading global write; no stack args \
         collected; got inputs={inputs:?}"
    );
    Ok(())
}

// ── Bug #2026-05-02: chain order ≠ slot order ───────────────────────────────
//
// On i386 cdecl, gcc/clang -O2 routinely emits stack-arg pushes in source
// order (arg0 first, arg1 second, …), but the IR memory chain reflects
// program order — the *last* arg stored shows up as the most-recent
// store on the chain.  The original walker required successive
// stores to land at `anchor + stack_arg_offsets[args.len()]`, which only
// ever matches when the compiler happens to push args in slot-descending
// order.  These tests pin the corrected behaviour: collection succeeds
// when the chain delivers args in *any* order, as long as every chain
// store's offset belongs to the convention's stack-arg-offset set.

/// i386 `free(arg0, arg1)` shape — the original repro from
/// `exec_free_args` in the FreeBSD i386 10.0 kernel.  Args are stored at
/// `(%esp)` (= sp+0) and `0x4(%esp)` (= sp+4) in program order, then the
/// `call` instruction pushes the return address at sp-4.  Memory chain
/// backward from the Call: ret-addr push (-4) → arg1 push (+4) → arg0
/// push (0).  The walker must collect both args even though arg1 is the
/// most-recent stack store on the chain.
#[test]
fn cdecl_args_pushed_in_program_order_collected() -> Result<()> {
    let sp = stack_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_arg_offsets(vec![4, 8, 12, 16, 20, 24, 28, 32])
        .build_fn_single_region()?;
    let sp_v0 = b.read_variable(&sp)?;
    // arg0 = 11 stored at sp + 0  (cdecl: outgoing-args region is at the
    // bottom of the frame, written without first decrementing SP).
    let arg0 = b.build_int_const(11u64, ValueType::I32)?;
    b.build_store(sp_v0, arg0, rsleigh::VnSpace::RAM)?;

    // arg1 = 22 stored at sp + 4.
    let four = b.build_int_const(4u64, ValueType::I32)?;
    let sp_plus_4 = b.build_int_binary_operation(sp_v0, four, IntBinaryOp::Add, ValueType::I32)?;
    let arg1 = b.build_int_const(22u64, ValueType::I32)?;
    b.build_store(sp_plus_4, arg1, rsleigh::VnSpace::RAM)?;

    // Implicit `call` ret-addr push at sp - 4.
    let sp_after_call_push = b.build_sub_as_add_neg(sp_v0, four, ValueType::I32)?;
    b.write_variable(&sp, sp_after_call_push)?;
    let retaddr = b.build_int_const(0x1234u64, ValueType::I32)?;
    b.build_store(sp_after_call_push, retaddr, rsleigh::VnSpace::RAM)?;

    let target = b.build_int_const(0x1000u64, ValueType::I32)?;
    b.build_call(target, None)?;
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    // x86 cdecl: ret addr at offset 0, args at +4, +8, +12, …
    pipeline.add_post_pass(CallStackArgCollect::new());
    pipeline.run(&mut fg, &mut crate::OptCtx::empty())?;

    let call_id = find_call(fg.graph())?;
    let inputs: Vec<ValueId> = fg.node_inputs(call_id).into_iter().collect();
    assert_eq!(
        inputs.len(),
        6,
        "expected ctrl+mem+target+sp+2 stack args; got {inputs:?}"
    );
    let arg0_kind = *fg.kind_of_value(inputs[4]);
    let arg1_kind = *fg.kind_of_value(inputs[5]);
    assert!(
        matches!(arg0_kind, NodeKind::IntConst(11)),
        "arg0 should be 11, got {arg0_kind:?}"
    );
    assert!(
        matches!(arg1_kind, NodeKind::IntConst(22)),
        "arg1 should be 22, got {arg1_kind:?}"
    );
    Ok(())
}

/// i386 `kmap_free_wakeup(arg0, arg1, arg2)` shape — the second repro
/// from the same function.  Compiler emits stores in arbitrary order:
/// arg1 first, arg0 second, arg2 third.  Memory chain backward from the
/// Call: ret-addr push (-4) → arg2 (+8) → arg0 (0) → arg1 (+4).  No two
/// successive chain stores are at adjacent slots, so the original
/// in-order walker bailed with `args = []`.  After the fix all three
/// must land in the right slots.
#[test]
fn cdecl_three_args_in_arbitrary_order_collected() -> Result<()> {
    let sp = stack_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_arg_offsets(vec![4, 8, 12, 16, 20, 24, 28, 32])
        .build_fn_single_region()?;
    let sp_v0 = b.read_variable(&sp)?;
    let four = b.build_int_const(4u64, ValueType::I32)?;
    let eight = b.build_int_const(8u64, ValueType::I32)?;

    // arg1 = 22 at sp + 4.
    let sp_plus_4 = b.build_int_binary_operation(sp_v0, four, IntBinaryOp::Add, ValueType::I32)?;
    let arg1 = b.build_int_const(22u64, ValueType::I32)?;
    b.build_store(sp_plus_4, arg1, rsleigh::VnSpace::RAM)?;

    // arg0 = 11 at sp + 0.
    let arg0 = b.build_int_const(11u64, ValueType::I32)?;
    b.build_store(sp_v0, arg0, rsleigh::VnSpace::RAM)?;

    // arg2 = 33 at sp + 8.
    let sp_plus_8 = b.build_int_binary_operation(sp_v0, eight, IntBinaryOp::Add, ValueType::I32)?;
    let arg2 = b.build_int_const(33u64, ValueType::I32)?;
    b.build_store(sp_plus_8, arg2, rsleigh::VnSpace::RAM)?;

    // Implicit `call` ret-addr push at sp - 4.
    let sp_after_call_push = b.build_sub_as_add_neg(sp_v0, four, ValueType::I32)?;
    b.write_variable(&sp, sp_after_call_push)?;
    let retaddr = b.build_int_const(0x1234u64, ValueType::I32)?;
    b.build_store(sp_after_call_push, retaddr, rsleigh::VnSpace::RAM)?;

    let target = b.build_int_const(0x1000u64, ValueType::I32)?;
    b.build_call(target, None)?;
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    pipeline.add_post_pass(CallStackArgCollect::new());
    pipeline.run(&mut fg, &mut crate::OptCtx::empty())?;

    let call_id = find_call(fg.graph())?;
    let inputs: Vec<ValueId> = fg.node_inputs(call_id).into_iter().collect();
    assert_eq!(
        inputs.len(),
        7,
        "expected ctrl+mem+target+sp+3 stack args; got {inputs:?}"
    );
    for (slot_idx, expected) in [11u128, 22, 33].iter().enumerate() {
        let kind = *fg.kind_of_value(inputs[4 + slot_idx]);
        assert!(
            matches!(kind, NodeKind::IntConst(v) if v == *expected),
            "arg{slot_idx} should be {expected}, got {kind:?}"
        );
    }
    Ok(())
}

/// When a single arg slot is written twice on the chain (e.g. the
/// program zeroed the slot earlier and then overwrote it with the real
/// arg right before the call), the value seen by the callee is the
/// *most recent* one.  Walking backward, the first sighting of a slot
/// is the most recent; later sightings of the same slot are stale and
/// must be ignored.
#[test]
fn most_recent_value_wins_for_repeated_slot() -> Result<()> {
    let sp = stack_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_arg_offsets(vec![4, 8, 12, 16, 20, 24, 28, 32])
        .build_fn_single_region()?;
    let sp_v0 = b.read_variable(&sp)?;
    let four = b.build_int_const(4u64, ValueType::I32)?;

    // Stale arg0 = 0xBAD at sp + 0 (older write).
    let stale = b.build_int_const(0xBADu64, ValueType::I32)?;
    b.build_store(sp_v0, stale, rsleigh::VnSpace::RAM)?;

    // Real arg1 = 22 at sp + 4.
    let sp_plus_4 = b.build_int_binary_operation(sp_v0, four, IntBinaryOp::Add, ValueType::I32)?;
    let arg1 = b.build_int_const(22u64, ValueType::I32)?;
    b.build_store(sp_plus_4, arg1, rsleigh::VnSpace::RAM)?;

    // Real arg0 = 11 at sp + 0 — overwrites the stale value.
    let arg0 = b.build_int_const(11u64, ValueType::I32)?;
    b.build_store(sp_v0, arg0, rsleigh::VnSpace::RAM)?;

    // Implicit `call` ret-addr push.
    let sp_after_call_push = b.build_sub_as_add_neg(sp_v0, four, ValueType::I32)?;
    b.write_variable(&sp, sp_after_call_push)?;
    let retaddr = b.build_int_const(0x1234u64, ValueType::I32)?;
    b.build_store(sp_after_call_push, retaddr, rsleigh::VnSpace::RAM)?;

    let target = b.build_int_const(0x1000u64, ValueType::I32)?;
    b.build_call(target, None)?;
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    pipeline.add_post_pass(CallStackArgCollect::new());
    pipeline.run(&mut fg, &mut crate::OptCtx::empty())?;

    let call_id = find_call(fg.graph())?;
    let inputs: Vec<ValueId> = fg.node_inputs(call_id).into_iter().collect();
    assert_eq!(
        inputs.len(),
        6,
        "expected ctrl+mem+target+sp+2 stack args; got {inputs:?}"
    );
    let arg0_kind = *fg.kind_of_value(inputs[4]);
    assert!(
        matches!(arg0_kind, NodeKind::IntConst(11)),
        "arg0 must be the most-recent write (11), not the stale 0xBAD; got {arg0_kind:?}"
    );
    Ok(())
}

/// A store whose offset is *outside* the convention's stack-arg
/// window must terminate the walk — exactly the safety property the
/// original in-order rule provided, now expressed as set-membership.
///
/// Layout (chain-order, latest first): `ret-addr@-12 → arg0@-8 →
/// arg1@-4 → local@-16`.  The OOW local at -16 (relative offset -4
/// from the anchor at -12, NOT in the slot table {4, 8}) must abort
/// collection so the local's value never leaks into a subsequent (yet
/// to be added) arg slot if the convention table grows.
#[test]
fn out_of_window_stack_store_terminates_walk() -> Result<()> {
    let sp = stack_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_arg_offsets(vec![4, 8])
        .build_fn_single_region()?;
    let sp_v0 = b.read_variable(&sp)?;
    let four = b.build_int_const(4u64, ValueType::I32)?;
    let sixteen = b.build_int_const(16u64, ValueType::I32)?;

    // Local at sp - 16 (above the outgoing-args region — offset -16 is
    // NOT in the convention's stack-arg slot set).
    let sp_minus_16 = b.build_sub_as_add_neg(sp_v0, sixteen, ValueType::I32)?;
    let local = b.build_int_const(0xDEADu64, ValueType::I32)?;
    b.build_store(sp_minus_16, local, rsleigh::VnSpace::RAM)?;

    // arg1 = 22 at sp - 4.
    let sp_minus_4 = b.build_sub_as_add_neg(sp_v0, four, ValueType::I32)?;
    let arg1 = b.build_int_const(22u64, ValueType::I32)?;
    b.build_store(sp_minus_4, arg1, rsleigh::VnSpace::RAM)?;

    // arg0 = 11 at sp - 8.
    let eight = b.build_int_const(8u64, ValueType::I32)?;
    let sp_minus_8 = b.build_sub_as_add_neg(sp_v0, eight, ValueType::I32)?;
    let arg0 = b.build_int_const(11u64, ValueType::I32)?;
    b.build_store(sp_minus_8, arg0, rsleigh::VnSpace::RAM)?;

    // Implicit `call` ret-addr push at sp - 12.
    let twelve = b.build_int_const(12u64, ValueType::I32)?;
    let sp_minus_12 = b.build_sub_as_add_neg(sp_v0, twelve, ValueType::I32)?;
    b.write_variable(&sp, sp_minus_12)?;
    let retaddr = b.build_int_const(0x1234u64, ValueType::I32)?;
    b.build_store(sp_minus_12, retaddr, rsleigh::VnSpace::RAM)?;

    let target = b.build_int_const(0x1000u64, ValueType::I32)?;
    b.build_call(target, None)?;
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    // 2-slot cdecl table: anchor at +0 (ret-addr), arg0 at +4, arg1 at +8.
    pipeline.add_post_pass(CallStackArgCollect::new());
    pipeline.run(&mut fg, &mut crate::OptCtx::empty())?;

    let call_id = find_call(fg.graph())?;
    let inputs: Vec<ValueId> = fg.node_inputs(call_id).into_iter().collect();
    let collected: Vec<u128> = inputs[3..]
        .iter()
        .filter_map(|&out| {
            if let NodeKind::IntConst(v) = *fg.kind_of_value(out) {
                Some(v)
            } else {
                None
            }
        })
        .collect();
    assert!(
        !collected.contains(&0xDEAD_u128),
        "OOW local 0xDEAD must not be collected as an arg; got {collected:?}"
    );
    // Both real args must still be collected — OOW termination only
    // bounds the upstream walk, not the args already accumulated.
    assert_eq!(
        inputs.len(),
        6,
        "expected ctrl+mem+target+sp+2 stack args; got {inputs:?}"
    );
    let arg0_kind = *fg.kind_of_value(inputs[4]);
    let arg1_kind = *fg.kind_of_value(inputs[5]);
    assert!(
        matches!(arg0_kind, NodeKind::IntConst(11)),
        "arg0 should be 11, got {arg0_kind:?}"
    );
    assert!(
        matches!(arg1_kind, NodeKind::IntConst(22)),
        "arg1 should be 22, got {arg1_kind:?}"
    );
    Ok(())
}

// ── Per-call CC override stack-arg-offsets tests ──────────────────────────

/// When no per-call override is present, `CallStackArgCollect` uses the
/// function-default CC's `stack_arg_offsets` table.
///
/// Builds a function with one store at offset +4 from SP (matching the
/// default table `[4, 8]`) and a Call with no override entry on the
/// `Function` side-table.  Asserts the arg is collected.
#[test]
fn call_stack_arg_collect_uses_default_when_no_override() -> Result<()> {
    let sp = stack_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_arg_offsets(vec![4, 8])
        .build_fn_single_region()?;
    let sp_v0 = b.read_variable(&sp)?;
    // Store arg0 = 77 at sp + 4.
    let four = b.build_int_const(4u64, ValueType::I32)?;
    let sp_plus_4 = b.build_int_binary_operation(sp_v0, four, IntBinaryOp::Add, ValueType::I32)?;
    let arg0 = b.build_int_const(77u64, ValueType::I32)?;
    b.build_store(sp_plus_4, arg0, rsleigh::VnSpace::RAM)?;

    // Anchor store at sp + 0 (ret-addr-push role).
    let anchor = b.build_int_const(0xABCDu64, ValueType::I32)?;
    b.build_store(sp_v0, anchor, rsleigh::VnSpace::RAM)?;

    let target = b.build_int_const(0x2000u64, ValueType::I32)?;
    b.build_call(target, None)?;
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    // No side-table entry for the Call — pass uses default offsets [4, 8].
    let mut pipeline = cf_rp_pipeline();
    pipeline.add_post_pass(CallStackArgCollect::new());
    pipeline.run(&mut fg, &mut crate::OptCtx::empty())?;

    let call_id = find_call(fg.graph())?;
    let inputs: Vec<ValueId> = fg.node_inputs(call_id).into_iter().collect();
    // ctrl + mem + target + sp + arg0 = 5 inputs.
    assert_eq!(
        inputs.len(),
        5,
        "default-CC arg at offset +4 must be collected; got inputs={inputs:?}"
    );
    let arg0_kind = *fg.kind_of_value(inputs[4]);
    assert!(
        matches!(arg0_kind, NodeKind::IntConst(77)),
        "arg0 should be IntConst(77), got {arg0_kind:?}"
    );
    Ok(())
}

/// When a per-call override is recorded on the `Function` side-table,
/// `CallStackArgCollect` uses the override's `stack_arg_offsets` table
/// rather than the function-default.
///
/// The function-default table is `[4, 8]` (no slot at offset 0).  The
/// override table is `[0, 4]` (slot 0 at offset 0).  One store places
/// `IntConst(66)` at `sp + 0` — slot 0 under the override, outside
/// the default table.
///
/// Without the override the pass collects 0 args (offset 0 is not in
/// `[4, 8]`).  With the override stamped on the Call's side-table entry
/// the pass collects `IntConst(66)` as arg 0.
#[test]
fn call_stack_arg_collect_uses_override_when_present() -> Result<()> {
    #![allow(clippy::unwrap_used)]

    let sp = stack_vn();

    // Store IntConst(66) at sp + 0 (offset 0 — slot 0 under the override
    // table [0, 4], but NOT in the default table [4, 8]).
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_arg_offsets(vec![4, 8])
        .build_fn_single_region()?;
    let sp_v0 = b.read_variable(&sp)?;
    let arg0 = b.build_int_const(66u64, ValueType::I32)?;
    b.build_store(sp_v0, arg0, rsleigh::VnSpace::RAM)?;

    let target = b.build_int_const(0x5000u64, ValueType::I32)?;
    b.build_call(target, None)?;
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    // Record an override CC whose stack_arg_offsets are [0, 4] on the Call.
    // The stack-arg-offsets override is now derived from the stored CC.
    let override_cc = strider_target::BuiltCallingConvention::try_new(
        vec![], // arg_passing_regs
        vec![], // callee_saved_regs
        vec![], // ret_val_regs
        vec![], // ret_val_regs_float
        sp,     // stack_vn
        vec![0, 4],
        0,     // ret_stack_pop
        None,  // link_register_vn
        false, // preserves_memory
    )
    .unwrap();
    let call_id = fg
        .walk_kind(|k| matches!(k, NodeKind::Call))
        .next()
        .expect("Call node must exist");
    fg.set_call_cc(call_id, override_cc);

    // Run optimization with the default table [4, 8].  The pass must read
    // the per-call override [0, 4] and collect the arg at offset 0.
    let mut pipeline = cf_rp_pipeline();
    pipeline.add_post_pass(CallStackArgCollect::new());
    pipeline.run(&mut fg, &mut crate::OptCtx::empty())?;

    let call_id_post = fg
        .walk_kind(|k| matches!(k, NodeKind::Call))
        .next()
        .expect("Call node must still exist");
    let inputs: Vec<ValueId> = fg.node_inputs(call_id_post).into_iter().collect();
    // ctrl + mem + target + sp + arg0_at_+0 = 5 inputs.
    assert_eq!(
        inputs.len(),
        5,
        "override CC [0,4] must collect arg at offset +0; got {inputs:?}"
    );
    let arg0_kind = *fg.kind_of_value(inputs[4]);
    assert!(
        matches!(arg0_kind, NodeKind::IntConst(66)),
        "arg0 should be IntConst(66) from override table, got {arg0_kind:?}"
    );
    Ok(())
}

// ── Side-table source-of-truth pin ─────────────────────────────────────────

/// Pins that `CallStackArgCollect` reads the store's offset from
/// `Function::stack_offsets` when a side-table entry is present, rather than
/// re-deriving it via `decompose_sp`.
///
/// After normalisation the arg0 store's address is replaced with an opaque
/// `IntConst(0xDEAD_BEEF)` node so `decompose_sp` would return `None` for it
/// (non-SP-rooted → pass-through in the old path, skipped entirely).  Then
/// `Function::stack_offsets` is populated manually with offset 4 for that
/// store (mimicking what `StackOffsetDetect` would record).
///
/// With the side-table as the source of truth the arg is collected.  Without
/// it, `decompose_sp` would pass through the store (non-SP-rooted → `None`)
/// and no arg would be appended.
#[test]
fn call_stack_arg_collect_reads_offset_from_side_table_not_decompose() -> Result<()> {
    #![allow(clippy::unwrap_used)]

    let sp = stack_vn();
    // Build a minimal function: store arg0=77 at sp+4, anchor at sp+0, call.
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_arg_offsets(vec![4, 8])
        .build_fn_single_region()?;
    let sp_v0 = b.read_variable(&sp)?;
    // arg0 = 77 at sp + 4.
    let four = b.build_int_const(4u64, ValueType::I32)?;
    let sp_plus_4 = b.build_int_binary_operation(sp_v0, four, IntBinaryOp::Add, ValueType::I32)?;
    let arg0 = b.build_int_const(77u64, ValueType::I32)?;
    b.build_store(sp_plus_4, arg0, rsleigh::VnSpace::RAM)?;

    // Anchor store at sp+0 (ret-addr-push role).
    let anchor = b.build_int_const(0x1234u64, ValueType::I32)?;
    b.build_store(sp_v0, anchor, rsleigh::VnSpace::RAM)?;

    let target = b.build_int_const(0x2000u64, ValueType::I32)?;
    b.build_call(target, None)?;
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    // Canonicalize so `decompose_sp` would work if called.
    {
        let prep = cf_rp_pipeline();
        prep.run(&mut fg, &mut crate::OptCtx::empty())?;
    }

    // Find the arg0 store: the Store whose data input is IntConst(77).
    let arg0_store = fg
        .walk()
        .find(|&n| {
            if !matches!(fg.node_kind(n), NodeKind::Store(_)) {
                return false;
            }
            let inputs = fg.node_inputs(n);
            inputs.len() == 3
                && matches!(fg.node_kind(fg.producer(inputs[2])), NodeKind::IntConst(77))
        })
        .expect("arg0 Store(IntConst(77)) must exist");

    // Replace the arg0 store's address (input slot 1) with an opaque constant
    // so decompose_sp returns None for it.  Without the side-table, the walker
    // would pass through this store (non-SP-rooted) and arg0 would not be
    // collected.
    let opaque_addr = {
        let mut ef = strider_ir::EditFunction::try_for_built(&mut fg).unwrap();
        ef.build_int_const(0xDEAD_BEEFu64, ValueType::I32).unwrap()
    };
    let addr_input_id = fg.node_input_id_at(arg0_store, 1).unwrap();
    fg.graph_mut().update_input(addr_input_id, opaque_addr);

    // Stamp the opaque node with the sentinel fingerprint so IR validation
    // doesn't reject the manually-wired graph.
    fg.set_asm_fingerprint(
        fg.producer(opaque_addr),
        vec![strider_ir_test_utils::SENTINEL_LIFT_ADDR],
    );

    // Populate the side-table: (base, offset) = (InitialVar(sp) output, 4) —
    // what StackOffsetDetect would have recorded from the original sp+4
    // address.  The base must equal what the anchor store (sp+0) resolves to
    // via decompose_sp, so the fast-path and slow-path stores agree on one
    // SP root (the per-store base-consistency check).
    let sp_base = fg
        .walk_kind(|k| matches!(*k, NodeKind::InitialVar(vn) if vn == sp))
        .next()
        .map(|n| fg.node_outputs(n).iter().copied().next().unwrap())
        .expect("InitialVar(sp) node must exist");
    fg.set_stack_offset(arg0_store, sp_base, 4);

    // Run CallStackArgCollect with slot table [4, 8] (offset 0 = anchor).
    let pass = CallStackArgCollect::new();
    pass.run_one(&mut fg, &mut crate::OptCtx::empty())?;

    let call_id = find_call(fg.graph())?;
    let inputs: Vec<ValueId> = fg.node_inputs(call_id).into_iter().collect();
    // ctrl + mem + target + sp + arg0 = 5 inputs.
    assert_eq!(
        inputs.len(),
        5,
        "side-table offset must be used to collect arg0 even with opaque address; got inputs={inputs:?}"
    );
    let arg0_kind = *fg.kind_of_value(inputs[4]);
    assert!(
        matches!(arg0_kind, NodeKind::IntConst(77)),
        "arg0 should be IntConst(77), got {arg0_kind:?}"
    );
    Ok(())
}
