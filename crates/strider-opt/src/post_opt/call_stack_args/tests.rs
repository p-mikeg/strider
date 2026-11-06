use super::*;
use crate::AssumptionOptions;
use crate::Result;
use crate::mem_analysis::WALK_STEPS;
use crate::mem_ssa::CLIMB_STEPS;
use crate::test_support::{CF_RP, COLLAPSE, assert_near_linear, at, run_passes, steps};
use strider_ir::node::{NodeId, NodeKind, ValueId, ValueType};
use strider_ir::{Function, IRBuilderExt, IRViewer, IRWalker, IntBinaryOp};
use strider_ir_test_utils::IrBuilderEx;
use strider_ir_test_utils::IrWalkerEx;
use strider_ir_test_utils::{sp_frame, stack_args_at, stack_vn_x86 as stack_vn};

fn const_val(fg: &Function, v: ValueId, ctx: &str) -> u128 {
    fg.int_const_u128(v).unwrap_or_else(|| {
        panic!(
            "collected arg should be an IntConst, got {:?}; {ctx}",
            fg.kind_of_value(v)
        )
    })
}

/// One step of a call site's setup over a 32-bit SP; offsets are from the
/// entry SP.
#[derive(Clone, Copy)]
enum St {
    /// `sp -= 4; [sp] = v`.
    Push(u64),
    /// `[sp + off] = v`, 4 bytes.
    At(i64, u64),
    /// `[sp + off] = v`, `bits` wide.
    Wide(i64, u128, u16),
    /// `sp = entry_sp + off`.
    SpAt(i64),
    /// `[addr] = v`, a global.
    Global(u64, u64),
}
use St::{At, Global, Push, SpAt, Wide};

/// `steps`, then `call 0x1000`, over a 32-bit frame whose stack arguments
/// start at `base`, one every 4 bytes.
fn cdecl_fn(base: i128, steps: &[St]) -> Result<Function> {
    let sp = stack_vn();
    let i32_ty = ValueType::int(32);
    let mut b = sp_frame(sp)
        .stack_args(stack_args_at(base, 4))
        .build_fn_single_region()?;
    let entry = b.read_variable(&sp)?;
    let mut cur = entry;
    for &step in steps {
        let (addr, data) = match step {
            Push(v) => {
                let four = b.build_int_const(4u64, i32_ty)?;
                cur = b.build_sub_as_add_neg(cur, four, i32_ty)?;
                b.write_variable(&sp, cur)?;
                (cur, b.build_int_const(v, i32_ty)?)
            }
            At(off, v) => (
                at(&mut b, entry, off, i32_ty)?,
                b.build_int_const(v, i32_ty)?,
            ),
            Wide(off, v, bits) => (
                at(&mut b, entry, off, i32_ty)?,
                b.build_int_const(v, ValueType::int(bits))?,
            ),
            SpAt(off) => {
                cur = at(&mut b, entry, off, i32_ty)?;
                b.write_variable(&sp, cur)?;
                continue;
            }
            Global(addr, v) => (
                b.build_int_const(addr, i32_ty)?,
                b.build_int_const(v, i32_ty)?,
            ),
        };
        b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
    }
    let target = b.build_int_const(0x1000u64, i32_ty)?;
    b.build_call_cc(target, None)?;
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    b.build()
}

/// [`cdecl_fn`] collected under `claims`: the collected arguments, each a
/// constant.
fn collected_under(claims: &AssumptionOptions, base: i128, steps: &[St]) -> Result<Vec<u128>> {
    let mut fg = cdecl_fn(base, steps)?;
    fg.assume(claims);
    let inputs = collect_call_inputs(&mut fg)?;
    Ok(inputs[4..]
        .iter()
        .map(|&v| const_val(&fg, v, "collected arg"))
        .collect())
}

fn collected(base: i128, steps: &[St]) -> Result<Vec<u128>> {
    collected_under(&AssumptionOptions::none(), base, steps)
}

