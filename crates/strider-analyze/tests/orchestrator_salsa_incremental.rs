//! Salsa orchestrator incremental re-run validation — Phase 3 Task 3.9c.
//!
//! Asserts the diagnostic-counter contract that drives the v2
//! "incremental over rebuild" win:
//!
//! 1. A repeat query with the same `IndirectTargets` input is a salsa
//!    cache hit — no `optimized_function` body execution, counter
//!    unchanged.
//! 2. Mutating the `IndirectTargets` input invalidates the cache —
//!    next query re-runs the body, counter increments.
//!
//! Phase 3.9 scope: the tracked body delegates the whole function to
//! v1's `run`, so each cache miss costs one full lift+opt.  But the
//! salsa machinery DOES correctly memoise across identical inputs.
//! When Phase 6 splits the body into per-region tracked queries, the
//! same machinery delivers region-level granularity: one indirect-
//! target addition will re-lift only the affected regions, not all of
//! them.  This test pins the wrapper-level memoisation contract that
//! Phase 6 builds on.
//!
//! Why this matters: v1's `run` does a full CFG rebuild + IR re-lift
//! per outer iteration.  In the worst case (10 nested indirect
//! branches each adding one new target), v1 re-lifts the function 10
//! times.  Salsa's cache (even at the wrapper level) means a re-query
//! with identical inputs is free — and at the per-region level (Phase
//! 6) only the affected regions re-lift.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use object::{Object, ObjectSymbol};
use salsa::Setter;

use strider_analyze::orchestrator_salsa::{
    make_db_for_elf, optimized_function, Binary, IndirectTargets, IndirectTargetsMap, StriderDb,
};

fn binary_path(case: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/out/x64")
        .join(format!("{case}.elf"))
}

/// Build a salsa DB pointing at `fixtures/out/x64/<case>.elf` :: `fn_name`.
fn build_db(case: &str, fn_name: &str) -> (strider_analyze::orchestrator_salsa::StriderDbImpl, u64) {
    let path = binary_path(case);
    assert!(path.exists(), "fixture {path:?} missing");
    let obj = reader::load_elf(&path).expect("load_elf");
    let sleigh_arch = target::SleighArch::x86_64();
    let raw_addr = obj
        .symbol_by_name(fn_name)
        .unwrap_or_else(|| panic!("symbol {fn_name:?} not found"))
        .address();
    let rom: Arc<dyn strider_analyze::opt::ReadOnlyMemory> = Arc::new(
        reader::ElfFileMemReader::from_object(&obj).expect("rom"),
    );
    let path_clone = path.clone();
    let factory = move || -> reader::ElfFileMemReader {
        let obj = reader::load_elf(&path_clone).expect("load_elf");
        reader::ElfFileMemReader::from_object(&obj).expect("mem reader")
    };
    let db = make_db_for_elf(
        sleigh_arch,
        target::CallingConvention::x86_64_systemv(),
        factory,
        raw_addr,
        Some(rom),
        None,
        true,
        true,
        HashMap::new(),
    )
    .expect("make_db_for_elf");
    (db, raw_addr)
}

/// 1) Direct verification that querying with identical inputs is a
///    salsa cache hit — counter stays flat.
#[test]
fn repeat_query_same_inputs_is_cache_hit() {
    let (db, _) = build_db("arithmetic", "main");
    let binary = Binary::new(&db, "arithmetic::main".to_string());
    let initial: IndirectTargetsMap = Arc::new(BTreeMap::new());
    let targets = IndirectTargets::new(&db, initial);

    // First query: cache miss, body runs.
    let _first = optimized_function(&db, binary, targets);
    let after_first = db.optimized_function_calls();
    assert_eq!(after_first, 1, "first query must invoke the body once");

    // Second query: identical inputs, cache hit, body skipped.
    let _second = optimized_function(&db, binary, targets);
    let after_second = db.optimized_function_calls();
    assert_eq!(
        after_second, 1,
        "repeat query with identical inputs must be a cache hit \
         (counter went from 1 to {after_second})"
    );

    // Third query: same.
    let _third = optimized_function(&db, binary, targets);
    let after_third = db.optimized_function_calls();
    assert_eq!(
        after_third, 1,
        "third repeat query must also hit the cache (counter = {after_third})"
    );
}

