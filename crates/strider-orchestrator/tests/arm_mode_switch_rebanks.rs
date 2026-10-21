//! A processor-mode change re-banks `r13`/`r14`, `r8`-`r12` in and out of FIQ,
//! and the SPSR (Arm ARM DDI 0406C B1.3.2), so a value written before the
//! change is not the one read after it. `cps #mode` and every `msr` that
//! writes the CPSR's control field (mask bit 0, `CPSRWriteByInstr` writing
//! `M[4:0]`, B9.3.11 / B9.3.12) can change mode; an `msr` to the flags,
//! extension or status field alone, or to the SPSR, does not, and neither
//! does one that rewrites the mode the CPU is already in, which is how an
//! interrupt-mask restore reads (`mrs r2, cpsr; ...; msr cpsr_c, r2`).

mod common;

use common::Arch;
use strider_ir::node::{NodeKind, ValueId};
use strider_ir::{Function, IRViewer};

const BASE: u64 = 0x1000;

/// `mov r4, lr`, so the function returns through `bx r4` whatever the mode
/// switch does to `lr`.
const A32_SAVE_LR: u32 = 0xe1a0_400e;
/// `mov r8, #5`
const A32_R8_IS_5: u32 = 0xe3a0_8005;
/// `mov r0, r8; bx r4`
const A32_RETURN_R8: [u32; 2] = [0xe1a0_0008, 0xe12f_ff14];

fn analyze(arch: Arch, bytes: Vec<u8>, entry: u64) -> Function {
    let (mut strider, cc) = common::strider_over_bytes(arch, bytes, BASE, None);
    let result = strider
        .analyze(entry, &cc, &Default::default(), None)
        .expect("analyze");
    assert!(result.incomplete().is_empty(), "{:?}", result.incomplete());
    result.function
}

fn a32(words: &[u32]) -> Function {
    analyze(
        Arch::Arm,
        common::words_image(words, strider_target::Endianness::Little),
        BASE,
    )
}

fn thumb(halves: &[u16]) -> Function {
    let bytes = halves.iter().flat_map(|h| h.to_le_bytes()).collect();
    analyze(Arch::ArmThumb, bytes, BASE | 1)
}

/// A mode switch's output, or a phi taking one where the switch is
/// conditional on the mode changing.
fn rebanked(f: &Function, v: ValueId) -> bool {
    let producer = f.producer(v);
    match f.node_kind(producer) {
        NodeKind::CallOther { .. } => true,
        NodeKind::Phi => f
            .phi_data_inputs(producer)
            .any(|i| matches!(f.kind_of_value(i), NodeKind::CallOther { .. })),
        _ => false,
    }
}

/// What `r0` holds at the return: `Some(n)` for a constant, `None` for a
/// re-banked value, and a panic for anything else.
fn r0_at_return(f: &Function) -> Option<u128> {
    let r0 = common::returned(f)[0];
    if let Some(n) = f.int_const_u128(r0) {
        return Some(n);
    }
    assert!(
        rebanked(f, r0),
        "r0 is neither a constant nor re-banked: {:?}",
        f.kind_of_value(r0)
    );
    None
}

/// `mov r8, #5; msr cpsr_c, #0xd1` (FIQ) `; msr cpsr_c, #0xd3` (SVC) `; mov
/// r0, r8`: the 5 went to `r8_fiq`, and back in SVC `r8` is the entry value.
#[test]
fn msr_to_fiq_and_back_rebanks_r8() {
    let f = a32(&[
        A32_SAVE_LR,
        0xe321_f0d1,
        A32_R8_IS_5,
        0xe321_f0d3,
        A32_RETURN_R8[0],
        A32_RETURN_R8[1],
    ]);
    assert_eq!(r0_at_return(&f), None);
}

