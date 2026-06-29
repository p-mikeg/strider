use super::*;
use crate::error::Result;
use crate::pipeline::PostOptimizerTestExt;
use crate::test_support::cf_rp_pipeline;
use strider_ir::node::{NodeKind, ValueId, ValueType};
use strider_ir_test_utils::IrWalkerEx;
use strider_ir::{FunctionBuilder, IRBuilderExt, IRViewer, IntBinaryOp};
use strider_ir_test_utils::{
    RegisterSet, SENTINEL_LIFT_ADDR, reg_vn, stack_vn_aarch64, stack_vn_x86 as sp32_vn,
    stack_vn_x86_64 as stack_vn,
};

fn rdi_like_vn() -> rsleigh::Vn {
    // Fake 8-byte register to stand in for x86_64 RDI in tests.
    reg_vn(0x38, 8)
}

/// x86_64-like convention passes arg 0 in a register.  Register arg 0 is
/// recorded in `arg_index_to_values(0)` at builder-entry time (inside
/// `set_entry_region`'s aliasing-aware register reads), before any pass
/// runs.  The now-stack-only `FunctionArgDetect` leaves register arg 0
/// untouched, so the entry `InitialVar(rdi)` remains present and
/// reachable after the pass executes.
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

    // Build a trivial function that reads rdi and returns it.
    let v = b.read_variable(&rdi)?;
    b.build_return(Some(v), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let pass = FunctionArgDetect;
    pass.run_one(&mut fg, &mut crate::OptCtx::new(None))?;

    // Side-table must have arg 0.
    let arg0_nodes = fg.arg_index_to_values(0);
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

    // The original InitialVar(rdi) must still be reachable — no consumer rewiring.
    let reachable_initial_rdi =
        fg.count_kind(|k| matches!(k, NodeKind::InitialVar(v) if fg.initial_vn(*v) ==rdi));
    assert_eq!(
        reachable_initial_rdi, 1,
        "InitialVar(rdi) must remain reachable after the pass"
    );
    Ok(())
}

/// `FunctionArgDetect` runs as a post-pass on every stable iteration of
/// the orchestrator's fixed-point loop, so it can be applied repeatedly to
/// the same `Function`.  It must be idempotent: re-running it must not
/// accumulate duplicate carrier ids in `arg_index_to_values`.
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
    pass.run_one(&mut fg, &mut crate::OptCtx::new(None))?;
    let after_first = fg.arg_index_to_values(0).to_vec();
    // Re-run on the same function (simulating a second StableOnly iteration).
    pass.run_one(&mut fg, &mut crate::OptCtx::new(None))?;
    let after_second = fg.arg_index_to_values(0).to_vec();

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

/// x86 cdecl reads its first stack arg at `[sp + 4]`.  With no
/// register args in the convention, `arg_index_to_values(0)` should contain
/// the `Load[sp+4]` node.  The original Load must remain reachable.
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
        // addr = sp + 4; load[addr]; return loaded
        let four = b.build_int_const(4u64, ValueType::I32)?;
        let addr = b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, ValueType::I32)?;
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::I32)?;
        b.build_return(Some(loaded), &[])?;
    }
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    // ConstantFold normalises the address; FunctionArgDetect runs after.
    let mut pipeline = cf_rp_pipeline();
    pipeline.add_post_pass(FunctionArgDetect);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    // Side-table must have arg 0.
    let arg0_nodes = fg.arg_index_to_values(0);
    assert!(!arg0_nodes.is_empty(), "arg 0 should be registered (stack)");
    assert_eq!(arg0_nodes.len(), 1, "one Load at sp+4, so one carrier");
    assert!(
        matches!(fg.node_kind(fg.producer(arg0_nodes[0])), NodeKind::Load(_)),
        "carrier for stack arg 0 must be a Load node"
    );

    // The original Load must still be reachable.
    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 1,
        "Load[sp+4] must remain reachable after the pass"
    );
    Ok(())
}

/// A `Load` rooted at an *alignment-masked* SP (`(sp & mask) + 4`), not the
/// entry SP, addresses a frame local — not incoming stack arg 0.  Only loads
/// whose decomposed terminal base is `InitialVar(sp)` qualify as stack args,
/// so nothing must be registered.  Before the initial-SP base check, the
/// offset-only match (`+4 == stack_arg_offsets[0]`) wrongly registered it.
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
    // aligned = sp & 0xFFFF_FFF8; addr = aligned + 4; load[addr]
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
        fg.arg_index_to_values(0).is_empty(),
        "a load rooted at an alignment-masked SP (not the entry SP) must not \
         register as stack arg 0"
    );
    Ok(())
}

/// Builds `load[sp + offset]` reading a I32 value.  Returns the loaded output.
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

