//! Probe: does `strider_orchestrator::Strider::analyze` (the orchestrator) resolve the
//! `indirect_branch_resolved` fixture end-to-end?
//!
//! The existing `indirect_branch.rs` test bypasses the orchestrator and
//! calls `build_ir` + the classifier directly.  This file fills the
//! "Multiple-resolution → CFG-rebuild → Multiple-disappears" gap by
//! driving `strider_orchestrator::Strider::analyze` against the real ELF — the same path the
//! Python `strider.run(...)` binding takes.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

mod common;

use object::{Object, ObjectSymbol};
use strider_ir::{IRViewer, IRWalker};

fn run_orchestrator_on(
    arch: common::Arch,
    case: &str,
    fn_name: &str,
) -> anyhow::Result<strider_ir::Function> {
    let path = common::binary_path(arch, case);
    if !path.exists() {
        panic!("missing test binary {path:?}; run `make -C fixtures`");
    }
    let obj = strider_reader::load_elf(&path).expect("load_elf");
    let sleigh_arch = arch.sleigh();
    let mem = strider_reader::ElfFileMemReader::from_object(&obj).expect("mem reader");
    let sleigh = rsleigh::Sleigh::new(sleigh_arch.sla_spec(), sleigh_arch.pspec(), mem)
        .expect("real sleigh new");
    let raw_addr = obj.symbol_by_name(fn_name).expect("symbol").address();
    let addr = match arch {
        common::Arch::Arm | common::Arch::ArmThumb => raw_addr & !1u64,
        _ => raw_addr,
    };

    let rom: Box<dyn strider_orchestrator::opt::ReadOnlyMemory> =
        Box::new(strider_reader::ElfFileMemReader::from_object(&obj).expect("rom"));

    let regs = sleigh.regs().expect("regs");
    let cc = arch.cc().build(&regs).expect("build cc");
    let lift_opts = strider_orchestrator::LiftOptions {
        cfg: strider_cfg::CfgOptions {
            allow_code_before_start_addr: true,
            ..Default::default()
        },
        ..strider_orchestrator::LiftOptions::default()
    };
    let mut strider = strider_orchestrator::Strider::new(sleigh_arch, sleigh, Some(rom))
        .expect("Strider::new");
    strider
        .analyze(
            addr,
            &cc,
            &lift_opts,
            &strider_orchestrator::opt::OptOptions::default(),
            None,
        )
        .map(|r| r.function)
}

#[test]
fn orchestrator_resolves_indirect_branch_x86() {
    let function = run_orchestrator_on(
        common::Arch::X86,
        "indirect_branch",
        "indirect_branch_resolved",
    )
    .expect("orchestrator must converge");
    assert!(function.graph().all_node_ids().count() > 0);
}

/// Count live `IndirectBranch` placeholders in the converged IR.  Zero means
/// every indirect branch the function had was resolved to concrete edges.
fn count_indirect_branch_placeholders(function: &strider_ir::Function) -> usize {
    function
        .walk()
        .filter(|nid| {
            matches!(
                function.node_kind(*nid),
                strider_ir::node::NodeKind::IndirectBranch
            )
        })
        .count()
}

/// Count live `If` nodes — used to confirm a non-table switch lowered to a
/// real comparison chain rather than collapsing away.
fn count_if_nodes(function: &strider_ir::Function) -> usize {
    function
        .walk()
        .filter(|nid| matches!(function.node_kind(*nid), strider_ir::node::NodeKind::If))
        .count()
}

#[test]
fn orchestrator_resolves_switch_jump_table_x86() {
    let function = run_orchestrator_on(common::Arch::X86, "switch", "dispatch_value")
        .expect("orchestrator must converge on switch fixture");
    // The IR must have NO IndirectBranch placeholder remaining.
    assert_eq!(
        count_indirect_branch_placeholders(&function),
        0,
        "switch jump table must lower to switch edges"
    );
}