/// [`CF_RP`] plus `CallStackArgCollect`, then the sole Call's inputs.
fn collect_call_inputs(fg: &mut Function) -> Result<Vec<ValueId>> {
    let mut pipeline = crate::test_support::pipeline(CF_RP);
    pipeline.add_post_pass(&CallStackArgCollect);
    pipeline.run(fg, &crate::OptCtx::new(None))?;
    Ok(fg.node_inputs(find_call(fg)).into_iter().collect())
}

fn find_call(fg: &Function) -> NodeId {
    fg.first_kind(|k| matches!(k, NodeKind::Call { .. }))
}

/// Each row's setup collects exactly its dense prefix of argument slots, in
/// slot order, whatever order the chain wrote them in.
#[test]
fn collects_the_dense_argument_prefix() -> Result<()> {
    let ten: Vec<St> = (0..10).rev().map(|i| Push(100 + i)).collect();
    let rows: &[(&str, i128, &[St], &[u128])] = &[
        ("push arg1; push arg0", 0, &[Push(22), Push(11)], &[11, 22]),
        // `StackArgs` collection is unbounded.
        (
            "ten pushes",
            0,
            &ten,
            &[100, 101, 102, 103, 104, 105, 106, 107, 108, 109],
        ),
        ("one push", 0, &[Push(99)], &[99]),
        // i386 `free(arg0, arg1)`: args written to `(%esp)` and `4(%esp)` in
        // program order, then `call` pushes the return address, so the chain
        // backward is ret-addr, arg1, arg0.
        (
            "args in program order",
            4,
            &[At(0, 11), At(4, 22), Push(0x1234)],
            &[11, 22],
        ),
        // i386 `kmap_free_wakeup`: arg1, arg0, arg2 in that order.
        (
            "args in arbitrary order",
            4,
            &[At(4, 22), At(0, 11), At(8, 33), Push(0x1234)],
            &[11, 22, 33],
        ),
        // Walking backward, the first sighting of a slot is its live write.
        (
            "the most recent write of a slot",
            4,
            &[At(0, 0xBAD), At(4, 22), At(0, 11), Push(0x1234)],
            &[11, 22],
        ),
        // Every reaching SP-relative store in the window is collected, so a
        // store disjoint from the pushes is a slot, not a terminator.
        (
            "a store between the pushes",
            0,
            &[Push(22), At(0, 0xAAAA), Push(11)],
            &[11, 22, 0xAAAA],
        ),
        // Prologue zero-inits and a `push ebx` save land in the window and,
        // lowered to memory, are indistinguishable from argument pushes, so
        // all 7 are collected and the caller disambiguates: `push ebx`,
        // `sub esp, 16`, 4x zero-init, push arg1, push arg0, the call's
        // return-address push.
        (
            "local inits in the window",
            4,
            &[
                Push(0xEB),
                SpAt(-20),
                At(-20, 0),
                At(-16, 0),
                At(-12, 0),
                At(-8, 0),
                Push(1),
                Push(42),
                Push(0x1234),
            ],
            &[42, 1, 0, 0, 0, 0, 0xEB],
        ),
        // Chain, latest first: ret-addr@-12, arg0@-8, arg1@-4, local@-16.  The
        // local is below slot 0, so the walk ends there and it does not leak
        // into a slot.
        (
            "a store below the window ends the walk",
            4,
            &[
                At(-16, 0xDEAD),
                At(-4, 22),
                At(-8, 11),
                SpAt(-12),
                At(-12, 0x1234),
            ],
            &[11, 22],
        ),
        // With no per-call override the function-default convention is used.
        (
            "the default convention's offsets",
            4,
            &[At(4, 77), At(0, 0xABCD)],
            &[77],
        ),
        // The cursor advances past every slot a wide store covers.
        (
            "an 8-byte store spans slots 0 and 1",
            0,
            &[Wide(0, 0xDEAD_BEEF_CAFE_BABE, 64), At(8, 7)],
            &[0xDEAD_BEEF_CAFE_BABE, 7],
        ),
        (
            "an I80 store spans three slots",
            0,
            &[Wide(0, 0xABCD, 80), At(12, 7)],
            &[0xABCD, 7],
        ),
        (
            "an I128 store spans four slots",
            0,
            &[Wide(0, 0xABCD, 128), At(16, 7)],
            &[0xABCD, 7],
        ),
        (
            "a hole ends the prefix",
            0,
            &[At(0, 0xA0), At(8, 0xA2)],
            &[0xA0],
        ),
        ("slot 0 missing", 4, &[At(4, 22), Push(0x1234)], &[]),
        ("no stores", 0, &[], &[]),
        // The live 8 bytes are neither store's data, so slot 0 is unknown.
        (
            "a wide store patched in part",
            0,
            &[Wide(0, 0xAAAA_AAAA_AAAA_AAAA, 64), At(4, 0xBBBB_BBBB)],
            &[],
        ),
    ];
    for &(why, base, steps, want) in rows {
        assert_eq!(collected(base, steps)?, want, "{why}");
    }
    Ok(())
}