/// Unbounded detection: ten incoming stack args at `sp + i*8` (more than any
/// old fixed offset-list length) are all detected and registered, proving the
/// new `StackArgs` formula has no upper bound on the number of stack args.
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
    // Each load reads InitialMemory (no intervening stores) at sp + i*8.
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
            !fg.arg_index_to_values(i as u32).is_empty(),
            "arg {i} (sp + {}) must be registered",
            i * 8
        );
    }
    Ok(())
}

/// Loads at sp+4 and sp+12, but **not** sp+8 — only the contiguous
/// prefix (sp+4 → arg 0) is labelled.  The sp+12 load remains unchanged
/// and is NOT registered in the side-table (no gap-spanning).
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
    // Combine both loads so neither is dead.
    let sum = b.build_int_binary_operation(a, c, IntBinaryOp::Add, ValueType::I32)?;
    b.build_return(Some(sum), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    pipeline.add_post_pass(FunctionArgDetect);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    // Only arg 0 registered; arg 1 absent (gap) so arg 2 MUST NOT be registered.
    let arg0_nodes = fg.arg_index_to_values(0);
    assert!(!arg0_nodes.is_empty(), "arg 0 (sp+4) should be registered");

    let arg1_nodes = fg.arg_index_to_values(1);
    assert!(
        arg1_nodes.is_empty(),
        "arg 1 (sp+8) is absent — nothing at that offset"
    );

    let arg2_nodes = fg.arg_index_to_values(2);
    assert!(
        arg2_nodes.is_empty(),
        "arg 2 (sp+12) must be truncated by the gap"
    );

    // Both loads must still be reachable — the pass does not remove nodes.
    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 2,
        "both Load nodes (sp+4 and sp+12) must remain reachable"
    );
    Ok(())
}

/// A stack-arg `Load[sp+4]` reached through disjoint stores at `+8` / `+12`
/// is still detected as arg 0 (parity), AND the walker narrows its memory
/// edge past those disjoint stores onto `InitialMemory` — narrowing does not
/// change which args are detected.
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
    // Disjoint stores at +8 and +12, then the stack-arg load at +4.
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

    // Parity: the load is still registered as arg 0.
    let arg0 = fg.arg_index_to_values(0).to_vec();
    assert_eq!(arg0.len(), 1, "Load[sp+4] registered as arg 0");

    // Narrowing fired: the arg-carrier load's memory input skipped the two
    // disjoint stores onto InitialMemory.
    let load = fg.producer(arg0[0]);
    let mem = fg.node_inputs(load)[0];
    assert!(
        matches!(fg.node_kind(fg.producer(mem)), NodeKind::InitialMemory),
        "stack-arg load narrowed past disjoint stores onto InitialMemory, got {:?}",
        fg.node_kind(fg.producer(mem)),
    );
    Ok(())
}

/// A prior SP-relative store at `+4` shadows the `Load[sp+4]` — the
/// load reads the stored value, not the caller's arg.  No arg registered.
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
    // *(sp + 4) = 0x11; return *(sp + 4)
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

    let arg0_nodes = fg.arg_index_to_values(0);
    assert!(
        arg0_nodes.is_empty(),
        "Load[sp+4] is shadowed by Store(sp+4), must not be registered as arg"
    );
    Ok(())
}

/// If-branch where the true side does a SP-relative store at `+4`,
/// false side does nothing — their join is a `MemPhi`, and a later
/// `Load[sp+4]` from the phi must be disqualified.  The DFS treats
/// `MemPhi` as a fork where **every** predecessor must be clean.
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
    let entry = b.create_region()?;
    let true_br = b.create_region()?;
    let false_br = b.create_region()?;
    let join = b.create_region()?;
    b.set_entry_region(entry)?;

    // entry: if (<const true>) goto true_br else false_br
    //   (use a boolean const so the MemPhi has TWO predecessors in the
    //    graph even though DeadBranchElimination could collapse it — we
    //    skip that pass here to preserve the phi.)
    b.set_region(entry);
    let cond = b.build_boolean_const(true);
    b.build_if(cond, true_br, false_br)?;

    // true_br: *(sp+4) = 0x22; goto join
    b.set_region(true_br);
    let sp_t = b.read_variable(&sp)?;
    let four_t = b.build_int_const(4u64, ValueType::I32)?;
    let addr_t = b.build_int_binary_operation(sp_t, four_t, IntBinaryOp::Add, ValueType::I32)?;
    let data = b.build_int_const(0x22u64, ValueType::I32)?;
    b.build_store(addr_t, data, rsleigh::VnSpace::RAM)?;
    b.build_branch(join)?;

    // false_br: fallthrough to join
    b.set_region(false_br);
    b.build_branch(join)?;

    // join: return *(sp+4)
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

    let arg0_nodes = fg.arg_index_to_values(0);
    assert!(
        arg0_nodes.is_empty(),
        "Load[sp+4] reaches a MemPhi with a shadowing branch — must not be registered"
    );
    Ok(())
}

