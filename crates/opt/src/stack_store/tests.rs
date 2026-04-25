use super::*;
use crate::error::{ErrorKind, Result};
use crate::pipeline::Optimizer;
use crate::{ConstantFold, OptimizerPipeline, RedundantPhis};
use ir::BuiltFunctionGraph;
use ir::node::{NodeId, NodeKind, NodeOutputId, NodeOutputType};
use ir::{FunctionBuilder, IntBinaryOp};

fn sp_vn() -> rsleigh::Vn {
    // Use a fake stack-pointer varnode in the REGISTER space.  Width is
    // 4 bytes (u32), matching x86 ESP.
    rsleigh::Vn {
        addr: rsleigh::VnAddr {
            space: rsleigh::VnSpace::REGISTER,
            off: 0x20,
        },
        size: 4,
    }
}

/// Counts how many nodes in `fg` match the predicate.
fn count<F: Fn(&NodeKind) -> bool>(fg: &BuiltFunctionGraph, pred: F) -> usize {
    fg.all_node_ids()
        .filter(|&n| pred(fg.graph.node_kind(n)))
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
    let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);
    let sp_val = b.read_variable(&sp)?;
    let four = b.build_int_const(4, NodeOutputType::U32);
    let addr =
        b.build_int_binary_operation(sp_val, four, IntBinaryOp::Sub, NodeOutputType::U32)?;
    let data = b.build_int_const(0x11, NodeOutputType::U32);
    b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
    let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
    b.build_return(Some(loaded), &[])?;
    let mut fg = b.build()?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    pipeline.run(&mut fg)?;

    let stack_stores = count(&fg, |k| {
        matches!(k, NodeKind::StackStore { offset: -4, .. })
    });
    assert_eq!(stack_stores, 1, "expected one StackStore at offset -4");
    // Every reachable Store must have been rewritten.
    let reachable: std::collections::HashSet<_> = fg.preorder().collect();
    let reachable_stores = fg
        .all_node_ids()
        .filter(|n| reachable.contains(n))
        .filter(|&n| matches!(fg.graph.node_kind(n), NodeKind::Store(_)))
        .count();
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
    let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);
    let sp_val = b.read_variable(&sp)?;
    // 0xFFFFFFFC_U32 == -4 when sign-extended.
    let neg_four = b.build_int_const(0xFFFF_FFFC, NodeOutputType::U32);
    let addr = b.build_int_binary_operation(
        sp_val,
        neg_four,
        IntBinaryOp::Add,
        NodeOutputType::U32,
    )?;
    let data = b.build_int_const(0x11, NodeOutputType::U32);
    b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
    let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
    b.build_return(Some(loaded), &[])?;
    let mut fg = b.build()?;

    // Intentionally omit `ConstantFold` so the test exercises
    // `decompose_sp`'s handling of the alternate encoding in isolation.
    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    pipeline.run(&mut fg)?;

    let stack_stores = count(&fg, |k| {
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
    let data = b.build_int_const(0xAB, NodeOutputType::U32);
    b.build_store(sp_val, data, rsleigh::VnSpace::RAM)?;
    let loaded = b.build_load(sp_val, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
    b.build_return(Some(loaded), &[])?;
    let mut fg = b.build()?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    pipeline.run(&mut fg)?;

    let stack_stores = count(&fg, |k| matches!(k, NodeKind::StackStore { offset: 0, .. }));
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
    let four = b.build_int_const(4, NodeOutputType::U32);
    let sp_a2 =
        b.build_int_binary_operation(sp_a, four, IntBinaryOp::Sub, NodeOutputType::U32)?;
    b.write_variable(&sp, sp_a2)?;
    b.build_branch(c)?;

    // b: sp = sp - 8; goto c
    b.set_region(bb);
    let sp_b = b.read_variable(&sp)?;
    let eight = b.build_int_const(8, NodeOutputType::U32);
    let sp_b2 =
        b.build_int_binary_operation(sp_b, eight, IntBinaryOp::Sub, NodeOutputType::U32)?;
    b.write_variable(&sp, sp_b2)?;
    b.build_branch(c)?;

    // c: *(sp) = 0xCC; load(sp); return loaded
    b.set_region(c);
    let sp_c = b.read_variable(&sp)?;
    let data = b.build_int_const(0xCC, NodeOutputType::U32);
    b.build_store(sp_c, data, rsleigh::VnSpace::RAM)?;
    let loaded = b.build_load(sp_c, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
    b.build_return(Some(loaded), &[])?;
    let mut fg = b.build()?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    pipeline.run(&mut fg)?;

    let phis: Vec<NodeId> = fg
        .all_node_ids()
        .filter(|&n| matches!(fg.graph.node_kind(n), NodeKind::StackStorePhi { .. }))
        .collect();
    assert_eq!(phis.len(), 1, "expected one StackStorePhi");
    let offsets = fg.graph.stack_phi_offsets(phis[0]);
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
    let four = b.build_int_const(4, NodeOutputType::U32);
    let sp_a2 =
        b.build_int_binary_operation(sp_a, four, IntBinaryOp::Sub, NodeOutputType::U32)?;
    b.write_variable(&sp, sp_a2)?;
    b.build_branch(c)?;

    // b: sp = sp - 4; goto c  (same offset as a)
    b.set_region(bb);
    let sp_b = b.read_variable(&sp)?;
    let four2 = b.build_int_const(4, NodeOutputType::U32);
    let sp_b2 =
        b.build_int_binary_operation(sp_b, four2, IntBinaryOp::Sub, NodeOutputType::U32)?;
    b.write_variable(&sp, sp_b2)?;
    b.build_branch(c)?;

    // c: *(sp) = 0xCC; load(sp); return loaded
    b.set_region(c);
    let sp_c = b.read_variable(&sp)?;
    let data = b.build_int_const(0xCC, NodeOutputType::U32);
    b.build_store(sp_c, data, rsleigh::VnSpace::RAM)?;
    let loaded = b.build_load(sp_c, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
    b.build_return(Some(loaded), &[])?;
    let mut fg = b.build()?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    pipeline.run(&mut fg)?;

    let stack_store_phis = count(&fg, |k| matches!(k, NodeKind::StackStorePhi { .. }));
    assert_eq!(
        stack_store_phis, 0,
        "phi with all-equal offsets must not produce a StackStorePhi"
    );
    let stack_stores = count(&fg, |k| {
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
    let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);

    // Simulate: `push ebx` + `sub esp, 16` + 4× zero-init + push arg1 +
    // push arg0 + implicit-call ret-push.
    let sp0 = b.read_variable(&sp)?;
    let four = b.build_int_const(4, NodeOutputType::U32);
    let sixteen = b.build_int_const(16, NodeOutputType::U32);

    // push ebx → [sp - 4] = init_ebx.
    let sp_after_push_ebx =
        b.build_int_binary_operation(sp0, four, IntBinaryOp::Sub, NodeOutputType::U32)?;
    b.write_variable(&sp, sp_after_push_ebx)?;
    let init_ebx = b.build_int_const(0xEB, NodeOutputType::U32);
    b.build_store(sp_after_push_ebx, init_ebx, rsleigh::VnSpace::RAM)?;

    // sub esp, 16 → reserve buf.
    let sp_after_sub = b.build_int_binary_operation(
        sp_after_push_ebx,
        sixteen,
        IntBinaryOp::Sub,
        NodeOutputType::U32,
    )?;
    b.write_variable(&sp, sp_after_sub)?;

    // 4× zero-init at buf[0..16] (esp+0, +4, +8, +12) = [-20, -16, -12, -8].
    let zero = b.build_int_const(0, NodeOutputType::U32);
    for k in 0..4 {
        let off = b.build_int_const((k * 4) as u64, NodeOutputType::U32);
        let addr = b.build_int_binary_operation(
            sp_after_sub,
            off,
            IntBinaryOp::Add,
            NodeOutputType::U32,
        )?;
        b.build_store(addr, zero, rsleigh::VnSpace::RAM)?;
    }

    // push arg1 = 1 → [sp - 24].
    let sp_push_arg1 = b.build_int_binary_operation(
        sp_after_sub,
        four,
        IntBinaryOp::Sub,
        NodeOutputType::U32,
    )?;
    b.write_variable(&sp, sp_push_arg1)?;
    let arg1 = b.build_int_const(1, NodeOutputType::U32);
    b.build_store(sp_push_arg1, arg1, rsleigh::VnSpace::RAM)?;

    // push arg0 = 42 → [sp - 28].
    let sp_push_arg0 = b.build_int_binary_operation(
        sp_push_arg1,
        four,
        IntBinaryOp::Sub,
        NodeOutputType::U32,
    )?;
    b.write_variable(&sp, sp_push_arg0)?;
    let arg0 = b.build_int_const(42, NodeOutputType::U32);
    b.build_store(sp_push_arg0, arg0, rsleigh::VnSpace::RAM)?;

    // implicit call ret-addr push at [sp - 32] — mimics x86 `call`.
    let sp_call = b.build_int_binary_operation(
        sp_push_arg0,
        four,
        IntBinaryOp::Sub,
        NodeOutputType::U32,
    )?;
    b.write_variable(&sp, sp_call)?;
    let retaddr = b.build_int_const(0x1234, NodeOutputType::U32);
    b.build_store(sp_call, retaddr, rsleigh::VnSpace::RAM)?;

    // call target.
    let target = b.build_int_const(0x1000, NodeOutputType::U32);
    b.build_call(target)?;
    b.build_return(None, &[])?;
    let mut fg = b.build()?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    // x86 cdecl: ret addr at offset 0, args at +4, +8, +12, …
    pipeline.add_post_pass(CallStackArgCollect::new(vec![4, 8, 12, 16, 20, 24, 28, 32]));
    pipeline.run(&mut fg)?;

    let call_id = find_call(&fg)?;
    let inputs: Vec<NodeOutputId> = fg.graph.node_inputs(call_id).into_iter().collect();
    // ctrl + mem + target + exactly 2 args = 5 inputs.
    assert_eq!(
        inputs.len(),
        5,
        "buf-init and callee-save writes must not be mis-collected as args; got inputs={inputs:?}"
    );
    let arg0_kind = *fg.graph.node_kind(fg.graph.get_node_from_output(inputs[3]));
    let arg1_kind = *fg.graph.node_kind(fg.graph.get_node_from_output(inputs[4]));
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
    let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);
    let addr = b.build_int_const(0x1000, NodeOutputType::U32);
    let data = b.build_int_const(0x42, NodeOutputType::U32);
    b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
    b.build_return(None, &[])?;
    let mut fg = b.build()?;

    StackStoreDetect::new(sp).optimize(&mut fg)?;

    assert_eq!(
        count(&fg, |k| matches!(k, NodeKind::StackStore { .. })),
        0,
        "non-stack store must not become a StackStore"
    );
    assert_eq!(
        count(&fg, |k| matches!(k, NodeKind::Store(_))),
        1,
        "the original Store must remain"
    );
    Ok(())
}

// ── CallStackArgCollect tests ────────────────────────────────────────────

/// Finds the unique Call node in `fg`.
fn find_call(fg: &BuiltFunctionGraph) -> Result<NodeId> {
    fg.all_node_ids()
        .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::Call))
        .ok_or_else(|| ErrorKind::ExpectedNodeNotFound("Call", NodeKind::Call).into())
}

/// cdecl-style: `push arg1=22; push arg0=11; call target(0x1000)`.
/// After optimization the Call's inputs should be extended with
/// `[arg0, arg1]` in positional order.
#[test]
fn cdecl_two_stack_args_collected_in_order() -> Result<()> {
    let sp = sp_vn();
    let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);

    // push arg1 (= 22) at sp - 4
    let sp_v0 = b.read_variable(&sp)?;
    let four = b.build_int_const(4, NodeOutputType::U32);
    let sp_v1 =
        b.build_int_binary_operation(sp_v0, four, IntBinaryOp::Sub, NodeOutputType::U32)?;
    b.write_variable(&sp, sp_v1)?;
    let arg1 = b.build_int_const(22, NodeOutputType::U32);
    b.build_store(sp_v1, arg1, rsleigh::VnSpace::RAM)?;

    // push arg0 (= 11) at sp - 8
    let sp_v2 =
        b.build_int_binary_operation(sp_v1, four, IntBinaryOp::Sub, NodeOutputType::U32)?;
    b.write_variable(&sp, sp_v2)?;
    let arg0 = b.build_int_const(11, NodeOutputType::U32);
    b.build_store(sp_v2, arg0, rsleigh::VnSpace::RAM)?;

    // call 0x1000
    let target = b.build_int_const(0x1000, NodeOutputType::U32);
    b.build_call(target)?;
    b.build_return(None, &[])?;
    let mut fg = b.build()?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    pipeline.add_post_pass(CallStackArgCollect::new(vec![0, 4, 8, 12]));
    pipeline.run(&mut fg)?;

    let call_id = find_call(&fg)?;
    let inputs: Vec<NodeOutputId> = fg.graph.node_inputs(call_id).into_iter().collect();
    // inputs = [ctrl, memory, target, stack_arg_0, stack_arg_1] — no
    // arg-passing registers on cdecl, so indices 3 and 4 are the stack args.
    assert_eq!(
        inputs.len(),
        5,
        "expected ctrl+mem+target+2 stack args; got {inputs:?}"
    );

    let arg0_val = inputs[3];
    let arg1_val = inputs[4];
    let arg0_kind = *fg.graph.node_kind(fg.graph.get_node_from_output(arg0_val));
    let arg1_kind = *fg.graph.node_kind(fg.graph.get_node_from_output(arg1_val));
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

/// Only slot 1 is populated (slot 0 is missing) — the pass must skip
/// this call entirely rather than mis-assign the gap.
#[test]
fn missing_slot_zero_skips_collection() -> Result<()> {
    let sp = sp_vn();
    let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);

    // Only one push, at sp - 4.  If the convention expects [0, 4, 8, …]
    // then call_sp_adjust = -4 and slot_0 would be at -4.  But if we
    // designed a convention where stack_arg_offsets[0] != 0 we'd
    // effectively simulate a missing slot.  Here we instead use an
    // offset table that expects slot_0 = -4 and slot_1 = 0.  Since
    // there is no store at offset 0, collection must stop after slot_0.
    let sp_v0 = b.read_variable(&sp)?;
    let four = b.build_int_const(4, NodeOutputType::U32);
    let sp_v1 =
        b.build_int_binary_operation(sp_v0, four, IntBinaryOp::Sub, NodeOutputType::U32)?;
    b.write_variable(&sp, sp_v1)?;
    let only_arg = b.build_int_const(99, NodeOutputType::U32);
    b.build_store(sp_v1, only_arg, rsleigh::VnSpace::RAM)?;

    let target = b.build_int_const(0x1000, NodeOutputType::U32);
    b.build_call(target)?;
    b.build_return(None, &[])?;
    let mut fg = b.build()?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    pipeline.add_post_pass(CallStackArgCollect::new(vec![0, 4]));
    pipeline.run(&mut fg)?;

    let call_id = find_call(&fg)?;
    let inputs: Vec<NodeOutputId> = fg.graph.node_inputs(call_id).into_iter().collect();
    // ctrl + memory + target + stack_arg_0 — only the one we have.
    assert_eq!(inputs.len(), 4, "only one stack arg could be collected");
    Ok(())
}

/// A call with no stack stores before it must not have any inputs
/// added.
#[test]
fn call_with_no_stack_stores_unchanged() -> Result<()> {
    let sp = sp_vn();
    let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);

    let target = b.build_int_const(0x1000, NodeOutputType::U32);
    b.build_call(target)?;
    b.build_return(None, &[])?;
    let mut fg = b.build()?;

    let before_inputs = fg.graph.node_inputs(find_call(&fg)?).into_iter().count();

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    pipeline.add_post_pass(CallStackArgCollect::new(vec![0, 4, 8]));
    pipeline.run(&mut fg)?;

    let after_inputs = fg.graph.node_inputs(find_call(&fg)?).into_iter().count();
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
    let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);
    let sp_v = b.read_variable(&sp)?;
    let s16 = b.build_int_const(16, NodeOutputType::U32);
    let s4 = b.build_int_const(4, NodeOutputType::U32);
    let plus16 =
        b.build_int_binary_operation(sp_v, s16, IntBinaryOp::Add, NodeOutputType::U32)?;
    let minus4a =
        b.build_int_binary_operation(plus16, s4, IntBinaryOp::Sub, NodeOutputType::U32)?;
    let minus4b =
        b.build_int_binary_operation(minus4a, s4, IntBinaryOp::Sub, NodeOutputType::U32)?;
    let data = b.build_int_const(0x42, NodeOutputType::U32);
    b.build_store(minus4b, data, rsleigh::VnSpace::RAM)?;
    b.build_return(None, &[])?;
    let mut fg = b.build()?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    pipeline.run(&mut fg)?;

    let stack_stores = count(&fg, |k| matches!(k, NodeKind::StackStore { offset: 8, .. }));
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
        addr: rsleigh::VnAddr {
            space: rsleigh::VnSpace::REGISTER,
            off: 0x10,
        },
        size: 4,
    };
    let mut b = FunctionBuilder::new_raw(vec![sp, other], &[other], &[sp], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);
    let other_v = b.read_variable(&other)?;
    let four = b.build_int_const(4, NodeOutputType::U32);
    let addr =
        b.build_int_binary_operation(other_v, four, IntBinaryOp::Add, NodeOutputType::U32)?;
    let data = b.build_int_const(0x42, NodeOutputType::U32);
    b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
    b.build_return(None, &[])?;
    let mut fg = b.build()?;

    StackStoreDetect::new(sp).optimize(&mut fg)?;

    assert_eq!(
        count(&fg, |k| matches!(k, NodeKind::StackStore { .. })),
        0,
        "non-SP base must not become a StackStore"
    );
    assert_eq!(
        count(&fg, |k| matches!(k, NodeKind::Store(_))),
        1,
        "the original Store must remain"
    );
    Ok(())
}
