//! Full lift + optimizer pipeline over x86 / x86_64 ELF fixtures, the same
//! cases again through `Strider::analyze`'s resolution loop, and synthetic
//! scaling curves for individual passes and the pattern matcher.
//!
//! ```text
//! cargo bench -p strider-orchestrator --bench scaling -- --save-baseline before
//! cargo bench -p strider-orchestrator --bench scaling -- --baseline before
//! ```

#![allow(clippy::useless_conversion)]

use std::hint::black_box;
use strider_ir::IRViewer;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};

use strider_ir::node::{ValueKind, ValueType};
use strider_ir::{IRBuilder, IRBuilderExt, IntBinaryOp};
use strider_ir_test_utils::{IrWalkerEx, RegisterSet, stack_vn_aarch64};
use strider_orchestrator::opt::{
    ConstantFold, LoadForward, OptimizerPipeline, PhiCollapse, RegionCollapse,
};

#[allow(dead_code)]
#[path = "../tests/common/fixtures.rs"]
mod fixtures;

use fixtures::{Fixture, fixture_arch};

#[derive(Clone, Copy)]
struct Case {
    arch_name: &'static str,
    case: &'static str,
    fn_name: &'static str,
}

const CASES: &[Case] = &[
    Case {
        arch_name: "x86",
        case: "complex",
        fn_name: "complex_dispatch",
    },
    Case {
        arch_name: "x86",
        case: "complex",
        fn_name: "multi_arg_call_in_branch",
    },
    Case {
        arch_name: "x86",
        case: "indirect_branch",
        fn_name: "main",
    },
    Case {
        arch_name: "x86",
        case: "calls",
        fn_name: "main",
    },
    Case {
        arch_name: "x64",
        case: "complex",
        fn_name: "complex_dispatch",
    },
    Case {
        arch_name: "x64",
        case: "indirect_branch",
        fn_name: "main",
    },
];

/// Follows a branch below the entry as an edge: fixture functions sit above
/// code they branch into.
const CFG_OPTS: fn() -> strider_cfg::CfgOptions<'static> = || strider_cfg::CfgOptions {
    bound: strider_cfg::FunctionBound::Open {
        code_before_start: true,
    },
    ..Default::default()
};

fn analyze_case(c: Case) -> strider_ir::Function {
    let fixture = Fixture::open(fixture_arch(c.arch_name), c.case);
    let sleigh_arch = (fixture.arch.sleigh)();
    let mem = strider_reader::ElfFileMemReader::new(
        fixture.elf(),
        strider_reader::elf::RelocationMode::Unapplied,
    )
    .expect("mem reader");
    let mut ana = strider_orchestrator::Lifter::new(sleigh_arch, mem).expect("Lifter::new");
    let cc = (fixture.arch.cc)()
        .build(ana.sleigh_regs())
        .expect("build cc");
    let addr = fixture.symbol(c.fn_name);
    let cfg = ana
        .build_cfg(strider_cfg::MachineInsnAddr::from(addr), &CFG_OPTS())
        .expect("Cfg build");
    let mut function = ana.build_ir(&cfg, cc).expect("build_ir").function;
    let rom_for_opt = strider_reader::ElfFileMemReader::new(
        fixture.elf(),
        strider_reader::elf::RelocationMode::Unapplied,
    )
    .expect("rom reader (opt)");
    let p = strider_orchestrator::opt::default_pipeline();
    function.assume(&strider_orchestrator::opt::AssumptionOptions::default());
    p.run(
        &mut function,
        &strider_orchestrator::opt::OptCtx::new(Some(&rom_for_opt)),
    )
    .expect("optimizer pipeline");
    function
}

/// `analyze_case` plus the indirect-branch resolution loop.
fn resolve_case(c: Case) -> strider_orchestrator::AnalyzeResult {
    let fixture = Fixture::open(fixture_arch(c.arch_name), c.case);
    let (mut strider, cc) = fixture.strider();
    let addr = fixture.symbol(c.fn_name);
    let opts = strider_orchestrator::AnalyzeOptions {
        cfg: CFG_OPTS(),
        ..Default::default()
    };
    strider.analyze(addr, &cc, &opts, None).expect("analyze")
}