/// Soundness floor with `stack_global_disjoint` cleared: a global store
/// between the pushes and the Call terminates the walk, so only the pushes
/// nearer the Call are collected.  Setting it recovers the upstream args.
#[test]
fn a_global_store_ends_the_walk_only_at_the_floor() -> Result<()> {
    let interleaved: Vec<St> = [44, 33, 22, 11]
        .into_iter()
        .zip([0xCAFE_0000u64, 0xCAFE_0010, 0xCAFE_0020, 0xCAFE_0030])
        .flat_map(|(arg, global)| [Push(arg), Global(global, 0x1234)])
        .collect();
    for claimed in [false, true] {
        let claims = AssumptionOptions {
            stack_global_disjoint: claimed,
            ..AssumptionOptions::none()
        };
        let between = [Push(22), Global(0xDEAD_BEEF, 0x1234), Push(11)];
        let (want, want_none): (&[u128], &[u128]) = if claimed {
            (&[11, 22], &[11, 22, 33, 44])
        } else {
            (&[11], &[])
        };
        assert_eq!(
            collected_under(&claims, 0, &between)?,
            want,
            "claimed {claimed}"
        );
        assert_eq!(
            collected_under(&claims, 0, &interleaved)?,
            want_none,
            "claimed {claimed}: a global store is the chain's first node"
        );
    }
    Ok(())
}

/// `push ebx; push 7; call`: the save of the function's own callee-saved
/// `ebx` ends the prefix, so only `7` is collected.
#[test]
fn a_callee_saved_spill_ends_the_prefix() -> Result<()> {
    let sp = stack_vn();
    let ebx = strider_ir_test_utils::reg_vn(0x0c, 4);
    let i32_ty = ValueType::int(32);
    let mut b = sp_frame(sp)
        .tracked(ebx)
        .callee_saved(ebx)
        .stack_args(stack_args_at(0, 4))
        .build_fn_single_region()?;
    let mut cur = b.read_variable(&sp)?;
    let saved = b.read_variable(&ebx)?;
    let seven = b.build_int_const(7u64, i32_ty)?;
    for data in [saved, seven] {
        let four = b.build_int_const(4u64, i32_ty)?;
        cur = b.build_sub_as_add_neg(cur, four, i32_ty)?;
        b.write_variable(&sp, cur)?;
        b.build_store(cur, data, rsleigh::VnSpace::RAM)?;
    }
    let target = b.build_int_const(0x1000u64, i32_ty)?;
    b.build_call_cc(target, None)?;
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;
    let inputs = collect_call_inputs(&mut fg)?;
    let args: Vec<u128> = inputs[4..]
        .iter()
        .map(|&v| const_val(&fg, v, "collected arg"))
        .collect();
    assert_eq!(args, [7]);
    Ok(())
}

