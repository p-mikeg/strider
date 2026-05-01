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

use criterion::{Criterion, criterion_group, criterion_main};
use object::{Object, ObjectSymbol};

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

fn analyze_case(c: Case) -> ir::BuiltFunctionGraph {
    let path = binary_path(c.arch_name, c.case);
    let obj = reader::load_elf(&path).expect("load_elf");
    let sleigh_arch = match c.arch_name {
        "x86" => strider::SleighArch::x86(),
        "x64" => strider::SleighArch::x86_64(),
        _ => panic!("unsupported arch {}", c.arch_name),
    };
    let cc = match c.arch_name {
        "x86" => strider::CallingConvention::x86_cdecl(),
        "x64" => strider::CallingConvention::x86_64_systemv_abi(),
        // The earlier `match c.arch_name` guards this — we only
        // reach this point on supported arches.  Use `panic!`
        // (the bench's `clippy::panic` is allow-listed) rather
        // than `unreachable!` (not on the allow list).
        _ => panic!("unsupported arch {}", c.arch_name),
    };
    let probe = rsleigh::mem_readers::BufMemReader::new(vec![], 0);
    let regs = rsleigh::Sleigh::new(sleigh_arch.sla_spec, sleigh_arch.pspec, probe)
        .expect("probe sleigh")
        .regs()
        .expect("probe regs");
    let ana = strider::Strider::new(sleigh_arch, regs, cc).expect("Strider::new");
    let mem = reader::ElfFileMemReader::from_object(&obj).expect("mem reader");
    let sleigh = rsleigh::Sleigh::new(sleigh_arch.sla_spec, sleigh_arch.pspec, mem)
        .expect("real sleigh");
    let raw_addr = obj
        .symbol_by_name(c.fn_name)
        .unwrap_or_else(|| panic!("symbol {:?} not found in {path:?}", c.fn_name))
        .address();
    let addr = raw_addr;
    let rom_for_cfg: Arc<dyn opt::ReadOnlyMemory> = Arc::new(
        reader::ElfFileMemReader::from_object(&obj).expect("rom reader (cfg)"),
    );
    let mut cfg_opts_b = cfg::OptionsBuilder::new()
        .allow_code_before_start_addr()
        .set_read_only_memory(rom_for_cfg);
    if let Some(lr) = ana.calling_convention().link_register_vn {
        cfg_opts_b = cfg_opts_b.set_link_register(lr);
    }
    let cfg_opts = cfg_opts_b.build();
    let cfg = cfg::Builder::with_endianness(sleigh, addr, cfg_opts, sleigh_arch.endianness)
        .build()
        .expect("Cfg build");
    let mut graph = ana.analyze_cfg(&cfg).expect("analyze_cfg").graph;
    let rom_for_opt = reader::ElfFileMemReader::from_object(&obj).expect("rom reader (opt)");
    let mut p = ana.build_optimizer_pipeline();
    p.add(opt::LoadReadOnly(rom_for_opt));
    p.run(&mut graph.graph, graph.entry).expect("optimizer pipeline");
    graph
}

fn bench_pipeline(c: &mut Criterion) {
    let mut group = c.benchmark_group("strider/pipeline");
    group.sample_size(20); // pipeline runs are slow-ish; fewer samples
    for case in CASES {
        let id = format!("{}/{}::{}", case.arch_name, case.case, case.fn_name);
        group.bench_function(&id, |b| {
            b.iter(|| {
                let g = analyze_case(*case);
                black_box(g);
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_pipeline);
criterion_main!(benches);
