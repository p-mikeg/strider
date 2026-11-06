//! ARM NEON instructions whose register effects reach past their named
//! operands.

use strider_ir::node::NodeKind;
use strider_ir::{IRViewer, IRWalker};

fn lift(words: &[u32]) -> strider_ir::Function {
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    let mut lifter = strider_lift::Lifter::new(
        strider_target::SleighArch::arm(),
        rsleigh::mem_readers::BufMemReader::new(bytes, 0x1000),
    )
    .expect("lifter");
    let cc = strider_target::CallingConvention::arm_aapcs()
        .build(lifter.sleigh_regs())
        .expect("cc");
    let cfg = lifter
        .build_cfg(0x1000u64.into(), &strider_cfg::CfgOptions::default())
        .expect("cfg");
    lifter.build_ir(&cfg, cc).expect("ir").function
}

/// `vmrs` after `vqadd` reads the FPSCR the saturating op may have set, not
/// the value `vmsr` wrote before it.
#[test]
fn a_saturating_op_writes_fpscr() {
    let f = lift(&[
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