/// A per-call override wins over the function default.  The default's first
/// slot is at +4 and the override's at +0, so the store at `sp + 0` is slot 0
/// under the override but below the default's first slot.
#[test]
fn call_stack_arg_collect_uses_override_when_present() -> Result<()> {
    let sp = stack_vn();

    let mut b = sp_frame(sp)
        .stack_args(stack_args_at(4, 4))
        .build_fn_single_region()?;
    let sp_v0 = b.read_variable(&sp)?;
    let arg0 = b.build_int_const(66u64, ValueType::int(32))?;
    b.build_store(sp_v0, arg0, rsleigh::VnSpace::RAM)?;

    let target = b.build_int_const(0x5000u64, ValueType::int(32))?;
    b.build_call_cc(target, None)?;
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    // The offsets override is derived from this stored CC.
    let override_cc = strider_target::BuiltCallingConvention {
        stack_ptr: sp,
        stack_args: Some(strider_target::StackArgs {
            base_offset: 0,
            increment: 4,
        }),
        ..Default::default()
    };
    override_cc.validate().unwrap();
    fg.set_call_cc(find_call(&fg), override_cc);

    let inputs = collect_call_inputs(&mut fg)?;
    // ctrl + mem + target + sp + the arg at +0 = 5 inputs.
    assert_eq!(
        inputs[4..]
            .iter()
            .map(|&v| const_val(&fg, v, "override"))
            .collect::<Vec<_>>(),
        [66],
        "override CC [0,4] must collect the arg at offset +0"
    );
    Ok(())
}

/// The store's offset must come from the side-table memory class when an entry
/// exists, not from re-deriving it.  The arg0 store's address is replaced with
/// an opaque constant so decomposition returns `None`, and the side-table is
/// populated by hand.  Reading the side-table collects the arg; re-deriving
/// collects none.
#[test]
fn call_stack_arg_collect_reads_offset_from_side_table_not_decompose() -> Result<()> {
    let sp = stack_vn();
    let mut b = sp_frame(sp)
        .stack_args(stack_args_at(4, 4))
        .build_fn_single_region()?;
    let sp_v0 = b.read_variable(&sp)?;
    let sp_plus_4 = b.build_const_binop(4u64, sp_v0, IntBinaryOp::Add, ValueType::int(32))?;
    let arg0 = b.build_int_const(77u64, ValueType::int(32))?;
    b.build_store(sp_plus_4, arg0, rsleigh::VnSpace::RAM)?;

    // Anchor store at sp+0 (ret-addr-push role).
    let anchor = b.build_int_const(0x1234u64, ValueType::int(32))?;
    b.build_store(sp_v0, anchor, rsleigh::VnSpace::RAM)?;

    let target = b.build_int_const(0x2000u64, ValueType::int(32))?;
    b.build_call_cc(target, None)?;
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    // Canonicalize so decomposition would work if it were used.
    run_passes(&mut fg, CF_RP)?;

    let arg0_store = fg
        .walk()
        .find(|&n| {
            if !matches!(fg.node_kind(n), NodeKind::Store(_)) {
                return false;
            }
            let inputs = fg.node_inputs(n);
            inputs.len() == 3 && fg.int_const_u128(inputs[2]) == Some(77)
        })
        .expect("arg0 Store(IntConst(77)) must exist");

    // Slot 1 is the address, made opaque so decomposition returns None.
    let opaque_addr = {
        let mut ef = strider_ir::EditFunction::new(&mut fg);
        ef.build_int_const(0xDEAD_BEEFu64, ValueType::int(32))
            .unwrap()
    };
    let addr_input_id = fg.node_input_id_at(arg0_store, 1).unwrap();
    fg.graph_mut().update_input(addr_input_id, opaque_addr);

    // The sentinel fingerprint keeps IR validation from rejecting the graph.
    let opaque_producer = fg.producer(opaque_addr);
    fg.side_tables_mut().extend_asm_fingerprint(
        opaque_producer,
        &[strider_ir_test_utils::SENTINEL_LIFT_ADDR],
    );

    // The base must equal what the anchor store at sp+0 resolves to, so both
    // stores agree on one SP root.
    let sp_base = fg
        .walk_kind(|k| matches!(*k, NodeKind::InitialVar(id) if fg.initial_vn(id) == sp))
        .next()
        .map(|n| fg.node_outputs(n).iter().copied().next().unwrap())
        .expect("InitialVar(sp) node must exist");
    let arg0_store_addr = fg.store_addr(arg0_store);
    fg.set_stack_slot(arg0_store_addr, sp_base, 4);

    let pass = CallStackArgCollect;
    crate::pipeline::run_post(&pass, &mut fg, &crate::OptCtx::new(None))?;

    let inputs: Vec<ValueId> = fg.node_inputs(find_call(&fg)).into_iter().collect();
    assert_eq!(
        inputs[4..]
            .iter()
            .map(|&v| const_val(&fg, v, "side table"))
            .collect::<Vec<_>>(),
        [77],
        "the side-table offset collects arg0 even through an opaque address"
    );
    Ok(())
}

