use super::*;
use anyhow::anyhow;
use crate::error::Result;
use crate::pipeline::Optimizer;
use crate::{ConstantFold, OptimizerPipeline, RedundantPhis};
use ir::BuiltFunctionGraph;
use ir::node::{NodeId, NodeKind, NodeOutputId, NodeOutputType};
use ir::test_utils::sp_vn_x86 as sp_vn;
use ir::{FunctionBuilder, IntBinaryOp};

/// Counts how many nodes in `fg` match the predicate.
fn count<F: Fn(&NodeKind) -> bool>(fg: pattern::RewriteCtxView<'_>, pred: F) -> usize {
    fg.all_node_ids()
        .filter(|&n| pred(fg.node_kind(n)))
        .count()
}

/// Simple straight-line program: `*(sp - 4) = 0x11; return *(sp - 4)`.  After
/// `ConstantFold` reassociates the address to `sp + 0xFFFFFFFC`, the
/// pass should replace the `Store` with a `StackStore { offset: -4 }`.
/// The trailing `Load` keeps the memory chain alive so `RedundantPhis`
/// doesn't detach the store as dead.
#[test]
fn simple_sp_minus_4_becomes_stack_store() -> Result<()> {
    let sp = sp_vn();
    let mut fg = ir::test_utils::make_sp_fn(sp, |b, sp_val| {
        let four = b.build_int_const(4u64, NodeOutputType::U32)?;
        let addr =
            b.build_int_sub(sp_val, four, NodeOutputType::U32)?;
        let data = b.build_int_const(0x11u64, NodeOutputType::U32)?;
        b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    pipeline.run(&mut fg.graph, fg.entry)?;

    let stack_stores = count((&fg).into(), |k| {
        matches!(k, NodeKind::StackStore { offset: -4, .. })
    });
    assert_eq!(stack_stores, 1, "expected one StackStore at offset -4");
    // Every reachable Store must have been rewritten.
    let reachable_stores =
        crate::test_support::count_reachable((&fg).into(), |k| matches!(k, NodeKind::Store(_)));
    assert_eq!(reachable_stores, 0, "no reachable Store must remain");
    Ok(())
}

/// `add esp, 0xFFFFFFFC` and `sub esp, 4` are two encodings of the same
/// SP adjustment.  `decompose_sp` must recognise `Add(sp, 0xFFFFFFFC_U32)`
/// as `sp + (-4)` via `int_const_signed`'s bit-width-aware sign extension,
/// producing a `StackStore { offset: -4 }` directly — without relying on
/// `ConstantFold` to reassociate the address first.
#[test]
fn add_sp_with_negative_unsigned_constant_becomes_stack_store() -> Result<()> {
    let sp = sp_vn();
    let mut fg = ir::test_utils::make_sp_fn(sp, |b, sp_val| {
        // 0xFFFFFFFC_U32 == -4 when sign-extended.
        let neg_four = b.build_int_const(0xFFFF_FFFCu64, NodeOutputType::U32)?;
        let addr = b.build_int_binary_operation(
            sp_val,
            neg_four,
            IntBinaryOp::Add,
            NodeOutputType::U32,
        )?;
        let data = b.build_int_const(0x11u64, NodeOutputType::U32)?;
        b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    // Intentionally omit `ConstantFold` so the test exercises
    // `decompose_sp`'s handling of the alternate encoding in isolation.
    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    pipeline.run(&mut fg.graph, fg.entry)?;

    let stack_stores = count((&fg).into(), |k| {
        matches!(k, NodeKind::StackStore { offset: -4, .. })
    });
    assert_eq!(
        stack_stores, 1,
        "Add(sp, 0xFFFFFFFC_U32) must decompose to offset -4 without ConstantFold",
    );
    Ok(())
}

/// `*sp = X` where `sp` is an entry-only phi (single reachable predecessor):
/// `RedundantPhis` collapses the phi inside the fixed-point loop, then
/// `StackStoreDetect` picks up a straight InitialVar(sp) + 0.
#[test]
fn phi_sp_collapses_to_stack_store() -> Result<()> {
    let sp = sp_vn();
    let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
    // Two regions: entry → body.  Body reads sp (which is a phi of the
    // single entry predecessor) and stores at sp + 0.
    let entry = b.create_region()?;
    let body = b.create_region()?;
    b.set_entry_region(entry)?;
    b.set_region(entry);
    b.build_branch(body)?;
    b.set_region(body);
    let sp_val = b.read_variable(&sp)?;
    let data = b.build_int_const(0xABu64, NodeOutputType::U32)?;
    b.build_store(sp_val, data, rsleigh::VnSpace::RAM)?;
    let loaded = b.build_load(sp_val, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
    b.build_return(Some(loaded), &[])?;
    let mut fg = b.build()?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    pipeline.run(&mut fg.graph, fg.entry)?;

    let stack_stores = count((&fg).into(), |k| matches!(k, NodeKind::StackStore { offset: 0, .. }));
    assert_eq!(
        stack_stores, 1,
        "phi-of-single-predecessor-sp must collapse then yield StackStore at 0"
    );
    Ok(())
}

/// Two reachable predecessors adjust SP by different amounts and merge
/// at a block that stores through the SP-phi.  The address cannot be
/// reduced to a single constant, so the rewrite produces
/// `StackStorePhi { offsets: [-4, -8] }`.
#[test]
fn phi_of_offsets_becomes_stack_store_phi() -> Result<()> {
    let sp = sp_vn();
    let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
    let entry = b.create_region()?;
    let a = b.create_region()?;
    let bb = b.create_region()?;
    let c = b.create_region()?;
    b.set_entry_region(entry)?;

    // entry: if (true) goto a else goto b
    b.set_region(entry);
    let cond = b.build_boolean_const(true);
    b.build_if(cond, a, bb)?;

    // a: sp = sp - 4; goto c
    b.set_region(a);
    let sp_a = b.read_variable(&sp)?;
    let four = b.build_int_const(4u64, NodeOutputType::U32)?;
    let sp_a2 =
        b.build_int_sub(sp_a, four, NodeOutputType::U32)?;
    b.write_variable(&sp, sp_a2)?;
    b.build_branch(c)?;

    // b: sp = sp - 8; goto c
    b.set_region(bb);
    let sp_b = b.read_variable(&sp)?;
    let eight = b.build_int_const(8u64, NodeOutputType::U32)?;
    let sp_b2 =
        b.build_int_sub(sp_b, eight, NodeOutputType::U32)?;
    b.write_variable(&sp, sp_b2)?;
    b.build_branch(c)?;

    // c: *(sp) = 0xCC; load(sp); return loaded
    b.set_region(c);
    let sp_c = b.read_variable(&sp)?;
    let data = b.build_int_const(0xCCu64, NodeOutputType::U32)?;
    b.build_store(sp_c, data, rsleigh::VnSpace::RAM)?;
    let loaded = b.build_load(sp_c, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
    b.build_return(Some(loaded), &[])?;
    let mut fg = b.build()?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    pipeline.run(&mut fg.graph, fg.entry)?;

    let phis: Vec<NodeId> = fg
        .all_node_ids()
        .filter(|&n| matches!(fg.node_kind(n), NodeKind::StackStorePhi { .. }))
        .collect();
    assert_eq!(phis.len(), 1, "expected one StackStorePhi");
    let offsets = fg.stack_phi_offsets(phis[0]);
    let mut sorted: Vec<i64> = offsets.to_vec();
    sorted.sort();
    assert_eq!(
        sorted,
        vec![-8, -4],
        "expected per-branch offsets -4 and -8"
    );
    Ok(())
}

/// Two reachable predecessors both adjust SP by the same amount and merge
/// at a block that stores through the SP-phi.  Because every predecessor
/// resolves to the same `(base, offset)`, the phi is structurally
/// redundant — the rewrite must produce a plain `StackStore`, not a
/// degenerate `StackStorePhi { offsets: [-4, -4] }`.
#[test]
fn phi_with_equal_offsets_collapses_to_stack_store() -> Result<()> {
    let sp = sp_vn();
    let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
    let entry = b.create_region()?;
    let a = b.create_region()?;
    let bb = b.create_region()?;
    let c = b.create_region()?;
    b.set_entry_region(entry)?;

    b.set_region(entry);
    let cond = b.build_boolean_const(true);
    b.build_if(cond, a, bb)?;

    // a: sp = sp - 4; goto c
    b.set_region(a);
    let sp_a = b.read_variable(&sp)?;
    let four = b.build_int_const(4u64, NodeOutputType::U32)?;
    let sp_a2 =
        b.build_int_sub(sp_a, four, NodeOutputType::U32)?;
    b.write_variable(&sp, sp_a2)?;
    b.build_branch(c)?;

    // b: sp = sp - 4; goto c  (same offset as a)
    b.set_region(bb);
    let sp_b = b.read_variable(&sp)?;
    let four2 = b.build_int_const(4u64, NodeOutputType::U32)?;
    let sp_b2 =
        b.build_int_sub(sp_b, four2, NodeOutputType::U32)?;
    b.write_variable(&sp, sp_b2)?;
    b.build_branch(c)?;

    // c: *(sp) = 0xCC; load(sp); return loaded
    b.set_region(c);
    let sp_c = b.read_variable(&sp)?;
    let data = b.build_int_const(0xCCu64, NodeOutputType::U32)?;
    b.build_store(sp_c, data, rsleigh::VnSpace::RAM)?;
    let loaded = b.build_load(sp_c, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
    b.build_return(Some(loaded), &[])?;
    let mut fg = b.build()?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    pipeline.run(&mut fg.graph, fg.entry)?;

    let stack_store_phis = count((&fg).into(), |k| matches!(k, NodeKind::StackStorePhi { .. }));
    assert_eq!(
        stack_store_phis, 0,
        "phi with all-equal offsets must not produce a StackStorePhi"
    );
    let stack_stores = count((&fg).into(), |k| {
        matches!(k, NodeKind::StackStore { offset: -4, .. })
    });
    assert_eq!(
        stack_stores, 1,
        "phi with all-equal offsets must collapse to a plain StackStore"
    );
    Ok(())
}

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
    let sp = sp_vn();
    let mut fg = ir::test_utils::make_sp_fn(sp, |b, sp0| {
        // Simulate: `push ebx` + `sub esp, 16` + 4× zero-init + push arg1 +
        // push arg0 + implicit-call ret-push.
        let four = b.build_int_const(4u64, NodeOutputType::U32)?;
        let sixteen = b.build_int_const(16u64, NodeOutputType::U32)?;

        // push ebx → [sp - 4] = init_ebx.
        let sp_after_push_ebx =
            b.build_int_sub(sp0, four, NodeOutputType::U32)?;
        b.write_variable(&sp, sp_after_push_ebx)?;
        let init_ebx = b.build_int_const(0xEBu64, NodeOutputType::U32)?;
        b.build_store(sp_after_push_ebx, init_ebx, rsleigh::VnSpace::RAM)?;

        // sub esp, 16 → reserve buf.
        let sp_after_sub = b.build_int_sub(sp_after_push_ebx, sixteen, NodeOutputType::U32,
        )?;
        b.write_variable(&sp, sp_after_sub)?;

        // 4× zero-init at buf[0..16] (esp+0, +4, +8, +12) = [-20, -16, -12, -8].
        let zero = b.build_int_const(0u64, NodeOutputType::U32)?;
        for k in 0..4 {
            let off = b.build_int_const((k * 4) as u64, NodeOutputType::U32)?;
            let addr = b.build_int_binary_operation(
                sp_after_sub,
                off,
                IntBinaryOp::Add,
                NodeOutputType::U32,
            )?;
            b.build_store(addr, zero, rsleigh::VnSpace::RAM)?;
        }

        // push arg1 = 1 → [sp - 24].
        let sp_push_arg1 = b.build_int_sub(sp_after_sub, four, NodeOutputType::U32,
        )?;
        b.write_variable(&sp, sp_push_arg1)?;
        let arg1 = b.build_int_const(1u64, NodeOutputType::U32)?;
        b.build_store(sp_push_arg1, arg1, rsleigh::VnSpace::RAM)?;

        // push arg0 = 42 → [sp - 28].
        let sp_push_arg0 = b.build_int_sub(sp_push_arg1, four, NodeOutputType::U32,
        )?;
        b.write_variable(&sp, sp_push_arg0)?;
        let arg0 = b.build_int_const(42u64, NodeOutputType::U32)?;
        b.build_store(sp_push_arg0, arg0, rsleigh::VnSpace::RAM)?;

        // implicit call ret-addr push at [sp - 32] — mimics x86 `call`.
        let sp_call = b.build_int_sub(sp_push_arg0, four, NodeOutputType::U32,
        )?;
        b.write_variable(&sp, sp_call)?;
        let retaddr = b.build_int_const(0x1234u64, NodeOutputType::U32)?;
        b.build_store(sp_call, retaddr, rsleigh::VnSpace::RAM)?;

        // call target.
        let target = b.build_int_const(0x1000u64, NodeOutputType::U32)?;
        b.build_call(target)?;
        b.build_return(None, &[])?;
        Ok(())
    })?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    // x86 cdecl: ret addr at offset 0, args at +4, +8, +12, …
    pipeline.add_post_pass(CallStackArgCollect::new(
        vec![4, 8, 12, 16, 20, 24, 28, 32],
        sp,
    ));
    pipeline.run(&mut fg.graph, fg.entry)?;

    let call_id = find_call((&fg).into())?;
    let inputs: Vec<NodeOutputId> = fg.node_inputs(call_id).into_iter().collect();
    // ctrl + mem + target + exactly 2 args = 5 inputs.
    assert_eq!(
        inputs.len(),
        5,
        "buf-init and callee-save writes must not be mis-collected as args; got inputs={inputs:?}"
    );
    let arg0_kind = *fg.kind_of_output(inputs[3]);
    let arg1_kind = *fg.kind_of_output(inputs[4]);
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

/// A non-stack store (address is an arbitrary integer constant) must be
/// left completely untouched.
#[test]
fn non_stack_store_is_untouched() -> Result<()> {
    let sp = sp_vn();
    let mut fg = ir::test_utils::make_sp_fn(sp, |b, _sp_val| {
        let addr = b.build_int_const(0x1000u64, NodeOutputType::U32)?;
        let data = b.build_int_const(0x42u64, NodeOutputType::U32)?;
        b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
        b.build_return(None, &[])?;
        Ok(())
    })?;

    StackStoreDetect::new(sp).optimize(&mut fg.graph, fg.entry)?;

    assert_eq!(
        count((&fg).into(), |k| matches!(k, NodeKind::StackStore { .. })),
        0,
        "non-stack store must not become a StackStore"
    );
    assert_eq!(
        count((&fg).into(), |k| matches!(k, NodeKind::Store(_))),
        1,
        "the original Store must remain"
    );
    Ok(())
}

// ── CallStackArgCollect tests ────────────────────────────────────────────

/// Finds the unique Call node in `fg`.
fn find_call(fg: pattern::RewriteCtxView<'_>) -> Result<NodeId> {
    fg.all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Call))
        .ok_or_else(|| anyhow!("expected Call node, got {:?}", NodeKind::Call))
}

/// cdecl-style: `push arg1=22; push arg0=11; call target(0x1000)`.
/// After optimization the Call's inputs should be extended with
/// `[arg0, arg1]` in positional order.
#[test]
fn cdecl_two_stack_args_collected_in_order() -> Result<()> {
    let sp = sp_vn();
    let mut fg = ir::test_utils::make_sp_fn(sp, |b, sp_v0| {
        // push arg1 (= 22) at sp - 4
        let four = b.build_int_const(4u64, NodeOutputType::U32)?;
        let sp_v1 =
            b.build_int_sub(sp_v0, four, NodeOutputType::U32)?;
        b.write_variable(&sp, sp_v1)?;
        let arg1 = b.build_int_const(22u64, NodeOutputType::U32)?;
        b.build_store(sp_v1, arg1, rsleigh::VnSpace::RAM)?;

        // push arg0 (= 11) at sp - 8
        let sp_v2 =
            b.build_int_sub(sp_v1, four, NodeOutputType::U32)?;
        b.write_variable(&sp, sp_v2)?;
        let arg0 = b.build_int_const(11u64, NodeOutputType::U32)?;
        b.build_store(sp_v2, arg0, rsleigh::VnSpace::RAM)?;

        // call 0x1000
        let target = b.build_int_const(0x1000u64, NodeOutputType::U32)?;
        b.build_call(target)?;
        b.build_return(None, &[])?;
        Ok(())
    })?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    pipeline.add_post_pass(CallStackArgCollect::new(vec![0, 4, 8, 12], sp));
    pipeline.run(&mut fg.graph, fg.entry)?;

    let call_id = find_call((&fg).into())?;
    let inputs: Vec<NodeOutputId> = fg.node_inputs(call_id).into_iter().collect();
    // inputs = [ctrl, memory, target, stack_arg_0, stack_arg_1] — no
    // arg-passing registers on cdecl, so indices 3 and 4 are the stack args.
    assert_eq!(
        inputs.len(),
        5,
        "expected ctrl+mem+target+2 stack args; got {inputs:?}"
    );

    let arg0_val = inputs[3];
    let arg1_val = inputs[4];
    let arg0_kind = *fg.kind_of_output(arg0_val);
    let arg1_kind = *fg.kind_of_output(arg1_val);
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
    let sp = sp_vn();
    let mut fg = ir::test_utils::make_sp_fn(sp, |b, sp_v0| {
        let four = b.build_int_const(4u64, NodeOutputType::U32)?;
        let sp_v1 =
            b.build_int_sub(sp_v0, four, NodeOutputType::U32)?;
        b.write_variable(&sp, sp_v1)?;
        let only_arg = b.build_int_const(99u64, NodeOutputType::U32)?;
        b.build_store(sp_v1, only_arg, rsleigh::VnSpace::RAM)?;

        let target = b.build_int_const(0x1000u64, NodeOutputType::U32)?;
        b.build_call(target)?;
        b.build_return(None, &[])?;
        Ok(())
    })?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    pipeline.add_post_pass(CallStackArgCollect::new(vec![0, 4], sp));
    pipeline.run(&mut fg.graph, fg.entry)?;

    let call_id = find_call((&fg).into())?;
    let inputs: Vec<NodeOutputId> = fg.node_inputs(call_id).into_iter().collect();
    // ctrl + memory + target + stack_arg_0 — only the one we have.
    assert_eq!(inputs.len(), 4, "only one stack arg could be collected");
    Ok(())
}

/// Slot 1 is filled but slot 0 is empty — `dense_prefix` truncates at
/// the first `None`, so zero args are appended.  Pattern queries doing
/// `arg(0)` would otherwise mis-bind to a hole; the truncation makes
/// the missing slot visible as "no args" rather than "args starting at
/// slot 1."
///
/// To produce a chain where slot 0 is missing, use a cdecl-style table
/// `[4, 8]` and a chain where the anchor (rel=0, OOW, `is_first_store`
/// exception) is followed by a single store at rel=8 (slot 1) and no
/// store at rel=4 (slot 0).
#[test]
fn missing_slot_zero_skips_collection() -> Result<()> {
    let sp = sp_vn();
    let mut fg = ir::test_utils::make_sp_fn(sp, |b, sp_v0| {
        let four = b.build_int_const(4u64, NodeOutputType::U32)?;

        // arg1 at sp + 4 (rel = +4 from sp_v0; will be rel = 8 from
        // anchor at sp - 4 below).
        let sp_plus_4 =
            b.build_int_binary_operation(sp_v0, four, IntBinaryOp::Add, NodeOutputType::U32)?;
        let arg1 = b.build_int_const(22u64, NodeOutputType::U32)?;
        b.build_store(sp_plus_4, arg1, rsleigh::VnSpace::RAM)?;

        // Implicit `call` ret-addr push at sp - 4 — chain anchor.
        let sp_minus_4 =
            b.build_int_sub(sp_v0, four, NodeOutputType::U32)?;
        b.write_variable(&sp, sp_minus_4)?;
        let retaddr = b.build_int_const(0x1234u64, NodeOutputType::U32)?;
        b.build_store(sp_minus_4, retaddr, rsleigh::VnSpace::RAM)?;

        let target = b.build_int_const(0x1000u64, NodeOutputType::U32)?;
        b.build_call(target)?;
        b.build_return(None, &[])?;
        Ok(())
    })?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    // x86 cdecl-style: ret addr at offset 0, args at +4 and +8.
    pipeline.add_post_pass(CallStackArgCollect::new(vec![4, 8], sp));
    pipeline.run(&mut fg.graph, fg.entry)?;

    let call_id = find_call((&fg).into())?;
    let inputs: Vec<NodeOutputId> = fg.node_inputs(call_id).into_iter().collect();
    // ctrl + memory + target only — slot 1 was filled but slot 0's hole
    // truncates the dense prefix to empty, so no args appended.
    assert_eq!(
        inputs.len(),
        3,
        "missing slot 0 must drop the slot-1 fill from the appended args; got {inputs:?}"
    );
    Ok(())
}

/// A call with no stack stores before it must not have any inputs
/// added.
#[test]
fn call_with_no_stack_stores_unchanged() -> Result<()> {
    let sp = sp_vn();
    let mut fg = ir::test_utils::make_sp_fn(sp, |b, _sp_val| {
        let target = b.build_int_const(0x1000u64, NodeOutputType::U32)?;
        b.build_call(target)?;
        b.build_return(None, &[])?;
        Ok(())
    })?;

    let before_inputs = fg.node_inputs(find_call((&fg).into())?).into_iter().count();

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    pipeline.add_post_pass(CallStackArgCollect::new(vec![0, 4, 8], sp));
    pipeline.run(&mut fg.graph, fg.entry)?;

    let after_inputs = fg.node_inputs(find_call((&fg).into())?).into_iter().count();
    assert_eq!(
        before_inputs, after_inputs,
        "no args should have been collected"
    );
    Ok(())
}

// ── Comprehensive tests added in Task 5 ──────────────────────────────────────

/// SP arithmetic mixing Add and Sub of constants in both directions:
/// `((sp + 16) - 4) - 4 = sp + 8`. Must reduce via decompose_sp's recursive
/// shifted handling.
#[test]
fn detect_mixed_add_sub_reduces() -> Result<()> {
    let sp = sp_vn();
    let mut fg = ir::test_utils::make_sp_fn(sp, |b, sp_v| {
        let s16 = b.build_int_const(16u64, NodeOutputType::U32)?;
        let s4 = b.build_int_const(4u64, NodeOutputType::U32)?;
        let plus16 =
            b.build_int_binary_operation(sp_v, s16, IntBinaryOp::Add, NodeOutputType::U32)?;
        let minus4a =
            b.build_int_sub(plus16, s4, NodeOutputType::U32)?;
        let minus4b =
            b.build_int_sub(minus4a, s4, NodeOutputType::U32)?;
        let data = b.build_int_const(0x42u64, NodeOutputType::U32)?;
        b.build_store(minus4b, data, rsleigh::VnSpace::RAM)?;
        b.build_return(None, &[])?;
        Ok(())
    })?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    pipeline.run(&mut fg.graph, fg.entry)?;

    let stack_stores = count((&fg).into(), |k| matches!(k, NodeKind::StackStore { offset: 8, .. }));
    assert_eq!(
        stack_stores, 1,
        "((sp+16)-4)-4 must reduce to a single StackStore at offset 8"
    );
    Ok(())
}

/// A non-SP base (an `Add` of a non-SP register and a constant) must NOT be
/// rewritten — `decompose_sp` returns `None` and the original Store stays.
#[test]
fn detect_non_sp_base_skipped() -> Result<()> {
    let sp = sp_vn();
    // A second register at a different offset that's not SP.
    let other = rsleigh::Vn {
        addr_off: 0x10,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 4,
    };
    let mut b = FunctionBuilder::new_raw(vec![sp, other], &[other], &[sp], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);
    let other_v = b.read_variable(&other)?;
    let four = b.build_int_const(4u64, NodeOutputType::U32)?;
    let addr =
        b.build_int_binary_operation(other_v, four, IntBinaryOp::Add, NodeOutputType::U32)?;
    let data = b.build_int_const(0x42u64, NodeOutputType::U32)?;
    b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
    b.build_return(None, &[])?;
    let mut fg = b.build()?;

    StackStoreDetect::new(sp).optimize(&mut fg.graph, fg.entry)?;

    assert_eq!(
        count((&fg).into(), |k| matches!(k, NodeKind::StackStore { .. })),
        0,
        "non-SP base must not become a StackStore"
    );
    assert_eq!(
        count((&fg).into(), |k| matches!(k, NodeKind::Store(_))),
        1,
        "the original Store must remain"
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
    let sp = sp_vn();
    let mut fg = ir::test_utils::make_sp_fn(sp, |b, sp_v0| {
        // push arg1 (= 22) at sp - 4.
        let four = b.build_int_const(4u64, NodeOutputType::U32)?;
        let sp_v1 =
            b.build_int_sub(sp_v0, four, NodeOutputType::U32)?;
        b.write_variable(&sp, sp_v1)?;
        let arg1 = b.build_int_const(22u64, NodeOutputType::U32)?;
        b.build_store(sp_v1, arg1, rsleigh::VnSpace::RAM)?;

        // Trash store at sp + 0 (in stack-arg-offset range for the cdecl
        // table {0, 4, 8, 12}).  This addresses the same memory class as
        // arg slots; the walker must not silently pass through it.
        let trash = b.build_int_const(0xAAAAu64, NodeOutputType::U32)?;
        b.build_store(sp_v0, trash, rsleigh::VnSpace::RAM)?;

        // push arg0 (= 11) at sp - 8.
        let sp_v2 =
            b.build_int_sub(sp_v1, four, NodeOutputType::U32)?;
        b.write_variable(&sp, sp_v2)?;
        let arg0 = b.build_int_const(11u64, NodeOutputType::U32)?;
        b.build_store(sp_v2, arg0, rsleigh::VnSpace::RAM)?;

        // call.
        let target = b.build_int_const(0x1000u64, NodeOutputType::U32)?;
        b.build_call(target)?;
        b.build_return(None, &[])?;
        Ok(())
    })?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    pipeline.add_post_pass(CallStackArgCollect::new(vec![0, 4, 8, 12], sp));
    pipeline.run(&mut fg.graph, fg.entry)?;

    let call_id = find_call((&fg).into())?;
    let inputs: Vec<NodeOutputId> = fg.node_inputs(call_id).into_iter().collect();
    let collected_arg_consts: Vec<u128> = inputs[3..]
        .iter()
        .filter_map(|&out| {
            if let NodeKind::IntConst(v) = *fg.kind_of_output(out) {
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

/// NEW behaviour: a `Store` to a constant address (e.g. a `.data` global) on
/// the memory chain between stack-arg pushes and a `Call` must NOT terminate
/// the walker.  Such stores cannot alias the stack-arg space, so the walker
/// should pass through them and continue collecting the upstream stack-args.
///
/// Models the `volatile int g_sink_int = …;` barrier-pattern that gcc/clang
/// at -O2 freely interleave with stack-arg pushes — the cause #2
/// reproducer.
#[test]
fn walker_passes_through_non_aliasing_global_store() -> Result<()> {
    let sp = sp_vn();
    let mut fg = ir::test_utils::make_sp_fn(sp, |b, sp_v0| {
        // push arg1 = 22 at sp - 4.
        let four = b.build_int_const(4u64, NodeOutputType::U32)?;
        let sp_v1 =
            b.build_int_sub(sp_v0, four, NodeOutputType::U32)?;
        b.write_variable(&sp, sp_v1)?;
        let arg1 = b.build_int_const(22u64, NodeOutputType::U32)?;
        b.build_store(sp_v1, arg1, rsleigh::VnSpace::RAM)?;

        // Volatile global write: store to a fixed `.data` address (constant).
        // `decompose_sp` returns None for a non-SP-rooted address; the new
        // walker branch must continue past it.
        let global_addr = b.build_int_const(0xDEAD_BEEFu64, NodeOutputType::U32)?;
        let global_data = b.build_int_const(0x1234u64, NodeOutputType::U32)?;
        b.build_store(global_addr, global_data, rsleigh::VnSpace::RAM)?;

        // push arg0 = 11 at sp - 8.
        let sp_v2 =
            b.build_int_sub(sp_v1, four, NodeOutputType::U32)?;
        b.write_variable(&sp, sp_v2)?;
        let arg0 = b.build_int_const(11u64, NodeOutputType::U32)?;
        b.build_store(sp_v2, arg0, rsleigh::VnSpace::RAM)?;

        // call 0x1000.
        let target = b.build_int_const(0x1000u64, NodeOutputType::U32)?;
        b.build_call(target)?;
        b.build_return(None, &[])?;
        Ok(())
    })?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    // cdecl-style offsets: ret-addr at 0, args at +4, +8, +12.
    pipeline.add_post_pass(CallStackArgCollect::new(vec![0, 4, 8, 12], sp));
    pipeline.run(&mut fg.graph, fg.entry)?;

    let call_id = find_call((&fg).into())?;
    let inputs: Vec<NodeOutputId> = fg.node_inputs(call_id).into_iter().collect();
    // ctrl + memory + target + 2 stack args = 5.
    assert_eq!(
        inputs.len(),
        5,
        "walker must pass through the non-aliasing global store and collect both stack args; got inputs={inputs:?}"
    );
    let arg0_kind = *fg.kind_of_output(inputs[3]);
    let arg1_kind = *fg.kind_of_output(inputs[4]);
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

/// NEW behaviour, multi-store stress: many stack-arg pushes interleaved with
/// multiple non-aliasing global stores between every push.  Walker must
/// collect all 4 stack args, mirroring the `forward_16` fixture.
#[test]
fn walker_collects_stack_args_across_volatile_global_writes() -> Result<()> {
    let sp = sp_vn();
    let arg_vals: [u64; 4] = [11, 22, 33, 44];
    let mut fg = ir::test_utils::make_sp_fn(sp, |b, sp_initial| {
        let four = b.build_int_const(4u64, NodeOutputType::U32)?;

        let mut sp_cur = sp_initial;
        let global_data = b.build_int_const(0x1234u64, NodeOutputType::U32)?;
        // Push args in reverse order (arg3 first → highest negative offset),
        // and emit a volatile global store after each push.  Final memory
        // chain (latest first) is:
        //   global → push arg0 → global → push arg1 → global → push arg2 →
        //   global → push arg3 → entry_mem.
        for (i, base_global_addr) in [0xCAFE0000u64, 0xCAFE0010, 0xCAFE0020, 0xCAFE0030]
            .into_iter()
            .enumerate()
        {
            let arg_idx = 3 - i; // push arg3 first, arg0 last.
            sp_cur =
                b.build_int_sub(sp_cur, four, NodeOutputType::U32)?;
            b.write_variable(&sp, sp_cur)?;
            let arg = b.build_int_const(arg_vals[arg_idx], NodeOutputType::U32)?;
            b.build_store(sp_cur, arg, rsleigh::VnSpace::RAM)?;

            // Non-aliasing global write right after each push.
            let g_addr = b.build_int_const(base_global_addr, NodeOutputType::U32)?;
            b.build_store(g_addr, global_data, rsleigh::VnSpace::RAM)?;
        }

        // call 0x1000.
        let target = b.build_int_const(0x1000u64, NodeOutputType::U32)?;
        b.build_call(target)?;
        b.build_return(None, &[])?;
        Ok(())
    })?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    // 4 cdecl-like stack-arg offsets.  arg0 ends up at sp - 4 (anchor),
    // arg1 at sp - 8 (= anchor + 4), arg2 at sp - 12 (= anchor + 8),
    // arg3 at sp - 16 (= anchor + 12).  AArch64-style table starting at 0.
    pipeline.add_post_pass(CallStackArgCollect::new(vec![0, 4, 8, 12], sp));
    pipeline.run(&mut fg.graph, fg.entry)?;

    let call_id = find_call((&fg).into())?;
    let inputs: Vec<NodeOutputId> = fg.node_inputs(call_id).into_iter().collect();
    // ctrl + mem + target + 4 args = 7 inputs.
    assert_eq!(
        inputs.len(),
        7,
        "walker must collect all 4 stack args across 4 interleaved global writes; got inputs={inputs:?}"
    );
    for (slot_idx, expected) in arg_vals.iter().enumerate() {
        let kind = *fg.kind_of_output(inputs[3 + slot_idx]);
        let expected_u128: u128 = (*expected).into();
        assert!(
            matches!(kind, NodeKind::IntConst(v) if v == expected_u128),
            "arg{slot_idx} should be {expected}, got {kind:?}"
        );
    }
    Ok(())
}

// ── Bug #2026-05-02: chain order ≠ slot order ───────────────────────────────
//
// On i386 cdecl, gcc/clang -O2 routinely emits stack-arg pushes in source
// order (arg0 first, arg1 second, …), but the IR memory chain reflects
// program order — the *last* arg stored shows up as the most-recent
// `StackStore` on the chain.  The original walker required successive
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
    let sp = sp_vn();
    let mut fg = ir::test_utils::make_sp_fn(sp, |b, sp_v0| {
        // arg0 = 11 stored at sp + 0  (cdecl: outgoing-args region is at the
        // bottom of the frame, written without first decrementing SP).
        let arg0 = b.build_int_const(11u64, NodeOutputType::U32)?;
        b.build_store(sp_v0, arg0, rsleigh::VnSpace::RAM)?;

        // arg1 = 22 stored at sp + 4.
        let four = b.build_int_const(4u64, NodeOutputType::U32)?;
        let sp_plus_4 =
            b.build_int_binary_operation(sp_v0, four, IntBinaryOp::Add, NodeOutputType::U32)?;
        let arg1 = b.build_int_const(22u64, NodeOutputType::U32)?;
        b.build_store(sp_plus_4, arg1, rsleigh::VnSpace::RAM)?;

        // Implicit `call` ret-addr push at sp - 4.
        let sp_after_call_push =
            b.build_int_sub(sp_v0, four, NodeOutputType::U32)?;
        b.write_variable(&sp, sp_after_call_push)?;
        let retaddr = b.build_int_const(0x1234u64, NodeOutputType::U32)?;
        b.build_store(sp_after_call_push, retaddr, rsleigh::VnSpace::RAM)?;

        let target = b.build_int_const(0x1000u64, NodeOutputType::U32)?;
        b.build_call(target)?;
        b.build_return(None, &[])?;
        Ok(())
    })?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    // x86 cdecl: ret addr at offset 0, args at +4, +8, +12, …
    pipeline.add_post_pass(CallStackArgCollect::new(
        vec![4, 8, 12, 16, 20, 24, 28, 32],
        sp,
    ));
    pipeline.run(&mut fg.graph, fg.entry)?;

    let call_id = find_call((&fg).into())?;
    let inputs: Vec<NodeOutputId> = fg.node_inputs(call_id).into_iter().collect();
    assert_eq!(
        inputs.len(),
        5,
        "expected ctrl+mem+target+2 stack args; got {inputs:?}"
    );
    let arg0_kind = *fg.kind_of_output(inputs[3]);
    let arg1_kind = *fg.kind_of_output(inputs[4]);
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
    let sp = sp_vn();
    let mut fg = ir::test_utils::make_sp_fn(sp, |b, sp_v0| {
        let four = b.build_int_const(4u64, NodeOutputType::U32)?;
        let eight = b.build_int_const(8u64, NodeOutputType::U32)?;

        // arg1 = 22 at sp + 4.
        let sp_plus_4 =
            b.build_int_binary_operation(sp_v0, four, IntBinaryOp::Add, NodeOutputType::U32)?;
        let arg1 = b.build_int_const(22u64, NodeOutputType::U32)?;
        b.build_store(sp_plus_4, arg1, rsleigh::VnSpace::RAM)?;

        // arg0 = 11 at sp + 0.
        let arg0 = b.build_int_const(11u64, NodeOutputType::U32)?;
        b.build_store(sp_v0, arg0, rsleigh::VnSpace::RAM)?;

        // arg2 = 33 at sp + 8.
        let sp_plus_8 =
            b.build_int_binary_operation(sp_v0, eight, IntBinaryOp::Add, NodeOutputType::U32)?;
        let arg2 = b.build_int_const(33u64, NodeOutputType::U32)?;
        b.build_store(sp_plus_8, arg2, rsleigh::VnSpace::RAM)?;

        // Implicit `call` ret-addr push at sp - 4.
        let sp_after_call_push =
            b.build_int_sub(sp_v0, four, NodeOutputType::U32)?;
        b.write_variable(&sp, sp_after_call_push)?;
        let retaddr = b.build_int_const(0x1234u64, NodeOutputType::U32)?;
        b.build_store(sp_after_call_push, retaddr, rsleigh::VnSpace::RAM)?;

        let target = b.build_int_const(0x1000u64, NodeOutputType::U32)?;
        b.build_call(target)?;
        b.build_return(None, &[])?;
        Ok(())
    })?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    pipeline.add_post_pass(CallStackArgCollect::new(
        vec![4, 8, 12, 16, 20, 24, 28, 32],
        sp,
    ));
    pipeline.run(&mut fg.graph, fg.entry)?;

    let call_id = find_call((&fg).into())?;
    let inputs: Vec<NodeOutputId> = fg.node_inputs(call_id).into_iter().collect();
    assert_eq!(
        inputs.len(),
        6,
        "expected ctrl+mem+target+3 stack args; got {inputs:?}"
    );
    for (slot_idx, expected) in [11u128, 22, 33].iter().enumerate() {
        let kind = *fg.kind_of_output(inputs[3 + slot_idx]);
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
    let sp = sp_vn();
    let mut fg = ir::test_utils::make_sp_fn(sp, |b, sp_v0| {
        let four = b.build_int_const(4u64, NodeOutputType::U32)?;

        // Stale arg0 = 0xBAD at sp + 0 (older write).
        let stale = b.build_int_const(0xBADu64, NodeOutputType::U32)?;
        b.build_store(sp_v0, stale, rsleigh::VnSpace::RAM)?;

        // Real arg1 = 22 at sp + 4.
        let sp_plus_4 =
            b.build_int_binary_operation(sp_v0, four, IntBinaryOp::Add, NodeOutputType::U32)?;
        let arg1 = b.build_int_const(22u64, NodeOutputType::U32)?;
        b.build_store(sp_plus_4, arg1, rsleigh::VnSpace::RAM)?;

        // Real arg0 = 11 at sp + 0 — overwrites the stale value.
        let arg0 = b.build_int_const(11u64, NodeOutputType::U32)?;
        b.build_store(sp_v0, arg0, rsleigh::VnSpace::RAM)?;

        // Implicit `call` ret-addr push.
        let sp_after_call_push =
            b.build_int_sub(sp_v0, four, NodeOutputType::U32)?;
        b.write_variable(&sp, sp_after_call_push)?;
        let retaddr = b.build_int_const(0x1234u64, NodeOutputType::U32)?;
        b.build_store(sp_after_call_push, retaddr, rsleigh::VnSpace::RAM)?;

        let target = b.build_int_const(0x1000u64, NodeOutputType::U32)?;
        b.build_call(target)?;
        b.build_return(None, &[])?;
        Ok(())
    })?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    pipeline.add_post_pass(CallStackArgCollect::new(
        vec![4, 8, 12, 16, 20, 24, 28, 32],
        sp,
    ));
    pipeline.run(&mut fg.graph, fg.entry)?;

    let call_id = find_call((&fg).into())?;
    let inputs: Vec<NodeOutputId> = fg.node_inputs(call_id).into_iter().collect();
    assert_eq!(
        inputs.len(),
        5,
        "expected ctrl+mem+target+2 stack args; got {inputs:?}"
    );
    let arg0_kind = *fg.kind_of_output(inputs[3]);
    assert!(
        matches!(arg0_kind, NodeKind::IntConst(11)),
        "arg0 must be the most-recent write (11), not the stale 0xBAD; got {arg0_kind:?}"
    );
    Ok(())
}

/// A `StackStore` whose offset is *outside* the convention's stack-arg
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
    let sp = sp_vn();
    let mut fg = ir::test_utils::make_sp_fn(sp, |b, sp_v0| {
        let four = b.build_int_const(4u64, NodeOutputType::U32)?;
        let sixteen = b.build_int_const(16u64, NodeOutputType::U32)?;

        // Local at sp - 16 (above the outgoing-args region — offset -16 is
        // NOT in the convention's stack-arg slot set).
        let sp_minus_16 =
            b.build_int_sub(sp_v0, sixteen, NodeOutputType::U32)?;
        let local = b.build_int_const(0xDEADu64, NodeOutputType::U32)?;
        b.build_store(sp_minus_16, local, rsleigh::VnSpace::RAM)?;

        // arg1 = 22 at sp - 4.
        let sp_minus_4 =
            b.build_int_sub(sp_v0, four, NodeOutputType::U32)?;
        let arg1 = b.build_int_const(22u64, NodeOutputType::U32)?;
        b.build_store(sp_minus_4, arg1, rsleigh::VnSpace::RAM)?;

        // arg0 = 11 at sp - 8.
        let eight = b.build_int_const(8u64, NodeOutputType::U32)?;
        let sp_minus_8 =
            b.build_int_sub(sp_v0, eight, NodeOutputType::U32)?;
        let arg0 = b.build_int_const(11u64, NodeOutputType::U32)?;
        b.build_store(sp_minus_8, arg0, rsleigh::VnSpace::RAM)?;

        // Implicit `call` ret-addr push at sp - 12.
        let twelve = b.build_int_const(12u64, NodeOutputType::U32)?;
        let sp_minus_12 =
            b.build_int_sub(sp_v0, twelve, NodeOutputType::U32)?;
        b.write_variable(&sp, sp_minus_12)?;
        let retaddr = b.build_int_const(0x1234u64, NodeOutputType::U32)?;
        b.build_store(sp_minus_12, retaddr, rsleigh::VnSpace::RAM)?;

        let target = b.build_int_const(0x1000u64, NodeOutputType::U32)?;
        b.build_call(target)?;
        b.build_return(None, &[])?;
        Ok(())
    })?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    // 2-slot cdecl table: anchor at +0 (ret-addr), arg0 at +4, arg1 at +8.
    pipeline.add_post_pass(CallStackArgCollect::new(vec![4, 8], sp));
    pipeline.run(&mut fg.graph, fg.entry)?;

    let call_id = find_call((&fg).into())?;
    let inputs: Vec<NodeOutputId> = fg.node_inputs(call_id).into_iter().collect();
    let collected: Vec<u128> = inputs[3..]
        .iter()
        .filter_map(|&out| {
            if let NodeKind::IntConst(v) = *fg.kind_of_output(out) {
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
        5,
        "expected ctrl+mem+target+2 stack args; got {inputs:?}"
    );
    let arg0_kind = *fg.kind_of_output(inputs[3]);
    let arg1_kind = *fg.kind_of_output(inputs[4]);
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