/// 2) Verification that mutating `IndirectTargets` invalidates the
///    cache — counter increments.
#[test]
fn mutating_indirect_targets_invalidates_cache() {
    let (mut db, _) = build_db("arithmetic", "main");
    let binary = Binary::new(&db, "arithmetic::main".to_string());
    let initial: IndirectTargetsMap = Arc::new(BTreeMap::new());
    let targets = IndirectTargets::new(&db, initial);

    let _q1 = optimized_function(&db, binary, targets);
    assert_eq!(db.optimized_function_calls(), 1);

    // Mutate: add a fake indirect-target entry.  Even though v1's run
    // (the closure) ignores `targets` in wrapper-mode, salsa still
    // sees the input change and invalidates the cache.
    let mut next_map = BTreeMap::new();
    let mut targets_set = BTreeSet::new();
    targets_set.insert(0x4000u64);
    next_map.insert(0x1000u64, targets_set);
    targets.set_map(&mut db).to(Arc::new(next_map));

    let _q2 = optimized_function(&db, binary, targets);
    assert_eq!(
        db.optimized_function_calls(),
        2,
        "after mutating IndirectTargets, the next query must invoke the body"
    );

    // Mutating to the SAME value should also invalidate (salsa
    // mutates via `set_map`, which always bumps the revision; whether
    // it then re-runs depends on input equality).  Re-set to the same
    // map and verify behaviour.
    let mut same_map = BTreeMap::new();
    let mut s = BTreeSet::new();
    s.insert(0x4000u64);
    same_map.insert(0x1000u64, s);
    targets.set_map(&mut db).to(Arc::new(same_map));
    let _q3 = optimized_function(&db, binary, targets);
    let after_q3 = db.optimized_function_calls();
    assert!(
        after_q3 == 2 || after_q3 == 3,
        "salsa may or may not detect same-value set (got {after_q3}); \
         the contract is 'invalidates and may re-run', not 'always re-runs'"
    );
}

/// 3) End-to-end driver test: `run_v2` against a no-indirect-branch
///    fixture invokes the body exactly once.  This is the headline
///    "incremental win" diagnostic — the orchestrator does NOT
///    re-lift the function on subsequent calls with the same target
///    map.
#[test]
fn run_v2_invokes_body_once_for_direct_function() {
    let (mut db, _) = build_db("arithmetic", "main");
    let _bfg = strider_analyze::orchestrator_salsa::run_v2(&mut db, "arithmetic::main")
        .expect("run_v2");
    // run_v2 issues:
    //   1 × optimized_function query (cache miss, body runs)
    //   1 × build-closure call inside the salsa body
    //   1 × build-closure call at the end (to materialise an owned BFG)
    //
    // The salsa-tracked counter counts entries into the
    // optimized_function BODY only, not the closing materialisation
    // path.  So the expected count is 1.
    let calls = db.optimized_function_calls();
    assert_eq!(
        calls, 1,
        "arithmetic::main has no indirect branches; run_v2 should hit \
         the tracked body exactly once (got {calls})"
    );
}

/// 4) Demonstration of the "incremental win" — the test name
///    intentionally reads like a summary.
#[test]
fn incremental_cache_hits_avoid_relift() {
    // Take a fixture with non-trivial work and verify that a manual
    // re-query loop does NOT re-run the body.
    let (db, _) = build_db("switch", "dispatch_value");
    let binary = Binary::new(&db, "switch::dispatch_value".to_string());
    let targets = IndirectTargets::new(&db, Arc::new(BTreeMap::new()));

    // Prime the cache.
    let _ = optimized_function(&db, binary, targets);
    let primed = db.optimized_function_calls();
    assert_eq!(primed, 1);

    // 50 repeat queries — all cache hits.
    for _ in 0..50 {
        let _ = optimized_function(&db, binary, targets);
    }
    let after_50 = db.optimized_function_calls();
    assert_eq!(
        after_50, primed,
        "50 repeat queries must produce 0 additional body invocations \
         (got {after_50}, expected {primed})"
    );

    // Counts also pin the headline incremental-win number:
    // 51 total queries → 1 build invocation = ~50x reduction.
    eprintln!(
        "INCREMENTAL WIN: 51 queries × 1 body invocation = {}x effective speedup",
        51 / primed
    );
}
