use std::cell::Cell;
use std::thread::LocalKey;

use anyhow::anyhow;

use strider_ir::node::{NodeId, NodeKind};
use strider_ir::{FunctionBuilder, Graph, IRBuilderExt, IRViewer, IntBinaryOp, ValueType};
use strider_ir::{ReadOnlyMemory, node::ValueId};

pub(crate) use strider_ir_test_utils::{Rng, make_empty_fn as make_fn, make_fn_with_var};

use crate::{ConstantFold, LoadForward, Optimizer, OptimizerPipeline, PhiCollapse, RegionCollapse};

/// One [`crate::run_one`] of `pass` over no rom.
pub(crate) fn run_pass(pass: &dyn Optimizer, fg: &mut strider_ir::Function) -> crate::Result<bool> {
    crate::run_one(pass, fg, &crate::OptCtx::new(None))
}

/// One [`crate::run_one`] of `pass` over `rom`.
pub(crate) fn run_pass_rom(
    pass: &dyn Optimizer,
    fg: &mut strider_ir::Function,
    rom: &dyn ReadOnlyMemory,
) -> crate::Result<bool> {
    crate::run_one(pass, fg, &crate::OptCtx::new(Some(rom)))
}

/// Runs `pass` until a run reports no change, each iteration with a fresh
/// `OptCtx` over no rom. Returns the number of iterations that DID report a
/// change, so `0` means the first run was already a no-op.
pub(crate) fn run_to_fixed_point(
    pass: &dyn Optimizer,
    fg: &mut strider_ir::Function,
) -> crate::Result<usize> {
    let mut iterations = 0;
    while run_pass(pass, fg)? {
        iterations += 1;
    }
    Ok(iterations)
}

/// `PhiCollapse` + `RegionCollapse`.
pub(crate) const COLLAPSE: &[&dyn Optimizer] = &[&PhiCollapse, &RegionCollapse];

/// `ConstantFold` + [`COLLAPSE`].
pub(crate) const CF_RP: &[&dyn Optimizer] = &[&ConstantFold, &PhiCollapse, &RegionCollapse];

/// [`CF_RP`] + `LoadForward`.
pub(crate) const STANDARD: &[&dyn Optimizer] =
    &[&ConstantFold, &PhiCollapse, &RegionCollapse, &LoadForward];

