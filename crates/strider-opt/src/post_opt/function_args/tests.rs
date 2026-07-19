use super::*;
use crate::error::Result;
use crate::test_support::cf_rp_pipeline;
use strider_ir::node::{NodeKind, ValueId, ValueType};
use strider_ir::{FunctionBuilder, IRBuilderExt, IRViewer, IntBinaryOp};
use strider_ir_test_utils::IrBuilderEx;
use strider_ir_test_utils::IrWalkerEx;
use strider_ir_test_utils::{
    RegisterSet, SENTINEL_LIFT_ADDR, reg_vn, stack_vn_aarch64, stack_vn_x86 as sp32_vn,
    stack_vn_x86_64 as stack_vn,
};

fn rdi_like_vn() -> rsleigh::Vn {
    // Stands in for x86_64 RDI.
    reg_vn(0x38, 8)
}

/// A register arg is recorded at builder entry, before any pass runs, and the
/// stack-only `FunctionArgDetect` must leave it and its `InitialVar` alone.
#[test]
fn reads_rdi_emits_function_arg_0() -> Result<()> {
    let rdi = rdi_like_vn();
    let sp = stack_vn();
    let mut b = RegisterSet::new()
        .tracked(rdi)
        .tracked(sp)
        .arg(rdi)
        .callee_saved(rdi)
        .ret(rdi)
        .build_fn_single_region()?;

    // Read rdi and return it.
    let v = b.read_variable(&rdi)?;
    b.build_return(Some(v), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let pass = FunctionArgDetect;
    crate::pipeline::run_post(&pass, &mut fg, &mut crate::OptCtx::new(None))?;

    let arg0_nodes = fg.side_tables().arg_index_to_values(0);
    assert!(
        !arg0_nodes.is_empty(),
        "arg 0 should be registered in the side-table"
    );
    assert_eq!(
        arg0_nodes.len(),
        1,
        "exactly one carrier for register arg 0"
    );
    let carrier = fg.producer(arg0_nodes[0]);
    assert!(
        matches!(fg.node_kind(carrier), NodeKind::InitialVar(v) if fg.initial_vn(*v) ==rdi),
        "carrier for arg 0 must be InitialVar(rdi)"
    );

    // Still reachable, since the pass rewires no consumers.
    let reachable_initial_rdi =
        fg.count_kind(|k| matches!(k, NodeKind::InitialVar(v) if fg.initial_vn(*v) ==rdi));
    assert_eq!(
        reachable_initial_rdi, 1,
        "InitialVar(rdi) must remain reachable after the pass"
    );
    Ok(())
}

/// The pass can be applied repeatedly to one `Function`, so a re-run must not
/// accumulate duplicate carrier ids.
#[test]
fn rerunning_pass_is_idempotent_no_duplicate_carriers() -> Result<()> {
    let rdi = rdi_like_vn();
    let sp = stack_vn();
    let mut b = RegisterSet::new()
        .tracked(rdi)
        .tracked(sp)
        .arg(rdi)
        .callee_saved(rdi)
        .ret(rdi)
        .build_fn_single_region()?;

    let v = b.read_variable(&rdi)?;
    b.build_return(Some(v), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let pass = FunctionArgDetect;
    crate::pipeline::run_post(&pass, &mut fg, &mut crate::OptCtx::new(None))?;
    let after_first = fg.side_tables().arg_index_to_values(0).to_vec();
    // Re-run on the same function.
    crate::pipeline::run_post(&pass, &mut fg, &mut crate::OptCtx::new(None))?;
    let after_second = fg.side_tables().arg_index_to_values(0).to_vec();

    assert_eq!(
        after_first, after_second,
        "re-running FunctionArgDetect must not change the carrier set (idempotent)"
    );
    assert_eq!(
        after_second.len(),
        1,
        "exactly one carrier for register arg 0 after re-run, not a duplicate"
    );
    Ok(())
}

/// x86 cdecl reads its first stack arg at `[sp + 4]`, and with no register
/// args that `Load` is arg 0.  It must also remain reachable.
#[test]
fn reads_stack_arg_0_on_x86_cdecl() -> Result<()> {
    let sp = sp32_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_args(Some(strider_target::StackArgs {
            base_offset: 4,
            increment: 8,
        }))
        .build_fn_single_region()?;
    let sp_val = b.read_variable(&sp)?;
    {
        let four = b.build_int_const(4u64, ValueType::I32)?;
        let addr = b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, ValueType::I32)?;
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::I32)?;
        b.build_return(Some(loaded), &[])?;
    }
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    // ConstantFold normalises the address before detection runs.
    let mut pipeline = cf_rp_pipeline();
    pipeline.add_post_pass(FunctionArgDetect);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let arg0_nodes = fg.side_tables().arg_index_to_values(0);
    assert!(!arg0_nodes.is_empty(), "arg 0 should be registered (stack)");
    assert_eq!(arg0_nodes.len(), 1, "one Load at sp+4, so one carrier");
    assert!(
        matches!(fg.node_kind(fg.producer(arg0_nodes[0])), NodeKind::Load(_)),
        "carrier for stack arg 0 must be a Load node"
    );

    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 1,
        "Load[sp+4] must remain reachable after the pass"
    );
    Ok(())
}

/// A `Load` rooted at an alignment-masked SP addresses a frame local, not an
/// incoming arg, so only an `InitialVar(sp)` base qualifies.
#[test]
fn aligned_sp_load_is_not_a_stack_arg() -> Result<()> {
    let sp = sp32_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_args(Some(strider_target::StackArgs {
            base_offset: 4,
            increment: 8,
        }))
        .build_fn_single_region()?;
    let sp_val = b.read_variable(&sp)?;
    let mask = b.build_int_const(0xFFFF_FFF8u64, ValueType::I32)?;
    let aligned = b.build_int_binary_operation(sp_val, mask, IntBinaryOp::And, ValueType::I32)?;
    let four = b.build_int_const(4u64, ValueType::I32)?;
    let addr = b.build_int_binary_operation(aligned, four, IntBinaryOp::Add, ValueType::I32)?;
    let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::I32)?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    pipeline.add_post_pass(FunctionArgDetect);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    assert!(
        fg.side_tables().arg_index_to_values(0).is_empty(),
        "a load rooted at an alignment-masked SP (not the entry SP) must not \
         register as stack arg 0"
    );
    Ok(())
}

