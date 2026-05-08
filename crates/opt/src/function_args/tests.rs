use super::*;
use crate::error::Result;
use crate::pipeline::Optimizer;
use ir::node::{FunctionArgSource, NodeKind, NodeOutputType};
use ir::test_utils::{reg_vn, sp_vn_x86_64 as sp_vn};
use ir::{FunctionBuilder, IntBinaryOp};

fn rdi_like_vn() -> rsleigh::Vn {
    // Fake 8-byte register to stand in for x86_64 RDI in tests.
    reg_vn(0x38, 8)
}

fn count<F: Fn(&NodeKind) -> bool>(fg: &BuiltFunctionGraph, pred: F) -> usize {
    fg.all_node_ids()
        .filter(|&n| pred(fg.node_kind(n)))
        .count()
}

/// Slice 1: x86_64-like convention passes arg 0 in a register.  A function
/// that reads that register once should, after `FunctionArgDetect` runs,
/// contain exactly one `FunctionArg { Register(rdi), 0 }` node, and the
/// original `InitialVar(rdi)` use should have been rewired to it.
#[test]
fn reads_rdi_emits_function_arg_0() -> Result<()> {
    let rdi = rdi_like_vn();
    let sp = sp_vn();
    // new_raw(all_vns, callee_saved, ret_val_regs, arg_passing_regs, ...)
    let mut b = FunctionBuilder::new_raw(vec![rdi, sp], &[], &[rdi], &[rdi], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);

    // Build a trivial function that reads rdi and returns it.
    let v = b.read_variable(&rdi)?;
    b.build_return(Some(v), &[])?;
    let mut fg = b.build()?;

    let pass = FunctionArgDetect::new(vec![rdi], sp, vec![]);
    pass.optimize(&mut fg.graph, fg.entry)?;

    let n_fa = count(&fg, |k| {
        matches!(
            k,
            NodeKind::FunctionArg {
                source: FunctionArgSource::Register(r),
                index: 0,
            } if *r == rdi
        )
    });
    assert_eq!(
        n_fa, 1,
        "expected exactly one FunctionArg {{ Register(rdi), 0 }}"
    );

    // The original InitialVar(rdi) should have no remaining live uses
    // (the Return should now source from the FunctionArg output).
    let reachable: std::collections::HashSet<_> = fg.preorder().collect();
    let reachable_initial_rdi = fg
        .all_node_ids()
        .filter(|n| reachable.contains(n))
        .filter(|&n| matches!(fg.node_kind(n), NodeKind::InitialVar(v) if *v == rdi))
        .count();
    assert_eq!(
        reachable_initial_rdi, 0,
        "InitialVar(rdi) should be detached after rewiring"
    );
    Ok(())
}

/// Fake 4-byte SP for x86-cdecl-like scenarios.
fn sp32_vn() -> rsleigh::Vn {
    rsleigh::Vn {
        addr_off: 0x20,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 4,
    }
}