fn bench_pipeline(c: &mut Criterion) {
    let mut group = c.benchmark_group("strider/pipeline");
    group.sample_size(20); // slow pipeline runs; fewer samples than default
    for case in CASES {
        let id = format!("{}/{}::{}", case.arch_name, case.case, case.fn_name);
        group.bench_function(&id, |b| {
            b.iter(|| {
                let function = analyze_case(*case);
                black_box(function);
            });
        });
    }
    group.finish();
}

fn bench_resolution_loop(c: &mut Criterion) {
    let mut group = c.benchmark_group("strider/analyze");
    group.sample_size(20);
    for case in CASES {
        let id = format!("{}/{}::{}", case.arch_name, case.case, case.fn_name);
        group.bench_function(&id, |b| {
            b.iter(|| {
                let result = resolve_case(*case);
                black_box(result);
            });
        });
    }
    group.finish();
}

mod synthetic {
    use super::*;

    /// An 8-byte VN to address stores off; any arch's would do.
    pub fn stack_vn() -> rsleigh::Vn {
        stack_vn_aarch64()
    }

    /// `n` SP-relative constant `Store`s and `n` matching `Load`s summed into
    /// the return, canonicalised for `LoadForward`.
    ///
    /// All `n` loads forward only because `stack_vn` names the stack pointer
    /// (else the addresses do not decompose) and `PhiCollapse` /
    /// `RegionCollapse` retire the builder's trivial header `MemPhi` (an
    /// opaque merge to the memory walk). [`bench_stack_store_chain`] asserts
    /// it.
    pub fn build_stack_store_chain(n: usize) -> strider_ir::Function {
        let sp = stack_vn();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .stack_vn(sp)
            .callee_saved(sp)
            .build_fn_single_region()
            .unwrap();
        let sp_val = b.read_variable(&sp).unwrap();
        let mut load_addrs: Vec<strider_ir::node::ValueId> = Vec::with_capacity(n);
        for i in 0..n {
            let off = -((i as i64 + 1) * 8) as u64;
            let off_const = b.build_int_const(off, ValueType::int(64)).unwrap();
            let addr = b
                .build_int_binary_operation(sp_val, off_const, IntBinaryOp::Add, ValueType::int(64))
                .unwrap();
            let v = b.build_int_const(i as u64, ValueType::int(64)).unwrap();
            b.build_store(addr, v, rsleigh::VnSpace::RAM).unwrap();
            load_addrs.push(addr);
        }
        let mut acc = b.build_int_const(0u64, ValueType::int(64)).unwrap();
        for addr in load_addrs {
            let loaded = b
                .build_load(addr, rsleigh::VnSpace::RAM, ValueType::int(64))
                .unwrap();
            acc = b
                .build_int_binary_operation(acc, loaded, IntBinaryOp::Add, ValueType::int(64))
                .unwrap();
        }
        b.build_return(Some(acc), &[]).unwrap();
        let mut fg = b.build().unwrap();
        let mut p = OptimizerPipeline::new();
        p.add(&ConstantFold);
        p.add(&PhiCollapse);
        p.add(&RegionCollapse);
        fg.assume(&strider_orchestrator::opt::AssumptionOptions::default());
        p.run(&mut fg, &strider_orchestrator::opt::OptCtx::new(None))
            .unwrap();
        fg
    }

    /// `n` chained if-else diamonds, optimised.
    pub fn run_diamond_cfg(n: usize) -> strider_ir::Function {
        let sp = stack_vn();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .callee_saved(sp)
            .build_fn()
            .unwrap();
        let entry = b.create_region_all().unwrap();
        b.set_entry_region_all(entry).unwrap();

        let mut prev_region = entry;
        for _ in 0..n {
            let true_arm = b.create_region_all().unwrap();
            let false_arm = b.create_region_all().unwrap();
            let merge = b.create_region_all().unwrap();

            b.set_region(prev_region);
            let cond = b.build_boolean_const(true);
            b.build_if(cond, true_arm, false_arm).unwrap();

            // Distinct constants per arm, so PhiCollapse keeps the merge.
            b.set_region(true_arm);
            let sp_t = b.read_variable(&sp).unwrap();
            let off_t = b.build_int_const(0xa_au64, ValueType::int(64)).unwrap();
            let _ = b
                .build_int_binary_operation(sp_t, off_t, IntBinaryOp::Add, ValueType::int(64))
                .unwrap();
            b.build_branch(merge).unwrap();

            b.set_region(false_arm);
            let sp_f = b.read_variable(&sp).unwrap();
            let off_f = b.build_int_const(0xb_bu64, ValueType::int(64)).unwrap();
            let _ = b
                .build_int_binary_operation(sp_f, off_f, IntBinaryOp::Add, ValueType::int(64))
                .unwrap();
            b.build_branch(merge).unwrap();

            prev_region = merge;
        }

        b.set_region(prev_region);
        let sp_final = b.read_variable(&sp).unwrap();
        b.build_return(Some(sp_final), &[]).unwrap();
        let mut fg = b.build().unwrap();
        let mut p = OptimizerPipeline::new();
        p.add(&ConstantFold);
        p.add(&PhiCollapse);
        p.add(&RegionCollapse);
        fg.assume(&strider_orchestrator::opt::AssumptionOptions::default());
        p.run(&mut fg, &strider_orchestrator::opt::OptCtx::new(None))
            .unwrap();
        fg
    }