fn build_sp_load(
    b: &mut FunctionBuilder,
    sp: &rsleigh::Vn,
    offset: u32,
) -> Result<strider_ir::node::ValueId> {
    let sp_val = b.read_variable(sp)?;
    let off_const = b.build_int_const(offset as u64, ValueType::I32)?;
    let addr = b.build_int_binary_operation(sp_val, off_const, IntBinaryOp::Add, ValueType::I32)?;
    let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::I32)?;
    Ok(loaded)
}

/// Ten incoming stack args, proving the `StackArgs` formula has no upper bound.
#[test]
fn detects_ten_contiguous_stack_args() -> Result<()> {
    const N: usize = 10;
    let sp = sp32_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_args(Some(strider_target::StackArgs {
            base_offset: 0,
            increment: 8,
        }))
        .build_fn_single_region()?;
    let _sp_val = b.read_variable(&sp)?;
    // No intervening stores, so each load reads InitialMemory.
    let mut acc = None;
    for i in 0..N {
        let loaded = build_sp_load(&mut b, &sp, (i * 8) as u32)?;
        acc = Some(match acc {
            None => loaded,
            Some(prev) => {
                b.build_int_binary_operation(prev, loaded, IntBinaryOp::Add, ValueType::I32)?
            }
        });
    }
    b.build_return(acc, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    pipeline.add_post_pass(FunctionArgDetect);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    for i in 0..N {
        assert!(
            !fg.side_tables().arg_index_to_values(i as u32).is_empty(),
            "arg {i} (sp + {}) must be registered",
            i * 8
        );
    }
    Ok(())
}

/// Loads at sp+4 and sp+12 but not sp+8: only the contiguous prefix is
/// labelled, so the sp+12 load is left unregistered.  No gap-spanning.
#[test]
fn stack_arg_gap_truncates() -> Result<()> {
    let sp = sp32_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_args(Some(strider_target::StackArgs {
            base_offset: 4,
            increment: 4,
        }))
        .build_fn_single_region()?;
    let _sp_val = b.read_variable(&sp)?;
    let a = build_sp_load(&mut b, &sp, 4)?;
    let c = build_sp_load(&mut b, &sp, 12)?;
    // Combined so neither load is dead.
    let sum = b.build_int_binary_operation(a, c, IntBinaryOp::Add, ValueType::I32)?;
    b.build_return(Some(sum), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    pipeline.add_post_pass(FunctionArgDetect);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    // The gap at arg 1 means arg 2 must not be registered either.
    let arg0_nodes = fg.side_tables().arg_index_to_values(0);
    assert!(!arg0_nodes.is_empty(), "arg 0 (sp+4) should be registered");

    let arg1_nodes = fg.side_tables().arg_index_to_values(1);
    assert!(
        arg1_nodes.is_empty(),
        "arg 1 (sp+8) is absent — nothing at that offset"
    );

    let arg2_nodes = fg.side_tables().arg_index_to_values(2);
    assert!(
        arg2_nodes.is_empty(),
        "arg 2 (sp+12) must be truncated by the gap"
    );

    // The pass removes no nodes.
    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 2,
        "both Load nodes (sp+4 and sp+12) must remain reachable"
    );
    Ok(())
}

/// A `Load[sp+4]` reached through disjoint stores at +8 and +12 is still arg 0,
/// and the walker also narrows its memory edge onto `InitialMemory`.  Narrowing
/// never changes which args are detected.
#[test]
fn stack_arg_load_chain_is_narrowed_without_changing_detection() -> Result<()> {
    let sp = sp32_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_args(Some(strider_target::StackArgs {
            base_offset: 4,
            increment: 8,
        }))
        .build_fn_single_region()?;
    let sp_val = b.read_variable(&sp)?;
    // Two disjoint stores, then the stack-arg load.
    for off in [8u64, 12u64] {
        let o = b.build_int_const(off, ValueType::I32)?;
        let addr = b.build_int_binary_operation(sp_val, o, IntBinaryOp::Add, ValueType::I32)?;
        let v = b.build_int_const(off, ValueType::I32)?;
        b.build_store(addr, v, rsleigh::VnSpace::RAM)?;
    }
    let four = b.build_int_const(4u64, ValueType::I32)?;
    let addr = b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, ValueType::I32)?;
    let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::I32)?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    pipeline.add_post_pass(FunctionArgDetect);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let arg0 = fg.side_tables().arg_index_to_values(0).to_vec();
    assert_eq!(arg0.len(), 1, "Load[sp+4] registered as arg 0");

    // The carrier load's memory input skipped both disjoint stores.
    let load = fg.producer(arg0[0]);
    let mem = fg.node_inputs(load)[0];
    assert!(
        matches!(fg.node_kind(fg.producer(mem)), NodeKind::InitialMemory),
        "stack-arg load narrowed past disjoint stores onto InitialMemory, got {:?}",
        fg.node_kind(fg.producer(mem)),
    );
    Ok(())
}

/// A prior store at +4 shadows the load, which then reads the stored value
/// rather than the caller's arg.
#[test]
fn prior_stackstore_shadows() -> Result<()> {
    let sp = sp32_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_args(Some(strider_target::StackArgs {
            base_offset: 4,
            increment: 8,
        }))
        .build_fn_single_region()?;
    let sp_val = b.read_variable(&sp)?;
    let four = b.build_int_const(4u64, ValueType::I32)?;
    let addr = b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, ValueType::I32)?;
    let data = b.build_int_const(0x11u64, ValueType::I32)?;
    b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
    let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::I32)?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    pipeline.add_post_pass(FunctionArgDetect);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let arg0_nodes = fg.side_tables().arg_index_to_values(0);
    assert!(
        arg0_nodes.is_empty(),
        "Load[sp+4] is shadowed by Store(sp+4), must not be registered as arg"
    );
    Ok(())
}

