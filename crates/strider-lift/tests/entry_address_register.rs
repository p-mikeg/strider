//! The ABI's entry-address register is the function's own entry address on
//! entry to it, so the prologue that turns it into a TOC or GOT pointer folds.

use strider_ir::{IRViewer, IRWalker};
use strider_lift::LiftOptions;

/// Whether the lift read `reg` as a runtime input, and whether the entry
/// address reached the graph as a literal.
fn reads_reg_and_entry_const(
    arch: strider_target::SleighArch,
    cc: strider_target::CallingConvention,
    reg: &str,
    bytes: Vec<u8>,
) -> (bool, bool) {
    let sleigh = rsleigh::Sleigh::new(
        arch.sla_spec(),
        arch.pspec(),
        rsleigh::mem_readers::BufMemReader::new(bytes, 0x1000),
    )
    .expect("sleigh");
    let mut lifter = strider_lift::lift::Lifter::new(arch, sleigh).expect("lifter");
    let reg_vn = lifter.sleigh_regs().name_to_vn(reg).expect("the register");
    let cc = cc.build(lifter.sleigh_regs()).expect("cc");
    let opts = LiftOptions::default();
    let cfg = lifter
        .build_cfg(0x1000u64.into(), &opts.cfg, opts.per_address_ccs)
        .expect("cfg");
    let f = lifter.build_ir_with(&cfg, cc, &opts).expect("ir").function;
    let initial = f
        .walk_kind(|k| matches!(k, strider_ir::node::NodeKind::InitialVar(_)))
        .any(|node| match f.node_kind(node) {
            strider_ir::node::NodeKind::InitialVar(id) => f.initial_vn(*id) == reg_vn,
            _ => false,
        });
    let literal = f
        .walk_kind(|k| matches!(k, strider_ir::node::NodeKind::IntConst(_)))
        .filter_map(|node| f.node_outputs(node).first().copied())
        .any(|v| f.int_const_u128(v) == Some(0x1000));
    (initial, literal)
}

/// `mr r3,r12; blr` under ELFv2, which guarantees r12 holds the global entry
/// address (2.2.2.1).
#[test]
fn elfv2_binds_r12_to_the_entry_address() {
    let (initial, literal) = reads_reg_and_entry_const(
        strider_target::SleighArch::ppc64le(),
        strider_target::CallingConvention::powerpc64_elf_v2(),
        "r12",
        vec![0x78, 0x63, 0x83, 0x7d, 0x20, 0x00, 0x80, 0x4e],
    );
    assert!(!initial, "r12 must not be read as a runtime input");
    assert!(literal, "r12's value must be the entry address literal");
}

/// ELFv1 makes no r12 promise: its analogue is the descriptor's TOC word.
#[test]
fn elfv1_leaves_r12_a_runtime_input() {
    let (initial, literal) = reads_reg_and_entry_const(
        strider_target::SleighArch::ppc64be(),
        strider_target::CallingConvention::powerpc64_elf_v1(),
        "r12",
        vec![0x7d, 0x83, 0x63, 0x78, 0x4e, 0x80, 0x00, 0x20],
    );
    assert!(initial, "r12 must stay a runtime input under ELFv1");
    assert!(!literal, "nothing binds the entry address under ELFv1");
}

/// MIPS abicalls: `jalr t9` leaves the callee's own address in t9, which the
/// prologue adds to a link-time constant to form `gp`.
#[test]
fn mips_binds_t9_to_the_entry_address() {
    // 1000: 0320 1025    or v0,t9,zero   (`move v0,t9`)
    // 1004: 03e0 0008    jr ra
    // 1008: 0000 0000    nop             (delay slot)
    let (initial, literal) = reads_reg_and_entry_const(
        strider_target::SleighArch::mipsbe64(),
        strider_target::CallingConvention::mips_n64(),
        "t9",
        vec![
            0x03, 0x20, 0x10, 0x25, 0x03, 0xe0, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00,
        ],
    );
    assert!(!initial, "t9 must not be read as a runtime input");
    assert!(literal, "t9's value must be the entry address literal");
}
