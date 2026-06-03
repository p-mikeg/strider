//! Probe: does `strider_orchestrator::run` (the orchestrator) resolve the
//! `indirect_branch_resolved` fixture end-to-end?
//!
//! The existing `indirect_branch.rs` test bypasses the orchestrator and
//! calls `analyze_cfg` + the classifier directly.  This file fills the
//! "Multiple-resolution → CFG-rebuild → Multiple-disappears" gap by
//! driving `strider_orchestrator::run` against the real ELF — the same path the
//! Python `strider.run(...)` binding takes.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

mod common;

use object::{Object, ObjectSymbol};

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

    let config = strider_orchestrator::RunConfig::new(
        sleigh_arch,
        arch.cc(),
        sleigh,
        addr.into(),
        strider_orchestrator::RunOptions::new()
            .rom(rom)
            .allow_code_before_start_addr(),
    )
    .expect("RunConfig::new");
    strider_orchestrator::run(config)
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

#[test]
fn orchestrator_resolves_switch_jump_table_x86() {
    let function = run_orchestrator_on(common::Arch::X86, "switch", "dispatch_value")
        .expect("orchestrator must converge on switch fixture");
    // The IR must have NO IndirectBranch placeholder remaining.
    let placeholders = function
        .walk()
        .filter(|nid| {
            matches!(
                function.node_kind(*nid),
                strider_ir::node::NodeKind::IndirectBranch
            )
        })
        .count();
    assert_eq!(
        placeholders, 0,
        "switch jump table must lower to switch edges"
    );
}