/// One branch stores at +4 and the other does nothing, so a later
/// `Load[sp+4]` from their `MemPhi` join must be disqualified: every
/// predecessor of a phi has to be clean.
#[test]
fn memphi_shadow_disqualifies() -> Result<()> {
    let sp = sp32_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_args(Some(strider_target::StackArgs {
            base_offset: 4,
            increment: 8,
        }))
        .build_fn()?;
    let entry = b.create_region_all()?;
    let true_br = b.create_region_all()?;
    let false_br = b.create_region_all()?;
    let join = b.create_region_all()?;
    b.set_entry_region_all(entry)?;

    // A boolean const keeps the MemPhi at two predecessors.
    // DeadBranchElimination would collapse it, so that pass is skipped here.
    b.set_region(entry);
    let cond = b.build_boolean_const(true);
    b.build_if(cond, true_br, false_br)?;

    b.set_region(true_br);
    let sp_t = b.read_variable(&sp)?;
    let four_t = b.build_int_const(4u64, ValueType::I32)?;
    let addr_t = b.build_int_binary_operation(sp_t, four_t, IntBinaryOp::Add, ValueType::I32)?;
    let data = b.build_int_const(0x22u64, ValueType::I32)?;
    b.build_store(addr_t, data, rsleigh::VnSpace::RAM)?;
    b.build_branch(join)?;

    b.set_region(false_br);
    b.build_branch(join)?;

    b.set_region(join);
    let sp_j = b.read_variable(&sp)?;
    let four_j = b.build_int_const(4u64, ValueType::I32)?;
    let addr_j = b.build_int_binary_operation(sp_j, four_j, IntBinaryOp::Add, ValueType::I32)?;
    let loaded = b.build_load(addr_j, rsleigh::VnSpace::RAM, ValueType::I32)?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    pipeline.add_post_pass(FunctionArgDetect);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let arg0_nodes = fg.side_tables().arg_index_to_values(0);
    assert!(
        arg0_nodes.is_empty(),
        "Load[sp+4] reaches a MemPhi with a shadowing branch — must not be registered"
    );
    Ok(())
}

/// One slot read at two widths, as aarch64 does with `x0` and `w0`, must
/// register BOTH `Load` nodes under index 0.
#[test]
fn narrower_load_at_arg_slot_uses_truncate() -> Result<()> {
    let sp = stack_vn_aarch64();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_args(Some(strider_target::StackArgs {
            base_offset: 0,
            increment: 8,
        }))
        .build_fn_single_region()?;
    let sp_val = b.read_variable(&sp)?;
    // Read sp+0 as I32 then as I64, combined so neither is dead.
    let narrow = b.build_load(sp_val, rsleigh::VnSpace::RAM, ValueType::I32)?;
    let wide = b.build_load(sp_val, rsleigh::VnSpace::RAM, ValueType::I64)?;
    let narrow_ext =
        b.extend_if_needed(narrow, ValueType::I64, strider_ir::ExtendOp::ZeroExtend)?;
    let sum = b.build_int_binary_operation(narrow_ext, wide, IntBinaryOp::Add, ValueType::I64)?;
    b.build_return(Some(sum), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    pipeline.add_post_pass(FunctionArgDetect);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let arg0_nodes = fg.side_tables().arg_index_to_values(0);
    assert_eq!(
        arg0_nodes.len(),
        2,
        "both Loads at sp+0 (I32 and I64) must be registered for arg 0"
    );
    assert!(
        arg0_nodes
            .iter()
            .all(|&v| matches!(fg.node_kind(fg.producer(v)), NodeKind::Load(_))),
        "all registered carriers for stack arg 0 must be Load nodes"
    );

    // The pass removes neither.
    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 2,
        "both Loads must remain reachable after the pass"
    );
    Ok(())
}

/// 32-bit cdecl `f(double a, int b)`: `a` spans two slots at `sp+4`, `b` sits
/// at `sp+12`, and they must come out as ordinals 0 and 1.
#[test]
fn wide_arg_then_narrow_arg_indexed_by_ordinal() -> Result<()> {
    let sp = sp32_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_args(Some(strider_target::StackArgs {
            base_offset: 4,
            increment: 4,
        }))
        .build_fn_single_region()?;
    let sp_val = b.read_variable(&sp)?;
    // The 8-byte `double`, spanning slots 0 and 1.
    let four = b.build_int_const(4u64, ValueType::I32)?;
    let addr_a = b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, ValueType::I32)?;
    let a = b.build_load(addr_a, rsleigh::VnSpace::RAM, ValueType::I64)?;
    // The `int`, at slot 2.
    let twelve = b.build_int_const(12u64, ValueType::I32)?;
    let addr_b = b.build_int_binary_operation(sp_val, twelve, IntBinaryOp::Add, ValueType::I32)?;
    let bv = b.build_load(addr_b, rsleigh::VnSpace::RAM, ValueType::I32)?;
    let b_ext = b.extend_if_needed(bv, ValueType::I64, strider_ir::ExtendOp::ZeroExtend)?;
    let sum = b.build_int_binary_operation(a, b_ext, IntBinaryOp::Add, ValueType::I64)?;
    b.build_return(Some(sum), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    pipeline.add_post_pass(FunctionArgDetect);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let arg0 = fg.side_tables().arg_index_to_values(0);
    assert_eq!(arg0.len(), 1, "wide arg (double) at sp+4 is ordinal 0");
    assert!(
        matches!(fg.node_kind(fg.producer(arg0[0])), NodeKind::Load(_)),
        "arg 0 carrier must be a Load node"
    );

    let arg1 = fg.side_tables().arg_index_to_values(1);
    assert_eq!(
        arg1.len(),
        1,
        "narrow arg (int) at sp+12 is ordinal 1 — not lost to the slot-1 gap \
         the wide arg leaves behind"
    );
    assert!(
        matches!(fg.node_kind(fg.producer(arg1[0])), NodeKind::Load(_)),
        "arg 1 carrier must be a Load node"
    );
    Ok(())
}

