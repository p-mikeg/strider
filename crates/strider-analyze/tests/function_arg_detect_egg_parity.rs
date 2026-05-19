//! Phase 3 Task 3.7b parity test — `FunctionArgDetect` v1 vs
//! `FunctionArgDetectEgg` v2.
//!
//! `FunctionArgDetect` is a function-boundary post-pass with two
//! halves:
//!   1. Register args — rename `InitialVar(reg)` consumers to a
//!      canonical `FunctionArg { Register(reg), i }`.
//!   2. Stack args — collect `Load[sp + K]` nodes whose chain doesn't
//!      alias the slot, group by `K`, truncate at the first gap, and
//!      emit one `FunctionArg { Stack { offset: K }, j }` per
//!      surviving slot.
//!
//! Memory chains are excluded from the egraph's value slice by
//! construction, so v2 is a faithful direct port of v1.  See
//! `crates/strider-analyze/src/opt/function_arg_detect_egg.rs` for the
//! rationale.  Both passes MUST produce structurally identical
//! `FunctionArg` emission and consumer rewiring for every supported
//! shape.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

use strider_analyze::opt::{
    ConstantFold, FunctionArgDetect, OptimizerPipeline,
    function_arg_detect_egg::FunctionArgDetectEgg,
};
use strider_ir::node::{FunctionArgSource, NodeKind, NodeOutputType};
use strider_ir::test_utils::{reg_vn, sp_vn_x86_64 as sp_vn};
use strider_ir::{BuiltFunctionGraph, FunctionBuilder, IntBinaryOp};

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Summarises the function-arg classification: for every reachable
/// `FunctionArg`, record the source (Register(off, size) | Stack(off))
/// and the positional index.  Sorted so order-of-emission doesn't
/// affect the comparison.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone)]
enum ArgSrc {
    Reg(u64, u32, u32), // (addr_off, size, index)
    Stack(i64, u32),    // (offset, index)
}

fn summarise_args(fg: &BuiltFunctionGraph) -> Vec<ArgSrc> {
    let mut out: Vec<ArgSrc> = strider_ir::walk::walk_graph(&fg.graph, fg.entry)
        .filter_map(|n| match *fg.graph.node_kind(n) {
            NodeKind::FunctionArg { source, index } => match source {
                FunctionArgSource::Register(vn) => Some(ArgSrc::Reg(vn.addr_off, vn.size, index)),
                FunctionArgSource::Stack { offset, .. } => Some(ArgSrc::Stack(offset, index)),
            },
            _ => None,
        })
        .collect();
    out.sort();
    out
}

/// Count of reachable `InitialVar(reg)` nodes — should drop when a
/// register arg's consumers have been rewired to a `FunctionArg`.
fn count_initial_var_reachable(fg: &BuiltFunctionGraph, target: rsleigh::Vn) -> usize {
    strider_ir::walk::walk_graph(&fg.graph, fg.entry)
        .filter(|&n| matches!(fg.graph.node_kind(n), NodeKind::InitialVar(v) if *v == target))
        .count()
}

/// Count of reachable `Load` nodes — should drop when stack-arg loads
/// have been rewired to `FunctionArg`.
fn count_loads_reachable(fg: &BuiltFunctionGraph) -> usize {
    strider_ir::walk::walk_graph(&fg.graph, fg.entry)
        .filter(|&n| matches!(fg.graph.node_kind(n), NodeKind::Load(_)))
        .count()
}

/// Fake 8-byte register to stand in for x86_64 RDI.
fn rdi_like_vn() -> rsleigh::Vn {
    reg_vn(0x38, 8)
}

/// Fake 8-byte register to stand in for x86_64 RSI.
fn rsi_like_vn() -> rsleigh::Vn {
    reg_vn(0x40, 8)
}

// ── Tests ────────────────────────────────────────────────────────────────────

