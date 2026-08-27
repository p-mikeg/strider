//! Full lift + optimizer pipeline over x86 / x86_64 ELF fixtures.
//!
//! ```text
//! cargo bench --bench scaling -- --save-baseline before
//! cargo bench --bench scaling -- --baseline before
//! ```

#![allow(clippy::useless_conversion)]

use std::hint::black_box;
use std::path::PathBuf;
use strider_ir::IRViewer;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use object::{Object, ObjectSymbol};

use strider_ir::node::{ValueKind, ValueType};
use strider_ir::{IRBuilder, IRBuilderExt, IntBinaryOp};
use strider_ir_test_utils::{RegisterSet, stack_vn_aarch64};
use strider_orchestrator::opt::{
    ConstantFold, LoadForward, OptimizerPipeline, PhiCollapse, RegionCollapse,
};

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

fn binary_path(arch_name: &str, case: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/out")
        .join(arch_name)
        .join(format!("{case}.elf"))
}

fn analyze_case(c: Case) -> strider_ir::Function {
    let path = binary_path(c.arch_name, c.case);
    let obj = strider_reader::load_elf(&path).expect("load_elf");
    let obj = obj.file();
    let sleigh_arch = match c.arch_name {
        "x86" => strider_target::SleighArch::x86(),
        "x64" => strider_target::SleighArch::x86_64(),
        _ => panic!("unsupported arch {}", c.arch_name),
    };
    let cc = match c.arch_name {
        "x86" => strider_target::CallingConvention::x86_cdecl(),
        "x64" => strider_target::CallingConvention::x86_64_systemv(),
        // Already matched above. Uses panic! (not unreachable!) since
        // only clippy::panic is on this bench's allow list.
        _ => panic!("unsupported arch {}", c.arch_name),
    };
    let mem = strider_reader::ElfFileMemReader::from_object(&obj).expect("mem reader");
    let sleigh = rsleigh::Sleigh::new(sleigh_arch.sla_spec(), sleigh_arch.pspec(), mem)
        .expect("real sleigh");
    let mut ana = strider_orchestrator::Lifter::new(sleigh_arch, sleigh).expect("Lifter::new");
    let cc = cc.build(ana.sleigh_regs()).expect("build cc");
    let raw_addr = obj
        .symbol_by_name(c.fn_name)
        .unwrap_or_else(|| panic!("symbol {:?} not found in {path:?}", c.fn_name))
        .address();
    let addr = raw_addr;
    let cfg_opts = strider_cfg::CfgOptions {
        allow_code_before_start_addr: true,
        ..Default::default()
    };
    let cfg = ana
        .build_cfg(
            strider_cfg::MachineInsnAddr::from(addr),
            &cfg_opts,
            &Default::default(),
        )
        .expect("Cfg build");
    let mut function = ana.build_ir(&cfg, cc).expect("build_ir").function;
    let rom_for_opt =
        strider_reader::ElfFileMemReader::from_object(&obj).expect("rom reader (opt)");
    let p = strider_orchestrator::opt::default_pipeline();
    p.run(
        &mut function,
        &mut strider_orchestrator::opt::OptCtx::new(Some(&rom_for_opt)),
    )
    .expect("optimizer pipeline");
    function
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

// Synthetic benches independent of ELF fixtures; each parameterises over
// problem size N to plot scaling curves separately from pipeline cost.
// Helpers stay in a private module and never recurse: recursion would
// inflate Criterion's per-iteration `iter_batched` cost unpredictably.

mod synthetic {
    use super::*;

    /// Synthetic 8-byte stack-pointer VN. Doesn't have to match a real
    /// arch: `LoadForward` only cares that it's the SP varnode passed
    /// into the pass constructor.
    pub fn stack_vn() -> rsleigh::Vn {
        stack_vn_aarch64()
    }

    /// Builds `n` SP-relative `Store`s at distinct offsets, each storing
    /// a fresh `IntConst`, followed by `n` matching `Load`s chained
    /// through `Add`s into the return value. Runs `ConstantFold` first
    /// so the bench measures `LoadForward` in isolation.
    pub fn build_stack_store_chain(n: usize) -> strider_ir::Function {
        let sp = stack_vn();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .callee_saved(sp)
            .build_fn_single_region()
            .unwrap();
        let sp_val = b.read_variable(&sp).unwrap();
        let mut load_addrs: Vec<strider_ir::Value> = Vec::with_capacity(n);
        for i in 0..n {
            let off = -((i as i64 + 1) * 8) as u64;
            let off_const = b.build_int_const(off, ValueType::I64).unwrap();
            let addr = b
                .build_int_binary_operation(sp_val, off_const, IntBinaryOp::Add, ValueType::I64)
                .unwrap();
            let v = b.build_int_const(i as u64, ValueType::I64).unwrap();
            b.build_store(addr, v, rsleigh::VnSpace::RAM).unwrap();
            load_addrs.push(addr);
        }
        // Left-fold the loads through Add so every one reaches the return.
        let mut acc = b.build_int_const(0u64, ValueType::I64).unwrap();
        for addr in load_addrs {
            let loaded = b
                .build_load(addr, rsleigh::VnSpace::RAM, ValueType::I64)
                .unwrap();
            acc = b
                .build_int_binary_operation(acc, loaded, IntBinaryOp::Add, ValueType::I64)
                .unwrap();
        }
        b.build_return(Some(acc), &[]).unwrap();
        let mut fg = b.build().unwrap();
        let mut p = OptimizerPipeline::new();
        p.add(ConstantFold::new());
        p.run(&mut fg, &mut strider_orchestrator::opt::OptCtx::new(None))
            .unwrap();
        fg
    }

    /// Builds `n` if-else diamonds chained sequentially, each merging
    /// back before the next branches; merge-phi count grows linearly
    /// in `n`. Benches validator + optimiser scaling on merge-heavy IRs.
    pub fn run_diamond_cfg(n: usize) -> strider_ir::Function {
        let sp = stack_vn();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .callee_saved(sp)
            .build_fn()
            .unwrap();
        let entry = b.create_region_all().unwrap();
        b.set_entry_region_all(entry).unwrap();

        // Each iteration creates true/false/merge regions, routes
        // prev -> If -> merge, then advances prev to merge.
        let mut prev_region = entry;
        for _ in 0..n {
            let true_arm = b.create_region_all().unwrap();
            let false_arm = b.create_region_all().unwrap();
            let merge = b.create_region_all().unwrap();

            b.set_region(prev_region);
            let cond = b.build_boolean_const(true);
            b.build_if(cond, true_arm, false_arm).unwrap();

            // Distinct offset constants per arm so PhiCollapse can't
            // collapse the merge.
            b.set_region(true_arm);
            let sp_t = b.read_variable(&sp).unwrap();
            let off_t = b.build_int_const(0xa_au64, ValueType::I64).unwrap();
            let _ = b
                .build_int_binary_operation(sp_t, off_t, IntBinaryOp::Add, ValueType::I64)
                .unwrap();
            b.build_branch(merge).unwrap();

            b.set_region(false_arm);
            let sp_f = b.read_variable(&sp).unwrap();
            let off_f = b.build_int_const(0xb_bu64, ValueType::I64).unwrap();
            let _ = b
                .build_int_binary_operation(sp_f, off_f, IntBinaryOp::Add, ValueType::I64)
                .unwrap();
            b.build_branch(merge).unwrap();

            prev_region = merge;
        }

        b.set_region(prev_region);
        let sp_final = b.read_variable(&sp).unwrap();
        b.build_return(Some(sp_final), &[]).unwrap();
        let mut fg = b.build().unwrap();
        // Measures build + pipeline together: build dominates, but this
        // also pins the validator's per-region cost.
        let mut p = OptimizerPipeline::new();
        p.add(ConstantFold::new());
        p.add(PhiCollapse);
        p.add(RegionCollapse);
        p.run(&mut fg, &mut strider_orchestrator::opt::OptCtx::new(None))
            .unwrap();
        fg
    }

    /// Builds a stack-array indirect-branch dispatch with `n` targets
    /// (`n` must be a power of 2): constants at contiguous SP-relative
    /// offsets, loaded via `arg & (n-1) * stride`. Measures lift +
    /// stable-subset cost only; the indirect-branch resolver isn't run.
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
            let off_const = b.build_int_const(off, ValueType::I64).unwrap();
            let addr = b
                .build_int_binary_operation(sp_val, off_const, IntBinaryOp::Add, ValueType::I64)
                .unwrap();
            let target = b
                .build_int_const(0x4000 + i as u64, ValueType::I64)
                .unwrap();
            b.build_store(addr, target, rsleigh::VnSpace::RAM).unwrap();
        }
        let arg_val = b.read_variable(&arg_vn).unwrap();
        // create_node_attributed, not raw create_node: the latter bypasses
        // attribution and trips the always-on fingerprint validator.
        let arg_u32 = b.create_node_attributed(
            strider_ir::node::NodeKind::Truncate,
            [arg_val],
            [ValueKind::Typed(ValueType::I32)],
            &[],
        );
        let arg_u32_value = b.function().node_outputs_exact::<1>(arg_u32).unwrap()[0];
        let mask_c = b.build_int_const(mask, ValueType::I32).unwrap();
        let masked = b
            .build_int_binary_operation(arg_u32_value, mask_c, IntBinaryOp::And, ValueType::I32)
            .unwrap();
        let idx_u64 = b.create_node_attributed(
            strider_ir::node::NodeKind::Extend(strider_ir::ExtendOp::ZeroExtend),
            [masked],
            [ValueKind::Typed(ValueType::I64)],
            &[],
        );
        let idx_u64_value = b.function().node_outputs_exact::<1>(idx_u64).unwrap()[0];
        let stride = b.build_int_const(8u64, ValueType::I64).unwrap();
        let idx_scaled = b
            .build_int_binary_operation(idx_u64_value, stride, IntBinaryOp::Mul, ValueType::I64)
            .unwrap();
        let base = b.build_int_const(0u64, ValueType::I64).unwrap();
        let sp_plus_base = b
            .build_int_binary_operation(sp_val, base, IntBinaryOp::Add, ValueType::I64)
            .unwrap();
        let load_addr = b
            .build_int_binary_operation(sp_plus_base, idx_scaled, IntBinaryOp::Add, ValueType::I64)
            .unwrap();
        let loaded = b
            .build_load(load_addr, rsleigh::VnSpace::RAM, ValueType::I64)
            .unwrap();
        b.build_return(Some(loaded), &[]).unwrap();
        let mut fg = b.build().unwrap();
        let mut p = OptimizerPipeline::new();
        p.add(ConstantFold::new());
        p.run(&mut fg, &mut strider_orchestrator::opt::OptCtx::new(None))
            .unwrap();
        fg
    }

    /// Build a function with `n` distinct `IntConst` nodes added
    /// together.  Used to bench pattern-matcher cross-product joins
    /// (`find_joined`) with shared captures.
    pub fn build_many_int_consts(n: usize) -> strider_ir::Function {
        let mut b = strider_ir_test_utils::empty_builder().unwrap();
        let region = b.create_region_all().unwrap();
        b.set_entry_region_all(region).unwrap();
        b.set_region(region);
        // Each IntConst(i) is a distinct dedup-cache key, so this yields
        // N distinct root nodes for the matcher to walk.
        let mut acc = b.build_int_const(0u64, ValueType::I64).unwrap();
        for i in 1..=n {
            let c = b.build_int_const(i as u64, ValueType::I64).unwrap();
            acc = b
                .build_int_binary_operation(acc, c, IntBinaryOp::Add, ValueType::I64)
                .unwrap();
        }
        b.build_return(Some(acc), &[]).unwrap();
        b.build().unwrap()
    }
}

fn bench_stack_store_chain(c: &mut Criterion) {
    let mut group = c.benchmark_group("synthetic/stack_store_chain");
    for n in [100usize, 500, 1_000] {
        group.bench_function(format!("n_{n}"), |b| {
            b.iter_batched(
                || synthetic::build_stack_store_chain(n),
                |mut fg| {
                    let pass = LoadForward::default();
                    let _ = strider_orchestrator::opt::run_one(
                        &pass,
                        &mut fg,
                        &mut strider_orchestrator::opt::OptCtx::new(None),
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
        // Two patterns that share a capture `x`:
        //   pat1: add(_, x).capture(y)  (x = rhs of every Add)
        //   pat2: any_int_const()    (x = every IntConst)
        // The cross-product join over shared `x` exercises the
        // matcher's bindings-equality path on every (Add, IntConst)
        // pair where they coincide.
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
    bench_stack_store_chain,
    bench_diamond_cfg,
    bench_wide_jump_table,
    bench_find_joined_shared_capture,
);
criterion_main!(benches);