/// Span 4: an `I128` at `sp+0` covers slots 0..3, so the ordinal must advance
/// by exactly one across all of them and the following `I32` at `sp+16` must
/// not be lost to the three covered slots.
#[test]
fn span_four_wide_arg_then_narrow_arg_indexed_by_ordinal() -> Result<()> {
    let sp = sp32_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_args(Some(strider_target::StackArgs {
            base_offset: 0,
            increment: 4,
        }))
        .build_fn_single_region()?;
    let sp_val = b.read_variable(&sp)?;
    // 16 bytes, spanning slots 0..3.
    let a = b.build_load(sp_val, rsleigh::VnSpace::RAM, ValueType::I128)?;
    // Slot 4.
    let sixteen = b.build_int_const(16u64, ValueType::I32)?;
    let addr_b = b.build_int_binary_operation(sp_val, sixteen, IntBinaryOp::Add, ValueType::I32)?;
    let bv = b.build_load(addr_b, rsleigh::VnSpace::RAM, ValueType::I32)?;
    // Truncate the I128 to I32 so the sum is well-typed and neither load dies.
    let a_trunc = {
        let trunc = strider_ir_test_utils::sentinel_node(
            b.function_mut(),
            NodeKind::Truncate,
            [a],
            [strider_ir::node::ValueKind::Typed(ValueType::I32)],
        );
        b.function().node_outputs_exact::<1>(trunc).unwrap()[0]
    };
    let sum = b.build_int_binary_operation(a_trunc, bv, IntBinaryOp::Add, ValueType::I32)?;
    b.build_return(Some(sum), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    pipeline.add_post_pass(FunctionArgDetect);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let arg0 = fg.side_tables().arg_index_to_values(0);
    assert_eq!(
        arg0.len(),
        1,
        "the I128 (16-byte) wide arg at sp+0 spans four slots but is ordinal 0"
    );
    assert!(
        matches!(fg.node_kind(fg.producer(arg0[0])), NodeKind::Load(_)),
        "arg 0 carrier must be a Load node"
    );

    let arg1 = fg.side_tables().arg_index_to_values(1);
    assert_eq!(
        arg1.len(),
        1,
        "the narrow I32 at sp+16 is ordinal 1 — not lost to the three slots \
         (1..3) the wide I128 covers"
    );
    assert!(
        matches!(fg.node_kind(fg.producer(arg1[0])), NodeKind::Load(_)),
        "arg 1 carrier must be a Load node"
    );
    Ok(())
}

/// An unused arg register is registered at builder entry regardless, then
/// dropped by `compact` once DCE makes its InitialVar unreachable.
#[test]
fn unused_register_arg_dropped_by_compact() -> Result<()> {
    let rdi = rdi_like_vn();
    let sp = stack_vn();
    let mut b = RegisterSet::new()
        .tracked(rdi)
        .tracked(sp)
        .arg(rdi)
        .callee_saved(rdi)
        .ret(rdi)
        .build_fn_single_region()?;

    // Returning a constant leaves rdi unread.
    let c = b.build_int_const(0u64, ValueType::I64)?;
    b.build_return(Some(c), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    // At build time arg 0 is registered regardless of use.
    assert!(
        !fg.side_tables().arg_index_to_values(0).is_empty(),
        "arg 0 registered at build time"
    );

    // Compaction drops the unreachable InitialVar and its arg-table entry.
    fg.compact()?;
    assert!(
        fg.side_tables().arg_index_to_values(0).is_empty(),
        "unused arg carrier dropped after compact"
    );
    assert_eq!(
        fg.side_tables().iter_arg_indices().count(),
        0,
        "table empty after compact"
    );
    Ok(())
}

/// The second and third register args land at indices 1 and 2, not just arg 0,
/// each carried by its own `InitialVar`.
#[test]
fn second_and_third_register_args_recorded_at_their_indices() -> Result<()> {
    let r0 = reg_vn(0x38, 8);
    let r1 = reg_vn(0x30, 8);
    let r2 = reg_vn(0x28, 8);
    let mut b = RegisterSet::new()
        .tracked(r0)
        .tracked(r1)
        .tracked(r2)
        .arg(r0)
        .arg(r1)
        .arg(r2)
        .build_fn_single_region()?;
    let a = b.read_variable(&r1)?;
    let c = b.read_variable(&r2)?;
    let sum = b.build_int_binary_operation(a, c, IntBinaryOp::Add, ValueType::I64)?;
    b.build_return(Some(sum), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    crate::pipeline::run_post(&FunctionArgDetect, &mut fg, &mut crate::OptCtx::new(None))?;

    for (idx, vn) in [(0u32, r0), (1, r1), (2, r2)] {
        let carriers = fg.side_tables().arg_index_to_values(idx);
        assert_eq!(carriers.len(), 1, "exactly one carrier for arg {idx}");
        assert!(
            matches!(fg.node_kind(fg.producer(carriers[0])), NodeKind::InitialVar(v) if fg.initial_vn(*v) ==vn),
            "arg {idx} carrier must be InitialVar of its CC register"
        );
    }
    Ok(())
}

/// Two register args plus a stack arg at `sp+8` must register at indices 0, 1,
/// and 2.
#[test]
fn x86_64_mixed_reg_and_stack() -> Result<()> {
    let rdi = rdi_like_vn();
    let rsi = rsleigh::Vn {
        addr_off: 0x30,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 8,
    };
    let sp = stack_vn();
    // No ret-val regs on the CC, so the validator's arity check leaves the
    // Return tail unchecked.  Fine here: the variadic value is scaffolding,
    // not a CC-mandated slot.
    let mut b = RegisterSet::new()
        .tracked(rdi)
        .tracked(rsi)
        .tracked(sp)
        .arg(rdi)
        .arg(rsi)
        .stack_vn(sp)
        .stack_args(Some(strider_target::StackArgs {
            base_offset: 8,
            increment: 8,
        }))
        .callee_saved(rdi)
        .build_fn_single_region()?;

    let a = b.read_variable(&rdi)?;
    let bb = b.read_variable(&rsi)?;
    let sp_val = b.read_variable(&sp)?;
    let eight = b.build_int_const(8u64, ValueType::I64)?;
    let addr = b.build_int_binary_operation(sp_val, eight, IntBinaryOp::Add, ValueType::I64)?;
    let c = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::I64)?;
    let ab = b.build_int_binary_operation(a, bb, IntBinaryOp::Add, ValueType::I64)?;
    let abc = b.build_int_binary_operation(ab, c, IntBinaryOp::Add, ValueType::I64)?;
    b.build_return(Some(abc), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    pipeline.add_post_pass(FunctionArgDetect);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let arg0 = fg.side_tables().arg_index_to_values(0);
    assert!(!arg0.is_empty(), "arg 0 (rdi) should be registered");
    assert!(
        matches!(fg.node_kind(fg.producer(arg0[0])), NodeKind::InitialVar(v) if fg.initial_vn(*v) ==rdi),
        "arg 0 carrier must be InitialVar(rdi)"
    );

    let arg1 = fg.side_tables().arg_index_to_values(1);
    assert!(!arg1.is_empty(), "arg 1 (rsi) should be registered");
    assert!(
        matches!(fg.node_kind(fg.producer(arg1[0])), NodeKind::InitialVar(v) if fg.initial_vn(*v) ==rsi),
        "arg 1 carrier must be InitialVar(rsi)"
    );

    let arg2 = fg.side_tables().arg_index_to_values(2);
    assert!(!arg2.is_empty(), "arg 2 (sp+8) should be registered");
    assert!(
        matches!(fg.node_kind(fg.producer(arg2[0])), NodeKind::Load(_)),
        "arg 2 carrier must be a Load node"
    );
    Ok(())
}

/// A store at a DIFFERENT offset whose byte range still overlaps the load's
/// must shadow it.  Here the store covers [0,8) and the load [4,12).
#[test]
fn overlapping_stackstore_at_different_offset_shadows() -> Result<()> {
    let sp = stack_vn_aarch64();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_args(Some(strider_target::StackArgs {
            base_offset: 4,
            increment: 8,
        }))
        .build_fn_single_region()?;
    let sp_val = b.read_variable(&sp)?;
    let wide_data = b.build_int_const(0xDEAD_BEEF_CAFE_BABEu64, ValueType::I64)?;
    b.build_store(sp_val, wide_data, rsleigh::VnSpace::RAM)?;

    let four = b.build_int_const(4u64, ValueType::I64)?;
    let addr4 = b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, ValueType::I64)?;
    let loaded = b.build_load(addr4, rsleigh::VnSpace::RAM, ValueType::I64)?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    pipeline.add_post_pass(FunctionArgDetect);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let arg0_nodes = fg.side_tables().arg_index_to_values(0);
    assert!(
        arg0_nodes.is_empty(),
        "Load[sp+4] overlaps with Store(sp+0, size=8) — must not be registered"
    );
    Ok(())
}

/// The dual of the overlap case: a nearby store whose range is DISJOINT must
/// not shadow.  Store covers [0,4), load covers [4,8), so sp+4 is still arg 0.
#[test]
fn disjoint_stackstore_at_nearby_offset_is_not_shadow() -> Result<()> {
    let sp = sp32_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_args(Some(strider_target::StackArgs {
            base_offset: 4,
            increment: 8,
        }))
        .build_fn_single_region()?;
    let sp_val = b.read_variable(&sp)?;
    // Covers [0,4).
    let a = b.build_int_const(0x11u64, ValueType::I32)?;
    b.build_store(sp_val, a, rsleigh::VnSpace::RAM)?;

    // Covers [4,8), disjoint from the store.
    let four = b.build_int_const(4u64, ValueType::I32)?;
    let addr4 = b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, ValueType::I32)?;
    let loaded = b.build_load(addr4, rsleigh::VnSpace::RAM, ValueType::I32)?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    pipeline.add_post_pass(FunctionArgDetect);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let arg0_nodes = fg.side_tables().arg_index_to_values(0);
    assert!(
        !arg0_nodes.is_empty(),
        "disjoint Store(sp+0, size=4) must not shadow Load[sp+4] — arg 0 should be registered"
    );
    assert!(
        matches!(fg.node_kind(fg.producer(arg0_nodes[0])), NodeKind::Load(_)),
        "carrier for arg 0 must be a Load node"
    );
    Ok(())
}

/// Overlap through a `MemPhi`: one arm overlaps the load's range and the other
/// is disjoint.  Any overlapping predecessor is a shadow, so the load is
/// disqualified.  The arms cover [2,6) and [8,12) against a load at [4,8).
#[test]
fn memphi_partial_overlap_shadows() -> Result<()> {
    let sp = sp32_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_args(Some(strider_target::StackArgs {
            base_offset: 4,
            increment: 8,
        }))
        .build_fn()?;
    let entry = b.create_region_all()?;
    let then_r = b.create_region_all()?;
    let else_r = b.create_region_all()?;
    let merge = b.create_region_all()?;
    b.set_entry_region_all(entry)?;

    b.set_region(entry);
    let cond = b.build_boolean_const(true);
    b.build_if(cond, then_r, else_r)?;

    // Covers [2,6).
    b.set_region(then_r);
    let sp_t = b.read_variable(&sp)?;
    let two_t = b.build_int_const(2u64, ValueType::I32)?;
    let addr_t = b.build_int_binary_operation(sp_t, two_t, IntBinaryOp::Add, ValueType::I32)?;
    let data_t = b.build_int_const(0x11u64, ValueType::I32)?;
    b.build_store(addr_t, data_t, rsleigh::VnSpace::RAM)?;
    b.build_branch(merge)?;

    // Covers [8,12).
    b.set_region(else_r);
    let sp_e = b.read_variable(&sp)?;
    let eight_e = b.build_int_const(8u64, ValueType::I32)?;
    let addr_e = b.build_int_binary_operation(sp_e, eight_e, IntBinaryOp::Add, ValueType::I32)?;
    let data_e = b.build_int_const(0x22u64, ValueType::I32)?;
    b.build_store(addr_e, data_e, rsleigh::VnSpace::RAM)?;
    b.build_branch(merge)?;

    // Covers [4,8).
    b.set_region(merge);
    let sp_m = b.read_variable(&sp)?;
    let four_m = b.build_int_const(4u64, ValueType::I32)?;
    let addr_m = b.build_int_binary_operation(sp_m, four_m, IntBinaryOp::Add, ValueType::I32)?;
    let loaded = b.build_load(addr_m, rsleigh::VnSpace::RAM, ValueType::I32)?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    pipeline.add_post_pass(FunctionArgDetect);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let arg0_nodes = fg.side_tables().arg_index_to_values(0);
    assert!(
        arg0_nodes.is_empty(),
        "MemPhi with an overlapping-range Store predecessor must disqualify Load[sp+4]"
    );
    Ok(())
}

/// An isolated high-offset load with nothing below it registers no args at
/// all: nothing starts the contiguous prefix.
#[test]
fn isolated_high_offset_load_dropped() -> Result<()> {
    let sp = sp32_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_args(Some(strider_target::StackArgs {
            base_offset: 4,
            increment: 4,
        }))
        .build_fn_single_region()?;
    let _sp_val = b.read_variable(&sp)?;
    let v = build_sp_load(&mut b, &sp, 12)?;
    b.build_return(Some(v), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    pipeline.add_post_pass(FunctionArgDetect);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    assert_eq!(
        fg.side_tables().iter_arg_indices().count(),
        0,
        "isolated sp+12 load must not be registered without arg 0/1"
    );
    Ok(())
}

/// `sub rsp, 0xFFFFFFFFFFFFFFFC` is an alternate encoding of `add rsp, 4`:
/// sign-extended the constant is -4, so `Sub(sp, -4) = sp + 4` and the
/// resulting `Load` must still be a candidate for offset +4.
#[test]
fn load_via_sub_negative_unsigned_recognised_as_stack_arg() -> Result<()> {
    use crate::{ConstantFold, OptimizerPipeline, PhiCollapse, RegionCollapse};

    let sp = stack_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_args(Some(strider_target::StackArgs {
            base_offset: 4,
            increment: 8,
        }))
        .build_fn_single_region()?;
    let sp_val = b.read_variable(&sp)?;
    // -4 read as signed i64.
    let neg_four = b.build_int_const(0xFFFF_FFFF_FFFF_FFFCu64, ValueType::I64)?;
    let addr = b.build_sub_as_add_neg(sp_val, neg_four, ValueType::I64)?;
    let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::I64)?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    // ConstantFold folds `Add(_, Neg(K))` to `Add(_, IntConst(-K))`, the shape
    // decomposition sees in production; it does not peel the `Neg` itself.
    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold::new());
    pipeline.add(PhiCollapse);
    pipeline.add(RegionCollapse);
    pipeline.add_post_pass(FunctionArgDetect);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let arg0_nodes = fg.side_tables().arg_index_to_values(0);
    assert!(
        !arg0_nodes.is_empty(),
        "Sub(sp, 0xFFFFFFFFFFFFFFFC_U64) must decompose to offset +4 and be registered as arg 0",
    );
    assert!(
        matches!(fg.node_kind(fg.producer(arg0_nodes[0])), NodeKind::Load(_)),
        "carrier for arg 0 (stack-arg via negative sub) must be a Load node"
    );
    Ok(())
}