/// `slots` argument stores under a lowered SP, then the call that consumes
/// them.  Counts the memory-SSA steps one collection costs.
fn collect_walk_steps(slots: usize) -> Result<u64> {
    let sp = stack_vn();
    let stack_args = strider_target::StackArgs {
        base_offset: 0,
        increment: 4,
    };
    let mut b = sp_frame(sp)
        .stack_args(Some(stack_args))
        .build_fn_single_region()?;
    let entry_sp = b.read_variable(&sp)?;
    let frame_bytes = 4 * slots as i64;
    for i in 0..slots {
        let addr = b.build_const_binop(
            (4 * i as i64 - frame_bytes) as u64,
            entry_sp,
            IntBinaryOp::Add,
            ValueType::int(32),
        )?;
        let arg = b.build_int_const(0x100 + i as u64, ValueType::int(32))?;
        b.build_store(addr, arg, rsleigh::VnSpace::RAM)?;
    }
    let call_sp = b.build_const_binop(
        (-frame_bytes) as u64,
        entry_sp,
        IntBinaryOp::Add,
        ValueType::int(32),
    )?;
    b.write_variable(&sp, call_sp)?;
    let target = b.build_int_const(0x1000u64, ValueType::int(32))?;
    b.build_call(target, &[], &[], 0)?;
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;
    run_passes(&mut fg, COLLAPSE)?;

    let call_id = find_call(&fg);
    fg.assume(&crate::AssumptionOptions {
        stack_global_disjoint: true,
        ..crate::AssumptionOptions::none()
    });
    steps(&[&WALK_STEPS], || {
        let mut edit = crate::EditFunction::new(&mut fg);
        let args = collect_stack_args(
            &mut edit,
            call_id,
            stack_args.increment,
            &MemQuery::default(),
        );
        assert_eq!(args.len(), slots, "every argument slot must be collected");
        Ok(())
    })
}

/// Doubling the argument count may only double the work: one reverse scan of
/// the call's chain answers every slot, rather than one walk per slot.
#[test]
fn collecting_stack_args_is_linear_in_the_slot_count() -> Result<()> {
    assert_near_linear("argument slots", 100, collect_walk_steps, 260)
}

