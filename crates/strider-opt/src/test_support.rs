use anyhow::anyhow;

use strider_ir::node::{NodeId, NodeKind};
use strider_ir::{Graph, IRViewer, Value};

pub(crate) use strider_ir_test_utils::{make_empty_fn as make_fn, make_fn_with_var};

use crate::{ConstantFold, LoadForward, Optimizer, OptimizerPipeline, PhiCollapse, RegionCollapse};

/// Runs `pass` until a run reports no change, each iteration with a fresh
/// default `OptCtx`. Returns the number of iterations that DID report a
/// change, so `0` means the first run was already a no-op.
pub(crate) fn run_to_fixed_point(
    pass: &dyn Optimizer,
    fg: &mut strider_ir::Function,
) -> crate::Result<usize> {
    let mut iterations = 0;
    while crate::run_one(pass, fg, &mut crate::OptCtx::new(None))?.changed() {
        iterations += 1;
    }
    Ok(iterations)
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

/// `ConstantFold` + `PhiCollapse` + `RegionCollapse`.
pub(crate) fn cf_rp_pipeline() -> OptimizerPipeline {
    let mut p = OptimizerPipeline::new();
    p.add(ConstantFold::new());
    p.add(PhiCollapse);
    p.add(RegionCollapse);
    p
}

/// `cf_rp_pipeline` plus `LoadForward`.
pub(crate) fn standard_test() -> OptimizerPipeline {
    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(ConstantFold::new());
    pipeline.add(PhiCollapse);
    pipeline.add(RegionCollapse);
    pipeline.add(LoadForward::default());
    pipeline
}

/// An [`crate::OptCtx`] with `stack_global_disjoint` cleared. The other
/// default-on claim, `assume_incoming_args_survive_calls`, stays set.
pub(crate) fn octx_structural_only() -> crate::OptCtx<'static> {
    let mut ctx = crate::OptCtx::new(None);
    ctx.options.assumptions.stack_global_disjoint = false;
    ctx
}

/// An [`crate::OptCtx`] at the pipeline default, `stack_global_disjoint` set.
pub(crate) fn octx_stack_global_disjoint() -> crate::OptCtx<'static> {
    let mut ctx = crate::OptCtx::new(None);
    ctx.options.assumptions.stack_global_disjoint = true;
    ctx
}

/// Input[2] of the first `Return`, after ctrl and mem.
pub(crate) fn return_value(graph: &Graph) -> crate::Result<Value> {
    let ret = graph
        .all_node_ids()
        .find(|&n| matches!(graph.node_kind(n), NodeKind::Return))
        .ok_or_else(|| anyhow!("no return node found in function"))?;
    Ok(graph.node_inputs(ret)[2])
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

/// Memory-graph shapes the scaling tests analyse at two sizes, each built over
/// 64-bit constant addresses so no SP phi needs collapsing first.
pub(crate) mod memory_shapes {
    use strider_ir::node::ValueType;
    use strider_ir::{FunctionBuilder, IRBuilderExt, IntBinaryOp, Value};
    use strider_ir_test_utils::{SENTINEL_LIFT_ADDR, sp_frame, stack_vn_x86_64};

    const TY: ValueType = ValueType::I64;
    /// A slot no shape reads, taking the loads that would otherwise be dead.
    const SINK: usize = usize::MAX >> 8;

    fn address(b: &mut FunctionBuilder, slot: usize) -> crate::Result<Value> {
        b.build_int_const(0x10_0000 + 8 * slot as u64, TY)
    }

    fn store(b: &mut FunctionBuilder, slot: usize, value: Value) -> crate::Result<()> {
        let addr = address(b, slot)?;
        b.build_store(addr, value, rsleigh::VnSpace::RAM)?;
        Ok(())
    }

    fn store_const(b: &mut FunctionBuilder, slot: usize, value: u64) -> crate::Result<()> {
        let v = b.build_int_const(value, TY)?;
        store(b, slot, v)
    }

    fn load(b: &mut FunctionBuilder, slot: usize) -> crate::Result<Value> {
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
                let one = b.build_int_const(1u64, TY)?;
                let next = b.build_int_binary_operation(counter, one, IntBinaryOp::Add, TY)?;
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
                let one = b.build_int_const(1u64, TY)?;
                let next = b.build_int_binary_operation(counter, one, IntBinaryOp::Add, TY)?;
                store(b, k, next)?;
                b.build_branch(header)?;
                b.set_region(exit);
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
        let mut collapse = crate::OptimizerPipeline::new();
        collapse.add(crate::PhiCollapse);
        collapse.add(crate::RegionCollapse);
        collapse.run(&mut fg, &mut crate::OptCtx::new(None))?;
        Ok(fg)
    }

    fn sp_plus(b: &mut FunctionBuilder, sp: rsleigh::Vn, k: i64) -> crate::Result<Value> {
        let sp_value = b.read_variable(&sp)?;
        let k = b.build_int_const(k as u64, ValueType::I32)?;
        b.build_int_binary_operation(sp_value, k, IntBinaryOp::Add, ValueType::I32)
    }

    fn call32(b: &mut FunctionBuilder) -> crate::Result<()> {
        let target = b.build_int_const(0x1000u64, ValueType::I32)?;
        b.build_call(target, &[], &[], 0)?;
        Ok(())
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
                        let arg = b.build_int_const(i as u64, ValueType::I32)?;
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
                let arg = b.build_load(slot, rsleigh::VnSpace::RAM, ValueType::I32)?;
                let sink = b.build_int_const(0x9000u64, ValueType::I32)?;
                b.build_store(sink, arg, rsleigh::VnSpace::RAM)?;
            }
            Ok(())
        })
    }
}