// The `Store(_)` arm of `mem_chain_is_dirty`, in four cases: SP-rooted
// overlapping (dirty), non-SP (pass-through), SP-rooted disjoint
// (pass-through), and SP-rooted phi (conservatively dirty).

/// An SP-rooted store whose range matches the load's must mark the chain dirty.
#[test]
fn mem_chain_is_dirty_terminates_at_overlapping_store_to_sp_rel_addr() -> Result<()> {
    let sp = sp32_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_args(Some(strider_target::StackArgs {
            base_offset: 4,
            increment: 8,
        }))
        .build_fn_single_region()?;
    let sp_val = b.read_variable(&sp)?;
    // Covers [4,8).
    let four = b.build_int_const(4u64, ValueType::I32)?;
    let addr = b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, ValueType::I32)?;
    let data = b.build_int_const(0x11u64, ValueType::I32)?;
    b.build_store(addr, data, rsleigh::VnSpace::RAM)?;

    // Same range, so it must shadow.
    let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::I32)?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    pipeline.add_post_pass(FunctionArgDetect);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let arg0_nodes = fg.side_tables().arg_index_to_values(0);
    assert!(
        arg0_nodes.is_empty(),
        "plain Store(sp+4, I32) overlaps Load[sp+4]: chain must be dirty — no arg registered"
    );
    Ok(())
}