/// PPC64 passes a `float` right-justified in its 8-byte slot (`stfs` at
/// slot + 4), so on big-endian a store ending at the slot's end is the slot's
/// argument; on little-endian the same store is not.
///
/// Mutation: accept only a store at the slot's start.
fn collect_right_justified(order: strider_target::Endianness) -> Result<Vec<u128>> {
    let sp = strider_ir_test_utils::stack_vn_x86_64();
    let mut b = sp_frame(sp)
        .stack_args(stack_args_at(8, 8))
        .byte_order(order)
        .build_fn_single_region()?;
    let sp_v0 = b.read_variable(&sp)?;
    let call_sp =
        b.build_const_binop((-32i64) as u64, sp_v0, IntBinaryOp::Add, ValueType::int(64))?;
    b.write_variable(&sp, call_sp)?;
    for (off, value, ty) in [
        (12u64, 0xAu64, ValueType::int(32)),
        (16, 0xB, ValueType::int(64)),
    ] {
        let at = b.build_const_binop(off, call_sp, IntBinaryOp::Add, ValueType::int(64))?;
        let v = b.build_int_const(value, ty)?;
        b.build_store(at, v, rsleigh::VnSpace::RAM)?;
    }
    let target = b.build_int_const(0x1000u64, ValueType::int(64))?;
    b.build_call_cc(target, None)?;
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let inputs = collect_call_inputs(&mut fg)?;
    Ok(inputs[4..]
        .iter()
        .map(|&v| const_val(&fg, v, "collect_right_justified"))
        .collect())
}

#[test]
fn a_right_justified_big_endian_argument_is_its_slots() -> Result<()> {
    assert_eq!(
        collect_right_justified(strider_target::Endianness::Big)?,
        [0xA, 0xB]
    );
    Ok(())
}

#[test]
fn a_little_endian_store_past_its_slots_start_is_no_argument() -> Result<()> {
    assert_eq!(
        collect_right_justified(strider_target::Endianness::Little)?,
        [0u128; 0]
    );
    Ok(())
}

/// `Function.optimize` re-runs the default pipeline over an already optimized
/// `Function`, so the collected tail must be replaced, not extended.
#[test]
fn repeated_pipeline_runs_leave_the_collected_tail_alone() -> Result<()> {
    let mut fg = cdecl_fn(0, &[Push(22), Push(11)])?;
    let pipeline = crate::default_pipeline();
    for run in 1..=3 {
        pipeline.run(&mut fg, &crate::OptCtx::new(None))?;
        let inputs: Vec<ValueId> = fg.node_inputs(find_call(&fg)).into_iter().collect();
        let args: Vec<u128> = inputs[4..]
            .iter()
            .map(|&v| const_val(&fg, v, "rerun"))
            .collect();
        assert_eq!(args, [11, 22], "run {run}");
    }
    Ok(())
}

/// A frame at the top of an `I128` stack pointer's signed range, with the
/// first probe (`first_slot`) or the one after a collected arg past `i128::MAX`.
fn collect_at_the_offset_carrier_top(first_slot: i128, frame: i128) -> Result<usize> {
    let sp = rsleigh::Vn {
        addr_off: 0x100,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 16,
    };
    let mut b = sp_frame(sp)
        .stack_args(stack_args_at(first_slot, 4))
        .build_fn_single_region()?;
    let sp0 = b.read_variable(&sp)?;
    let sp1 = b.build_const_binop(frame as u128, sp0, IntBinaryOp::Add, ValueType::int(128))?;
    b.write_variable(&sp, sp1)?;
    let arg = b.build_int_const(7u64, ValueType::int(32))?;
    b.build_store(sp1, arg, rsleigh::VnSpace::RAM)?;
    let target = b.build_int_const(0x1000u64, ValueType::int(128))?;
    b.build_call_cc(target, None)?;
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    Ok(collect_call_inputs(&mut fg)?.len())
}

#[test]
fn a_probe_past_the_offset_carrier_collects_nothing() -> Result<()> {
    assert_eq!(collect_at_the_offset_carrier_top(4, i128::MAX)?, 4);
    Ok(())
}

#[test]
fn the_slot_after_the_offset_carrier_ends_collection() -> Result<()> {
    assert_eq!(collect_at_the_offset_carrier_top(0, i128::MAX - 3)?, 5);
    Ok(())
}