/// Slice 2: x86 cdecl reads its first stack arg at `[sp + 4]`.  With no
/// register args in the convention, the `Load[sp+4]` should be rewritten
/// to a single `FunctionArg { Stack{offset:4}, 0 }` node and all consumers
/// of the load rewired to it.
#[test]
fn reads_stack_arg_0_on_x86_cdecl() -> Result<()> {
    use crate::{ConstantFold, OptimizerPipeline};

    let sp = sp32_vn();
    let mut fg = ir::test_utils::make_sp_fn(sp, |b, sp_val| {
        // addr = sp + 4; load[addr]; return loaded
        let four = b.build_int_const(4u64, NodeOutputType::U32)?;
        let addr =
            b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, NodeOutputType::U32)?;
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    // ConstantFold normalises the address; FunctionArgDetect runs after.
    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add_post_pass(FunctionArgDetect::new(vec![], sp, vec![4]));
    pipeline.run(&mut fg.graph, fg.entry)?;

    let n_fa = count(&fg, |k| {
        matches!(
            k,
            NodeKind::FunctionArg {
                source: FunctionArgSource::Stack { offset: 4, .. },
                index: 0,
            }
        )
    });
    assert_eq!(
        n_fa, 1,
        "expected exactly one FunctionArg {{ Stack{{+4}}, 0 }}"
    );

    // The original Load should no longer be reachable (its single consumer,
    // the Return, now sources from the FunctionArg).
    let reachable: std::collections::HashSet<_> = fg.preorder().collect();
    let reachable_loads = fg
        .all_node_ids()
        .filter(|n| reachable.contains(n))
        .filter(|&n| matches!(fg.node_kind(n), NodeKind::Load(_)))
        .count();
    assert_eq!(
        reachable_loads, 0,
        "Load[sp+4] should be detached after rewiring"
    );
    Ok(())
}

/// Builds `load[sp + offset]` reading a U32 value.  Returns the loaded output.
fn build_sp_load(
    b: &mut FunctionBuilder,
    sp: &rsleigh::Vn,
    offset: u32,
) -> Result<ir::node::NodeOutputId> {
    let sp_val = b.read_variable(sp)?;
    let off_const = b.build_int_const(offset as u64, NodeOutputType::U32)?;
    let addr =
        b.build_int_binary_operation(sp_val, off_const, IntBinaryOp::Add, NodeOutputType::U32)?;
    let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
    Ok(loaded)
}

/// Slice 3: loads at sp+4 and sp+12, but **not** sp+8 — only the contiguous
/// prefix (sp+4 → arg 0) is labelled.  The sp+12 load remains unchanged
/// (i.e. it does **not** get FunctionArg index 2), and no gap-index node
/// is emitted.
#[test]
fn stack_arg_gap_truncates() -> Result<()> {
    use crate::{ConstantFold, OptimizerPipeline};

    let sp = sp32_vn();
    let mut fg = ir::test_utils::make_sp_fn(sp, |b, _sp_val| {
        let a = build_sp_load(b, &sp, 4)?;
        let c = build_sp_load(b, &sp, 12)?;
        // Combine both loads so neither is dead.
        let sum = b.build_int_binary_operation(a, c, IntBinaryOp::Add, NodeOutputType::U32)?;
        b.build_return(Some(sum), &[])?;
        Ok(())
    })?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add_post_pass(FunctionArgDetect::new(vec![], sp, vec![4, 8, 12]));
    pipeline.run(&mut fg.graph, fg.entry)?;

    // Only arg 0 emitted; arg 1 absent (gap) and arg 2 MUST NOT be emitted.
    let arg0 = count(&fg, |k| {
        matches!(
            k,
            NodeKind::FunctionArg {
                source: FunctionArgSource::Stack { offset: 4, .. },
                index: 0,
            }
        )
    });
    let arg1 = count(&fg, |k| {
        matches!(
            k,
            NodeKind::FunctionArg {
                source: FunctionArgSource::Stack { offset: 8, .. },
                ..
            }
        )
    });
    let arg2 = count(&fg, |k| {
        matches!(
            k,
            NodeKind::FunctionArg {
                source: FunctionArgSource::Stack { offset: 12, .. },
                ..
            }
        )
    });
    assert_eq!(arg0, 1, "arg 0 (sp+4) should be emitted");
    assert_eq!(arg1, 0, "arg 1 (sp+8) is absent");
    assert_eq!(arg2, 0, "arg 2 (sp+12) must be truncated by the gap");

    // The sp+12 load must still exist and be reachable.
    let reachable: std::collections::HashSet<_> = fg.preorder().collect();
    let reachable_loads = fg
        .all_node_ids()
        .filter(|n| reachable.contains(n))
        .filter(|&n| matches!(fg.node_kind(n), NodeKind::Load(_)))
        .count();
    assert_eq!(
        reachable_loads, 1,
        "sp+12 Load should remain (sp+4 Load replaced)"
    );
    Ok(())
}

/// Slice 4: a prior `StackStore{+4}` shadows the `Load[sp+4]` — the load
/// reads the stored value, not the caller's arg.  No FunctionArg emitted.
#[test]
fn prior_stackstore_shadows() -> Result<()> {
    use crate::{ConstantFold, OptimizerPipeline, RedundantPhis, StackStoreDetect};

    let sp = sp32_vn();
    let mut fg = ir::test_utils::make_sp_fn(sp, |b, sp_val| {
        // *(sp + 4) = 0x11; return *(sp + 4)
        let four = b.build_int_const(4u64, NodeOutputType::U32)?;
        let addr =
            b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, NodeOutputType::U32)?;
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
    pipeline.add_post_pass(FunctionArgDetect::new(vec![], sp, vec![4]));
    pipeline.run(&mut fg.graph, fg.entry)?;

    let any_fa = count(&fg, |k| matches!(k, NodeKind::FunctionArg { .. }));
    assert_eq!(
        any_fa, 0,
        "Load[sp+4] is shadowed by StackStore{{+4}}, not a function arg"
    );
    Ok(())
}

/// Slice 4 (audit B2 blocker): if-branch where the true side does
/// `StackStore{+4}`, false side does nothing — their join is a `MemPhi`,
/// and a later `Load[sp+4]` from the phi must be disqualified.  The DFS
/// treats `MemPhi` as a fork where **every** predecessor must be clean.
#[test]
fn memphi_shadow_disqualifies() -> Result<()> {
    use crate::{ConstantFold, OptimizerPipeline, RedundantPhis, StackStoreDetect};

    let sp = sp32_vn();
    let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
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
    let four_t = b.build_int_const(4u64, NodeOutputType::U32)?;
    let addr_t = b.build_int_binary_operation(
        sp_t,
        four_t,
        IntBinaryOp::Add,
        NodeOutputType::U32,
    )?;
    let data = b.build_int_const(0x22u64, NodeOutputType::U32)?;
    b.build_store(addr_t, data, rsleigh::VnSpace::RAM)?;
    b.build_branch(join)?;

    // false_br: fallthrough to join
    b.set_region(false_br);
    b.build_branch(join)?;

    // join: return *(sp+4)
    b.set_region(join);
    let sp_j = b.read_variable(&sp)?;
    let four_j = b.build_int_const(4u64, NodeOutputType::U32)?;
    let addr_j = b.build_int_binary_operation(
        sp_j,
        four_j,
        IntBinaryOp::Add,
        NodeOutputType::U32,
    )?;
    let loaded = b.build_load(addr_j, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
    b.build_return(Some(loaded), &[])?;
    let mut fg = b.build()?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    pipeline.add_post_pass(FunctionArgDetect::new(vec![], sp, vec![4]));
    pipeline.run(&mut fg.graph, fg.entry)?;

    let any_fa = count(&fg, |k| matches!(k, NodeKind::FunctionArg { .. }));
    assert_eq!(
        any_fa, 0,
        "Load[sp+4] reaches a MemPhi with a shadowing branch — disqualified"
    );
    Ok(())
}

/// 8-byte SP varnode for aarch64-like scenarios.
fn sp64_vn() -> rsleigh::Vn {
    rsleigh::Vn {
        addr_off: 0x40,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 8,
    }
}

/// Slice 5 (audit I2): if the same stack-arg slot is read at multiple
/// widths — e.g. aarch64 reading both `x0` (8 bytes) and `w0` (4 bytes)
/// from `sp+0` — emit **one** `FunctionArg` at the widest observed width
/// and route narrower reads through `Truncate(FunctionArg)`.
#[test]
fn narrower_load_at_arg_slot_uses_truncate() -> Result<()> {
    use crate::{ConstantFold, OptimizerPipeline};

    let sp = sp64_vn();
    let mut fg = ir::test_utils::make_sp_fn(sp, |b, sp_val| {
        // Read sp+0 as U32, then sp+0 as U64.  Combine so neither is dead.
        let narrow = b.build_load(sp_val, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
        let wide = b.build_load(sp_val, rsleigh::VnSpace::RAM, NodeOutputType::U64)?;
        let narrow_ext =
            b.extend_if_needed(narrow, NodeOutputType::U64, ir::ExtendOp::ZeroExtend)?;
        let sum = b.build_int_binary_operation(
            narrow_ext,
            wide,
            IntBinaryOp::Add,
            NodeOutputType::U64,
        )?;
        b.build_return(Some(sum), &[])?;
        Ok(())
    })?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add_post_pass(FunctionArgDetect::new(vec![], sp, vec![0]));
    pipeline.run(&mut fg.graph, fg.entry)?;

    // Exactly one FunctionArg at offset 0.
    let fa_count = count(&fg, |k| {
        matches!(
            k,
            NodeKind::FunctionArg {
                source: FunctionArgSource::Stack { offset: 0, .. },
                index: 0,
            }
        )
    });
    assert_eq!(fa_count, 1, "exactly one FunctionArg at offset 0");

    // That one FunctionArg must be at U64 (the widest observed load).
    let fa_node = fg
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::FunctionArg { .. }))
        .expect("FunctionArg exists");
    let [fa_out] = fg.node_outputs_exact::<1>(fa_node)?;
    assert_eq!(
        fg.output_kind(fa_out).as_value(),
        Some(NodeOutputType::U64),
        "FunctionArg output width should match widest load (U64)"
    );

    // The narrow (U32) use must be re-routed through a `Truncate` node
    // whose input is the FunctionArg's output.
    let reachable: std::collections::HashSet<_> = fg.preorder().collect();
    let trunc_from_fa = fg
        .all_node_ids()
        .filter(|n| reachable.contains(n))
        .filter(|&n| matches!(fg.node_kind(n), NodeKind::Truncate))
        .filter(|&n| {
            let inputs = fg.node_inputs(n);
            inputs.len() == 1 && inputs[0] == fa_out
        })
        .count();
    assert_eq!(
        trunc_from_fa, 1,
        "expected one Truncate consuming the FunctionArg output"
    );
    Ok(())
}

/// Audit I4: an `InitialVar(arg_reg)` with no live uses must not produce a
/// `FunctionArg` node.  The pass is not pinning unreferenced registers.
/// `FunctionArgDetect` runs after the fixed-point loop, so the setup here
/// includes `RedundantPhis` to strip phantom phi consumers the builder
/// creates during variable tracking.
#[test]
fn unused_register_arg_yields_no_node() -> Result<()> {
    use crate::{OptimizerPipeline, RedundantPhis};

    let rdi = rdi_like_vn();
    let sp = sp_vn();
    let mut b = FunctionBuilder::new_raw(vec![rdi, sp], &[], &[rdi], &[rdi], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);

    // Return a constant — rdi is never read.
    let c = b.build_int_const(0u64, NodeOutputType::U64)?;
    b.build_return(Some(c), &[])?;
    let mut fg = b.build()?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(RedundantPhis);
    pipeline.add_post_pass(FunctionArgDetect::new(vec![rdi], sp, vec![]));
    pipeline.run(&mut fg.graph, fg.entry)?;

    let n_fa = count(&fg, |k| matches!(k, NodeKind::FunctionArg { .. }));
    assert_eq!(
        n_fa, 0,
        "unused InitialVar(rdi) must not be labelled as FunctionArg"
    );
    Ok(())
}

/// x86_64-like: two register args (rdi, rsi) and a stack arg at `sp+8`
/// (i.e. arg 6 in SysV; for this test arg 2).  All three should become
/// `FunctionArg` nodes, indexed 0, 1, and 2 respectively.
#[test]
fn x86_64_mixed_reg_and_stack() -> Result<()> {
    use crate::{ConstantFold, OptimizerPipeline};

    let rdi = rdi_like_vn();
    let rsi = rsleigh::Vn {
        addr_off: 0x30,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 8,
    };
    let sp = sp_vn();
    let mut b = FunctionBuilder::new_raw(
        vec![rdi, rsi, sp],
        &[],
        &[rdi],
        &[rdi, rsi],
        None,
        0,
    )?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);

    let a = b.read_variable(&rdi)?;
    let bb = b.read_variable(&rsi)?;
    let sp_val = b.read_variable(&sp)?;
    let eight = b.build_int_const(8u64, NodeOutputType::U64)?;
    let addr =
        b.build_int_binary_operation(sp_val, eight, IntBinaryOp::Add, NodeOutputType::U64)?;
    let c = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U64)?;
    let ab = b.build_int_binary_operation(a, bb, IntBinaryOp::Add, NodeOutputType::U64)?;
    let abc = b.build_int_binary_operation(ab, c, IntBinaryOp::Add, NodeOutputType::U64)?;
    b.build_return(Some(abc), &[])?;
    let mut fg = b.build()?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add_post_pass(FunctionArgDetect::new(vec![rdi, rsi], sp, vec![8]));
    pipeline.run(&mut fg.graph, fg.entry)?;

    let fa_reg0 = count(&fg, |k| {
        matches!(
            k,
            NodeKind::FunctionArg {
                source: FunctionArgSource::Register(r),
                index: 0,
            } if *r == rdi
        )
    });
    let fa_reg1 = count(&fg, |k| {
        matches!(
            k,
            NodeKind::FunctionArg {
                source: FunctionArgSource::Register(r),
                index: 1,
            } if *r == rsi
        )
    });
    let fa_stack2 = count(&fg, |k| {
        matches!(
            k,
            NodeKind::FunctionArg {
                source: FunctionArgSource::Stack { offset: 8, .. },
                index: 2,
            }
        )
    });
    assert_eq!(fa_reg0, 1, "rdi → FunctionArg index 0");
    assert_eq!(fa_reg1, 1, "rsi → FunctionArg index 1");
    assert_eq!(fa_stack2, 1, "sp+8 → FunctionArg index 2");
    Ok(())
}