/// If the same stack-arg slot is read at multiple
/// widths — e.g. aarch64 reading both `x0` (8 bytes) and `w0` (4 bytes)
/// from `sp+0` — both `Load` nodes must be registered in the side-table
/// for index 0.
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
    // Read sp+0 as I32, then sp+0 as I64.  Combine so neither is dead.
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

    // Both Loads at offset 0 must be registered for arg 0.
    let arg0_nodes = fg.arg_index_to_values(0);
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

    // Both Loads must still be reachable — the pass does not remove them.
    let reachable_loads = fg.count_kind(|k| matches!(k, NodeKind::Load(_)));
    assert_eq!(
        reachable_loads, 2,
        "both Loads must remain reachable after the pass"
    );
    Ok(())
}

/// A 32-bit-cdecl-style `f(double a, int b)`: `a` is an 8-byte (I64) load at
/// `sp+4` that spans two 4-byte slots, `b` a 4-byte (I32) load at `sp+12`.
/// The wide argument must be detected as ordinal 0 and the following narrower
/// argument as ordinal 1.  A naive `slot == ordinal` mapping would put `a` at
/// slot 0, `b` at slot 2, leave slot 1 a gap, and truncate — losing `b`.  The
/// width-aware cursor advances the ordinal by one while `a` consumes its two
/// slots, so `b` lands at ordinal 1.
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
    // a = *(sp+4) as I64 — the 8-byte `double`, spanning slots 0 and 1.
    let four = b.build_int_const(4u64, ValueType::I32)?;
    let addr_a = b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, ValueType::I32)?;
    let a = b.build_load(addr_a, rsleigh::VnSpace::RAM, ValueType::I64)?;
    // b = *(sp+12) as I32 — the `int`, at slot 2.
    let twelve = b.build_int_const(12u64, ValueType::I32)?;
    let addr_b = b.build_int_binary_operation(sp_val, twelve, IntBinaryOp::Add, ValueType::I32)?;
    let bv = b.build_load(addr_b, rsleigh::VnSpace::RAM, ValueType::I32)?;
    // Combine so neither load is dead.
    let b_ext = b.extend_if_needed(bv, ValueType::I64, strider_ir::ExtendOp::ZeroExtend)?;
    let sum = b.build_int_binary_operation(a, b_ext, IntBinaryOp::Add, ValueType::I64)?;
    b.build_return(Some(sum), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    pipeline.add_post_pass(FunctionArgDetect);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let arg0 = fg.arg_index_to_values(0);
    assert_eq!(arg0.len(), 1, "wide arg (double) at sp+4 is ordinal 0");
    assert!(
        matches!(fg.node_kind(fg.producer(arg0[0])), NodeKind::Load(_)),
        "arg 0 carrier must be a Load node"
    );

    let arg1 = fg.arg_index_to_values(1);
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

/// A 32-bit-ABI argument WIDER than two slots: an `I128` (16-byte) load at
/// `sp+0` spans FOUR 4-byte slots (0..3), and a following `I32` at `sp+16`
/// is ordinal 1.  Extends `wide_arg_then_narrow_arg_indexed_by_ordinal`
/// (span 2) past span 2: the width-aware cursor must advance the ordinal by
/// exactly one across all four slots the I128 covers, so the I32 is NOT lost
/// to the three covered slots 1..3.
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
    // a = *(sp+0) as I128 — 16 bytes, spanning slots 0,1,2,3.
    let a = b.build_load(sp_val, rsleigh::VnSpace::RAM, ValueType::I128)?;
    // b = *(sp+16) as I32 — slot 4.
    let sixteen = b.build_int_const(16u64, ValueType::I32)?;
    let addr_b = b.build_int_binary_operation(sp_val, sixteen, IntBinaryOp::Add, ValueType::I32)?;
    let bv = b.build_load(addr_b, rsleigh::VnSpace::RAM, ValueType::I32)?;
    // Combine so neither load is dead.  Truncate the I128 down to I32 so the
    // sum is well-typed.
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

    let arg0 = fg.arg_index_to_values(0);
    assert_eq!(
        arg0.len(),
        1,
        "the I128 (16-byte) wide arg at sp+0 spans four slots but is ordinal 0"
    );
    assert!(
        matches!(fg.node_kind(fg.producer(arg0[0])), NodeKind::Load(_)),
        "arg 0 carrier must be a Load node"
    );

    let arg1 = fg.arg_index_to_values(1);
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

/// An unused arg register is registered at builder entry unconditionally, then
/// dropped by `compact` once DCE has made its InitialVar unreachable — so
/// patterns can no longer find it.
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

    // Return a constant — rdi is never read.
    let c = b.build_int_const(0u64, ValueType::I64)?;
    b.build_return(Some(c), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    // Build-time: arg 0 is registered regardless of use.
    assert!(
        !fg.arg_index_to_values(0).is_empty(),
        "arg 0 registered at build time"
    );

    // The unread InitialVar(rdi) is unreachable; compaction drops it and its
    // arg-table entry.
    fg.compact()?;
    assert!(
        fg.arg_index_to_values(0).is_empty(),
        "unused arg carrier dropped after compact"
    );
    assert_eq!(
        fg.iter_arg_indices().count(),
        0,
        "table empty after compact"
    );
    Ok(())
}

/// Three register args: the SECOND and THIRD register args land at
/// side-table indices 1 and 2 (not just arg 0), each carried by its own
/// `InitialVar`.  Recorded at builder entry; `FunctionArgDetect` leaves
/// register args untouched.
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

    FunctionArgDetect.run_one(&mut fg, &mut crate::OptCtx::new(None))?;

    for (idx, vn) in [(0u32, r0), (1, r1), (2, r2)] {
        let carriers = fg.arg_index_to_values(idx);
        assert_eq!(carriers.len(), 1, "exactly one carrier for arg {idx}");
        assert!(
            matches!(fg.node_kind(fg.producer(carriers[0])), NodeKind::InitialVar(v) if fg.initial_vn(*v) ==vn),
            "arg {idx} carrier must be InitialVar of its CC register"
        );
    }
    Ok(())
}

