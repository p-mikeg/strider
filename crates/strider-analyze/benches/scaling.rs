//! End-to-end scaling benchmark for the strider pipeline.
//!
//! Runs the full lift + optimizer pipeline against a representative set of
//! x86 / x86_64 ELF fixtures.  Used to track the per-task wins in the
//! 2026-05-01 scaling-bottlenecks plan.
//!
//! Run via: `cargo bench --bench scaling -- --save-baseline before`
//! Compare:  `cargo bench --bench scaling -- --baseline before`

#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

use std::hint::black_box;
use std::path::PathBuf;
use std::sync::Arc;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use object::{Object, ObjectSymbol};

use strider_ir::node::{NodeOutputType, NodeOutputKind};
use strider_ir::{FunctionBuilder, IntBinaryOp};
use strider_ir_test_utils::{stack_vn_aarch64, RegisterSet};
use strider_analyze::opt::{
    ConstantFold, Optimizer, OptimizerPipeline, RedundantPhis, LoadForward,
};

#[derive(Clone, Copy)]
struct Case {
    arch_name: &'static str,
    case: &'static str,
    fn_name: &'static str,
}

const CASES: &[Case] = &[
    Case { arch_name: "x86", case: "complex",          fn_name: "complex_dispatch" },
    Case { arch_name: "x86", case: "complex",          fn_name: "multi_arg_call_in_branch" },
    Case { arch_name: "x86", case: "indirect_branch",  fn_name: "main" },
    Case { arch_name: "x86", case: "calls",            fn_name: "main" },
    Case { arch_name: "x64", case: "complex",          fn_name: "complex_dispatch" },
    Case { arch_name: "x64", case: "indirect_branch",  fn_name: "main" },
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
    let sleigh_arch = match c.arch_name {
        "x86" => strider_target::SleighArch::x86(),
        "x64" => strider_target::SleighArch::x86_64(),
        _ => panic!("unsupported arch {}", c.arch_name),
    };
    let cc = match c.arch_name {
        "x86" => strider_target::CallingConvention::x86_cdecl()
            .expect("x86_cdecl preset must be registered"),
        "x64" => strider_target::CallingConvention::x86_64_systemv()
            .expect("x86_64_systemv preset must be registered"),
        // The earlier `match c.arch_name` guards this — we only
        // reach this point on supported arches.  Use `panic!`
        // (the bench's `clippy::panic` is allow-listed) rather
        // than `unreachable!` (not on the allow list).
        _ => panic!("unsupported arch {}", c.arch_name),
    };
    let probe = rsleigh::mem_readers::BufMemReader::new(vec![], 0);
    let regs = rsleigh::Sleigh::new(sleigh_arch.sla_spec(), sleigh_arch.pspec(), probe)
        .expect("probe sleigh")
        .regs()
        .expect("probe regs");
    let ana = strider_analyze::Strider::new(sleigh_arch, regs, cc).expect("Strider::new");
    let mem = strider_reader::ElfFileMemReader::from_object(&obj).expect("mem reader");
    let sleigh = rsleigh::Sleigh::new(sleigh_arch.sla_spec(), sleigh_arch.pspec(), mem)
        .expect("real sleigh");
    let raw_addr = obj
        .symbol_by_name(c.fn_name)
        .unwrap_or_else(|| panic!("symbol {:?} not found in {path:?}", c.fn_name))
        .address();
    let addr = raw_addr;
    let rom_for_cfg: Arc<dyn strider_analyze::opt::ReadOnlyMemory> = Arc::new(
        strider_reader::ElfFileMemReader::from_object(&obj).expect("rom reader (cfg)"),
    );
    let mut cfg_opts_b = strider_lift::cfg::OptionsBuilder::new()
        .allow_code_before_start_addr()
        .set_read_only_memory(rom_for_cfg);
    if let Some(lr) = ana.calling_convention().link_register_vn {
        cfg_opts_b = cfg_opts_b.set_link_register(lr);
    }
    let cfg_opts = cfg_opts_b.build();
    // Use `for_arch` so both endianness AND `ArchPreset` are derived
    // atomically.  (The deleted `Builder::with_endianness` ctor would
    // silently default the preset to `X86_64`.)
    let cfg = strider_lift::cfg::Builder::for_arch(&sleigh_arch, sleigh, addr, cfg_opts)
        .build()
        .expect("Cfg build");
    let mut function = ana.analyze_cfg(&cfg).expect("analyze_cfg").function;
    let rom_for_opt = strider_reader::ElfFileMemReader::from_object(&obj).expect("rom reader (opt)");
    let mut p = ana.build_optimizer_pipeline();
    p.add(strider_analyze::opt::LoadReadOnly::new(std::sync::Arc::new(rom_for_opt)));
    let entry = function.entry().unwrap();
    p.run(&mut function, entry).expect("optimizer pipeline");
    function
}

