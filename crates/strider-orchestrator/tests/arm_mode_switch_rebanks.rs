//! A processor-mode change re-banks `r13`/`r14`, `r8`-`r12` in and out of FIQ,
//! and the SPSR (Arm ARM DDI 0406C B1.3.2), so a value written before the
//! change is not the one read after it. `cps #mode` and every `msr` that
//! writes the CPSR's control field (mask bit 0, `CPSRWriteByInstr` writing
//! `M[4:0]`, B9.3.11 / B9.3.12) can change mode; an `msr` to the flags,
//! extension or status field alone, or to the SPSR, does not, and neither
//! does one that rewrites the mode the CPU is already in, which is how an
//! interrupt-mask restore reads (`mrs r2, cpsr; ...; msr cpsr_c, r2`).
//!
//! That is the sla's own model, which these tests run under unless they say
//! otherwise: `msr_register_keeps_mode` off. The claim, on by default, takes a
//! register-form `msr` to restore the mode in force.

mod common;

use common::Arch;
use strider_ir::node::{NodeKind, ValueId};
use strider_ir::{AssumptionOptions, Function, IRViewer};
use strider_orchestrator::AnalyzeOptions;

const BASE: u64 = 0x1000;

/// `mov r4, lr`, so the function returns through `bx r4` whatever the mode
/// switch does to `lr`.
const A32_SAVE_LR: u32 = 0xe1a0_400e;
/// `mov r8, #5`
const A32_R8_IS_5: u32 = 0xe3a0_8005;
/// `mov r0, r8; bx r4`
const A32_RETURN_R8: [u32; 2] = [0xe1a0_0008, 0xe12f_ff14];

/// The defaults, with `msr_register_keeps_mode` as given.
fn keeping_mode(keeps: bool) -> AnalyzeOptions<'static> {
    AnalyzeOptions {
        assumptions: AssumptionOptions {
            msr_register_keeps_mode: keeps,
            ..AssumptionOptions::default()
        },
        ..AnalyzeOptions::default()
    }
}

fn analyze_with(arch: Arch, bytes: Vec<u8>, entry: u64, opts: &AnalyzeOptions) -> Function {
    let (mut strider, cc) = common::strider_over_bytes(arch, bytes, BASE, None);
    let result = strider.analyze(entry, &cc, opts, None).expect("analyze");
    assert!(result.incomplete().is_empty(), "{:?}", result.incomplete());
    result.function
}

fn a32_with(words: &[u32], opts: &AnalyzeOptions) -> Function {
    let bytes = common::words_image(words, strider_target::Endianness::Little);
    analyze_with(Arch::Arm, bytes, BASE, opts)
}

fn thumb_with(halves: &[u16], opts: &AnalyzeOptions) -> Function {
    let bytes = halves.iter().flat_map(|h| h.to_le_bytes()).collect();
    analyze_with(Arch::ArmThumb, bytes, BASE | 1, opts)
}

fn a32(words: &[u32]) -> Function {
    a32_with(words, &keeping_mode(false))
}