    /// The jump-table shape with `n` (a power of 2) targets stored at
    /// contiguous stack offsets, one reloaded through `(arg & (n - 1)) *
    /// stride`, with no `IndirectBranch`; optimised by `ConstantFold` only.
    pub fn run_jump_table_scenario(n: usize) -> strider_ir::Function {
        assert!(
            n.is_power_of_two(),
            "jump-table fixture requires n = power of 2",
        );
        let mask = (n - 1) as u64;
        let sp = stack_vn();
        let arg_vn = rsleigh::Vn {
            addr_off: 0x38,
            addr_space: rsleigh::VnSpace::REGISTER,
            size: 8,
        };
        let mut b = RegisterSet::new()
            .tracked(sp)
            .tracked(arg_vn)
            .callee_saved(sp)
            .build_fn_single_region()
            .unwrap();
        let sp_val = b.read_variable(&sp).unwrap();

        for i in 0..n {
            let off = -((i as i64 + 1) * 8) as u64;
            let off_const = b.build_int_const(off, ValueType::int(64)).unwrap();
            let addr = b
                .build_int_binary_operation(sp_val, off_const, IntBinaryOp::Add, ValueType::int(64))
                .unwrap();
            let target = b
                .build_int_const(0x4000 + i as u64, ValueType::int(64))
                .unwrap();
            b.build_store(addr, target, rsleigh::VnSpace::RAM).unwrap();
        }
        let arg_val = b.read_variable(&arg_vn).unwrap();
        let arg_u32 = b.create_node_attributed(
            strider_ir::node::NodeKind::Truncate,
            [arg_val],
            [ValueKind::Typed(ValueType::int(32))],
            &[],
        );
        let arg_u32_value = b.function().node_outputs_exact::<1>(arg_u32).unwrap()[0];
        let mask_c = b.build_int_const(mask, ValueType::int(32)).unwrap();
        let masked = b
            .build_int_binary_operation(arg_u32_value, mask_c, IntBinaryOp::And, ValueType::int(32))
            .unwrap();
        let idx_u64 = b.create_node_attributed(
            strider_ir::node::NodeKind::Extend(strider_ir::ExtendOp::ZeroExtend),
            [masked],
            [ValueKind::Typed(ValueType::int(64))],
            &[],
        );
        let idx_u64_value = b.function().node_outputs_exact::<1>(idx_u64).unwrap()[0];
        let stride = b.build_int_const(8u64, ValueType::int(64)).unwrap();
        let idx_scaled = b
            .build_int_binary_operation(idx_u64_value, stride, IntBinaryOp::Mul, ValueType::int(64))
            .unwrap();
        let base = b.build_int_const(0u64, ValueType::int(64)).unwrap();
        let sp_plus_base = b
            .build_int_binary_operation(sp_val, base, IntBinaryOp::Add, ValueType::int(64))
            .unwrap();
        let load_addr = b
            .build_int_binary_operation(
                sp_plus_base,
                idx_scaled,
                IntBinaryOp::Add,
                ValueType::int(64),
            )
            .unwrap();
        let loaded = b
            .build_load(load_addr, rsleigh::VnSpace::RAM, ValueType::int(64))
            .unwrap();
        b.build_return(Some(loaded), &[]).unwrap();
        let mut fg = b.build().unwrap();
        let mut p = OptimizerPipeline::new();
        p.add(&ConstantFold);
        fg.assume(&strider_orchestrator::opt::AssumptionOptions::default());
        p.run(&mut fg, &strider_orchestrator::opt::OptCtx::new(None))
            .unwrap();
        fg
    }