/// Reading a single arg-passing register should produce a single
/// `FunctionArg { Register(rdi), 0 }`.  v1/v2 must produce the same
/// summary.
#[test]
fn parity_reads_rdi_emits_function_arg_0() {
    let rdi = rdi_like_vn();
    let sp = sp_vn();

    fn build(rdi: rsleigh::Vn, sp: rsleigh::Vn) -> BuiltFunctionGraph {
        let mut b = FunctionBuilder::new_raw(vec![rdi, sp], &[], &[rdi], &[rdi], None, 0).unwrap();
        let region = b.create_region().unwrap();
        b.set_entry_region(region).unwrap();
        b.set_region(region);
        b.set_lift_addr(Some(strider_ir::test_utils::SENTINEL_LIFT_ADDR));
        let v = b.read_variable(&rdi).unwrap();
        b.build_return(Some(v), &[]).unwrap();
        b.set_lift_addr(None);
        b.build().unwrap()
    }

    let mut fg_v1 = build(rdi, sp);
    FunctionArgDetect::new(vec![rdi], sp, vec![])
        .optimize(&mut fg_v1)
        .unwrap();

    let mut fg_v2 = build(rdi, sp);
    FunctionArgDetectEgg::new(vec![rdi], sp, vec![])
        .optimize(&mut fg_v2)
        .unwrap();

    let v1 = summarise_args(&fg_v1);
    let v2 = summarise_args(&fg_v2);
    assert_eq!(v1, v2, "v1={v1:?} v2={v2:?}");
    assert_eq!(v1.len(), 1, "expected one register arg, got {v1:?}");
    // Both should have detached the InitialVar(rdi).
    let iv_v1 = count_initial_var_reachable(&fg_v1, rdi);
    let iv_v2 = count_initial_var_reachable(&fg_v2, rdi);
    assert_eq!(iv_v1, iv_v2);
    assert_eq!(iv_v1, 0, "InitialVar(rdi) must be detached after rewiring");
}

/// x86 cdecl-style: `load[sp + 4]` reads the first stack arg.  Both
/// passes must produce a single `FunctionArg { Stack { offset: 4 }, 0 }`.
#[test]
fn parity_reads_stack_arg_0_on_x86_cdecl() {
    let sp = sp_vn();

    fn build(sp: rsleigh::Vn) -> BuiltFunctionGraph {
        strider_ir::test_utils::make_sp_fn(sp, |b, sp_val| {
            let four = b.build_int_const(4u64, NodeOutputType::U64)?;
            let addr =
                b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, NodeOutputType::U64)?;
            let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
            b.build_return(Some(loaded), &[])?;
            Ok(())
        })
        .unwrap()
    }

    fn run(opt: &dyn FunctionOptimizerLike, sp: rsleigh::Vn) -> BuiltFunctionGraph {
        let mut fg = build(sp);
        let mut pipeline = OptimizerPipeline::new();
        pipeline.add(ConstantFold);
        opt.add_post_pass(&mut pipeline);
        pipeline.run(&mut fg.graph, fg.entry).unwrap();
        fg
    }

    trait FunctionOptimizerLike {
        fn add_post_pass(&self, p: &mut OptimizerPipeline);
    }
    struct V1(rsleigh::Vn);
    struct V2(rsleigh::Vn);
    impl FunctionOptimizerLike for V1 {
        fn add_post_pass(&self, p: &mut OptimizerPipeline) {
            p.add_post_pass(FunctionArgDetect::new(vec![], self.0, vec![4]));
        }
    }
    impl FunctionOptimizerLike for V2 {
        fn add_post_pass(&self, p: &mut OptimizerPipeline) {
            p.add_post_pass(FunctionArgDetectEgg::new(vec![], self.0, vec![4]));
        }
    }

    let fg_v1 = run(&V1(sp), sp);
    let fg_v2 = run(&V2(sp), sp);
    let v1 = summarise_args(&fg_v1);
    let v2 = summarise_args(&fg_v2);
    assert_eq!(v1, v2);
    assert_eq!(v1.len(), 1, "expected one stack arg, got {v1:?}");
    // Both should have detached the original Load.
    assert_eq!(count_loads_reachable(&fg_v1), count_loads_reachable(&fg_v2));
    assert_eq!(count_loads_reachable(&fg_v1), 0);
}