fn bench_pipeline(c: &mut Criterion) {
    let mut group = c.benchmark_group("strider/pipeline");
    group.sample_size(20); // pipeline runs are slow-ish; fewer samples
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

// ──────────────────────────────────────────────────────────────────────────
// O12-O15 P4 SCALING BENCHMARKS
// ──────────────────────────────────────────────────────────────────────────
//
// Synthetic-fixture benches that don't depend on ELF fixtures.  Each
// bench parameterises over a problem-size N so we can plot scaling
// curves separately from the absolute pipeline cost.
//
// Helpers live in a private module so the bench-level globs stay
// disciplined.  None of the helpers recurse (Criterion's
// `iter_batched` calls them per-iteration; recursion would inflate
// per-sample cost unpredictably).

mod synthetic {
    use super::*;

    /// Synthetic 8-byte stack-pointer VN.  Same shape used in the
    /// existing `stack_array.rs` tests.  Doesn't have to match a real
    /// arch — `LoadForward` only cares that it's the SP varnode
    /// passed into the pass constructor.
    pub fn stack_vn() -> rsleigh::Vn {
        stack_vn_aarch64()
    }

    /// Build a function with `n` SP-relative `Store`s at distinct
    /// offsets (`-8 * (i+1)`), each storing a fresh `IntConst(i)`,
    /// followed by `n` `Load`s at the matching offsets that feed a
    /// chain of `Add`s producing the function's return value.
    /// The returned graph is ready to feed into `LoadForward`
    /// for the bench.  Pre-pass: `ConstantFold` is run inside the
    /// helper so the bench measures `LoadForward` in isolation.
    pub fn build_stack_store_chain(n: usize) -> strider_ir::Function {
        let sp = stack_vn();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .callee_saved(sp)
            .build_fn_single_region()
            .unwrap();
        let sp_val = b.read_variable(&sp).unwrap();
        // Build N stores at distinct SP offsets.
        let mut load_addrs: Vec<strider_ir::Value> = Vec::with_capacity(n);
        for i in 0..n {
            let off = -((i as i64 + 1) * 8) as u64;
            let off_const = b.build_int_const(off, NodeOutputType::U64).unwrap();
            let addr = b
                .build_int_binary_operation(sp_val, off_const, IntBinaryOp::Add, NodeOutputType::U64)
                .unwrap();
            let v = b.build_int_const(i as u64, NodeOutputType::U64).unwrap();
            b.build_store(addr, v, rsleigh::VnSpace::RAM).unwrap();
            load_addrs.push(addr);
        }
        // Build N loads at the same offsets.  Combine via a left-
        // folding chain of Adds so every loaded value reaches the
        // return.
        let mut acc = b.build_int_const(0u64, NodeOutputType::U64).unwrap();
        for addr in load_addrs {
            let loaded = b
                .build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U64)
                .unwrap();
            acc = b
                .build_int_binary_operation(acc, loaded, IntBinaryOp::Add, NodeOutputType::U64)
                .unwrap();
        }
        b.build_return(Some(acc), &[]).unwrap();
        let mut fg = b.build().unwrap();
        // Pre-pass: ConstantFold so the graph the bench measures
        // is ready for LoadForward.  This isolates the
        // forward-pass cost from the fold-pass cost.
        let mut p = OptimizerPipeline::new();
        p.add(ConstantFold);
        let entry = fg.entry().unwrap();
        p.run(&mut fg, entry).unwrap();
        fg
    }

    /// Build a function with `n` if-else diamonds chained sequentially.
    /// Each diamond merges back to the same control-state before the
    /// next branches; the merge varphi count grows linearly in `n`.
    /// Used to bench scaling of the validator + optimiser loop on
    /// merge-heavy IRs.
    pub fn run_diamond_cfg(n: usize) -> strider_ir::Function {
        let sp = stack_vn();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .callee_saved(sp)
            .build_fn()
            .unwrap();
        let entry = b.create_region().unwrap();
        b.set_entry_region(entry).unwrap();

        // Build N diamonds.  prev_region is the predecessor for the
        // next branch; on each iteration we create true / false / merge
        // sub-regions and route prev → If(true) / If(false) → merge,
        // then set prev = merge for the next iteration.
        let mut prev_region = entry;
        for _ in 0..n {
            let true_arm = b.create_region().unwrap();
            let false_arm = b.create_region().unwrap();
            let merge = b.create_region().unwrap();

            b.set_region(prev_region);
            let cond = b.build_boolean_const(true);
            b.build_if(cond, true_arm, false_arm).unwrap();

            // Each arm reads SP, adds a unique offset constant, and
            // branches to the merge.  This keeps both arms data-
            // distinct so `RedundantPhis` doesn't collapse the merge.
            b.set_region(true_arm);
            let sp_t = b.read_variable(&sp).unwrap();
            let off_t = b.build_int_const(0xa_au64, NodeOutputType::U64).unwrap();
            let _ = b
                .build_int_binary_operation(sp_t, off_t, IntBinaryOp::Add, NodeOutputType::U64)
                .unwrap();
            b.build_branch(merge).unwrap();

            b.set_region(false_arm);
            let sp_f = b.read_variable(&sp).unwrap();
            let off_f = b.build_int_const(0xb_bu64, NodeOutputType::U64).unwrap();
            let _ = b
                .build_int_binary_operation(sp_f, off_f, IntBinaryOp::Add, NodeOutputType::U64)
                .unwrap();
            b.build_branch(merge).unwrap();

            prev_region = merge;
        }

        // Final region: a clean Return.
        b.set_region(prev_region);
        let sp_final = b.read_variable(&sp).unwrap();
        b.build_return(Some(sp_final), &[]).unwrap();
        let mut fg = b.build().unwrap();
        // Run a small pipeline on the diamond graph.  Bench measures
        // build + pipeline together — the build dominates, but the
        // pipeline run pins the validator's per-region cost too.
        let mut p = OptimizerPipeline::new();
        p.add(ConstantFold);
        p.add(RedundantPhis);
        let entry = fg.entry().unwrap();
        p.run(&mut fg, entry).unwrap();
        fg
    }

    /// Build a function shaped like a stack-array indirect-branch
    /// dispatch with `n` targets (constants stored at contiguous
    /// SP-relative offsets, loaded via `arg & (n-1) * stride`).  `n`
    /// must be a power of 2.  The bench measures the full lift +
    /// stable-subset cost; the indirect-branch resolver isn't run
    /// here — callers can layer it on top if they want the resolve cost.
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
            let off_const = b.build_int_const(off, NodeOutputType::U64).unwrap();
            let addr = b
                .build_int_binary_operation(sp_val, off_const, IntBinaryOp::Add, NodeOutputType::U64)
                .unwrap();
            let target = b.build_int_const(0x4000 + i as u64, NodeOutputType::U64).unwrap();
            b.build_store(addr, target, rsleigh::VnSpace::RAM).unwrap();
        }
        let arg_val = b.read_variable(&arg_vn).unwrap();
        let arg_u32 = b.function_mut().create_node(
            strider_ir::node::NodeKind::Truncate,
            [arg_val],
            [NodeOutputKind::OutputType(NodeOutputType::U32)],
        );
        let arg_u32_out = b.function().node_outputs_exact::<1>(arg_u32).unwrap()[0];
        let mask_c = b.build_int_const(mask, NodeOutputType::U32).unwrap();
        let masked = b
            .build_int_binary_operation(arg_u32_out, mask_c, IntBinaryOp::And, NodeOutputType::U32)
            .unwrap();
        let idx_u64 = b.function_mut().create_node(
            strider_ir::node::NodeKind::Extend(strider_ir::ExtendOp::ZeroExtend),
            [masked],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        let idx_u64_out = b.function().node_outputs_exact::<1>(idx_u64).unwrap()[0];
        let stride = b.build_int_const(8u64, NodeOutputType::U64).unwrap();
        let idx_scaled = b
            .build_int_binary_operation(idx_u64_out, stride, IntBinaryOp::Mul, NodeOutputType::U64)
            .unwrap();
        let base = b.build_int_const(0u64, NodeOutputType::U64).unwrap();
        let sp_plus_base = b
            .build_int_binary_operation(sp_val, base, IntBinaryOp::Add, NodeOutputType::U64)
            .unwrap();
        let load_addr = b
            .build_int_binary_operation(sp_plus_base, idx_scaled, IntBinaryOp::Add, NodeOutputType::U64)
            .unwrap();
        let loaded = b
            .build_load(load_addr, rsleigh::VnSpace::RAM, NodeOutputType::U64)
            .unwrap();
        b.build_return(Some(loaded), &[]).unwrap();
        let mut fg = b.build().unwrap();
        let mut p = OptimizerPipeline::new();
        p.add(ConstantFold);
        let entry = fg.entry().unwrap();
        p.run(&mut fg, entry).unwrap();
        fg
    }

    /// Build a function with `n` distinct `IntConst` nodes added
    /// together.  Used to bench pattern-matcher cross-product joins
    /// (`find_all_requirements`) with shared captures.
    pub fn build_many_int_consts(n: usize) -> strider_ir::Function {
        let mut b = FunctionBuilder::empty().unwrap();
        let region = b.create_region().unwrap();
        b.set_entry_region(region).unwrap();
        b.set_region(region);
        // N consts → N adds → return.  Each `IntConst(i)` is a
        // distinct cache key (they hash on value), so we get N distinct
        // root nodes for the matcher to walk.
        let mut acc = b.build_int_const(0u64, NodeOutputType::U64).unwrap();
        for i in 1..=n {
            let c = b.build_int_const(i as u64, NodeOutputType::U64).unwrap();
            acc = b
                .build_int_binary_operation(acc, c, IntBinaryOp::Add, NodeOutputType::U64)
                .unwrap();
        }
        b.build_return(Some(acc), &[]).unwrap();
        b.build().unwrap()
    }
}

