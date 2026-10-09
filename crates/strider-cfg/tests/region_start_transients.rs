//! A `noflow` context var a `globalset` committed at an address is part of
//! that address's decode: ARM's `LRset`, which `mov lr, pc` commits at
//! `inst_next`, turns the `bx rN` there into a CALL rather than a jump. A
//! region STARTING at that address must decode it the same way a sequential
//! decode does.

use rsleigh::mem_readers::BufMemReader;
use strider_cfg::{Builder, Cfg, CfgOptions, FlowVars};
use strider_target::SleighArch;

const BASE: u64 = 0x1000;

fn build_arm(words: &[u32]) -> Cfg {
    let arch = SleighArch::arm();
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    let size = bytes.len() as u64;
    let mut sleigh = rsleigh::Sleigh::new(
        arch.sla_spec(),
        arch.pspec(),
        BufMemReader::new(bytes, BASE),
    )
    .expect("create Sleigh");
    let flow = FlowVars::discover(&sleigh).expect("flow vars");
    let transients: Vec<(&'static str, u32)> = arch
        .transient_decode_vars()
        .iter()
        .map(|name| (*name, sleigh.get_context_at(0, name).expect("default")))
        .collect();
    let mode = flow.snapshot(&sleigh, BASE);
    let opts = CfgOptions {
        fn_max_size: Some(size),
        ..CfgOptions::default()
    };
    Builder::for_arch(&arch, &mut sleigh, BASE, &opts)
        .with_flow_context(&flow, mode)
        .with_transient_defaults(&transients)
        .build()
        .expect("build")
}

/// Every p-code opcode any region decoded at `addr`.
fn opcodes_at(cfg: &Cfg, addr: u64) -> Vec<rsleigh::Opcode> {
    cfg.regions()
        .flat_map(|r| r.insns.iter())
        .filter(|i| i.addr.machine_addr.addr == addr)
        .map(|i| i.insn.opcode)
        .collect()
}

const ARM_MOV_LR_PC: u32 = 0xe1a0_e00f;
const ARM_MOVEQ_LR_PC: u32 = 0x01a0_e00f;
const ARM_BXEQ_R3: u32 = 0x012f_ff13;
const ARM_BX_LR: u32 = 0xe12f_ff1e;

/// gcc `-march=armv4t` emits this for a conditional indirect call: the
/// `bxCC`'s own CBRANCH starts a region mid-instruction, which re-decodes it.
#[test]
fn a_conditional_indirect_call_stays_a_call_across_the_region_it_splits() {
    let cfg = build_arm(&[ARM_MOV_LR_PC, ARM_BXEQ_R3, ARM_BX_LR]);
    let ops = opcodes_at(&cfg, 0x1004);
    assert!(
        ops.contains(&rsleigh::Opcode::CallIndirect),
        "the `bxeq` decodes as a call, as it does linearly: {ops:?}",
    );
    assert!(
        !ops.contains(&rsleigh::Opcode::BranchIndirect),
        "no region decoded the `bxeq` as a jump: {ops:?}",
    );
}

/// The same with the link register set conditionally, where the `movCC`'s own
/// CBRANCH makes the `bxCC` a branch target too.
#[test]
fn a_conditionally_set_link_register_still_makes_the_bx_a_call() {
    let cfg = build_arm(&[ARM_MOVEQ_LR_PC, ARM_BXEQ_R3, ARM_BX_LR]);
    let ops = opcodes_at(&cfg, 0x1004);
    assert!(
        ops.contains(&rsleigh::Opcode::CallIndirect),
        "the `bxeq` decodes as a call: {ops:?}",
    );
    assert!(
        !ops.contains(&rsleigh::Opcode::BranchIndirect),
        "no region decoded the `bxeq` as a jump: {ops:?}",
    );
}