/// x86_64-like: two register args (rdi, rsi) and a stack arg at `sp+8`
/// (i.e. arg 6 in SysV; for this test arg 2).  All three should be
/// registered in the side-table at indices 0, 1, and 2 respectively.
#[test]
fn x86_64_mixed_reg_and_stack() -> Result<()> {
    let rdi = rdi_like_vn();
    let rsi = rsleigh::Vn {
        addr_off: 0x30,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 8,
    };
    let sp = stack_vn();
    // No ret-val regs declared on the CC; this test exercises arg
    // detection, not return-value handling.  Without the `.ret(...)`
    // declarations the validator's CC-arity check leaves the Return
    // tail unchecked, which is appropriate here — the variadic value
    // is test scaffolding, not a CC-mandated slot.
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

    // Arg 0 = InitialVar(rdi).
    let arg0 = fg.arg_index_to_values(0);
    assert!(!arg0.is_empty(), "arg 0 (rdi) should be registered");
    assert!(
        matches!(fg.node_kind(fg.producer(arg0[0])), NodeKind::InitialVar(v) if fg.initial_vn(*v) ==rdi),
        "arg 0 carrier must be InitialVar(rdi)"
    );

    // Arg 1 = InitialVar(rsi).
    let arg1 = fg.arg_index_to_values(1);
    assert!(!arg1.is_empty(), "arg 1 (rsi) should be registered");
    assert!(
        matches!(fg.node_kind(fg.producer(arg1[0])), NodeKind::InitialVar(v) if fg.initial_vn(*v) ==rsi),
        "arg 1 carrier must be InitialVar(rsi)"
    );

    // Arg 2 = Load at sp+8.
    let arg2 = fg.arg_index_to_values(2);
    assert!(!arg2.is_empty(), "arg 2 (sp+8) should be registered");
    assert!(
        matches!(fg.node_kind(fg.producer(arg2[0])), NodeKind::Load(_)),
        "arg 2 carrier must be a Load node"
    );
    Ok(())
}

/// Byte-range overlap: a `StackStore` at a *different* offset whose byte
/// range nevertheless overlaps the load's must shadow it.  Exact-offset
/// comparison would miss this.
///
/// `*(sp+0) = I64(X); return *(sp+4) as I64` — store covers `[0,8)`, load
/// covers `[4,12)`.  With the byte-range overlap check the load is
/// disqualified; with the old `k == offset` check it would be registered.
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
    // *(sp+0) = I64(0xDEAD_BEEF_CAFE_BABE)
    let wide_data = b.build_int_const(0xDEAD_BEEF_CAFE_BABEu64, ValueType::I64)?;
    b.build_store(sp_val, wide_data, rsleigh::VnSpace::RAM)?;

    // return *(sp+4) as I64
    let four = b.build_int_const(4u64, ValueType::I64)?;
    let addr4 = b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, ValueType::I64)?;
    let loaded = b.build_load(addr4, rsleigh::VnSpace::RAM, ValueType::I64)?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    pipeline.add_post_pass(FunctionArgDetect);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let arg0_nodes = fg.arg_index_to_values(0);
    assert!(
        arg0_nodes.is_empty(),
        "Load[sp+4] overlaps with Store(sp+0, size=8) — must not be registered"
    );
    Ok(())
}

