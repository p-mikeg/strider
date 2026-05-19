//! V1 vs V2 orchestrator benchmark — Phase 6 Task 6.4.
//!
//! Measures three workload dimensions on the same fixture binary:
//!
//! 1. Single-function (cold)        — dominant per-function lift+optimize cost.
//! 2. Multi-function on same binary — measures whether v2 wins when many
//!    functions share the same `binary(path)` (ELF load + Sleigh probe).
//! 3. Repeat-query same function    — measures Salsa cache reuse: v1 does
//!    real work every iteration, v2 should serve from cache after the
//!    first call.
//!
//! Run via: `cargo bench --bench v1_vs_v2`.
//!
//! Originally this rewrite plan projected v2 to be ≥10× faster on
//! pattern-only workflows.  The reality is recorded in
//! `docs/superpowers/specs/2026-05-20-v1-vs-v2-benchmark.md`.

#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

use std::collections::HashMap;
use std::hint::black_box;
use std::path::PathBuf;
use std::sync::Arc;

use criterion::{Criterion, criterion_group, criterion_main};
use object::{Object, ObjectSymbol};

use strider_analyze::orchestrator_salsa::{make_db_for_elf, run_v2, StriderDbImpl};

// ── Fixture ──────────────────────────────────────────────────────────────────
//
// `control.elf` has many small-medium x64 functions with non-trivial
// control flow (loops, branches, nested control).  Functions all live in
// the same binary so v2's wrapper-level cache has an opportunity to
// help.

const ARCH_NAME: &str = "x64";
const CASE: &str = "control";

/// Functions to benchmark.  All exist in `control.elf` per `nm`.  The
/// list intentionally covers a range of sizes (`abs_val` is tiny,
/// `nested_loops` and `count_bits` are larger).
const FUNCTIONS: &[&str] = &[
    "abs_val",
    "max_val",
    "clamp",
    "select_three",
    "sum_to_n",
    "factorial",
    "count_bits",
    "nested_loops",
];

const SINGLE_FN: &str = "nested_loops";

fn binary_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/out")
        .join(ARCH_NAME)
        .join(format!("{CASE}.elf"))
}

// ── V1 path ──────────────────────────────────────────────────────────────────
//
// Mirrors `crates/strider-analyze/tests/orchestrator_salsa_parity.rs::run_v1`.
// Builds a fresh `RunConfig` per call so we measure the full cost
// of one function (ELF load + Sleigh probe + Strider::new + run).

fn run_v1_for_function(fn_name: &str) -> ir::BuiltFunctionGraph {
    let path = binary_path();
    let obj = reader::load_elf(&path).expect("load_elf");
    let sleigh_arch = target::SleighArch::x86_64();
    let probe = rsleigh::mem_readers::BufMemReader::new(vec![], 0);
    let regs = rsleigh::Sleigh::new(sleigh_arch.sla_spec(), sleigh_arch.pspec(), probe)
        .expect("probe sleigh new")
        .regs()
        .expect("probe sleigh regs");
    let strider = strider_analyze::Strider::new(
        sleigh_arch,
        regs,
        target::CallingConvention::x86_64_systemv(),
    )
    .expect("Strider::new");

    let mem = reader::ElfFileMemReader::from_object(&obj).expect("mem reader");
    let sleigh = rsleigh::Sleigh::new(sleigh_arch.sla_spec(), sleigh_arch.pspec(), mem)
        .expect("real sleigh new");
    let raw_addr = obj
        .symbol_by_name(fn_name)
        .unwrap_or_else(|| panic!("symbol {fn_name:?} not found"))
        .address();

    let rom: Arc<dyn strider_analyze::opt::ReadOnlyMemory> = Arc::new(
        reader::ElfFileMemReader::from_object(&obj).expect("rom"),
    );

    let config = strider_analyze::RunConfig {
        strider: &strider,
        start_addr: raw_addr.into(),
        sleigh,
        rom: Some(rom),
        fn_max_size: None,
        allow_code_before_start_addr: true,
        compact: true,
        per_address_ccs: HashMap::new(),
    };
    strider_analyze::run(config).expect("v1 run")
}

// ── V2 path (Salsa orchestrator) ─────────────────────────────────────────────

/// Builds a fresh `StriderDbImpl` for a given function.  Mirrors
/// `run_v2_for_fixture` in the parity test.
fn build_v2_db(fn_name: &str) -> (StriderDbImpl, String) {
    let path = binary_path();
    let obj = reader::load_elf(&path).expect("load_elf");
    let sleigh_arch = target::SleighArch::x86_64();
    let raw_addr = obj
        .symbol_by_name(fn_name)
        .unwrap_or_else(|| panic!("symbol {fn_name:?} not found"))
        .address();

    let rom: Arc<dyn strider_analyze::opt::ReadOnlyMemory> = Arc::new(
        reader::ElfFileMemReader::from_object(&obj).expect("rom"),
    );

    let path_for_closure = path.clone();
    let reader_factory = move || -> reader::ElfFileMemReader {
        let obj = reader::load_elf(&path_for_closure).expect("load_elf");
        reader::ElfFileMemReader::from_object(&obj).expect("mem reader")
    };

    let db = make_db_for_elf(
        sleigh_arch,
        target::CallingConvention::x86_64_systemv(),
        reader_factory,
        raw_addr,
        Some(rom),
        None,
        true,  // allow_code_before_start_addr
        true,  // compact
        HashMap::new(),
    )
    .expect("make_db_for_elf");

    let key = format!("{CASE}::{fn_name}");
    (db, key)
}

