//! ARM NEON instructions whose register effects reach past their named
//! operands: a saturating op sets FPSCR.QC, and a `VLDn` de-interleave stores
//! to the REGISTER space at an address no constant fold resolves.

use strider_ir::node::NodeKind;
use strider_ir::{IRViewer, IRWalker};

fn lift(words: &[u32]) -> (strider_ir::Function, rsleigh::SleighRegs) {
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    let mut lifter = strider_lift::Lifter::new(
        strider_target::SleighArch::arm(),
        rsleigh::mem_readers::BufMemReader::new(bytes, 0x1000),
    )
    .expect("lifter");
    let regs = lifter.sleigh_regs().clone();
    let cc = strider_target::CallingConvention::arm_aapcs()
        .build(&regs)
        .expect("cc");
    let cfg = lifter
        .build_cfg(0x1000u64.into(), &strider_cfg::CfgOptions::default())
        .expect("cfg");
    (lifter.build_ir(&cfg, cc).expect("ir").function, regs)
}

/// `vmrs` after `vqadd` reads the FPSCR the saturating op may have set, not
/// the value `vmsr` wrote before it.
#[test]
fn a_saturating_op_writes_fpscr() {
    let (f, _) = lift(&[
        0xe3a0_1000, // mov      r1, #0
        0xeee1_1a10, // vmsr     fpscr, r1
        0xf201_0012, // vqadd.s8 d0, d1, d2
        0xeef1_0a10, // vmrs     r0, fpscr
        0xe12f_ff1e, // bx       lr
    ]);
    let ret = f
        .walk_kind(|k| matches!(k, NodeKind::Return))
        .next()
        .expect("a Return node");
    let r0 = f.nth_input(ret, 2).expect("r0");
    assert!(
        matches!(f.node_kind(f.producer(r0)), NodeKind::CallOther { .. }),
        "r0 is {:?}, not the SatQ's fpscr",
        f.node_kind(f.producer(r0)),
    );
}

/// `vld2.8` stores through a loop-carried REGISTER-space address, which can
/// reach only the NEON file: SP and LR keep their SSA values.
#[test]
fn an_opaque_neon_register_store_leaves_sp_and_lr() {
    let (f, regs) = lift(&[
        0xe92d_4010, // push   {r4, lr}
        0xe24d_d010, // sub    sp, sp, #16
        0xe58d_0004, // str    r0, [sp, #4]
        0xf421_080f, // vld2.8 {d0-d1}, [r1]
        0xe59d_0004, // ldr    r0, [sp, #4]
        0xe28d_d010, // add    sp, sp, #16
        0xe8bd_8010, // pop    {r4, pc}
    ]);
    let reloaded = |name: &str| {
        let vn = regs.name_to_vn(name).expect(name);
        f.graph().all_node_ids().any(|id| {
            matches!(f.node_kind(id), NodeKind::Load(sp) if *sp == rsleigh::VnSpace::REGISTER)
                && f.int_const_u128(f.load_addr(id)) == Some(u128::from(vn.addr_off))
        })
    };
    assert!(reloaded("d0"), "the store's reach is re-read");
    assert!(!reloaded("sp"), "sp re-read from the REGISTER space");
    assert!(!reloaded("lr"), "lr re-read from the REGISTER space");
}