/// Byte-range overlap: a `StackStore` at a *different* offset whose byte
/// range nevertheless overlaps the load's must shadow it.  Exact-offset
/// comparison would miss this.
///
/// `*(sp+0) = U64(X); return *(sp+4) as U64` — store covers `[0,8)`, load
/// covers `[4,12)`.  With the byte-range overlap check the load is
/// disqualified; with the old `k == offset` check it is mis-labelled as a
/// function arg.
#[test]
fn overlapping_stackstore_at_different_offset_shadows() -> Result<()> {
    use crate::{ConstantFold, OptimizerPipeline, RedundantPhis, StackStoreDetect};

    let sp = sp64_vn();
    let mut fg = ir::test_utils::make_sp_fn(sp, |b, sp_val| {
        // *(sp+0) = U64(0xDEAD_BEEF_CAFE_BABE)
        let wide_data = b.build_int_const(0xDEAD_BEEF_CAFE_BABEu64, NodeOutputType::U64)?;
        b.build_store(sp_val, wide_data, rsleigh::VnSpace::RAM)?;

        // return *(sp+4) as U64
        let four = b.build_int_const(4u64, NodeOutputType::U64)?;
        let addr4 =
            b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, NodeOutputType::U64)?;
        let loaded = b.build_load(addr4, rsleigh::VnSpace::RAM, NodeOutputType::U64)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    pipeline.add_post_pass(FunctionArgDetect::new(vec![], sp, vec![4]));
    pipeline.run(&mut fg.graph, fg.entry)?;

    let any_fa = count(&fg, |k| matches!(k, NodeKind::FunctionArg { .. }));
    assert_eq!(
        any_fa, 0,
        "Load[sp+4] overlaps with StackStore{{+0, size=8}} — must be shadowed"
    );
    Ok(())
}

/// Regression guard for the dual of
/// `overlapping_stackstore_at_different_offset_shadows`: a nearby
/// `StackStore` whose range is *disjoint* from the load's must NOT shadow.
///
/// `*(sp+0) = U32(X); return *(sp+4) as U32` — store covers `[0,4)`, load
/// covers `[4,8)`.  No overlap ⇒ the sp+4 slot is still a valid arg 0.
#[test]
fn disjoint_stackstore_at_nearby_offset_is_not_shadow() -> Result<()> {
    use crate::{ConstantFold, OptimizerPipeline, RedundantPhis, StackStoreDetect};

    let sp = sp32_vn();
    let mut fg = ir::test_utils::make_sp_fn(sp, |b, sp_val| {
        // *(sp+0) = U32(0x11) — covers [0,4).
        let a = b.build_int_const(0x11u64, NodeOutputType::U32)?;
        b.build_store(sp_val, a, rsleigh::VnSpace::RAM)?;

        // return *(sp+4) as U32 — covers [4,8); disjoint from store.
        let four = b.build_int_const(4u64, NodeOutputType::U32)?;
        let addr4 =
            b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, NodeOutputType::U32)?;
        let loaded = b.build_load(addr4, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    pipeline.add_post_pass(FunctionArgDetect::new(vec![], sp, vec![4]));
    pipeline.run(&mut fg.graph, fg.entry)?;

    let fa_at_4 = count(&fg, |k| {
        matches!(
            k,
            NodeKind::FunctionArg {
                source: FunctionArgSource::Stack { offset: 4, .. },
                index: 0,
            }
        )
    });
    assert_eq!(
        fa_at_4, 1,
        "disjoint StackStore{{+0, size=4}} must not shadow Load[sp+4]"
    );
    Ok(())
}

/// Byte-range overlap through a `MemPhi`: one arm of the diamond stores
/// at an offset whose range overlaps the load's; the other arm's store is
/// disjoint.  Under `any()` semantics for MemPhi any overlapping predecessor
/// is a shadow, so the load must be disqualified — but the old exact-offset
/// check misses the overlap on the overlapping arm and mis-labels the load.
///
/// then: `*(sp+2) = U32` covers `[2,6)` — overlaps load `[4,8)`.
/// else: `*(sp+8) = U32` covers `[8,12)` — disjoint from load `[4,8)`.
/// merge: `return *(sp+4) as U32`.
#[test]
fn memphi_partial_overlap_shadows() -> Result<()> {
    use crate::{ConstantFold, OptimizerPipeline, RedundantPhis, StackStoreDetect};

    let sp = sp32_vn();
    let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
    let entry = b.create_region()?;
    let then_r = b.create_region()?;
    let else_r = b.create_region()?;
    let merge = b.create_region()?;
    b.set_entry_region(entry)?;

    b.set_region(entry);
    let cond = b.build_boolean_const(true);
    b.build_if(cond, then_r, else_r)?;

    // then: *(sp + 2) = U32(0x11)  — StackStore{+2, size 4} covers [2,6).
    b.set_region(then_r);
    let sp_t = b.read_variable(&sp)?;
    let two_t = b.build_int_const(2u64, NodeOutputType::U32)?;
    let addr_t =
        b.build_int_binary_operation(sp_t, two_t, IntBinaryOp::Add, NodeOutputType::U32)?;
    let data_t = b.build_int_const(0x11u64, NodeOutputType::U32)?;
    b.build_store(addr_t, data_t, rsleigh::VnSpace::RAM)?;
    b.build_branch(merge)?;

    // else: *(sp + 8) = U32(0x22)  — StackStore{+8, size 4} covers [8,12).
    b.set_region(else_r);
    let sp_e = b.read_variable(&sp)?;
    let eight_e = b.build_int_const(8u64, NodeOutputType::U32)?;
    let addr_e =
        b.build_int_binary_operation(sp_e, eight_e, IntBinaryOp::Add, NodeOutputType::U32)?;
    let data_e = b.build_int_const(0x22u64, NodeOutputType::U32)?;
    b.build_store(addr_e, data_e, rsleigh::VnSpace::RAM)?;
    b.build_branch(merge)?;

    // merge: return *(sp + 4) as U32  — covers [4,8).
    b.set_region(merge);
    let sp_m = b.read_variable(&sp)?;
    let four_m = b.build_int_const(4u64, NodeOutputType::U32)?;
    let addr_m =
        b.build_int_binary_operation(sp_m, four_m, IntBinaryOp::Add, NodeOutputType::U32)?;
    let loaded = b.build_load(addr_m, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
    b.build_return(Some(loaded), &[])?;
    let mut fg = b.build()?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.add(StackStoreDetect::new(sp));
    pipeline.add_post_pass(FunctionArgDetect::new(vec![], sp, vec![4]));
    pipeline.run(&mut fg.graph, fg.entry)?;

    let any_fa = count(&fg, |k| matches!(k, NodeKind::FunctionArg { .. }));
    assert_eq!(
        any_fa, 0,
        "MemPhi with an overlapping-range StackStore predecessor must disqualify Load[sp+4]"
    );
    Ok(())
}

/// Slice 3: an isolated high-offset load (sp+12) with no sp+4 or sp+8
/// produces no FunctionArg at all — nothing starts the contiguous prefix.
#[test]
fn isolated_high_offset_load_dropped() -> Result<()> {
    use crate::{ConstantFold, OptimizerPipeline};

    let sp = sp32_vn();
    let mut fg = ir::test_utils::make_sp_fn(sp, |b, _sp_val| {
        let v = build_sp_load(b, &sp, 12)?;
        b.build_return(Some(v), &[])?;
        Ok(())
    })?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add_post_pass(FunctionArgDetect::new(vec![], sp, vec![4, 8, 12]));
    pipeline.run(&mut fg.graph, fg.entry)?;

    let any_fa = count(&fg, |k| matches!(k, NodeKind::FunctionArg { .. }));
    assert_eq!(
        any_fa, 0,
        "isolated sp+12 load must not be labelled without arg 0/1"
    );
    Ok(())
}

/// `sub rsp, 0xFFFFFFFFFFFFFFFC` is an alternate encoding of `add rsp, 4`:
/// when the constant is sign-extended from its U64 bit width it becomes
/// `-4`, and `Sub(sp, -4) = sp + 4`.  `FunctionArgDetect` must recognise
/// the resulting `Load` as a candidate for stack-arg offset `+4` via
/// `int_const_signed`'s sign extension — without `ConstantFold`
/// rewriting the address into its canonical form first.
#[test]
fn load_via_sub_negative_unsigned_recognised_as_stack_arg() -> Result<()> {
    use crate::{OptimizerPipeline, RedundantPhis};

    let sp = sp_vn();
    let mut fg = ir::test_utils::make_sp_fn(sp, |b, sp_val| {
        // 0xFFFFFFFFFFFFFFFC_U64 == -4 when interpreted as signed i64.
        let neg_four = b.build_int_const(0xFFFF_FFFF_FFFF_FFFCu64, NodeOutputType::U64)?;
        let addr = b.build_int_sub(sp_val, neg_four, NodeOutputType::U64,
        )?;
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U64)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    // Omit `ConstantFold` so the alternate encoding reaches
    // `decompose_sp` as-lifted.
    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(RedundantPhis);
    pipeline.add_post_pass(FunctionArgDetect::new(vec![], sp, vec![4]));
    pipeline.run(&mut fg.graph, fg.entry)?;

    let fa = count(&fg, |k| {
        matches!(
            k,
            NodeKind::FunctionArg {
                source: FunctionArgSource::Stack { offset: 4, .. },
                index: 0,
            }
        )
    });
    assert_eq!(
        fa, 1,
        "Sub(sp, 0xFFFFFFFFFFFFFFFC_U64) must decompose to offset +4 and be recognised as stack arg 0",
    );
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// `mem_chain_is_dirty` plain-`Store` arm.
//
// The two prior fixes (commit 57005b9) updated `CallStackArgCollect` and
// `stack_load_forward::probe` so a plain `Store` whose address provably
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

/// Pin: a plain `Store(addr=sp+K, U32)` whose K overlaps the load's range
/// must mark the chain dirty (this was the pre-fix behaviour for ALL plain
/// Stores; here we keep it for SP-rooted overlapping Stores).  Pipeline
/// omits `StackStoreDetect` so the Store stays a plain `Store` on the
/// memory chain when `mem_chain_is_dirty` walks it.
#[test]
fn mem_chain_is_dirty_terminates_at_overlapping_store_to_sp_rel_addr() -> Result<()> {
    use crate::{ConstantFold, OptimizerPipeline};

    let sp = sp32_vn();
    let mut fg = ir::test_utils::make_sp_fn(sp, |b, sp_val| {
        // *(sp + 4) = U32(0x11)  — covers [4,8); a plain Store (no
        // StackStoreDetect in the pipeline).
        let four = b.build_int_const(4u64, NodeOutputType::U32)?;
        let addr =
            b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, NodeOutputType::U32)?;
        let data = b.build_int_const(0x11u64, NodeOutputType::U32)?;
        b.build_store(addr, data, rsleigh::VnSpace::RAM)?;

        // return *(sp + 4) as U32 — covers [4,8); same range, must shadow.
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add_post_pass(FunctionArgDetect::new(vec![], sp, vec![4]));
    pipeline.run(&mut fg.graph, fg.entry)?;

    let any_fa = count(&fg, |k| matches!(k, NodeKind::FunctionArg { .. }));
    assert_eq!(
        any_fa, 0,
        "plain Store(sp+4, U32) overlaps Load[sp+4]: chain must be dirty"
    );
    Ok(())
}

/// NEW: a plain `Store(addr=IntConst(global_addr), U32)` between the
/// function-entry memory and a `Load[sp+K]` candidate must NOT mark the
/// chain dirty.  Such a Store is provably non-stack-aliasing (its address
/// does not decompose to `sp + K`), so the walker should pass through it.
///
/// Models the cause #2 reproducer for `mem_chain_is_dirty`: gcc/clang
/// at -O2 freely interleave volatile global writes (`volatile int g = …;`
/// barriers) between the function-entry stack-arg loads and the call.
/// Without the fix, the global Store hits the `_ => true` catch-all and the
/// load is dropped from the FunctionArg group.
#[test]
fn mem_chain_is_dirty_passes_through_non_sp_store() -> Result<()> {
    use crate::{ConstantFold, OptimizerPipeline};

    let sp = sp32_vn();
    let mut fg = ir::test_utils::make_sp_fn(sp, |b, sp_val| {
        // Volatile global write: store to fixed `.data` address.  decompose_sp
        // returns None for an IntConst address — the new branch must continue
        // past it.
        let global_addr = b.build_int_const(0xDEAD_BEEFu64, NodeOutputType::U32)?;
        let global_data = b.build_int_const(0x1234u64, NodeOutputType::U32)?;
        b.build_store(global_addr, global_data, rsleigh::VnSpace::RAM)?;

        // return *(sp + 4) as U32 — the load's memory predecessor IS the
        // global Store above, so without the fix this is rejected as dirty.
        let four = b.build_int_const(4u64, NodeOutputType::U32)?;
        let addr =
            b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, NodeOutputType::U32)?;
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add_post_pass(FunctionArgDetect::new(vec![], sp, vec![4]));
    pipeline.run(&mut fg.graph, fg.entry)?;

    let fa = count(&fg, |k| {
        matches!(
            k,
            NodeKind::FunctionArg {
                source: FunctionArgSource::Stack { offset: 4, .. },
                index: 0,
            }
        )
    });
    assert_eq!(
        fa, 1,
        "non-SP-rooted Store must not mark chain dirty: Load[sp+4] should still qualify as stack arg 0"
    );
    Ok(())
}

/// NEW: an SP-rooted plain `Store(addr=sp+K2, U32)` whose byte range is
/// disjoint from the load's must NOT mark the chain dirty.  After
/// decomposing the Store address to `Terminal { offset: K2 }`, the
/// `ranges_disjoint(K2, store_size, K, load_size)` check should let the
/// walker recurse into the Store's MEM input.
///
/// `*(sp + 0) = U32(X)` covers `[0,4)`; `return *(sp + 4)` covers `[4,8)`
/// — disjoint, so sp+4 still qualifies as arg 0.
#[test]
fn mem_chain_is_dirty_passes_through_disjoint_sp_store() -> Result<()> {
    use crate::{ConstantFold, OptimizerPipeline};

    let sp = sp32_vn();
    let mut fg = ir::test_utils::make_sp_fn(sp, |b, sp_val| {
        // *(sp + 0) = U32(0x11) — covers [0,4); plain Store (no StackStoreDetect).
        let zero_data = b.build_int_const(0x11u64, NodeOutputType::U32)?;
        b.build_store(sp_val, zero_data, rsleigh::VnSpace::RAM)?;

        // return *(sp + 4) as U32 — covers [4,8); disjoint from [0,4).
        let four = b.build_int_const(4u64, NodeOutputType::U32)?;
        let addr4 =
            b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, NodeOutputType::U32)?;
        let loaded = b.build_load(addr4, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add_post_pass(FunctionArgDetect::new(vec![], sp, vec![4]));
    pipeline.run(&mut fg.graph, fg.entry)?;

    let fa = count(&fg, |k| {
        matches!(
            k,
            NodeKind::FunctionArg {
                source: FunctionArgSource::Stack { offset: 4, .. },
                index: 0,
            }
        )
    });
    assert_eq!(
        fa, 1,
        "disjoint SP-rooted Store(sp+0, U32) must not mark Load[sp+4] dirty: still arg 0"
    );
    Ok(())
}

/// Pin: a plain `Store` whose address decomposes to `SpExpr::Phi { … }`
/// (SP-rooted but flowing through a control-flow join with per-branch
/// offsets) must conservatively mark the chain dirty — handling phi-of-SP
/// would require per-pred range analysis.  Mirrors `stack_load_forward::probe`'s
/// posture for the same SpExpr variant.
///
/// Diamond: then-branch does `sp -= 4`, else-branch does `sp -= 8`.  At
/// the join, `read_variable(&sp)` produces a phi over the two SP versions;
/// storing through it lands at addr = `Phi(sp-4, sp-8)`.  A subsequent
/// `Load[sp_orig + 4]` (using the pre-branch SP) targets the stack-arg
/// slot, but the intervening Store's address phi cannot be range-checked,
/// so the chain must be dirty.
#[test]
fn mem_chain_is_dirty_terminates_at_overlapping_phi_of_sp() -> Result<()> {
    use crate::{ConstantFold, OptimizerPipeline, RedundantPhis};

    let sp = sp32_vn();
    let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
    let entry = b.create_region()?;
    let then_r = b.create_region()?;
    let else_r = b.create_region()?;
    let join = b.create_region()?;
    b.set_entry_region(entry)?;

    // entry: snapshot the original SP, then if(true) goto then else else.
    b.set_region(entry);
    let sp_orig = b.read_variable(&sp)?;
    let cond = b.build_boolean_const(true);
    b.build_if(cond, then_r, else_r)?;

    // then: sp -= 4
    b.set_region(then_r);
    let sp_t = b.read_variable(&sp)?;
    let four_t = b.build_int_const(4u64, NodeOutputType::U32)?;
    let sp_t_new =
        b.build_int_sub(sp_t, four_t, NodeOutputType::U32)?;
    b.write_variable(&sp, sp_t_new)?;
    b.build_branch(join)?;

    // else: sp -= 8
    b.set_region(else_r);
    let sp_e = b.read_variable(&sp)?;
    let eight_e = b.build_int_const(8u64, NodeOutputType::U32)?;
    let sp_e_new =
        b.build_int_sub(sp_e, eight_e, NodeOutputType::U32)?;
    b.write_variable(&sp, sp_e_new)?;
    b.build_branch(join)?;

    // join: store through the phi'd SP (address decomposes to SpExpr::Phi),
    // then load *(sp_orig + 4) and return it.
    b.set_region(join);
    let phi_sp = b.read_variable(&sp)?;
    let trash = b.build_int_const(0xAAu64, NodeOutputType::U32)?;
    b.build_store(phi_sp, trash, rsleigh::VnSpace::RAM)?;

    let four_j = b.build_int_const(4u64, NodeOutputType::U32)?;
    let addr_j =
        b.build_int_binary_operation(sp_orig, four_j, IntBinaryOp::Add, NodeOutputType::U32)?;
    let loaded = b.build_load(addr_j, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
    b.build_return(Some(loaded), &[])?;
    let mut fg = b.build()?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add(RedundantPhis);
    pipeline.add_post_pass(FunctionArgDetect::new(vec![], sp, vec![4]));
    pipeline.run(&mut fg.graph, fg.entry)?;

    let any_fa = count(&fg, |k| matches!(k, NodeKind::FunctionArg { .. }));
    assert_eq!(
        any_fa, 0,
        "Store with SpExpr::Phi address must conservatively mark chain dirty: no FunctionArg"
    );
    Ok(())
}

#[test]
fn mem_chain_is_dirty_handles_10k_disjoint_store_chain() -> Result<()> {
    use crate::{ConstantFold, OptimizerPipeline};

    // 10k-store chain pins the iterative form of `mem_chain_is_dirty`
    // (scale.md A3).  The prior recursive form would stack-overflow
    // on the default 8 MB Rust stack at this depth.
    const CHAIN_LEN: usize = 10_000;

    let sp = sp32_vn();
    let mut fg = ir::test_utils::make_sp_fn(sp, |b, sp_val| {
        // CHAIN_LEN disjoint stack stores at offsets [16, 20, 24, ...].
        for i in 0..CHAIN_LEN {
            let off = b.build_int_const(((i * 4) as u64) + 16, NodeOutputType::U32)?;
            let addr = b.build_int_binary_operation(
                sp_val, off, IntBinaryOp::Add, NodeOutputType::U32,
            )?;
            let val = b.build_int_const(i as u64, NodeOutputType::U32)?;
            b.build_store(addr, val, rsleigh::VnSpace::RAM)?;
        }
        // Load from sp+4 — disjoint from every store above.
        // The walker must pass through all 10k stores backwards.
        let four = b.build_int_const(4u64, NodeOutputType::U32)?;
        let addr4 = b.build_int_binary_operation(
            sp_val, four, IntBinaryOp::Add, NodeOutputType::U32,
        )?;
        let loaded = b.build_load(addr4, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
        b.build_return(Some(loaded), &[])?;
        Ok(())
    })?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold);
    pipeline.add_post_pass(FunctionArgDetect::new(vec![], sp, vec![4]));
    pipeline.run(&mut fg.graph, fg.entry)?;

    let fa = count(&fg, |k| {
        matches!(
            k,
            NodeKind::FunctionArg {
                source: FunctionArgSource::Stack { offset: 4, .. },
                index: 0,
            }
        )
    });
    assert_eq!(
        fa, 1,
        "10k disjoint stores must not mark the chain dirty: load at sp+4 forwards to FunctionArg"
    );
    Ok(())
}
