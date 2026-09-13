//! Under `escape_analysis`, a spilled local survives a call unless a register
//! the callee can read holds its address.  A register the sla keeps for its own
//! semantics (ARM's `mult_addr`, which every `push` leaves pointing into the
//! frame) is not one a callee reads.

use strider_ir::IRViewer;
use strider_orchestrator::LiftOptions;
use strider_orchestrator::opt::OptOptions;

mod common;

const BASE: u64 = 0x1000;
/// The callee, `bx lr` / `ret`, 0x1000 past the function.
const CALLEE: u64 = 0x1000;

/// The first returned value, folded to a constant, or `None` when it stays a
/// load.
fn returned_constant(arch: common::Arch, words: &[u32], callee_ret: u32) -> Option<u128> {
    let mut image = words.to_vec();
    image.resize(CALLEE as usize / 4, 0);
    image.push(callee_ret);
    let bytes = common::words_image(&image, strider_target::Endianness::Little);
    let (mut strider, cc) = common::strider_over_bytes(arch, bytes, BASE, None);
    let mut opts = OptOptions::default();
    opts.assumptions.escape_analysis = true;
    let r = strider
        .analyze(BASE, &cc, &LiftOptions::default(), &opts, None)
        .expect("analyze");
    let f = &r.function;
    f.int_const_u128(common::returned(f)[0])
}

/// `push {r4, lr}; sub sp, #8; mov r3, #11; str r3, [sp, #4]; <held>;
/// bl g; ldr r0, [sp, #4]; add sp, #8; pop {r4, pc}`.
fn arm(held: u32) -> Option<u128> {
    let words = [
        0xe92d_4010,
        0xe24d_d008,
        0xe3a0_300b,
        0xe58d_3004,
        held,
        0xeb00_03f9,
        0xe59d_0004,
        0xe28d_d008,
        0xe8bd_8010,
    ];
    returned_constant(common::Arch::Arm, &words, 0xe12f_ff1e)
}

/// `stp x29, x30, [sp, #-32]!; mov w1, #11; str w1, [sp, #28]; <held>; bl g;
/// ldr w0, [sp, #28]; ldp x29, x30, [sp], #32; ret`.
fn aarch64(held: u32) -> Option<u128> {
    let words = [
        0xa9be_7bfd,
        0x5280_0161,
        0xb900_1fe1,
        held,
        0x9400_03fc,
        0xb940_1fe0,
        0xa8c2_7bfd,
        0xd65f_03c0,
    ];
    returned_constant(common::Arch::Aarch64, &words, 0xd65f_03c0)
}

#[test]
fn arm_spill_forwards_across_a_call_after_a_push() {
    // mov ip, #4
    assert_eq!(arm(0xe3a0_c004), Some(11));
}

#[test]
fn arm_spill_stays_when_ip_holds_its_address() {
    // add ip, sp, #4
    assert_eq!(arm(0xe28d_c004), None);
}

#[test]
fn aarch64_spill_forwards_across_a_call() {
    // mov x8, #28
    assert_eq!(aarch64(0xd280_0388), Some(11));
}

#[test]
fn aarch64_spill_stays_when_x8_holds_its_address() {
    // add x8, sp, #28
    assert_eq!(aarch64(0x9100_73e8), None);
}