/// 32-bit: the call's SP is `sp + call_sp`, and `stores` are 4-byte stores at
/// `sp + offset`, oldest first.  The number of collected args.
fn collect_across_the_wrap(call_sp: u64, stores: &[u64]) -> Result<usize> {
    let mut steps = vec![SpAt(call_sp as i64)];
    steps.extend(
        stores
            .iter()
            .zip(1..)
            .map(|(&off, i)| At(off as i64, 0x11 * i)),
    );
    Ok(collected(0, &steps)?.len())
}

#[test]
fn a_nearer_store_wrapping_onto_a_slot_leaves_it_uncollected() -> Result<()> {
    assert_eq!(
        collect_across_the_wrap(0x8000_0000, &[0x8000_0000])?,
        1,
        "control: slot 0"
    );
    assert_eq!(
        collect_across_the_wrap(0x8000_0000, &[0x8000_0000, 0x7FFF_FFFE])?,
        0,
        "sp+0x7FFF_FFFE overwrites slot 0's low half mod 2^32, so the older \
         store is not its value"
    );
    Ok(())
}

#[test]
fn a_nearer_store_past_the_wrap_patches_a_slot_running_across_it() -> Result<()> {
    assert_eq!(
        collect_across_the_wrap(0x7FFF_FFFE, &[0x7FFF_FFFE])?,
        1,
        "control"
    );
    assert_eq!(
        collect_across_the_wrap(0x7FFF_FFFE, &[0x7FFF_FFFE, 0x8000_0000])?,
        0,
        "sp+0x8000_0000 overwrites slot 0's top half mod 2^32"
    );
    Ok(())
}

#[test]
fn a_slot_past_the_wrap_is_collected() -> Result<()> {
    assert_eq!(
        collect_across_the_wrap(0x7FFF_FFFC, &[0x7FFF_FFFC, 0x8000_0000])?,
        2,
        "slot 1 at sp+0x8000_0000 is its own store's"
    );
    Ok(())
}

/// The work of one `CallStackArgCollect` run over `fg`.
fn collect_steps(fg: &mut Function) -> Result<u64> {
    steps(&[&WALK_STEPS, &CLIMB_STEPS], || {
        crate::pipeline::run_post(&CallStackArgCollect, fg, &crate::OptCtx::new(None)).map(drop)
    })
}

/// Doubling the calls may only double the work of collecting their arguments.
#[test]
fn collecting_across_call_diamonds_costs_near_linear_work() -> Result<()> {
    assert_near_linear(
        "call diamonds",
        100,
        |n| collect_steps(&mut crate::test_support::memory_shapes::stack_arg_call_diamonds(n)?),
        260,
    )
}

/// Doubling the argument count may only double the work of collecting them
/// behind an irreducible memory graph.
#[test]
fn collecting_behind_an_irreducible_loop_is_linear_in_the_slot_count() -> Result<()> {
    let work = |k: usize| -> Result<u64> {
        let mut fg = crate::test_support::memory_shapes::irreducible_stack_args(k)?;
        assert!(
            !crate::mem_ssa::MemLayout::build(&fg).is_reducible(),
            "the memory graph is irreducible"
        );
        let work = collect_steps(&mut fg)?;
        let collected =
            fg.node_inputs(find_call(&fg)).len() - NodeKind::Call { cc: None }.input_head_len();
        assert_eq!(collected, k, "every argument is collected");
        Ok(work)
    };
    assert_near_linear("arguments", 100, work, 260)
}

/// Behind a merge whose arms disagree only above the arguments, every slot
/// is read by its own climb past the merge, so a scan reads at most
/// `MAX_FALLBACKS` slots that way and the rest end the prefix.
#[test]
fn collecting_behind_a_merge_reads_a_bounded_number_of_slots_past_it() -> Result<()> {
    let work = |k: usize| -> Result<u64> {
        let mut fg =
            crate::test_support::memory_shapes::irreducible_stack_args_merging(k, Some(k + 10))?;
        let work = collect_steps(&mut fg)?;
        let collected =
            fg.node_inputs(find_call(&fg)).len() - NodeKind::Call { cc: None }.input_head_len();
        assert_eq!(collected, 64, "the slots past the cap end the prefix");
        Ok(work)
    };
    assert_near_linear("arguments", 100, work, 260)
}