/// `passes` as one pipeline.
pub(crate) fn pipeline(passes: &[&'static dyn Optimizer]) -> OptimizerPipeline {
    let mut p = OptimizerPipeline::new();
    for &pass in passes {
        p.add(pass);
    }
    p
}

/// Runs `passes` as one pipeline over no rom.
pub(crate) fn run_passes(
    fg: &mut strider_ir::Function,
    passes: &[&'static dyn Optimizer],
) -> crate::Result<()> {
    pipeline(passes).run(fg, &crate::OptCtx::new(None))
}

/// `base + k`, `k` two's-complement at `ty` (at most 128 bits wide).
pub(crate) fn at(
    b: &mut FunctionBuilder,
    base: ValueId,
    k: i64,
    ty: ValueType,
) -> crate::Result<ValueId> {
    b.build_const_binop(i128::from(k) as u128, base, IntBinaryOp::Add, ty)
}

/// Zeroes each of `counters`, runs `f`, and returns their summed count.
pub(crate) fn steps(
    counters: &[&'static LocalKey<Cell<u64>>],
    f: impl FnOnce() -> crate::Result<()>,
) -> crate::Result<u64> {
    for c in counters {
        c.with(|c| c.set(0));
    }
    f()?;
    Ok(counters.iter().map(|c| c.with(Cell::get)).sum())
}

/// Asserts `work(2n)` is at most `ratio_pct`% of `work(n)`.
#[track_caller]
pub(crate) fn assert_near_linear(
    name: &str,
    n: usize,
    work: impl Fn(usize) -> crate::Result<u64>,
    ratio_pct: u64,
) -> crate::Result<()> {
    let small = work(n)?;
    let big = work(2 * n)?;
    assert!(
        big * 100 <= small * ratio_pct,
        "{name}: doubling {n} to {} took {small} steps to {big}, past {ratio_pct}%",
        2 * n
    );
    Ok(())
}

#[track_caller]
pub(crate) fn assert_return_kind(graph: &Graph, expected: NodeKind) {
    let got = return_kind(graph).expect("function must return a value");
    assert_eq!(got, expected, "return-value producer kind mismatch");
}

#[track_caller]
pub(crate) fn assert_returns_const(f: &strider_ir::Function, expected: u64) {
    let val = return_value(f.graph()).expect("function must return a value");
    let got = f.int_const_u128(val);
    assert_eq!(
        got,
        Some(u128::from(expected)),
        "return-value must be IntConst({expected:#x})"
    );
}

/// The first `Return`'s first value input.
pub(crate) fn return_value(graph: &Graph) -> crate::Result<ValueId> {
    let ret = graph
        .all_node_ids()
        .find(|&n| matches!(graph.node_kind(n), NodeKind::Return))
        .ok_or_else(|| anyhow!("no return node found in function"))?;
    Ok(graph.node_inputs(ret)[NodeKind::Return.input_head_len()])
}

pub(crate) fn return_kind(graph: &Graph) -> crate::Result<NodeKind> {
    let val = return_value(graph)?;
    Ok(*graph.kind_of_value(val))
}

/// Panics on zero or multiple `If` nodes; both mean the fixture is wrong.
pub(crate) fn find_unique_if(graph: &Graph) -> NodeId {
    let mut iter = graph
        .all_node_ids()
        .filter(|&n| matches!(graph.node_kind(n), NodeKind::If));
    let first = iter.next().expect("test fixture must contain an If node");
    assert!(
        iter.next().is_none(),
        "test fixture has more than one If node",
    );
    first
}

/// Memory-graph shapes the memory tests share. The scaling tests analyse most
/// of them at two sizes, each built over 64-bit constant addresses so no SP
/// phi needs collapsing first.
pub(crate) mod memory_shapes {
    use strider_ir::node::ValueType;
    use strider_ir::{FunctionBuilder, IRBuilderExt, IntBinaryOp, node::ValueId};
    use strider_ir_test_utils::{SENTINEL_LIFT_ADDR, sp_frame, stack_vn_x86_64};

    const TY: ValueType = ValueType::int(64);
    /// A slot no shape reads, taking the loads that would otherwise be dead.
    const SINK: usize = usize::MAX >> 8;

    fn address(b: &mut FunctionBuilder, slot: usize) -> crate::Result<ValueId> {
        b.build_int_const(0x10_0000 + 8 * slot as u64, TY)
    }

    fn store(b: &mut FunctionBuilder, slot: usize, value: ValueId) -> crate::Result<()> {
        let addr = address(b, slot)?;
        b.build_store(addr, value, rsleigh::VnSpace::RAM)?;
        Ok(())
    }

    fn store_const(b: &mut FunctionBuilder, slot: usize, value: u64) -> crate::Result<()> {
        let v = b.build_int_const(value, TY)?;
        store(b, slot, v)
    }

    fn load(b: &mut FunctionBuilder, slot: usize) -> crate::Result<ValueId> {
        let addr = address(b, slot)?;
        b.build_load(addr, rsleigh::VnSpace::RAM, TY)
    }

    /// A load kept live by storing it to [`SINK`].
    fn read(b: &mut FunctionBuilder, slot: usize) -> crate::Result<()> {
        let v = load(b, slot)?;
        store(b, SINK, v)
    }

    fn shape(
        body: impl FnOnce(&mut FunctionBuilder) -> crate::Result<()>,
    ) -> crate::Result<strider_ir::Function> {
        let mut b = sp_frame(stack_vn_x86_64()).build_fn()?;
        let entry = b.create_region_all()?;
        b.set_entry_region_all(entry)?;
        b.set_region(entry);
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        body(&mut b)?;
        b.build_return(None, &[])?;
        b.set_lift_addr(None);
        b.build()
    }

    /// `if (c) { then } else { els }`, continuing in the merge.
    fn diamond(
        b: &mut FunctionBuilder,
        then: impl FnOnce(&mut FunctionBuilder) -> crate::Result<()>,
        els: impl FnOnce(&mut FunctionBuilder) -> crate::Result<()>,
    ) -> crate::Result<()> {
        let (t, e, m) = (
            b.create_region_all()?,
            b.create_region_all()?,
            b.create_region_all()?,
        );
        let cond = b.build_boolean_const(true);
        b.build_if(cond, t, e)?;
        b.set_region(t);
        then(b)?;
        b.build_branch(m)?;
        b.set_region(e);
        els(b)?;
        b.build_branch(m)?;
        b.set_region(m);
        Ok(())
    }

    /// `n` stores to distinct slots, then reloads of `n` slots never written
    /// and of the `n` written ones.
    pub(crate) fn store_chain(n: usize) -> crate::Result<strider_ir::Function> {
        shape(|b| {
            for i in 0..n {
                store_const(b, i, i as u64)?;
            }
            for i in 0..n {
                read(b, n + i)?;
            }
            for i in 0..n {
                read(b, i)?;
            }
            Ok(())
        })
    }

    /// `n` diamonds each storing its own slot on both arms, then reloads of `n`
    /// slots never written.
    pub(crate) fn store_diamonds(n: usize) -> crate::Result<strider_ir::Function> {
        shape(|b| {
            for i in 0..n {
                diamond(b, |b| store_const(b, i, 1), |b| store_const(b, i, 2))?;
            }
            for i in 0..n {
                read(b, n + i)?;
            }
            Ok(())
        })
    }

    /// `n` diamonds with a call on one arm, each followed by a reload of a slot
    /// written before the first.
    pub(crate) fn call_diamonds(n: usize) -> crate::Result<strider_ir::Function> {
        shape(|b| {
            store_const(b, 0, 7)?;
            for _ in 0..n {
                diamond(
                    b,
                    |b| {
                        let target = b.build_int_const(0x1000u64, TY)?;
                        b.build_call(target, &[], &[], 0)?;
                        Ok(())
                    },
                    |_| Ok(()),
                )?;
                read(b, 0)?;
            }
            Ok(())
        })
    }

    /// One loop whose body updates each of `n` accumulators from the next.
    pub(crate) fn accumulator_loop(n: usize) -> crate::Result<strider_ir::Function> {
        shape(|b| {
            for i in 0..=n {
                store_const(b, i, i as u64)?;
            }
            let (header, body, exit) = (
                b.create_region_all()?,
                b.create_region_all()?,
                b.create_region_all()?,
            );
            b.build_branch(header)?;
            b.set_region(header);
            let cond = b.build_boolean_const(true);
            b.build_if(cond, body, exit)?;
            b.set_region(body);
            for i in 0..n {
                let (x, y) = (load(b, i)?, load(b, i + 1)?);
                let sum = b.build_int_binary_operation(x, y, IntBinaryOp::Add, TY)?;
                store(b, i, sum)?;
            }
            b.build_branch(header)?;
            b.set_region(exit);
            Ok(())
        })
    }

    /// `n` loops one after another, each reading a slot written before the
    /// first and bumping its own counter.
    pub(crate) fn sequential_loops(n: usize) -> crate::Result<strider_ir::Function> {
        let bound = n;
        shape(|b| {
            store_const(b, bound, 9)?;
            for k in 0..n {
                store_const(b, k, 0)?;
                let (header, body, exit) = (
                    b.create_region_all()?,
                    b.create_region_all()?,
                    b.create_region_all()?,
                );
                b.build_branch(header)?;
                b.set_region(header);
                read(b, bound)?;
                let cond = b.build_boolean_const(true);
                b.build_if(cond, body, exit)?;
                b.set_region(body);
                let counter = load(b, k)?;
                let next = b.build_const_binop(1u64, counter, IntBinaryOp::Add, TY)?;
                store(b, k, next)?;
                b.build_branch(header)?;
                b.set_region(exit);
            }
            Ok(())
        })
    }

    /// `n` loops nested in one another, each header reading a slot written
    /// before the outermost and each latch bumping its level's counter.
    pub(crate) fn nested_loops(n: usize) -> crate::Result<strider_ir::Function> {
        let bound = n;
        shape(|b| {
            store_const(b, bound, 9)?;
            let mut levels = Vec::with_capacity(n);
            for k in 0..n {
                store_const(b, k, 0)?;
                let (header, body, exit) = (
                    b.create_region_all()?,
                    b.create_region_all()?,
                    b.create_region_all()?,
                );
                b.build_branch(header)?;
                b.set_region(header);
                read(b, bound)?;
                let cond = b.build_boolean_const(true);
                b.build_if(cond, body, exit)?;
                b.set_region(body);
                levels.push((header, exit));
            }
            for (k, &(header, exit)) in levels.iter().enumerate().rev() {
                let counter = load(b, k)?;
                let next = b.build_const_binop(1u64, counter, IntBinaryOp::Add, TY)?;
                store(b, k, next)?;
                b.build_branch(header)?;
                b.set_region(exit);
            }
            Ok(())
        })
    }

    /// One spill, then `n` calls each followed by its reload, summed, its
    /// region phis over SP collapsed: the `-O0` shape each reload of which
    /// `escape_analysis` steps past every call above it.
    pub(crate) fn spill_call_chain(n: usize) -> crate::Result<strider_ir::Function> {
        let sp = stack_vn_x86_64();
        let mut fg = shape(|b| {
            let sp_val = b.read_variable(&sp)?;
            let slot = b.build_const_binop((-8i64) as u64, sp_val, IntBinaryOp::Add, TY)?;
            let seven = b.build_int_const(7u64, TY)?;
            b.build_store(slot, seven, rsleigh::VnSpace::RAM)?;
            let call_sp = b.build_const_binop((-64i64) as u64, sp_val, IntBinaryOp::Add, TY)?;
            b.write_variable(&sp, call_sp)?;
            let mut sum = seven;
            for _ in 0..n {
                let target = b.build_int_const(0x1000u64, TY)?;
                b.build_call(target, &[], &[], 0)?;
                let reload = b.build_load(slot, rsleigh::VnSpace::RAM, TY)?;
                sum = b.build_int_binary_operation(sum, reload, IntBinaryOp::Add, TY)?;
            }
            store(b, SINK, sum)
        })?;
        super::run_passes(&mut fg, super::COLLAPSE)?;
        fg.assume(&crate::AssumptionOptions {
            escape_analysis: true,
            ..crate::AssumptionOptions::none()
        });
        Ok(fg)
    }

    /// `n` stores over four slots, each followed by a read of a slot never
    /// written, behind a two-entry loop.  The loop is irreducible, so the
    /// memory graph has no position index and every query takes the slow
    /// climb.
    pub(crate) fn irreducible_store_chain(n: usize) -> crate::Result<strider_ir::Function> {
        const SLOTS: usize = 4;
        shape(|b| {
            let (top, mid, exit) = (
                b.create_region_all()?,
                b.create_region_all()?,
                b.create_region_all()?,
            );
            // The entry enters the loop at `mid` and the latch re-enters it at
            // `top`: two entries, so neither is a dominating header.
            let enter_mid = b.build_boolean_const(true);
            b.build_if(enter_mid, mid, top)?;
            b.set_region(top);
            store_const(b, SLOTS, 1)?;
            b.build_branch(mid)?;
            b.set_region(mid);
            store_const(b, SLOTS + 1, 2)?;
            let again = b.build_boolean_const(true);
            b.build_if(again, top, exit)?;
            b.set_region(exit);
            for i in 0..n {
                store_const(b, i % SLOTS, i as u64)?;
                read(b, SLOTS + 2 + i % SLOTS)?;
            }
            Ok(())
        })
    }

    /// A 32-bit function taking stack arguments at `sp + 4`, its region phis
    /// over SP collapsed.
    fn stack_shape(
        body: impl FnOnce(&mut FunctionBuilder, rsleigh::Vn) -> crate::Result<()>,
    ) -> crate::Result<strider_ir::Function> {
        let sp = strider_ir_test_utils::stack_vn_x86();
        let mut b = sp_frame(sp)
            .stack_args(strider_ir_test_utils::stack_args_at(4, 4))
            .build_fn()?;
        let entry = b.create_region_all()?;
        b.set_entry_region_all(entry)?;
        b.set_region(entry);
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        body(&mut b, sp)?;
        b.build_return(None, &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        super::run_passes(&mut fg, super::COLLAPSE)?;
        Ok(fg)
    }

    fn sp_plus(b: &mut FunctionBuilder, sp: rsleigh::Vn, k: i64) -> crate::Result<ValueId> {
        let sp_value = b.read_variable(&sp)?;
        super::at(b, sp_value, k, ValueType::int(32))
    }

    fn call32(b: &mut FunctionBuilder) -> crate::Result<()> {
        let target = b.build_int_const(0x1000u64, ValueType::int(32))?;
        b.build_call(target, &[], &[], 0)?;
        Ok(())
    }

    /// `k` stack arguments written below a lowered SP and one call, all behind
    /// a two-entry loop over frame locals: the memory graph is irreducible, so
    /// it has no position index and every probe of the call's argument slots
    /// takes the slow climb.  After the call, the slot just above the
    /// arguments is reloaded and the value stored to a global, so a forward
    /// of it asks the call's argument window across all `k` slots.
    pub(crate) fn irreducible_stack_args(k: usize) -> crate::Result<strider_ir::Function> {
        irreducible_stack_args_merging(k, None)
    }

    /// [`irreducible_stack_args`] with a diamond between the argument stores
    /// and the call, one arm storing to argument slot `merge`, so a probe of
    /// the window stops at the merge.
    pub(crate) fn irreducible_stack_args_merging(
        k: usize,
        merge: Option<usize>,
    ) -> crate::Result<strider_ir::Function> {
        stack_shape(|b, sp| {
            let (top, mid, exit) = (
                b.create_region_all()?,
                b.create_region_all()?,
                b.create_region_all()?,
            );
            let enter_mid = b.build_boolean_const(true);
            b.build_if(enter_mid, mid, top)?;
            b.set_region(top);
            let local = sp_plus(b, sp, -8)?;
            let one = b.build_int_const(1u64, ValueType::int(32))?;
            b.build_store(local, one, rsleigh::VnSpace::RAM)?;
            b.build_branch(mid)?;
            b.set_region(mid);
            let local = sp_plus(b, sp, -12)?;
            let two = b.build_int_const(2u64, ValueType::int(32))?;
            b.build_store(local, two, rsleigh::VnSpace::RAM)?;
            let again = b.build_boolean_const(true);
            b.build_if(again, top, exit)?;
            b.set_region(exit);
            let lowered = sp_plus(b, sp, -(4 * k as i64) - 64)?;
            b.write_variable(&sp, lowered)?;
            for i in 0..k {
                let slot = sp_plus(b, sp, 4 + 4 * i as i64)?;
                let arg = b.build_int_const(0x100 + i as u64, ValueType::int(32))?;
                b.build_store(slot, arg, rsleigh::VnSpace::RAM)?;
            }
            if let Some(merge) = merge {
                diamond(
                    b,
                    |b| {
                        let slot = sp_plus(b, sp, 4 + 4 * merge as i64)?;
                        let v = b.build_int_const(0x77u64, ValueType::int(32))?;
                        b.build_store(slot, v, rsleigh::VnSpace::RAM)?;
                        Ok(())
                    },
                    |_| Ok(()),
                )?;
            }
            call32(b)?;
            let above = sp_plus(b, sp, 4 + 4 * k as i64)?;
            let reloaded = b.build_load(above, rsleigh::VnSpace::RAM, ValueType::int(32))?;
            let global = b.build_int_const(0x9000u64, ValueType::int(32))?;
            b.build_store(global, reloaded, rsleigh::VnSpace::RAM)?;
            Ok(())
        })
    }

    /// `n` diamonds, each calling on one arm with a stack argument written
    /// just before.
    pub(crate) fn stack_arg_call_diamonds(n: usize) -> crate::Result<strider_ir::Function> {
        stack_shape(|b, sp| {
            let lowered = sp_plus(b, sp, -32)?;
            b.write_variable(&sp, lowered)?;
            for i in 0..n {
                diamond(
                    b,
                    |b| {
                        let slot = sp_plus(b, sp, 4)?;
                        let arg = b.build_int_const(i as u64, ValueType::int(32))?;
                        b.build_store(slot, arg, rsleigh::VnSpace::RAM)?;
                        call32(b)
                    },
                    |_| Ok(()),
                )?;
            }
            Ok(())
        })
    }

    /// `n` diamonds with a call on one arm, each followed by a read of the
    /// first incoming stack argument.
    pub(crate) fn argument_reads_between_calls(n: usize) -> crate::Result<strider_ir::Function> {
        stack_shape(|b, sp| {
            for _ in 0..n {
                diamond(b, call32, |_| Ok(()))?;
                let slot = sp_plus(b, sp, 4)?;
                let arg = b.build_load(slot, rsleigh::VnSpace::RAM, ValueType::int(32))?;
                let sink = b.build_int_const(0x9000u64, ValueType::int(32))?;
                b.build_store(sink, arg, rsleigh::VnSpace::RAM)?;
            }
            Ok(())
        })
    }

    /// A value spilled to the stack TOP, exactly where outgoing slot 0 sits, and
    /// reloaded after a call. The store is indistinguishable from an argument
    /// push, so the reload is pinned unless `callee_preserves_stack_args`
    /// empties the argument window.
    ///
    /// ```text
    /// sp' = sp - 64        ; this function's frame
    /// store 0x42 -> sp'+0  ; scratch spill, or f's arg0
    /// call f
    /// load  sp'+0
    /// ```
    ///
    /// Returns the builder, its terminator not yet built, and the reload.
    pub(crate) fn stack_top_spill() -> crate::Result<(FunctionBuilder, ValueId)> {
        let sp = strider_ir_test_utils::stack_vn_x86();
        let mut b = sp_frame(sp)
            .stack_args(strider_ir_test_utils::stack_args_at(0, 4))
            .build_fn_single_region()?;
        let entry_sp = b.read_variable(&sp)?;
        let call_sp = b.build_const_binop(
            (-64i64) as u64,
            entry_sp,
            IntBinaryOp::Add,
            ValueType::int(32),
        )?;
        b.write_variable(&sp, call_sp)?;
        let slot0 = b.build_const_binop(0u64, call_sp, IntBinaryOp::Add, ValueType::int(32))?;
        let spilled = b.build_int_const(0x42u64, ValueType::int(32))?;
        b.build_store(slot0, spilled, rsleigh::VnSpace::RAM)?;
        call32(&mut b)?;
        let reload = b.build_load(slot0, rsleigh::VnSpace::RAM, ValueType::int(32))?;
        Ok((b, reload))
    }
}

/// A sample of every `NodeKind` shape: each op variant, each payload-free
/// kind, and one of each payload kind.
pub(crate) fn sample_kinds() -> Vec<strider_ir::node::NodeKind> {
    use strider_ir::node::NodeKind as K;
    use strider_ir::{
        ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp, IntUnaryOp,
    };
    let mut v = vec![
        K::Entry,
        K::InitialMemory,
        K::Region,
        K::MemPhi,
        K::Phi,
        K::If,
        K::IndirectBranch,
        K::Unreachable,
        K::Return,
        K::Truncate,
        K::Popcount,
        K::Lzcount,
        K::FloatConst(0),
        K::IntToFloat,
        K::FloatToInt,
        K::FloatToFloat,
        K::IntBitsToFloat,
        K::FloatBitsToInt,
        K::CPoolRef,
        K::New,
        K::Call { cc: None },
        K::Load(rsleigh::VnSpace::RAM),
        K::Store(rsleigh::VnSpace::RAM),
        K::CallOther { user_op_id: 0 },
        K::InitialVar(strider_ir::node::InitialVnId::from_u32(0)),
        K::Switch(strider_ir::node::SwitchTableId::from_u32(0)),
        K::IntConst(strider_ir::node::IntConstId::from_u32(0)),
    ];
    v.extend(IntUnaryOp::ALL.map(K::IntUnaryOp));
    v.extend(IntBinaryOp::ALL.map(K::IntBinaryOp));
    v.extend(IntCmpOp::ALL.map(K::IntCmpOp));
    v.extend(ExtendOp::ALL.map(K::Extend));
    v.extend(FloatUnaryOp::ALL.map(K::FloatUnaryOp));
    v.extend(FloatBinaryOp::ALL.map(K::FloatBinaryOp));
    v.extend(FloatCmpOp::ALL.map(K::FloatCmpOp));
    v
}

/// The 1-bit logical NOT, `Xor(v, IntConst(1)):I1`.
pub(crate) fn bool_not(b: &mut FunctionBuilder, v: ValueId) -> crate::Result<ValueId> {
    b.build_const_binop(1u64, v, IntBinaryOp::Xor, ValueType::I1)
}

/// Whether `node` is [`bool_not`]'s shape, the constant on either side.
pub(crate) fn is_bool_not(f: &strider_ir::Function, node: NodeId) -> bool {
    *f.node_kind(node) == NodeKind::IntBinaryOp(IntBinaryOp::Xor)
        && f.node_inputs(node)
            .into_iter()
            .any(|v| f.value_type_opt(v) == Some(ValueType::I1) && f.int_const_u128(v) == Some(1))
}