fn thumb(halves: &[u16]) -> Function {
    thumb_with(halves, &keeping_mode(false))
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

/// A call keeps the mode: the callee returns through the caller's banked
/// `lr`, so a conforming one cannot come back in another mode. The restore
/// after it rewrites the mode the save read.
#[test]
fn a_call_between_the_save_and_the_restore_keeps_the_mode() {
    // mov r4, lr; mov r8, #5; mrs r5, cpsr; cpsid i; bl .+0x100;
    // msr cpsr_c, r5; mov r0, r8; bx r4
    let mut words = vec![
        A32_SAVE_LR,
        A32_R8_IS_5,
        0xe10f_5000,
        0xf10c_0080,
        0xeb00_003e,
        0xe121_f005,
        A32_RETURN_R8[0],
        A32_RETURN_R8[1],
    ];
    words.resize(words.len() + 0x50, 0);
    assert_eq!(r0_at_return(&a32(&words)), Some(5));
}

/// `dump_stack_lvl` from the arm 6.6-clang kernel (`0x8098ac2c`): an
/// interrupt-mask save and restore around a retry loop. The loop saves again
/// on its back edge, so the restore after it writes a phi of two `mrs`
/// results, each of whose mode bits is the mode in force at entry.
///
/// ```text
///       push {r4, r5, fp, lr}; mov r4, r0; mrs r5, cpsr; cpsid i
/// loop: bl a; cmp r0, #0; bne out
///       msr cpsr_c, r5; bl b; mrs r5, cpsr; cpsid i; b loop
/// out:  mov r0, r4; bl c; mov r0, #0; mov r1, #0; mov r2, r4; bl d; bl e
///       msr cpsr_c, r5; pop {r4, r5, fp, pc}
/// ```
fn dump_stack_lvl() -> Vec<u8> {
    let words = [
        0xe92d_4830,
        0xe1a0_4000,
        0xe10f_5000,
        0xf10c_0080,
        0xebdf_c2d0,
        0xe350_0000,
        0x1a00_0004,
        0xe121_f005,
        0xebdf_c2c6,
        0xe10f_5000,
        0xf10c_0080,
        0xeaff_fff7,
        0xe1a0_0004,
        0xebff_4617,
        0xe3a0_0000,
        0xe3a0_1000,
        0xe1a0_2004,
        0xebde_0272,
        0xebdf_c2df,
        0xe121_f005,
        0xe8bd_8830,
    ];
    common::words_image(&words, strider_target::Endianness::Little)
}

/// Both arms of the restored phi mask to the entry mode, so the restore keeps
/// `sp` and `lr`, and with them the return, with or without the claim.
#[test]
fn a_loop_restore_of_the_saved_mode_returns_without_the_claim() {
    for (name, opts) in [
        ("default", AnalyzeOptions::default()),
        ("claim off", keeping_mode(false)),
    ] {
        let (mut strider, cc) = common::strider_over_bytes(Arch::Arm, dump_stack_lvl(), BASE, None);
        let result = strider.analyze(BASE, &cc, &opts, None).expect("analyze");
        assert!(
            result.incomplete().is_empty(),
            "{name}: {:?}",
            result.incomplete()
        );
    }
}

/// `msr cpsr_c, r0; bx lr`: the mode comes from an argument, so only the claim
/// keeps `lr`, and with it the return, from re-banking.
#[test]
fn an_unplaced_register_restore_returns_only_under_the_claim() {
    let bx_lr = BASE + 4;
    let words = [0xe121_f000, 0xe12f_ff1e];
    for (name, opts, reported) in [
        ("default", AnalyzeOptions::default(), false),
        ("claim off", keeping_mode(false), true),
    ] {
        let bytes = common::words_image(&words, strider_target::Endianness::Little);
        let (mut strider, cc) = common::strider_over_bytes(Arch::Arm, bytes, BASE, None);
        let result = strider.analyze(BASE, &cc, &opts, None).expect("analyze");
        let unresolved: Vec<u64> = result
            .incomplete()
            .of(strider_cfg::Reason::UnresolvedBranch)
            .map(|a| a.machine_addr.addr)
            .collect();
        if reported {
            assert_eq!(unresolved, [bx_lr], "{name}");
        } else {
            assert!(
                result.incomplete().is_empty(),
                "{name}: {:?}",
                result.incomplete()
            );
        }
    }
}

/// Under the claim a register-form `msr` keeps `r8`; the immediate form and
/// `cps` still switch.
#[test]
fn the_claim_spares_only_the_register_form() {
    let on = keeping_mode(true);
    let mut wrong = Vec::new();
    for (name, msr, switches) in [
        ("msr cpsr_c, r1", 0xe121_f001, false),
        ("msr cpsr_fsxc, r1", 0xe12f_f001, false),
        ("msr cpsr_c, #0xd2", 0xe321_f0d2, true),
        ("cps #18", 0xf102_0012, true),
    ] {
        let f = a32_with(
            &[
                A32_SAVE_LR,
                A32_R8_IS_5,
                msr,
                A32_RETURN_R8[0],
                A32_RETURN_R8[1],
            ],
            &on,
        );
        let want = if switches { None } else { Some(5) };
        let got = r0_at_return(&f);
        if got != want {
            wrong.push(format!("{name}: r8 after it is {got:?}, want {want:?}"));
        }
    }
    for (name, msr) in [
        ("msr cpsr_c, r1", [0xf381, 0x8100]),
        ("msr cpsr_fc, r1", [0xf381, 0x8900]),
    ] {
        // mov r4, lr; mov.w r8, #5; <msr>; mov r0, r8; bx r4
        let f = thumb_with(
            &[0x4674, 0xf04f, 0x0805, msr[0], msr[1], 0x4640, 0x4720],
            &on,
        );
        let got = r0_at_return(&f);
        if got != Some(5) {
            wrong.push(format!("thumb {name}: r8 after it is {got:?}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Under the claim a register-form `msr` leaves CPSR.M as it was and still
/// writes the masks: `mrs` after it reads the mode `cps` entered and the I
/// bit the `msr` set.
#[test]
fn the_claim_keeps_the_mode_and_writes_the_masks() {
    // mov r4, lr; cps #17; mov r1, #0xd3; msr cpsr_c, r1; mrs r0, cpsr;
    // and r0, r0, #<mask>; bx r4
    let body = |and: u32| {
        [
            A32_SAVE_LR,
            0xf102_0011,
            0xe3a0_10d3,
            0xe121_f001,
            0xe10f_0000,
            and,
            A32_RETURN_R8[1],
        ]
    };
    let mut wrong = Vec::new();
    for (name, and, keeps, want) in [
        ("mode, claim on", 0xe200_001f, true, 0x11),
        ("mode, claim off", 0xe200_001f, false, 0x13),
        ("I bit, claim on", 0xe200_0080, true, 0x80),
    ] {
        let f = a32_with(&body(and), &keeping_mode(keeps));
        let got = f.int_const_u128(common::returned(&f)[0]);
        if got != Some(want) {
            wrong.push(format!("{name}: {got:x?}, want {want:#x}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