/// One `msr` between `mov r8, #5` and the read of `r8`.
#[test]
fn an_msr_writing_the_control_field_switches_mode() {
    let cases: [(&str, u32, bool); 10] = [
        ("msr cpsr_c, #0xd1", 0xe321_f0d1, true),
        ("msr cpsr_xc, #0xd1", 0xe323_f0d1, true),
        ("msr cpsr_fsxc, #0xd1", 0xe32f_f0d1, true),
        ("msr cpsr_c, r1", 0xe121_f001, true),
        ("msr cpsr_fsxc, r1", 0xe12f_f001, true),
        ("msr cpsr_f, #0xf0000000", 0xe328_f20f, false),
        ("msr cpsr_f, r1", 0xe128_f001, false),
        ("msr cpsr_x, r1", 0xe122_f001, false),
        ("msr cpsr_s, r1", 0xe124_f001, false),
        ("msr spsr_c, r1", 0xe161_f001, false),
    ];
    let mut wrong = Vec::new();
    for (name, msr, switches) in cases {
        let f = a32(&[
            A32_SAVE_LR,
            A32_R8_IS_5,
            msr,
            A32_RETURN_R8[0],
            A32_RETURN_R8[1],
        ]);
        let want = if switches { None } else { Some(5) };
        let got = r0_at_return(&f);
        if got != want {
            wrong.push(format!("{name}: r8 after it is {got:?}, want {want:?}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// The Thumb-2 `msr` (register) forms, under the same rule.
#[test]
fn a_thumb_msr_writing_the_control_field_switches_mode() {
    let cases: [(&str, [u16; 2], bool); 6] = [
        ("msr cpsr_c, r1", [0xf381, 0x8100], true),
        ("msr cpsr_fc, r1", [0xf381, 0x8900], true),
        ("msr cpsr_x, r1", [0xf381, 0x8200], false),
        ("msr cpsr_fs, r1", [0xf381, 0x8c00], false),
        ("msr cpsr_f, r1", [0xf381, 0x8800], false),
        ("msr spsr_c, r1", [0xf391, 0x8100], false),
    ];
    let mut wrong = Vec::new();
    for (name, msr, switches) in cases {
        // mov r4, lr; mov.w r8, #5; <msr>; mov r0, r8; bx r4
        let f = thumb(&[0x4674, 0xf04f, 0x0805, msr[0], msr[1], 0x4640, 0x4720]);
        let want = if switches { None } else { Some(5) };
        let got = r0_at_return(&f);
        if got != want {
            wrong.push(format!("{name}: r8 after it is {got:?}, want {want:?}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Every exception mode has its own SPSR, so `mrs r0, spsr` after a switch
/// does not read the SPSR that came in.
#[test]
fn a_mode_switch_rebanks_the_spsr() {
    let cases: [(&str, u32); 2] = [("cps #17", 0xf102_0011), ("msr cpsr_c, #0xd1", 0xe321_f0d1)];
    let mut wrong = Vec::new();
    for (name, switch) in cases {
        // mov r4, lr; <switch>; mrs r0, spsr; bx r4
        let f = a32(&[A32_SAVE_LR, switch, 0xe14f_0000, A32_RETURN_R8[1]]);
        let r0 = common::returned(&f)[0];
        if !rebanked(&f, r0) {
            wrong.push(format!(
                "{name}: spsr after it is {:?}",
                f.kind_of_value(r0)
            ));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Writing back the mode the CPU is in re-banks nothing: the interrupt-mask
/// save and restore around a critical section, and an `msr` naming the mode
/// a `cps` just entered.
#[test]
fn rewriting_the_current_mode_keeps_the_registers() {
    // From an unknown mode, then after `cps #19`.
    let a32_cases: [(&str, &[u32]); 3] = [
        (
            "mrs r2, cpsr; orr r3, r2, #0x80; msr cpsr_c, r3; msr cpsr_c, r2",
            &[0xe10f_2000, 0xe382_3080, 0xe121_f003, 0xe121_f002],
        ),
        (
            "mrs r2, cpsr; cpsid i; msr cpsr_c, r2",
            &[0xe10f_2000, 0xf10c_0080, 0xe121_f002],
        ),
        ("cps #19; msr cpsr_c, #0xd3", &[0xf102_0013, 0xe321_f0d3]),
    ];
    let mut wrong = Vec::new();
    for (name, body) in a32_cases {
        // mov r4, lr; <first of body>; mov r8, #5; <rest>; mov r0, r8; bx r4
        let mut words = vec![A32_SAVE_LR, body[0], A32_R8_IS_5];
        words.extend_from_slice(&body[1..]);
        words.extend(A32_RETURN_R8);
        let got = r0_at_return(&a32(&words));
        if got != Some(5) {
            wrong.push(format!("{name}: r8 after it is {got:?}"));
        }
    }
    // mov r4, lr; mov.w r8, #5; mrs r2, cpsr; cpsid i; msr cpsr_c, r2;
    // mov r0, r8; bx r4
    let f = thumb(&[
        0x4674, 0xf04f, 0x0805, 0xf3ef, 0x8200, 0xb672, 0xf382, 0x8100, 0x4640, 0x4720,
    ]);
    let got = r0_at_return(&f);
    if got != Some(5) {
        wrong.push(format!(
            "thumb mrs; cpsid i; msr cpsr_c: r8 after it is {got:?}"
        ));
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// `cps` records the mode it enters in CPSR.M, which `mrs` reads back.
#[test]
fn mrs_reads_the_mode_cps_entered() {
    // mov r4, lr; cps #17; mrs r0, cpsr; and r0, r0, #0x1f; bx r4
    let f = a32(&[
        A32_SAVE_LR,
        0xf102_0011,
        0xe10f_0000,
        0xe200_001f,
        A32_RETURN_R8[1],
    ]);
    assert_eq!(f.int_const_u128(common::returned(&f)[0]), Some(0x11));
}