/// Stack-arg gap truncation: loads at sp+4 and sp+12 but NOT sp+8.
/// Both passes must emit only arg 0 (sp+4) and leave sp+12 alone.
#[test]
fn parity_stack_arg_gap_truncates() {
    let sp = sp_vn();

    fn build_sp_load(
        b: &mut FunctionBuilder,
        sp: &rsleigh::Vn,
        offset: u32,
    ) -> anyhow::Result<strider_ir::node::NodeOutputId> {
        let sp_val = b.read_variable(sp)?;
        let off_const = b.build_int_const(offset as u64, NodeOutputType::U64)?;
        let addr =
            b.build_int_binary_operation(sp_val, off_const, IntBinaryOp::Add, NodeOutputType::U64)?;
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
        Ok(loaded)
    }

    fn build(sp: rsleigh::Vn) -> BuiltFunctionGraph {
        strider_ir::test_utils::make_sp_fn(sp, |b, _sp_val| {
            let a = build_sp_load(b, &sp, 4)?;
            let c = build_sp_load(b, &sp, 12)?;
            let sum = b.build_int_binary_operation(a, c, IntBinaryOp::Add, NodeOutputType::U32)?;
            b.build_return(Some(sum), &[])?;
            Ok(())
        })
        .unwrap()
    }

    let mut fg_v1 = build(sp);
    let mut pipeline_v1 = OptimizerPipeline::new();
    pipeline_v1.add(ConstantFold);
    pipeline_v1.add_post_pass(FunctionArgDetect::new(vec![], sp, vec![4, 8, 12]));
    pipeline_v1.run(&mut fg_v1.graph, fg_v1.entry).unwrap();

    let mut fg_v2 = build(sp);
    let mut pipeline_v2 = OptimizerPipeline::new();
    pipeline_v2.add(ConstantFold);
    pipeline_v2.add_post_pass(FunctionArgDetectEgg::new(vec![], sp, vec![4, 8, 12]));
    pipeline_v2.run(&mut fg_v2.graph, fg_v2.entry).unwrap();

    let v1 = summarise_args(&fg_v1);
    let v2 = summarise_args(&fg_v2);
    assert_eq!(v1, v2, "v1={v1:?} v2={v2:?}");
    assert_eq!(v1.len(), 1, "only arg 0 (sp+4) should be emitted; got {v1:?}");
}

