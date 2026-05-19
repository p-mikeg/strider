//! Salsa orchestrator parity tests — Phase 3 Task 3.9b.
//!
//! Runs the v2 (salsa-driven) orchestrator and v1 (imperative)
//! orchestrator on the same fixtures and asserts structural parity on
//! the final `BuiltFunctionGraph`.
//!
//! "Structural parity" here means: same total reachable-node count,
//! same number of `IndirectBranch` placeholders remaining (typically
//! zero on a converged fixture), same number of `Return` nodes.  A
//! byte-for-byte IR comparison is too brittle — egg-vs-imperative
//! ordering deltas already exist in the codebase — but the wrapper-mode
//! salsa orchestrator delegates the entire inner loop to v1's `run`,
//! so the resulting BFG is in fact identical modulo arena layout.
//!
//! Fixtures (x64 only — non-trivial indirect branches are amply
//! present here, and the test stays fast):
//!   - `switch::dispatch_value` (jump-table)
//!   - `indirect_branch::indirect_branch_resolved` (computed goto)
//!   - `arithmetic::main` (no indirect branches — sanity check that
//!     the salsa wrapper terminates after one outer iteration on a
//!     fully-direct function).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use object::{Object, ObjectSymbol};

use strider_analyze::orchestrator_salsa::{make_db_for_elf, run_v2, StriderDb};

fn binary_path(arch_name: &str, case: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/out")
        .join(arch_name)
        .join(format!("{case}.elf"))
}

struct V1RunOutcome {
    nodes: usize,
    indirect_branches: usize,
    returns: usize,
}

fn count_kind<F>(g: &ir::BuiltFunctionGraph, pred: F) -> usize
where
    F: Fn(&ir::node::NodeKind) -> bool,
{
    g.preorder().filter(|nid| pred(g.graph.node_kind(*nid))).count()
}

fn summarise(bfg: &ir::BuiltFunctionGraph) -> V1RunOutcome {
    V1RunOutcome {
        nodes: bfg.preorder().count(),
        indirect_branches: count_kind(bfg, |k| matches!(k, ir::node::NodeKind::IndirectBranch)),
        returns: count_kind(bfg, |k| matches!(k, ir::node::NodeKind::Return)),
    }
}

/// Run v1 directly against a fixture.  Mirrors
/// `crates/strider/tests/orchestrator_indirect_branch.rs::run_orchestrator_on`
/// without the test-fixture dependency.
fn run_v1(case: &str, fn_name: &str) -> ir::BuiltFunctionGraph {
    let path = binary_path("x64", case);
    assert!(path.exists(), "fixture {path:?} missing; run `make -C fixtures`");
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
        .unwrap_or_else(|| panic!("symbol {fn_name:?} not found in {path:?}"))
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

/// Run the salsa-driven v2 orchestrator against the same fixture.
fn run_v2_for_fixture(case: &str, fn_name: &str) -> (ir::BuiltFunctionGraph, usize) {
    let path = binary_path("x64", case);
    assert!(path.exists(), "fixture {path:?} missing; run `make -C fixtures`");
    let obj_owned = reader::load_elf(&path).expect("load_elf");
    let sleigh_arch = target::SleighArch::x86_64();
    let raw_addr = obj_owned
        .symbol_by_name(fn_name)
        .unwrap_or_else(|| panic!("symbol {fn_name:?} not found in {path:?}"))
        .address();

    // ROM: build a single Arc<dyn ReadOnlyMemory> that lives across all
    // closure invocations.
    let rom: Arc<dyn strider_analyze::opt::ReadOnlyMemory> = Arc::new(
        reader::ElfFileMemReader::from_object(&obj_owned).expect("rom"),
    );

    // Reader factory: every salsa cache miss builds a fresh
    // ElfFileMemReader from the same loaded object.  The factory
    // captures the loaded bytes via the path; for tests we re-load the
    // ELF each call (cheap given Linux fs cache).
    let path_for_closure = path.clone();
    let reader_factory = move || -> reader::ElfFileMemReader {
        let obj = reader::load_elf(&path_for_closure).expect("load_elf");
        reader::ElfFileMemReader::from_object(&obj).expect("mem reader")
    };

    let mut db = make_db_for_elf(
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

    let key = format!("{case}::{fn_name}");
    let bfg = run_v2(&mut db, &key).expect("salsa v2 run");
    let invocations = db.optimized_function_calls();
    (bfg, invocations)
}

fn assert_parity(case: &str, fn_name: &str) {
    let v1 = run_v1(case, fn_name);
    let (v2, _calls) = run_v2_for_fixture(case, fn_name);
    let s1 = summarise(&v1);
    let s2 = summarise(&v2);
    assert_eq!(
        s1.nodes, s2.nodes,
        "{case}::{fn_name}: v1 nodes={} != v2 nodes={}",
        s1.nodes, s2.nodes
    );
    assert_eq!(
        s1.indirect_branches, s2.indirect_branches,
        "{case}::{fn_name}: indirect-branch counts differ"
    );
    assert_eq!(
        s1.returns, s2.returns,
        "{case}::{fn_name}: return counts differ"
    );
}

#[test]
fn parity_arithmetic_main_x64() {
    assert_parity("arithmetic", "main");
}

#[test]
fn parity_switch_dispatch_value_x64() {
    assert_parity("switch", "dispatch_value");
}

#[test]
fn parity_indirect_branch_resolved_x64() {
    assert_parity("indirect_branch", "indirect_branch_resolved");
}

/// Phase 3 Task 3.9c — sanity check on the invocation counter.  A
/// single `run_v2` call against a fully-resolved fixture should drive
/// exactly one `optimized_function` invocation.
#[test]
fn single_invocation_for_direct_function() {
    let (_bfg, calls) = run_v2_for_fixture("arithmetic", "main");
    assert_eq!(
        calls, 1,
        "arithmetic::main is fully direct; salsa should hit the tracked \
         body exactly once (got {calls})"
    );
}