    /// The sum of `n` distinct `IntConst`s.
    pub fn build_many_int_consts(n: usize) -> strider_ir::Function {
        let mut b = strider_ir_test_utils::empty_builder().unwrap();
        let region = b.create_region_all().unwrap();
        b.set_entry_region_all(region).unwrap();
        b.set_region(region);
        let mut acc = b.build_int_const(0u64, ValueType::int(64)).unwrap();
        for i in 1..=n {
            let c = b.build_int_const(i as u64, ValueType::int(64)).unwrap();
            acc = b
                .build_int_binary_operation(acc, c, IntBinaryOp::Add, ValueType::int(64))
                .unwrap();
        }
        b.build_return(Some(acc), &[]).unwrap();
        b.build().unwrap()
    }
}

fn bench_stack_store_chain(c: &mut Criterion) {
    // A fixture that stops forwarding still yields a clean curve.
    {
        let mut fg = synthetic::build_stack_store_chain(32);
        fg.assume(&strider_orchestrator::opt::AssumptionOptions::default());
        let pass = LoadForward;
        strider_orchestrator::opt::run_one(
            &pass,
            &mut fg,
            &strider_orchestrator::opt::OptCtx::new(None),
        )
        .unwrap();
        assert_eq!(
            fg.count_kind(|k| matches!(k, strider_ir::node::NodeKind::Load(_))),
            0,
            "every load must forward, or this group benches one rewrite"
        );
    }
    let mut group = c.benchmark_group("synthetic/stack_store_chain");
    group.sample_size(20); // one sweep over n=1000 is ~80ms
    for n in [100usize, 500, 1_000] {
        group.bench_function(format!("n_{n}"), |b| {
            b.iter_batched(
                || {
                    let mut fg = synthetic::build_stack_store_chain(n);
                    fg.assume(&strider_orchestrator::opt::AssumptionOptions::default());
                    fg
                },
                |mut fg| {
                    let pass = LoadForward;
                    let _ = strider_orchestrator::opt::run_one_unvalidated(
                        &pass,
                        &mut fg,
                        &strider_orchestrator::opt::OptCtx::new(None),
                    );
                    black_box(fg);
                },
                BatchSize::LargeInput,
            );
        });
    }
    group.finish();
}

fn bench_diamond_cfg(c: &mut Criterion) {
    let mut group = c.benchmark_group("synthetic/diamond_cfg");
    for n in [100usize, 500, 1_000] {
        group.bench_function(format!("n_{n}_regions"), |b| {
            b.iter(|| {
                let function = synthetic::run_diamond_cfg(black_box(n));
                black_box(function);
            });
        });
    }
    group.finish();
}

fn bench_wide_jump_table(c: &mut Criterion) {
    let mut group = c.benchmark_group("synthetic/wide_jump_table");
    for n in [16usize, 64, 256] {
        group.bench_function(format!("n_{n}_targets"), |b| {
            b.iter(|| {
                let function = synthetic::run_jump_table_scenario(black_box(n));
                black_box(function);
            });
        });
    }
    group.finish();
}

fn bench_find_joined_shared_capture(c: &mut Criterion) {
    use strider_pattern::{Capture, MatchPat, Matcher, int_add, int_const, var};

    let mut group = c.benchmark_group("synthetic/find_joined_shared");
    for n in [100usize, 500, 1_000] {
        let fg = synthetic::build_many_int_consts(n);
        // Two patterns joined on `x`: the rhs of every Add, and every
        // IntConst.
        let x = Capture::new();
        let pat1 = int_add(strider_pattern::anything(), var(x)).into_pattern();
        let pat2 = int_const(x).into_pattern();
        group.bench_function(format!("n_{n}"), |bnch| {
            bnch.iter(|| {
                let m = Matcher::new(&fg);
                let pat_refs: Vec<&strider_pattern::Pattern> = vec![&pat1, &pat2];
                let result = m
                    .find_joined_constrained(&pat_refs, &[])
                    .expect("bench patterns are single-rooted");
                black_box(result);
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_pipeline,
    bench_resolution_loop,
    bench_stack_store_chain,
    bench_diamond_cfg,
    bench_wide_jump_table,
    bench_find_joined_shared_capture,
);
criterion_main!(benches);
