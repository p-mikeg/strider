//! The ABI's entry-address register is the function's own entry address on
//! entry to it, so the prologue that turns it into a TOC or GOT pointer folds.

use strider_ir::{IRViewer, IRWalker};

/// Whether the lift read `reg` as a runtime input, and whether the entry
/// address reached the graph as a literal.
fn reads_reg_and_entry_const(
    arch: strider_target::SleighArch,
    cc: strider_target::CallingConvention,
    reg: &str,
    bytes: Vec<u8>,
) -> (bool, bool) {
    let mut lifter =
        strider_lift::Lifter::new(arch, rsleigh::mem_readers::BufMemReader::new(bytes, 0x1000))
            .expect("lifter");
    let reg_vn = lifter.sleigh_regs().name_to_vn(reg).expect("the register");
    let cc = cc.build(lifter.sleigh_regs()).expect("cc");
    let cfg = lifter
        .build_cfg(0x1000u64.into(), &strider_cfg::CfgOptions::default())
        .expect("cfg");
    let f = lifter.build_ir(&cfg, cc).expect("ir").function;
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

/// At an entry that is also a loop header the entry address is the value on
/// the ENTRY edge: the loop reads t9's phi, whose entry operand is the
/// literal and whose back-edge operand is the incremented t9.
#[test]
fn a_loop_header_entry_binds_t9_on_the_entry_edge() {
    use strider_ir::node::{IntBinaryOp, NodeKind};
    // 1000: 6739 0001    daddiu t9,t9,1
    // 1004: 1720 fffe    bnez   t9,1000
    // 1008: 0000 0000    nop             (delay slot)
    // 100c: 03e0 0008    jr     ra
    // 1010: 0000 0000    nop             (delay slot)
    let bytes = vec![
        0x67, 0x39, 0x00, 0x01, 0x17, 0x20, 0xff, 0xfe, 0x00, 0x00, 0x00, 0x00, 0x03, 0xe0, 0x00,
        0x08, 0x00, 0x00, 0x00, 0x00,
    ];
    let mut lifter = strider_lift::Lifter::new(
        strider_target::SleighArch::mipsbe64(),
        rsleigh::mem_readers::BufMemReader::new(bytes, 0x1000),
    )
    .expect("lifter");
    let cc = strider_target::CallingConvention::mips_n64()
        .build(lifter.sleigh_regs())
        .expect("cc");
    let cfg = lifter
        .build_cfg(0x1000u64.into(), &strider_cfg::CfgOptions::default())
        .expect("cfg");
    let f = lifter.build_ir(&cfg, cc).expect("ir").function;
    let add = f
        .walk_kind(|k| matches!(k, NodeKind::IntBinaryOp(IntBinaryOp::Add)))
        .next()
        .expect("the daddiu");
    let [sum] = f.node_outputs(add)[..] else {
        panic!("one output")
    };
    let read = f
        .node_inputs(add)
        .into_iter()
        .find(|v| f.int_const_u128(*v) != Some(1))
        .expect("the t9 operand");
    let phi = f.producer(read);
    assert!(
        matches!(f.node_kind(phi), NodeKind::Phi),
        "the loop must read t9's phi, not {:?}",
        f.node_kind(phi)
    );
    let operands: Vec<_> = f.node_inputs(phi).into_iter().skip(1).collect();
    assert_eq!(operands.len(), 2, "entry edge and back edge");
    assert_eq!(f.int_const_u128(operands[0]), Some(0x1000), "entry operand");
    assert_eq!(operands[1], sum, "back-edge operand");
}

fn r2(regs: &rsleigh::SleighRegs) -> rsleigh::Vn {
    let vn = regs.name_to_vn("r2").expect("r2");
    assert_eq!(vn.addr_off, R2_OFFSET);
    vn
}

const R2_OFFSET: u64 = 0x10;

/// `mr r3,r2; blr` under ELFv1, lifted with `entry_values` binding `r2`.
fn lift_elfv1_mr_r3_r2(
    entry_values: impl FnOnce(&rsleigh::SleighRegs) -> Vec<(rsleigh::Vn, u64)>,
) -> anyhow::Result<strider_ir::Function> {
    let mut lifter = strider_lift::Lifter::new(
        strider_target::SleighArch::ppc64be(),
        rsleigh::mem_readers::BufMemReader::new(
            vec![0x7c, 0x43, 0x13, 0x78, 0x4e, 0x80, 0x00, 0x20],
            0x1000,
        ),
    )?;
    let cc = strider_target::CallingConvention::powerpc64_elf_v1().build(lifter.sleigh_regs())?;
    let values = entry_values(lifter.sleigh_regs());
    let cfg = lifter.build_cfg(0x1000u64.into(), &strider_cfg::CfgOptions::default())?;
    Ok(lifter
        .build_ir_with(
            &cfg,
            cc,
            &strider_cfg::CfgOptions::default(),
            &rustc_hash::FxHashMap::default(),
            &strider_ir::AssumptionOptions::none(),
            &values,
        )?
        .function)
}

/// The ELFv1 TOC word of the descriptor the caller entered through is `r2`
/// on entry.
#[test]
fn an_entry_value_binds_its_register() {
    let f = lift_elfv1_mr_r3_r2(|regs| vec![(r2(regs), 0x1000_8000)]).expect("lift");
    let r2_initial = f
        .walk_kind(|k| matches!(k, strider_ir::node::NodeKind::InitialVar(_)))
        .any(|node| match f.node_kind(node) {
            strider_ir::node::NodeKind::InitialVar(id) => f.initial_vn(*id).addr_off == R2_OFFSET,
            _ => false,
        });
    assert!(!r2_initial, "r2 must not be read as a runtime input");
    let literal = f
        .walk_kind(|k| matches!(k, strider_ir::node::NodeKind::IntConst(_)))
        .filter_map(|node| f.node_outputs(node).first().copied())
        .any(|v| f.int_const_u128(v) == Some(0x1000_8000));
    assert!(literal, "r2's value must be the TOC literal");
}

#[test]
fn an_entry_value_must_fit_a_register() {
    // The low word of big-endian `r2`.
    let r2_low = |regs: &rsleigh::SleighRegs| rsleigh::Vn {
        addr_off: r2(regs).addr_off + 4,
        size: 4,
        ..r2(regs)
    };
    let wide = lift_elfv1_mr_r3_r2(|regs| vec![(r2_low(regs), 1 << 32)])
        .err()
        .expect("too wide");
    assert!(
        wide.to_string().contains("not a register it fits"),
        "{wide}"
    );
    let fits = lift_elfv1_mr_r3_r2(|regs| vec![(r2_low(regs), 0xffff_ffff)]);
    assert!(fits.is_ok(), "{:?}", fits.err());
    let ram = rsleigh::Vn {
        addr_space: rsleigh::VnSpace::RAM,
        addr_off: 0x2000,
        size: 8,
    };
    let in_ram = lift_elfv1_mr_r3_r2(|_| vec![(ram, 0)])
        .err()
        .expect("not a register");
    assert!(
        in_ram.to_string().contains("not a register it fits"),
        "{in_ram}"
    );
}
