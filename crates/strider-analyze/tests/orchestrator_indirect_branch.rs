//! Probe: does `strider_analyze::run` (the orchestrator) resolve the
//! `indirect_branch_resolved` fixture end-to-end?
//!
//! The existing `indirect_branch.rs` test bypasses the orchestrator and
//! calls `analyze_cfg` + the classifier directly.  This file fills the
//! "Multiple-resolution → CFG-rebuild → Multiple-disappears" gap by
//! driving `strider_analyze::run` against the real ELF — the same path the
//! Python `strider.run(...)` binding takes.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used, clippy::unreachable)]

mod common;

use object::{Object, ObjectSymbol};
use std::sync::Arc;

fn run_orchestrator_on(arch: common::Arch, case: &str, fn_name: &str)
    -> anyhow::Result<strider_ir::BuiltFunctionGraph>
{
    let path = common::binary_path(arch, case);
    if !path.exists() {
        panic!("missing test binary {path:?}; run `make -C fixtures`");
    }
    let obj = reader::load_elf(&path).expect("load_elf");
    let sleigh_arch = arch.sleigh();
    let probe = rsleigh::mem_readers::BufMemReader::new(vec![], 0);
    let regs = rsleigh::Sleigh::new(sleigh_arch.sla_spec(), sleigh_arch.pspec(), probe)
        .expect("probe sleigh new")
        .regs()
        .expect("probe sleigh regs");
    let s = strider_analyze::Strider::new(sleigh_arch, regs, arch.cc()).expect("Strider::new");

    let mem = reader::ElfFileMemReader::from_object(&obj).expect("mem reader");
    let sleigh = rsleigh::Sleigh::new(sleigh_arch.sla_spec(), sleigh_arch.pspec(), mem)
        .expect("real sleigh new");
    let raw_addr = obj
        .symbol_by_name(fn_name)
        .expect("symbol")
        .address();
    let addr = match arch {
        common::Arch::Arm | common::Arch::ArmThumb => raw_addr & !1u64,
        _ => raw_addr,
    };

    let rom: Arc<dyn strider_analyze::opt::ReadOnlyMemory> = Arc::new(
        reader::ElfFileMemReader::from_object(&obj).expect("rom"),
    );

    let config = strider_analyze::Config {
        strider: &s,
        start_addr: addr.into(),
        sleigh,
        rom: Some(rom),
        fn_max_size: None,
        allow_code_before_start_addr: true,
        compact: true,
        per_address_ccs: std::collections::HashMap::new(),
    };
    strider_analyze::run(config)
}

#[test]
fn orchestrator_resolves_indirect_branch_x86() {
    let g = run_orchestrator_on(common::Arch::X86, "indirect_branch", "indirect_branch_resolved")
        .expect("orchestrator must converge");
    assert!(g.all_node_ids().count() > 0);
}

#[test]
fn orchestrator_resolves_switch_jump_table_x86() {
    let g = run_orchestrator_on(common::Arch::X86, "switch", "dispatch_value")
        .expect("orchestrator must converge on switch fixture");
    // The IR must have NO IndirectBranch placeholder remaining.
    let placeholders = g
        .preorder()
        .filter(|nid| matches!(g.node_kind(*nid), strider_ir::node::NodeKind::IndirectBranch))
        .count();
    assert_eq!(placeholders, 0, "switch jump table must lower to switch edges");
}