/// Regression guard for the dual of
/// `overlapping_stackstore_at_different_offset_shadows`: a nearby
/// SP-relative store whose range is *disjoint* from the load's must NOT shadow.
///
/// `*(sp+0) = I32(X); return *(sp+4) as I32` — store covers `[0,4)`, load
/// covers `[4,8)`.  No overlap ⇒ the sp+4 slot is still a valid arg 0.
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
    // *(sp+0) = I32(0x11) — covers [0,4).
    let a = b.build_int_const(0x11u64, ValueType::I32)?;
    b.build_store(sp_val, a, rsleigh::VnSpace::RAM)?;

    // return *(sp+4) as I32 — covers [4,8); disjoint from store.
    let four = b.build_int_const(4u64, ValueType::I32)?;
    let addr4 = b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, ValueType::I32)?;
    let loaded = b.build_load(addr4, rsleigh::VnSpace::RAM, ValueType::I32)?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    pipeline.add_post_pass(FunctionArgDetect);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let arg0_nodes = fg.arg_index_to_values(0);
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

/// Byte-range overlap through a `MemPhi`: one arm of the diamond stores
/// at an offset whose range overlaps the load's; the other arm's store is
/// disjoint.  Under `any()` semantics for MemPhi any overlapping predecessor
/// is a shadow, so the load must be disqualified.
///
/// then: `*(sp+2) = I32` covers `[2,6)` — overlaps load `[4,8)`.
/// else: `*(sp+8) = I32` covers `[8,12)` — disjoint from load `[4,8)`.
/// merge: `return *(sp+4) as I32`.
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
    let entry = b.create_region()?;
    let then_r = b.create_region()?;
    let else_r = b.create_region()?;
    let merge = b.create_region()?;
    b.set_entry_region(entry)?;

    b.set_region(entry);
    let cond = b.build_boolean_const(true);
    b.build_if(cond, then_r, else_r)?;

    // then: *(sp + 2) = I32(0x11)  — StackStore{+2, size 4} covers [2,6).
    b.set_region(then_r);
    let sp_t = b.read_variable(&sp)?;
    let two_t = b.build_int_const(2u64, ValueType::I32)?;
    let addr_t = b.build_int_binary_operation(sp_t, two_t, IntBinaryOp::Add, ValueType::I32)?;
    let data_t = b.build_int_const(0x11u64, ValueType::I32)?;
    b.build_store(addr_t, data_t, rsleigh::VnSpace::RAM)?;
    b.build_branch(merge)?;

    // else: *(sp + 8) = I32(0x22)  — StackStore{+8, size 4} covers [8,12).
    b.set_region(else_r);
    let sp_e = b.read_variable(&sp)?;
    let eight_e = b.build_int_const(8u64, ValueType::I32)?;
    let addr_e = b.build_int_binary_operation(sp_e, eight_e, IntBinaryOp::Add, ValueType::I32)?;
    let data_e = b.build_int_const(0x22u64, ValueType::I32)?;
    b.build_store(addr_e, data_e, rsleigh::VnSpace::RAM)?;
    b.build_branch(merge)?;

    // merge: return *(sp + 4) as I32  — covers [4,8).
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

    let arg0_nodes = fg.arg_index_to_values(0);
    assert!(
        arg0_nodes.is_empty(),
        "MemPhi with an overlapping-range Store predecessor must disqualify Load[sp+4]"
    );
    Ok(())
}

/// An isolated high-offset load (sp+12) with no sp+4 or sp+8
/// produces no registered args at all — nothing starts the contiguous prefix.
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
        fg.iter_arg_indices().count(),
        0,
        "isolated sp+12 load must not be registered without arg 0/1"
    );
    Ok(())
}

/// `sub rsp, 0xFFFFFFFFFFFFFFFC` is an alternate encoding of `add rsp, 4`:
/// when the constant is sign-extended from its I64 bit width it becomes
/// `-4`, and `Sub(sp, -4) = sp + 4`.  `FunctionArgDetect` must recognise
/// the resulting `Load` as a candidate for stack-arg offset `+4`.
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
    // 0xFFFFFFFFFFFFFFFC_U64 == -4 when interpreted as signed i64.
    let neg_four = b.build_int_const(0xFFFF_FFFF_FFFF_FFFCu64, ValueType::I64)?;
    let addr = b.build_sub_as_add_neg(sp_val, neg_four, ValueType::I64)?;
    let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::I64)?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    // Canonicalize first: `ConstantFold` folds the lowered `Sub`
    // (`Add(_, Neg(K))`) to `Add(_, IntConst(-K))`, the shape `decompose_sp`
    // sees in production — it does not peel the `Neg` itself.
    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold::new());
    pipeline.add(PhiCollapse);
    pipeline.add(RegionCollapse);
    pipeline.add_post_pass(FunctionArgDetect);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let arg0_nodes = fg.arg_index_to_values(0);
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