// ── sparse switch → comparison chain (no jump table) ────────────────────────
//
// `switch_sparse.c`'s labels are far apart, so the compiler emits a `cmp/je`
// chain instead of an indexed jump — the lifted IR never has an
// `IndirectBranch` at all.  This pins that a non-table switch flows through as
// ordinary `If` control flow.

#[test]
fn orchestrator_sparse_switch_is_if_chain_x64() {
    let function = run_orchestrator_on(common::Arch::X64, "switch_sparse", "sparse_dispatch")
        .expect("orchestrator must converge on sparse switch");
    assert_eq!(
        count_indirect_branch_placeholders(&function),
        0,
        "a sparse switch has no indirect branch to resolve",
    );
    assert!(
        count_if_nodes(&function) >= 2,
        "a sparse switch must lower to a real comparison chain of If nodes",
    );
}

// ── value_range-bounded jump tables ─────────────────────────────────────────
//
// `switch_value_range.c` has no explicit `& mask`, so the classifier cannot
// lean on `KnownBits` for the index bound (the way `switch.c` does) — it must
// walk the compiler's `cmp; ja` range-check `If` via `value_range`.
//
// The dense (`dispatch_unmasked`) shape resolves on both x86 and x64.  x64 only
// works because of two cooperating fixes: `CommonSubexpr` merges the duplicate
// `Truncate(rdi)` nodes a phi-collapse left behind (so the guard and the index
// share one node), and `value_range` propagates the bound through the
// `ZeroExtend(Truncate(..))` the lifter wraps the 64-bit index in.
//
// The offset-base (`dispatch_offset`) cases remain `#[ignore]`'d for a DIFFERENT
// gap: gcc lowers a dense switch whose cases start at a nonzero base into a
// COMPOUND range check — `If(Or(Less(k-N, count-1), Equal(k-last, 0)))` — i.e.
// "`k-N` is in the low range OR `k` is the last case".  CSE correctly unifies
// the index `k-N` with the value the `Less` guards, but `value_range`'s guard
// extraction does not decompose an `Or` of two comparisons, so the index stays
// unbounded.  Drop the `#[ignore]` once value_range understands a disjunctive
// range guard.

#[test]
fn orchestrator_resolves_unmasked_switch_via_value_range_x86() {
    let function = run_orchestrator_on(common::Arch::X86, "switch_value_range", "dispatch_unmasked")
        .expect("orchestrator must converge on unmasked switch");
    assert_eq!(
        count_indirect_branch_placeholders(&function),
        0,
        "unmasked jump table must resolve via the value_range If-bound",
    );
}

#[test]
fn orchestrator_resolves_unmasked_switch_via_value_range_x64() {
    let function = run_orchestrator_on(common::Arch::X64, "switch_value_range", "dispatch_unmasked")
        .expect("orchestrator must converge on unmasked switch");
    assert_eq!(
        count_indirect_branch_placeholders(&function),
        0,
        "x64 unmasked table resolves via CSE (dedup Truncate) + value_range ZeroExtend propagation",
    );
}

#[test]
#[ignore = "value_range gap: disjunctive guard If(Or(Less, Equal)) from an offset-base switch is not decomposed"]
fn orchestrator_resolves_offset_switch_via_value_range_x86() {
    let function = run_orchestrator_on(common::Arch::X86, "switch_value_range", "dispatch_offset")
        .expect("orchestrator must converge on offset switch");
    assert_eq!(
        count_indirect_branch_placeholders(&function),
        0,
        "offset jump table must resolve via the value_range If-bound",
    );
}

#[test]
#[ignore = "value_range gap: disjunctive guard If(Or(Less, Equal)) from an offset-base switch is not decomposed"]
fn orchestrator_resolves_offset_switch_via_value_range_x64() {
    let function = run_orchestrator_on(common::Arch::X64, "switch_value_range", "dispatch_offset")
        .expect("orchestrator must converge on offset switch");
    assert_eq!(
        count_indirect_branch_placeholders(&function),
        0,
        "x64 offset jump table must resolve via the value_range If-bound",
    );
}