fn run_v2_for_function(fn_name: &str) -> ir::BuiltFunctionGraph {
    let (mut db, key) = build_v2_db(fn_name);
    run_v2(&mut db, &key).expect("v2 run")
}

// ── Pattern application ──────────────────────────────────────────────────────
//
// Mirrors the user-workflow tail: after analysis, scan for every
// `pattern::call()`.  Cheap relative to the lift, so the bench's cold
// path is dominated by analysis; we include the pattern step so the
// bench reflects the full user workflow.

fn count_call_matches(bfg: &ir::BuiltFunctionGraph) -> usize {
    use strider_analyze::pattern::{Matcher, call};
    let pat: strider_analyze::pattern::Pat = call().into();
    Matcher::new(bfg).find_all(&pat).len()
}

// ── Bench 1: Single-function (cold) ──────────────────────────────────────────

fn bench_single_function_v1(c: &mut Criterion) {
    let mut group = c.benchmark_group("v1_vs_v2/single_function");
    group.sample_size(20);
    group.bench_function("v1/nested_loops", |b| {
        b.iter(|| {
            let bfg = run_v1_for_function(SINGLE_FN);
            let n = count_call_matches(&bfg);
            black_box((bfg, n));
        });
    });
    group.bench_function("v2/nested_loops", |b| {
        b.iter(|| {
            let bfg = run_v2_for_function(SINGLE_FN);
            let n = count_call_matches(&bfg);
            black_box((bfg, n));
        });
    });
    group.finish();
}

// ── Bench 2: Multi-function on same binary ───────────────────────────────────
//
// V1: iterates `FUNCTIONS`, calls `run_v1_for_function` per name.
// V2: iterates `FUNCTIONS`, builds a fresh DB per name (mirrors the
// natural API).  Note: v2's current Salsa cache is per-`StriderDbImpl`
// (one DB per analysis), so this scenario does NOT share cache across
// functions.  We measure it anyway because it reflects the user
// workflow described in the task spec.

fn bench_multi_function_v1(c: &mut Criterion) {
    let mut group = c.benchmark_group("v1_vs_v2/multi_function");
    group.sample_size(10); // 8 functions × work-per-call → fewer samples
    group.bench_function("v1/all_functions", |b| {
        b.iter(|| {
            let mut total = 0usize;
            for &fn_name in FUNCTIONS {
                let bfg = run_v1_for_function(fn_name);
                total += count_call_matches(&bfg);
            }
            black_box(total);
        });
    });
    group.bench_function("v2/all_functions", |b| {
        b.iter(|| {
            let mut total = 0usize;
            for &fn_name in FUNCTIONS {
                let bfg = run_v2_for_function(fn_name);
                total += count_call_matches(&bfg);
            }
            black_box(total);
        });
    });
    group.finish();
}

// ── Bench 3: Repeat-query same function ──────────────────────────────────────
//
// V1: rebuilds RunConfig + runs orchestrator 10× — pays the full cost
// every time.
// V2: builds the DB ONCE, calls run_v2 10× — the second-onwards call
// should be a Salsa cache hit (wrapper-level memoisation per
// `(Binary, IndirectTargets)` pair).

const REPEAT_QUERIES: usize = 10;

fn bench_repeat_query_v1(c: &mut Criterion) {
    let mut group = c.benchmark_group("v1_vs_v2/repeat_query");
    group.sample_size(10);
    group.bench_function("v1/10x_nested_loops", |b| {
        b.iter(|| {
            let mut total = 0usize;
            for _ in 0..REPEAT_QUERIES {
                let bfg = run_v1_for_function(SINGLE_FN);
                total += count_call_matches(&bfg);
            }
            black_box(total);
        });
    });
    group.bench_function("v2/10x_nested_loops", |b| {
        // Build DB once per iteration outside the timed inner loop's
        // notion (Criterion measures the closure body).  We deliberately
        // include DB construction so v2's amortised cost over 10 queries
        // is what the user sees.
        b.iter(|| {
            let (mut db, key) = build_v2_db(SINGLE_FN);
            let mut total = 0usize;
            for _ in 0..REPEAT_QUERIES {
                let bfg = run_v2(&mut db, &key).expect("v2 run");
                total += count_call_matches(&bfg);
            }
            black_box(total);
        });
    });
    group.finish();
}

// ── Sanity check ─────────────────────────────────────────────────────────────
//
// Before measuring, confirm v1 and v2 produce structurally equivalent
// graphs (same node count, same call-pattern hit count).  This runs as
// a `bench_function` with a single sample so any divergence is visible
// immediately rather than being silently buried in the timing numbers.

fn bench_sanity_check_parity(c: &mut Criterion) {
    let mut group = c.benchmark_group("v1_vs_v2/sanity");
    group.sample_size(10);
    group.bench_function("v1_v2_parity_nested_loops", |b| {
        b.iter(|| {
            let v1 = run_v1_for_function(SINGLE_FN);
            let v2 = run_v2_for_function(SINGLE_FN);
            let n1 = v1.preorder().count();
            let n2 = v2.preorder().count();
            assert_eq!(n1, n2, "v1 vs v2 node count mismatch: {n1} != {n2}");
            let c1 = count_call_matches(&v1);
            let c2 = count_call_matches(&v2);
            assert_eq!(c1, c2, "v1 vs v2 call-match count mismatch: {c1} != {c2}");
            black_box((v1, v2));
        });
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_sanity_check_parity,
    bench_single_function_v1,
    bench_multi_function_v1,
    bench_repeat_query_v1,
);
criterion_main!(benches);