/// Soundness floor: a cross-class store (a const-encoded global) cannot be
/// proven disjoint from an SP-rooted load, so under `Strict` it marks the chain
/// dirty.  Registering the load anyway would substitute a pre-entry value and
/// mask the global's write.
#[test]
fn mem_chain_is_dirty_on_non_sp_intervening_store() -> Result<()> {
    let sp = sp32_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_args(Some(strider_target::StackArgs {
            base_offset: 4,
            increment: 8,
        }))
        .build_fn_single_region()?;
    let sp_val = b.read_variable(&sp)?;
    // Volatile global write to a fixed `.data` address.
    let global_addr = b.build_int_const(0xDEAD_BEEFu64, ValueType::I32)?;
    let global_data = b.build_int_const(0x1234u64, ValueType::I32)?;
    b.build_store(global_addr, global_data, rsleigh::VnSpace::RAM)?;

    // The load's memory predecessor IS the global store above.
    let four = b.build_int_const(4u64, ValueType::I32)?;
    let addr = b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, ValueType::I32)?;
    let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::I32)?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    // Pin Strict explicitly: under the `StackGlobalDisjoint` default the
    // global write is assumed disjoint and the Load would be promoted.
    pipeline.add_post_pass(FunctionArgDetect);
    pipeline.run(&mut fg, &mut crate::test_support::octx_strict())?;

    let arg0_nodes = fg.side_tables().arg_index_to_values(0);
    assert!(
        arg0_nodes.is_empty(),
        "Strict mode: the cross-class intervening Store must mark the chain \
         dirty so the Load is NOT promoted to an incoming arg"
    );
    Ok(())
}