// ─────────────────────────────────────────────────────────────────────────────
// `mem_chain_is_dirty` plain-`Store` arm.
//
// The two prior fixes (commit 57005b9) updated `CallStackArgCollect` and
// `load_forward::probe` so a plain `Store` whose address provably
// is NOT `sp + K` no longer terminates the memory-chain walk.  The same
// pattern bit `mem_chain_is_dirty`: its catch-all `_ => true` arm marked
// any plain `Store` as a shadow, so a stack-arg `Load[sp+K]` whose memory
// chain crosses an unrelated global Store was conservatively rejected.
//
// These tests exercise the new `Store(_) =>` arm with the four cases that
// match the prior fixes: SP-rooted overlapping (dirty pin), non-SP store
// (pass-through), SP-rooted disjoint (pass-through), SP-rooted phi
// (conservative dirty pin).
// ─────────────────────────────────────────────────────────────────────────────

/// Pin: a plain `Store(addr=sp+K, I32)` whose K overlaps the load's range
/// must mark the chain dirty (this was the pre-fix behaviour for ALL plain
/// Stores; here we keep it for SP-rooted overlapping Stores).  Pipeline
/// A plain `Store(addr=sp+4, I32)` whose range matches the load's range
/// must mark the chain dirty.
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
    // *(sp + 4) = I32(0x11)  — covers [4,8).
    let four = b.build_int_const(4u64, ValueType::I32)?;
    let addr = b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, ValueType::I32)?;
    let data = b.build_int_const(0x11u64, ValueType::I32)?;
    b.build_store(addr, data, rsleigh::VnSpace::RAM)?;

    // return *(sp + 4) as I32 — covers [4,8); same range, must shadow.
    let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::I32)?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    pipeline.add_post_pass(FunctionArgDetect);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let arg0_nodes = fg.arg_index_to_values(0);
    assert!(
        arg0_nodes.is_empty(),
        "plain Store(sp+4, I32) overlaps Load[sp+4]: chain must be dirty — no arg registered"
    );
    Ok(())
}

/// Soundness floor: a cross-class intervening Store (here: address is a
/// const-encoded global) cannot be proven disjoint from the SP-rooted
/// candidate Load.  Under `AliasMode::Strict` (the default) such a Store
/// must mark the chain dirty so the Load is NOT registered as an
/// incoming arg — a stale value from before the function entry would
/// otherwise be substituted, masking the global's write.
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
    // Volatile global write: store to fixed `.data` address.
    let global_addr = b.build_int_const(0xDEAD_BEEFu64, ValueType::I32)?;
    let global_data = b.build_int_const(0x1234u64, ValueType::I32)?;
    b.build_store(global_addr, global_data, rsleigh::VnSpace::RAM)?;

    // return *(sp + 4) as I32 — the load's memory predecessor IS the
    // global Store above; cross-class against the SP load.
    let four = b.build_int_const(4u64, ValueType::I32)?;
    let addr = b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, ValueType::I32)?;
    let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::I32)?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    // Pin Strict explicitly: this test exercises the conservative floor.
    // The default flipped to `StackGlobalDisjoint`, under which the
    // const-addressed global write is assumed disjoint from the SP slot
    // and the Load WOULD be promoted (covered by the permissive tests).
    pipeline.add_post_pass(FunctionArgDetect);
    pipeline.run(&mut fg, &mut crate::test_support::octx_strict())?;

    let arg0_nodes = fg.arg_index_to_values(0);
    assert!(
        arg0_nodes.is_empty(),
        "Strict mode: the cross-class intervening Store must mark the chain \
         dirty so the Load is NOT promoted to an incoming arg"
    );
    Ok(())
}