fn bench_stack_store_chain(c: &mut Criterion) {
    let mut group = c.benchmark_group("synthetic/stack_store_chain");
    let sp = synthetic::stack_vn();
    for n in [100usize, 500, 1_000] {
        group.bench_function(format!("n_{n}"), |b| {
            b.iter_batched(
                || synthetic::build_stack_store_chain(n),
                |mut fg| {
                    let pass = LoadForward::new(sp, strider_target::Endianness::Little);
                    let entry = fg.entry().unwrap();
                    let _ = pass.optimize(&mut fg, entry);
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

fn bench_find_all_requirements_shared_capture(c: &mut Criterion) {
    use strider_analyze::pattern::{Capture, Matcher, add, any_int_const, var};

    let mut group = c.benchmark_group("synthetic/find_all_requirements_shared");
    for n in [100usize, 500, 1_000] {
        let fg = synthetic::build_many_int_consts(n);
        // Two patterns that share a capture `x`:
        //   pat1: add(_, x).capture(y)  (x = rhs of every Add)
        //   pat2: any_int_const(x)      (x = every IntConst)
        // The cross-product join over shared `x` exercises the
        // matcher's bindings-equality path on every (Add, IntConst)
        // pair where they coincide.
        let x = Capture::new();
        let pat1: strider_analyze::pattern::Pat = add(strider_analyze::pattern::any(), var(x)).into();
        let pat2: strider_analyze::pattern::Pat = any_int_const(x);
        group.bench_function(format!("n_{n}"), |bnch| {
            bnch.iter(|| {
                let m = Matcher::try_new(&fg).expect("bench fixture is built");
                let pat_refs: Vec<&strider_analyze::pattern::Pat> = vec![&pat1, &pat2];
                let result = m.find_all_requirements(&pat_refs);
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
    bench_find_all_requirements_shared_capture,
);
criterion_main!(benches);