/// An SP-rooted store whose byte range is disjoint from the load's must not
/// mark the chain dirty: [0,4) against [4,8), so sp+4 is still arg 0.
#[test]
fn mem_chain_is_dirty_passes_through_disjoint_sp_store() -> Result<()> {
    let sp = sp32_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_args(Some(strider_target::StackArgs {
            base_offset: 4,
            increment: 8,
        }))
        .build_fn_single_region()?;
    let sp_val = b.read_variable(&sp)?;
    // Covers [0,4).
    let zero_data = b.build_int_const(0x11u64, ValueType::I32)?;
    b.build_store(sp_val, zero_data, rsleigh::VnSpace::RAM)?;

    // Covers [4,8), disjoint from [0,4).
    let four = b.build_int_const(4u64, ValueType::I32)?;
    let addr4 = b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, ValueType::I32)?;
    let loaded = b.build_load(addr4, rsleigh::VnSpace::RAM, ValueType::I32)?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    pipeline.add_post_pass(FunctionArgDetect);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let arg0_nodes = fg.side_tables().arg_index_to_values(0);
    assert!(
        !arg0_nodes.is_empty(),
        "disjoint SP-rooted Store(sp+0, I32) must not mark Load[sp+4] dirty: still registered as arg 0"
    );
    Ok(())
}

/// A store through an SP-rooted phi that does NOT collapse to a single
/// terminal must conservatively mark the chain dirty.  The branches do
/// `sp -= 4` and `sp -= 8`, so the join phi disagrees, decomposition returns
/// `None`, and the store's address cannot be range-checked at all.
#[test]
fn mem_chain_is_dirty_terminates_at_overlapping_phi_of_sp() -> Result<()> {
    let sp = sp32_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_args(Some(strider_target::StackArgs {
            base_offset: 4,
            increment: 8,
        }))
        .build_fn()?;
    let entry = b.create_region_all()?;
    let then_r = b.create_region_all()?;
    let else_r = b.create_region_all()?;
    let join = b.create_region_all()?;
    b.set_entry_region_all(entry)?;

    // Snapshot the original SP before the diamond.
    b.set_region(entry);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let sp_orig = b.read_variable(&sp)?;
    let cond = b.build_boolean_const(true);
    b.build_if(cond, then_r, else_r)?;

    b.set_region(then_r);
    let sp_t = b.read_variable(&sp)?;
    let four_t = b.build_int_const(4u64, ValueType::I32)?;
    let sp_t_new = b.build_sub_as_add_neg(sp_t, four_t, ValueType::I32)?;
    b.write_variable(&sp, sp_t_new)?;
    b.build_branch(join)?;

    b.set_region(else_r);
    let sp_e = b.read_variable(&sp)?;
    let eight_e = b.build_int_const(8u64, ValueType::I32)?;
    let sp_e_new = b.build_sub_as_add_neg(sp_e, eight_e, ValueType::I32)?;
    b.write_variable(&sp, sp_e_new)?;
    b.build_branch(join)?;

    // Store through the phi'd SP, whose address decomposes to None, then load
    // the stack-arg slot off the original SP.
    b.set_region(join);
    let phi_sp = b.read_variable(&sp)?;
    let trash = b.build_int_const(0xAAu64, ValueType::I32)?;
    b.build_store(phi_sp, trash, rsleigh::VnSpace::RAM)?;

    let four_j = b.build_int_const(4u64, ValueType::I32)?;
    let addr_j = b.build_int_binary_operation(sp_orig, four_j, IntBinaryOp::Add, ValueType::I32)?;
    let loaded = b.build_load(addr_j, rsleigh::VnSpace::RAM, ValueType::I32)?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    pipeline.add_post_pass(FunctionArgDetect);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let arg0_nodes = fg.side_tables().arg_index_to_values(0);
    assert!(
        arg0_nodes.is_empty(),
        "Store through a non-collapsing SP-phi address must conservatively mark chain dirty: no arg registered"
    );
    Ok(())
}

#[test]
fn mem_chain_is_dirty_handles_10k_disjoint_store_chain() -> Result<()> {
    const CHAIN_LEN: usize = 10_000;

    let sp = sp32_vn();
    let mut b = RegisterSet::new()
        .tracked(sp)
        .callee_saved(sp)
        .stack_vn(sp)
        .stack_args(Some(strider_target::StackArgs {
            base_offset: 4,
            increment: 8,
        }))
        .build_fn_single_region()?;
    let sp_val = b.read_variable(&sp)?;
    // Disjoint stack stores at offsets 16, 20, 24, ...
    for i in 0..CHAIN_LEN {
        let off = b.build_int_const(((i * 4) as u64) + 16, ValueType::I32)?;
        let addr = b.build_int_binary_operation(sp_val, off, IntBinaryOp::Add, ValueType::I32)?;
        let val = b.build_int_const(i as u64, ValueType::I32)?;
        b.build_store(addr, val, rsleigh::VnSpace::RAM)?;
    }
    // Disjoint from every store above, so the walker passes through all of them.
    let four = b.build_int_const(4u64, ValueType::I32)?;
    let addr4 = b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, ValueType::I32)?;
    let loaded = b.build_load(addr4, rsleigh::VnSpace::RAM, ValueType::I32)?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    pipeline.add_post_pass(FunctionArgDetect);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let arg0_nodes = fg.side_tables().arg_index_to_values(0);
    assert!(
        !arg0_nodes.is_empty(),
        "10k disjoint stores must not mark the chain dirty: load at sp+4 should be registered as arg 0"
    );
    Ok(())
}