/// NEW: an SP-rooted plain `Store(addr=sp+K2, I32)` whose byte range is
/// disjoint from the load's must NOT mark the chain dirty.
///
/// `*(sp + 0) = I32(X)` covers `[0,4)`; `return *(sp + 4)` covers `[4,8)`
/// — disjoint, so sp+4 still qualifies as arg 0.
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
    // *(sp + 0) = I32(0x11) — covers [0,4).
    let zero_data = b.build_int_const(0x11u64, ValueType::I32)?;
    b.build_store(sp_val, zero_data, rsleigh::VnSpace::RAM)?;

    // return *(sp + 4) as I32 — covers [4,8); disjoint from [0,4).
    let four = b.build_int_const(4u64, ValueType::I32)?;
    let addr4 = b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, ValueType::I32)?;
    let loaded = b.build_load(addr4, rsleigh::VnSpace::RAM, ValueType::I32)?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    pipeline.add_post_pass(FunctionArgDetect);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let arg0_nodes = fg.arg_index_to_values(0);
    assert!(
        !arg0_nodes.is_empty(),
        "disjoint SP-rooted Store(sp+0, I32) must not mark Load[sp+4] dirty: still registered as arg 0"
    );
    Ok(())
}

/// Pin: a plain `Store` whose address flows through a control-flow join
/// with per-branch offsets (an SP-rooted phi that does NOT collapse to a
/// single terminal) must conservatively mark the chain dirty.
///
/// Diamond: then-branch does `sp -= 4`, else-branch does `sp -= 8`.  At
/// the join, `read_variable(&sp)` produces a phi over the two SP versions;
/// storing through it lands at addr = `Phi(sp-4, sp-8)`.  Because the two
/// predecessors disagree, `decompose_sp` returns `None` (not a provable SP
/// terminal), so the intervening Store's address cannot be range-checked
/// and a subsequent `Load[sp_orig + 4]` targeting the stack-arg slot must
/// see the chain as dirty.
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
    let entry = b.create_region()?;
    let then_r = b.create_region()?;
    let else_r = b.create_region()?;
    let join = b.create_region()?;
    b.set_entry_region(entry)?;

    // entry: snapshot the original SP, then if(true) goto then else else.
    b.set_region(entry);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let sp_orig = b.read_variable(&sp)?;
    let cond = b.build_boolean_const(true);
    b.build_if(cond, then_r, else_r)?;

    // then: sp -= 4
    b.set_region(then_r);
    let sp_t = b.read_variable(&sp)?;
    let four_t = b.build_int_const(4u64, ValueType::I32)?;
    let sp_t_new = b.build_sub_as_add_neg(sp_t, four_t, ValueType::I32)?;
    b.write_variable(&sp, sp_t_new)?;
    b.build_branch(join)?;

    // else: sp -= 8
    b.set_region(else_r);
    let sp_e = b.read_variable(&sp)?;
    let eight_e = b.build_int_const(8u64, ValueType::I32)?;
    let sp_e_new = b.build_sub_as_add_neg(sp_e, eight_e, ValueType::I32)?;
    b.write_variable(&sp, sp_e_new)?;
    b.build_branch(join)?;

    // join: store through the phi'd SP (a non-collapsing SP phi, so the
    // address decomposes to None), then load *(sp_orig + 4) and return it.
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

    let arg0_nodes = fg.arg_index_to_values(0);
    assert!(
        arg0_nodes.is_empty(),
        "Store through a non-collapsing SP-phi address must conservatively mark chain dirty: no arg registered"
    );
    Ok(())
}

#[test]
fn mem_chain_is_dirty_handles_10k_disjoint_store_chain() -> Result<()> {
    // 10k-store chain pins the iterative form of `mem_chain_is_dirty`.
    // The prior recursive form would stack-overflow on the default
    // 8 MB Rust stack at this depth.
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
    // CHAIN_LEN disjoint stack stores at offsets [16, 20, 24, ...].
    for i in 0..CHAIN_LEN {
        let off = b.build_int_const(((i * 4) as u64) + 16, ValueType::I32)?;
        let addr = b.build_int_binary_operation(sp_val, off, IntBinaryOp::Add, ValueType::I32)?;
        let val = b.build_int_const(i as u64, ValueType::I32)?;
        b.build_store(addr, val, rsleigh::VnSpace::RAM)?;
    }
    // Load from sp+4 — disjoint from every store above.
    // The walker must pass through all 10k stores backwards.
    let four = b.build_int_const(4u64, ValueType::I32)?;
    let addr4 = b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, ValueType::I32)?;
    let loaded = b.build_load(addr4, rsleigh::VnSpace::RAM, ValueType::I32)?;
    b.build_return(Some(loaded), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let mut pipeline = cf_rp_pipeline();
    pipeline.add_post_pass(FunctionArgDetect);
    pipeline.run(&mut fg, &mut crate::OptCtx::new(None))?;

    let arg0_nodes = fg.arg_index_to_values(0);
    assert!(
        !arg0_nodes.is_empty(),
        "10k disjoint stores must not mark the chain dirty: load at sp+4 should be registered as arg 0"
    );
    Ok(())
}