/// Mixed register + stack args: arg 0 = register, arg 1 = stack
/// at sp+4.  Both passes must emit two `FunctionArg` nodes with the
/// correct indices.
#[test]
fn parity_mixed_register_and_stack_args() {
    let rdi = rdi_like_vn();
    let sp = sp_vn();

    fn build(rdi: rsleigh::Vn, sp: rsleigh::Vn) -> BuiltFunctionGraph {
        let mut b = FunctionBuilder::new_raw(vec![rdi, sp], &[], &[rdi], &[rdi], None, 0).unwrap();
        let region = b.create_region().unwrap();
        b.set_entry_region(region).unwrap();
        b.set_region(region);
        b.set_lift_addr(Some(strider_ir::test_utils::SENTINEL_LIFT_ADDR));
        let rdi_val = b.read_variable(&rdi).unwrap();
        let sp_val = b.read_variable(&sp).unwrap();
        let four = b.build_int_const(4u64, NodeOutputType::U64).unwrap();
        let addr = b
            .build_int_binary_operation(sp_val, four, IntBinaryOp::Add, NodeOutputType::U64)
            .unwrap();
        let stack_arg = b
            .build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U64)
            .unwrap();
        let sum = b
            .build_int_binary_operation(rdi_val, stack_arg, IntBinaryOp::Add, NodeOutputType::U64)
            .unwrap();
        b.build_return(Some(sum), &[]).unwrap();
        b.set_lift_addr(None);
        b.build().unwrap()
    }

    let mut fg_v1 = build(rdi, sp);
    let mut pipeline_v1 = OptimizerPipeline::new();
    pipeline_v1.add(ConstantFold);
    pipeline_v1.add_post_pass(FunctionArgDetect::new(vec![rdi], sp, vec![4]));
    pipeline_v1.run(&mut fg_v1.graph, fg_v1.entry).unwrap();

    let mut fg_v2 = build(rdi, sp);
    let mut pipeline_v2 = OptimizerPipeline::new();
    pipeline_v2.add(ConstantFold);
    pipeline_v2.add_post_pass(FunctionArgDetectEgg::new(vec![rdi], sp, vec![4]));
    pipeline_v2.run(&mut fg_v2.graph, fg_v2.entry).unwrap();

    let v1 = summarise_args(&fg_v1);
    let v2 = summarise_args(&fg_v2);
    assert_eq!(v1, v2, "v1={v1:?} v2={v2:?}");
    assert_eq!(v1.len(), 2, "expected 2 args (1 reg + 1 stack); got {v1:?}");
    // Both register and stack args should be detached.
    assert_eq!(count_initial_var_reachable(&fg_v1, rdi), 0);
    assert_eq!(count_initial_var_reachable(&fg_v2, rdi), 0);
    assert_eq!(count_loads_reachable(&fg_v1), 0);
    assert_eq!(count_loads_reachable(&fg_v2), 0);
}

/// Negative test: a function with no arg-register reads and no
/// stack-arg loads should not emit any `FunctionArg`.  v1/v2 must agree.
#[test]
fn parity_no_args_emits_nothing() {
    let rdi = rdi_like_vn();
    let rsi = rsi_like_vn();
    let sp = sp_vn();

    fn build(sp: rsleigh::Vn) -> BuiltFunctionGraph {
        strider_ir::test_utils::make_sp_fn(sp, |b, _sp_val| {
            let v = b.build_int_const(42u64, NodeOutputType::U64)?;
            b.build_return(Some(v), &[])?;
            Ok(())
        })
        .unwrap()
    }

    let mut fg_v1 = build(sp);
    FunctionArgDetect::new(vec![rdi, rsi], sp, vec![4, 8])
        .optimize(&mut fg_v1)
        .unwrap();

    let mut fg_v2 = build(sp);
    FunctionArgDetectEgg::new(vec![rdi, rsi], sp, vec![4, 8])
        .optimize(&mut fg_v2)
        .unwrap();

    let v1 = summarise_args(&fg_v1);
    let v2 = summarise_args(&fg_v2);
    assert_eq!(v1, v2);
    assert_eq!(v1.len(), 0, "no args expected, got {v1:?}");
}

/// Trait shim so summarise tests can call .optimize(BFG) on both.
trait BfgOptimizer {
    fn optimize(&self, fg: &mut BuiltFunctionGraph) -> anyhow::Result<()>;
}

impl BfgOptimizer for FunctionArgDetect {
    fn optimize(&self, fg: &mut BuiltFunctionGraph) -> anyhow::Result<()> {
        use strider_analyze::opt::OptimizerRaw;
        self.optimize_raw(&mut fg.graph, fg.entry)?;
        Ok(())
    }
}

impl BfgOptimizer for FunctionArgDetectEgg {
    fn optimize(&self, fg: &mut BuiltFunctionGraph) -> anyhow::Result<()> {
        use strider_analyze::opt::OptimizerRaw;
        self.optimize_raw(&mut fg.graph, fg.entry)?;
        Ok(())
    }
}