/// A `CallOther` on the chain is gated purely by `calls_clobber`: the callee is
/// opaque, so nothing can be inferred from its arguments.
#[test]
fn callother_on_chain_gated_only_by_calls_clobber() -> Result<()> {
    let sp = sp32_vn();
    let build = |b: &mut strider_ir::FunctionBuilder| -> Result<()> {
        // Slot 0's address is sp itself.
        let sp_val = b.read_variable(&sp)?;
        // Its sole value-arg is &arg0.
        let (call_node, _result) = b.build_call_other_abi(
            42,
            "escape_helper",
            &[sp_val],
            &strider_target::BuiltCallOtherAbi {
                implicit_reads: Vec::new(),
                implicit_writes: Vec::new(),
                clobbers_memory: false,
            },
            None,
            false,
        )?;
        let call_mem_value = b.function().memory_output_of(call_node)?;
        b.advance_cur_region_memory(call_mem_value)?;
        let loaded = b.build_load(sp_val, rsleigh::VnSpace::RAM, ValueType::I32)?;
        b.build_return(Some(loaded), &[])?;
        b.set_lift_addr(None);
        Ok(())
    };

    let new_fn = || -> Result<strider_ir::Function> {
        let mut b = RegisterSet::new()
            .tracked(sp)
            .callee_saved(sp)
            .stack_vn(sp)
            .stack_args(Some(strider_target::StackArgs {
                base_offset: 0,
                increment: 8,
            }))
            .build_fn_single_region()?;
        build(&mut b)?;
        b.build()
    };

    // By default the CallOther does not block.
    let mut fg_default = new_fn()?;
    let mut p_default = cf_rp_pipeline();
    p_default.add_post_pass(FunctionArgDetect);
    p_default.run(&mut fg_default, &mut crate::OptCtx::new(None))?;
    assert!(
        !fg_default.side_tables().arg_index_to_values(0).is_empty(),
        "default (calls_clobber=false): a CallOther on the chain does not \
         block stack-arg promotion (the callee is opaque, no arg inspection)",
    );

    // With the toggle on, the CallOther marks the slot dirty.
    let mut fg_conservative = new_fn()?;
    let mut p_conservative = cf_rp_pipeline();
    p_conservative.add_post_pass(FunctionArgDetect);
    let mut octx_conservative = crate::OptCtx::new(None);
    octx_conservative.options.arg_alias.calls_clobber = true;
    p_conservative.run(&mut fg_conservative, &mut octx_conservative)?;
    assert!(
        fg_conservative
            .side_tables()
            .arg_index_to_values(0)
            .is_empty(),
        "calls_clobber=true: the CallOther on the chain marks the slot dirty",
    );
    Ok(())
}

/// The clobber toggle for a plain `Call`: by default a call does not on its own
/// shadow a stack-arg slot, so the load is registered; with `calls_clobber` any
/// call on the chain marks it dirty.
#[test]
fn calls_clobber_toggle_gates_arg_across_call() -> Result<()> {
    let sp = sp32_vn();
    // The Call's only value input is its constant target, which is not
    // SP-rooted, so the toggle alone governs the verdict.
    let build = |b: &mut FunctionBuilder, sp_val: ValueId| -> Result<()> {
        let target = b.build_int_const(0x1000u64, ValueType::I32)?;
        b.build_call_cc(target, None)?;
        let four = b.build_int_const(4u64, ValueType::I32)?;
        let addr4 = b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, ValueType::I32)?;
        let loaded = b.build_load(addr4, rsleigh::VnSpace::RAM, ValueType::I32)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    };

    // Default: the arg is detected across the Call.
    let mut fg_default = {
        let mut b = RegisterSet::new()
            .tracked(sp)
            .callee_saved(sp)
            .stack_vn(sp)
            .stack_args(Some(strider_target::StackArgs {
                base_offset: 4,
                increment: 8,
            }))
            .build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        build(&mut b, sp_val)?;
        b.set_lift_addr(None);
        b.build()?
    };
    let mut p_default = cf_rp_pipeline();
    p_default.add_post_pass(FunctionArgDetect);
    p_default.run(&mut fg_default, &mut crate::OptCtx::new(None))?;
    assert!(
        !fg_default.side_tables().arg_index_to_values(0).is_empty(),
        "default (calls_clobber=false): Load[sp+4] across a plain Call \
         is detected as arg 0",
    );

    // With the toggle on, the Call marks the slot dirty.
    let mut fg_conservative = {
        let mut b = RegisterSet::new()
            .tracked(sp)
            .callee_saved(sp)
            .stack_vn(sp)
            .stack_args(Some(strider_target::StackArgs {
                base_offset: 4,
                increment: 8,
            }))
            .build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        build(&mut b, sp_val)?;
        b.set_lift_addr(None);
        b.build()?
    };
    let mut p_conservative = cf_rp_pipeline();
    p_conservative.add_post_pass(FunctionArgDetect);
    let mut octx_conservative = crate::OptCtx::new(None);
    octx_conservative.options.arg_alias.calls_clobber = true;
    p_conservative.run(&mut fg_conservative, &mut octx_conservative)?;
    assert!(
        fg_conservative
            .side_tables()
            .arg_index_to_values(0)
            .is_empty(),
        "calls_clobber=true: the Call on the chain marks the slot dirty, \
         so Load[sp+4] is NOT registered as an arg",
    );
    Ok(())
}

/// `combine_phi` OR-combines its predecessors, which is the safety net letting
/// `cycle_verdict` return "clean for this edge" without losing soundness.
#[test]
fn function_args_combine_phi_or_semantics_pinned() {
    // Mirrors `combine_phi`: any dirty predecessor makes the phi dirty.
    fn combine_phi(preds: Vec<bool>) -> bool {
        preds.into_iter().any(|d| d)
    }
    assert!(
        combine_phi(vec![false, true]),
        "any() invariant: one dirty pred forces phi-combined verdict to dirty"
    );
    assert!(
        combine_phi(vec![true, false, false]),
        "any() invariant: first dirty pred forces phi-combined verdict to dirty"
    );
    assert!(
        !combine_phi(vec![false, false]),
        "all-clean preds combine to clean"
    );
    assert!(
        !combine_phi(vec![]),
        "empty pred set combines to clean (no information => assume clean for this edge)"
    );
    // `cycle_verdict`'s `false` sentinel is sound precisely because
    // `combine_phi` is `any()`: a cycle-broken sibling can still upgrade the
    // verdict to dirty.  Pinned together so neither can be swapped alone.
    let cycle_sentinel: bool = false;
    assert!(
        combine_phi(vec![cycle_sentinel, true]),
        "cycle_verdict()=false must still combine to dirty when a non-cycle pred is dirty"
    );
}