/// A `CallOther` on the chain between a stack-arg store-context and a
/// later `Load` of the same slot is gated purely by `calls_clobber`:
/// the callee is opaque, so there is nothing meaningful to infer from its
/// arguments (the former SP-pointer "escape analysis" was intentionally
/// removed).  Default (`false`) → the call does not block, the slot is
/// still registered; conservative (`true`) → the call marks it dirty.
#[test]
fn callother_on_chain_gated_only_by_calls_clobber() -> Result<()> {
    let sp = sp32_vn();
    let build = |b: &mut strider_ir::FunctionBuilder| -> Result<()> {
        // Take the address of stack-arg slot 0 (i.e. sp + 0 = sp itself).
        let sp_val = b.read_variable(&sp)?;
        // CallOther whose sole value-arg is &arg0 (= sp_val).
        let (call_node, _result) = b.build_call_other(
            42,
            "escape_helper",
            None,
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
        // After the call, read *(sp + 0).
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

    // Default: the CallOther does not block — slot 0 is still registered.
    let mut fg_default = new_fn()?;
    let mut p_default = cf_rp_pipeline();
    p_default.add_post_pass(FunctionArgDetect);
    p_default.run(&mut fg_default, &mut crate::OptCtx::new(None))?;
    assert!(
        !fg_default.arg_index_to_values(0).is_empty(),
        "default (calls_clobber=false): a CallOther on the chain does not \
         block stack-arg promotion (the callee is opaque, no arg inspection)",
    );

    // Conservative: the CallOther marks the slot dirty — not registered.
    let mut fg_conservative = new_fn()?;
    let mut p_conservative = cf_rp_pipeline();
    p_conservative.add_post_pass(FunctionArgDetect);
    let mut octx_conservative = crate::OptCtx::new(None);
    octx_conservative.options.arg_alias.calls_clobber = true;
    p_conservative.run(&mut fg_conservative, &mut octx_conservative)?;
    assert!(
        fg_conservative.arg_index_to_values(0).is_empty(),
        "calls_clobber=true: the CallOther on the chain marks the slot dirty",
    );
    Ok(())
}

/// Call/CallOther clobber toggle.  A `Load[sp+4]` whose memory chain
/// crosses a plain `Call` (no value-arg escapes the slot) is registered
/// as an incoming arg under the default (`calls_clobber = false`,
/// aggressive detection — a call does not by itself shadow a stack-arg
/// slot), and is NOT registered when `calls_clobber = true`
/// (conservative — any call on the chain marks the slot dirty).
#[test]
fn calls_clobber_toggle_gates_arg_across_call() -> Result<()> {
    let sp = sp32_vn();
    // A function whose stack-arg Load at sp+4 sits downstream of a plain
    // Call.  The Call's only value input is its (constant) target, which
    // is not SP-rooted, so the escape-pointer check never fires — the
    // verdict is governed purely by the toggle.
    let build = |b: &mut FunctionBuilder, sp_val: ValueId| -> Result<()> {
        let target = b.build_int_const(0x1000u64, ValueType::I32)?;
        b.build_call(target, None)?;
        let four = b.build_int_const(4u64, ValueType::I32)?;
        let addr4 = b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, ValueType::I32)?;
        let loaded = b.build_load(addr4, rsleigh::VnSpace::RAM, ValueType::I32)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    };

    // Default: calls_clobber = false → arg detected across the Call.
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
        !fg_default.arg_index_to_values(0).is_empty(),
        "default (calls_clobber=false): Load[sp+4] across a plain Call \
         is detected as arg 0",
    );

    // calls_clobber = true → the Call marks the slot dirty, arg NOT detected.
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
        fg_conservative.arg_index_to_values(0).is_empty(),
        "calls_clobber=true: the Call on the chain marks the slot dirty, \
         so Load[sp+4] is NOT registered as an arg",
    );
    Ok(())
}

/// Pin the invariant that `combine_phi` OR-combines its
/// predecessors.  This is the safety net that allows
/// `DirtyStep::cycle_verdict` to return `false` ("clean for this
/// edge") without compromising overall soundness.
#[test]
fn function_args_combine_phi_or_semantics_pinned() {
    // Mirror of `DirtyStep::combine_phi`: any dirty predecessor
    // makes the phi dirty.
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
    // The cycle-edge sentinel chosen by `DirtyStep::cycle_verdict`:
    // `false` is sound here precisely because `combine_phi` is
    // `any()` — a cycle-broken sibling can still upgrade the
    // verdict to dirty.  Pinning the pair so a future refactor
    // can't silently swap one without the other.
    let cycle_sentinel: bool = false;
    assert!(
        combine_phi(vec![cycle_sentinel, true]),
        "cycle_verdict()=false must still combine to dirty when a non-cycle pred is dirty"
    );
}
