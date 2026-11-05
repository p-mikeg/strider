//! The Sleigh register space's byte order is fixed by the sla's compile-time
//! `ENDIAN`, not by the data endianness. `arm_be_kernel` is BE8: a
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
//! So `Endianness::slice_shift_bits` keys off the sla's register endianness,
//! and `arm()` and `arm_be_kernel()` slice the same bytes identically.

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

/// The preserve masks every sub-register write builds, sorted.
///
/// This, not the shift count, is what the layout decides: `s0` sits at shift 0
/// under BOTH layouts, so `build_shift_by_const(0)` is elided and a shift-count
/// comparison holds trivially (measured: both sides produce none). The mask is
/// `!(0xffff_ffff << shift)`'s complement, so it moves the moment `s0` is read
/// out of the wrong half of `d0`.
fn preserve_masks(arch: SleighArch) -> Vec<u128> {
    let function = lift_snippet(arch);
    let mut masks: Vec<u128> = function
        .graph()
        .all_node_ids()
        .filter(|&n| {
            matches!(
                function.node_kind(n),
                NodeKind::IntBinaryOp(IntBinaryOp::And)
            )
        })
        .filter_map(|n| {
            let inputs = function.node_inputs(n);
            inputs.into_iter().find_map(|v| function.int_const_u128(v))
        })
        .collect();
    masks.sort_unstable();
    masks.dedup();
    masks
}

fn lift_snippet(arch: SleighArch) -> strider_ir::Function {
    let reader = BufMemReader::new(snippet_bytes(), BASE);
    let mut lifter = Lifter::new(arch, reader).expect("lifter");
    let cc = CallingConvention::arm_aapcs()
        .build(lifter.sleigh_regs())
        .expect("cc");
    let opts = CfgOptions {
        bound: strider_cfg::FunctionBound::size(snippet_bytes().len() as u64),
        ..Default::default()
    };
    let cfg = lifter
        .build_cfg(MachineInsnAddr::from(BASE), &opts)
        .expect("cfg");
    lifter.build_ir(&cfg, cc).expect("build_ir").function
}

#[test]
fn be8_slices_sub_registers_like_its_little_endian_sla() {
    let le = preserve_masks(SleighArch::arm());
    let be8 = preserve_masks(SleighArch::arm_be_kernel());
    assert_eq!(
        be8, le,
        "arm_be_kernel has arm's register layout, so the same bytes must slice \
         registers identically; a differing preserve mask means the \
         sub-register is being read out of the wrong half of its container"
    );
    assert!(
        le.contains(&0xffff_ffff_0000_0000u128),
        "the fixture must actually build an s0-in-d0 preserve mask, else the \
         comparison above holds vacuously; got {le:x?}"
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
