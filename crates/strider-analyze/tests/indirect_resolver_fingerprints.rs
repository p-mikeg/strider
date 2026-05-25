//! Layer-C asm-fingerprint invariant for the indirect-resolver mini-IR.
//!
//! The indirect-resolver in
//! `crates/strider-analyze/src/indirect_resolver.rs` builds a per-site
//! mini-IR via `strider_ir::FunctionBuilder::new_raw` and lifts the
//! region's value-producing pcode insns through
//! `strider_lift::pcode_lift::ValueLifter::lift`.  Every IR node born
//! from a pcode insn MUST carry its parent machine instruction's
//! address as an asm-fingerprint contributor (CLAUDE.md "Asm-fingerprint
//! side-table" contract: lifted, non-exempt nodes must have ≥1
//! fingerprint).
//!
//! The contract is checked unconditionally by `validate`.  This file pins
//! the mini-IR to that contract: a regression that strips `set_lift_addr`
//! from the resolver's lift loop surfaces here.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use strider_lift::cfg::{MachineInsnAddr, PcodeInsnAddr, RegionInstruction};
use rsleigh::mem_readers::BufMemReader;
use rsleigh::{Insn, Opcode, Vn, VnSpace};
use strider_analyze::indirect_resolver::build_resolver_mini_graph;

type TestReader = BufMemReader<Vec<u8>>;

fn make_x86_sleigh() -> rsleigh::Sleigh<TestReader> {
    let reader = BufMemReader::new(Vec::<u8>::new(), 0x0);
    rsleigh::Sleigh::new(
        rsleigh::sla_spec::SLA_SPEC_X86,
        rsleigh::pspec::PSPEC_X86,
        reader,
    )
    .expect("create x86 Sleigh")
}

fn reg4(off: u64) -> Vn {
    Vn { size: 4, addr_off: off, addr_space: VnSpace::REGISTER }
}

fn const_vn(val: u64, size: u32) -> Vn {
    Vn { size, addr_off: val, addr_space: VnSpace::CONST }
}

fn ri(machine: u64, insn_index: u64, insn: Insn) -> RegionInstruction {
    RegionInstruction {
        addr: PcodeInsnAddr { machine_addr: MachineInsnAddr::from(machine), insn_index },
        insn,
    }
}

fn branch_indirect(target_vn: Vn) -> Insn {
    Insn {
        opcode: Opcode::BranchIndirect,
        output: None,
        inputs: vec![target_vn].into(),
    }
}

/// Every reachable non-exempt node in the resolver's mini-IR must
/// carry an asm-fingerprint contributor.
///
/// Sequence:
///   1. `Copy reg, 0xdeadbeef` at machine address 0x1000.
///   2. `BranchIndirect reg`   at machine address 0x1004.
///
/// The mini-IR builds an `IntConst(0xdeadbeef)` (from the `Copy`'s
/// CONST operand) and a `Return` anchoring the target's value.  Both
/// kinds are non-exempt under the contract.
#[test]
fn resolver_mini_ir_passes_graph_invariants_asm_fingerprint_check() {
    let sleigh = make_x86_sleigh();
    let target = reg4(0);
    let region = vec![
        ri(
            0x1000,
            0,
            Insn {
                opcode: Opcode::Copy,
                output: Some(target),
                inputs: vec![const_vn(0xdead_beef, 4)].into(),
            },
        ),
        ri(0x1004, 0, branch_indirect(target)),
    ];
    let fg = build_resolver_mini_graph(
        &region,
        target,
        &sleigh,
        None,
        strider_target::Endianness::Little,
    )
    .expect("build mini-graph");

    // The graph-invariants asm-fingerprint check is unconditional in
    // `validate`.  Every node born from a real pcode insn must carry a
    // non-empty contributor list naming the parent machine instruction.
    let result = strider_ir::validate::validate(&fg, fg.entry().unwrap());
    assert!(
        result.is_ok(),
        "mini-IR violates Layer-C asm-fingerprint invariant: {:?}",
        result.err(),
    );
}
