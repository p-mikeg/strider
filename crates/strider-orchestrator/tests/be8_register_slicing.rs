//! The Sleigh register space's byte order is fixed by the sla's compile-time
//! `ENDIAN`, not by the data endianness.  `arm_be_kernel` is BE8: a
//! little-endian sla (LE instruction encoding) paired with big-endian data,
//! matching GHIDRA's `ARM:LEBE:32`, so its register block took the
//! little-endian branch of `ARM.sinc`:
//!
//! ```text
//! @if ENDIAN == "little"
//!   define register offset=0x0300 size=8 [ d0 d1 ... d31 ];   # d0 = LOW half of q0
//! @else
//!   define register offset=0x0300 size=8 [ d31 d30 ... d0 ];  # reversed
//! ```
//!
//! `calculate_reg_shift_from_container` must therefore key the shift off the
//! sla's register endianness: keying it off `arch.endianness()` puts `s0` at
//! bit 32 of `d0` instead of bit 0, reading and writing the wrong half of
//! every VFP/NEON sub-register.  `arm()` and `arm_be_kernel()` load the SAME
//! sla, so the same bytes must slice registers identically; only the data byte
//! order differs.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

use rsleigh::mem_readers::BufMemReader;
use strider_cfg::{CfgOptions, MachineInsnAddr};
use strider_ir::IRViewer;
use strider_ir::node::{IntBinaryOp, NodeKind};
use strider_orchestrator::Lifter;
use strider_target::{CallingConvention, Endianness, SleighArch};

const BASE: u64 = 0x1000;

/// `s0` is the low quarter of `d0` under the LE register layout, so touching
/// both forces `d0` to be the tracked container and `s0` a sliced sub-register.
fn snippet_bytes() -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&0xEE30_0A00u32.to_le_bytes()); // vadd.f32 s0, s0, s0
    v.extend_from_slice(&0xEE30_0B00u32.to_le_bytes()); // vadd.f64 d0, d0, d0
    v.extend_from_slice(&0xE12F_FF1Eu32.to_le_bytes()); // bx lr
    v
}

fn shift_node_count(arch: SleighArch) -> usize {
    let reader = BufMemReader::new(snippet_bytes(), BASE);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("sleigh");
    let mut lifter = Lifter::new(arch, sleigh).expect("lifter");
    let cc = CallingConvention::arm_aapcs()
        .build(lifter.sleigh_regs())
        .expect("cc");
    let opts = CfgOptions {
        fn_max_size: Some(snippet_bytes().len() as u64),
        ..Default::default()
    };
    let cfg = lifter
        .build_cfg(MachineInsnAddr::from(BASE), &opts, &Default::default())
        .expect("cfg");
    let function = lifter.build_ir(&cfg, cc).expect("build_ir").function;
    function
        .graph()
        .all_node_ids()
        .filter(|&n| {
            matches!(
                function.node_kind(n),
                NodeKind::IntBinaryOp(IntBinaryOp::ShiftRight | IntBinaryOp::ShiftLeft)
            )
        })
        .count()
}

#[test]
fn be8_slices_sub_registers_like_its_little_endian_sla() {
    let le = shift_node_count(SleighArch::arm());
    let be8 = shift_node_count(SleighArch::arm_be_kernel());
    assert_eq!(
        be8, le,
        "arm_be_kernel loads the same sla as arm, so the same bytes must slice \
         registers identically; a differing shift count means the sub-register \
         is being read out of the wrong half of its container"
    );
}

/// The data byte order is unchanged: only the register space follows the sla.
#[test]
fn be8_keeps_big_endian_data() {
    assert_eq!(SleighArch::arm_be_kernel().endianness(), Endianness::Big);
    assert_eq!(
        SleighArch::arm_be_kernel().register_endianness(),
        Endianness::Little,
        "BE8 pairs a little-endian sla with big-endian data"
    );
    // A genuinely big-endian sla keeps both big.
    assert_eq!(SleighArch::arm_be().endianness(), Endianness::Big);
    assert_eq!(SleighArch::arm_be().register_endianness(), Endianness::Big);
    // Everywhere else the two agree.
    assert_eq!(
        SleighArch::x86_64().register_endianness(),
        SleighArch::x86_64().endianness()
    );
}