/// Two argument stores, a listed allocator called under a lower SP, a merge
/// of a frame local, then the call: the arguments collected under
/// `allocators`.  The merge stops the window probe, so each slot is read by
/// its own, private, probe.
fn args_past_an_allocator(allocators: &[u64]) -> Result<usize> {
    const ALLOCATOR: u64 = 0x2000;
    let i32_ty = ValueType::int(32);
    let sp = stack_vn();
    let ret = strider_ir_test_utils::reg_vn(0x0, 4);
    let mut b = strider_ir_test_utils::RegisterSet::new()
        .tracked(sp)
        .tracked(ret)
        .callee_saved(sp)
        .ret(ret)
        .stack_vn(sp)
        .stack_args(stack_args_at(4, 4))
        .build_fn()?;
    let [start, then, els, merge] = [(); 4].map(|()| b.create_region_all());
    let (start, then, els, merge) = (start?, then?, els?, merge?);
    b.set_entry_region_all(start)?;
    b.set_region(start);
    b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
    let entry = b.read_variable(&sp)?;
    let call_sp = at(&mut b, entry, -64, i32_ty)?;
    b.write_variable(&sp, call_sp)?;
    for i in 0..2i64 {
        let slot = at(&mut b, call_sp, 4 + 4 * i, i32_ty)?;
        let arg = b.build_int_const(0x100 + i as u64, i32_ty)?;
        b.build_store(slot, arg, rsleigh::VnSpace::RAM)?;
    }
    let alloc_sp = at(&mut b, entry, -128, i32_ty)?;
    b.write_variable(&sp, alloc_sp)?;
    let target = b.build_int_const(ALLOCATOR, i32_ty)?;
    b.build_call(target, &[], &[ret], 0)?;
    b.write_variable(&sp, call_sp)?;
    let cond = b.build_boolean_const(true);
    b.build_if(cond, then, els)?;
    b.set_region(then);
    let local = at(&mut b, entry, -8, i32_ty)?;
    let v = b.build_int_const(0x77u64, i32_ty)?;
    b.build_store(local, v, rsleigh::VnSpace::RAM)?;
    b.build_branch(merge)?;
    b.set_region(els);
    b.build_branch(merge)?;
    b.set_region(merge);
    let target = b.build_int_const(0x1000u64, i32_ty)?;
    b.build_call(target, &[], &[], 0)?;
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;
    run_passes(&mut fg, COLLAPSE)?;
    fg.assume(&AssumptionOptions {
        stack_global_disjoint: true,
        noalias_allocators: allocators.iter().copied().collect(),
        ..AssumptionOptions::none()
    });
    crate::pipeline::run_post(&CallStackArgCollect, &mut fg, &crate::OptCtx::new(None))?;
    let call = fg
        .graph()
        .all_node_ids()
        .find(|&n| {
            matches!(fg.node_kind(n), NodeKind::Call { .. })
                && fg.int_const_u128(fg.node_inputs(n)[NodeKind::CALL_TARGET_SLOT]) == Some(0x1000)
        })
        .expect("the call");
    Ok(fg.node_inputs(call).len() - NodeKind::Call { cc: None }.input_head_len())
}

/// An allocator is a callee like any other: listing it does not let the
/// argument scan past it, since the arguments are what the collection
/// settles for good.
#[test]
fn a_listed_allocator_ends_the_collection_like_any_call() -> Result<()> {
    assert_eq!(args_past_an_allocator(&[])?, 0, "an unlisted call");
    assert_eq!(args_past_an_allocator(&[0x2000])?, 0, "a listed allocator");
    Ok(())
}
