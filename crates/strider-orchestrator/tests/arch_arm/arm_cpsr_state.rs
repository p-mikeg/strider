//! What `mrs` reads back and what `msr` writes, past the mode: the I, F and A
//! masks (CPSR bits 7, 6 and 8) that `cps` and `msr` set, and N, Z, C, V, Q,
//! which an `msr` with the flags field (mask bit 3) takes from its source and
//! every other `msr` leaves alone (Arm ARM DDI 0406C B9.3.11 / B9.3.12,
//! `CPSRWriteByInstr`; B9.3.9 MRS). T, E, GE and the IT bits are not
//! modelled, and read as zero.

use crate::common;

use crate::common::Arch;
use strider_ir::node::NodeKind;
use strider_ir::{AssumptionOptions, Function, IRViewer, IRWalker};
use strider_orchestrator::AnalyzeOptions;

const BASE: u64 = 0x1000;
/// `mov r4, lr`
const SAVE_LR: u32 = 0xe1a0_400e;
/// `bx r4`
const RETURN: u32 = 0xe12f_ff14;

fn analyze(arch: Arch, bytes: Vec<u8>, entry: u64, opts: &AnalyzeOptions) -> Function {
    let (mut strider, cc) = common::strider_over_bytes(arch, bytes, BASE, None);
    let result = strider.analyze(entry, &cc, opts, None).expect("analyze");
    assert!(result.incomplete().is_empty(), "{:?}", result.incomplete());
    result.function
}

/// The defaults less `msr_register_keeps_mode`, so a register-form `msr`
/// writes the mode it names.
fn mode_from_the_register() -> AnalyzeOptions<'static> {
    AnalyzeOptions {
        assumptions: AssumptionOptions {
            msr_register_keeps_mode: false,
            ..AssumptionOptions::default()
        },
        ..AnalyzeOptions::default()
    }
}

fn a32(body: &[u32], tail: &[u32]) -> Function {
    a32_with(body, tail, &AnalyzeOptions::default())
}

fn a32_with(body: &[u32], tail: &[u32], opts: &AnalyzeOptions) -> Function {
    let mut words = vec![SAVE_LR];
    words.extend_from_slice(body);
    words.extend_from_slice(tail);
    words.push(RETURN);
    words.resize(words.len() + 0x50, 0);
    analyze(
        Arch::Arm,
        common::words_image(&words, strider_target::Endianness::Little),
        BASE,
        opts,
    )
}

fn thumb(body: &[u16], tail: &[u16]) -> Function {
    thumb_with(body, tail, &AnalyzeOptions::default())
}

fn thumb_with(body: &[u16], tail: &[u16], opts: &AnalyzeOptions) -> Function {
    // mov r4, lr; <body>; <tail>; bx r4
    let mut halves = vec![0x4674];
    halves.extend_from_slice(body);
    halves.extend_from_slice(tail);
    halves.push(0x4720);
    let bytes = halves.iter().flat_map(|h| h.to_le_bytes()).collect();
    analyze(Arch::ArmThumb, bytes, BASE | 1, opts)
}

fn r0(f: &Function) -> Option<u128> {
    f.int_const_u128(common::returned(f)[0])
}

/// `mrs r0, cpsr; and r0, r0, #mask`
fn read_masks(mask: u32) -> [u32; 2] {
    let and = match mask {
        0x80 => 0xe200_0080,
        0x40 => 0xe200_0040,
        0x100 => 0xe200_0c01,
        0xc0 => 0xe200_00c0,
        _ => unreachable!(),
    };
    [0xe10f_0000, and]
}

/// `mrs` reads the masks `cps` and `msr` wrote, and an unknown one as unknown.
#[test]
fn mrs_reads_the_interrupt_masks() {
    let cases: [(&str, &[u32], u32, Option<u128>); 10] = [
        ("nothing", &[], 0x80, None),
        ("cpsid i", &[0xf10c_0080], 0x80, Some(0x80)),
        (
            "cpsid i; cpsie i",
            &[0xf10c_0080, 0xf108_0080],
            0x80,
            Some(0),
        ),
        ("cpsie i", &[0xf108_0080], 0x80, Some(0)),
        ("cpsid f", &[0xf10c_0040], 0x40, Some(0x40)),
        ("cpsid a", &[0xf10c_0100], 0x100, Some(0x100)),
        ("cpsid if", &[0xf10c_00c0], 0xc0, Some(0xc0)),
        ("cpsid f", &[0xf10c_0040], 0x80, None),
        ("msr cpsr_c, #0xd3", &[0xe321_f0d3], 0xc0, Some(0xc0)),
        ("msr cpsr_c, #0x13", &[0xe321_f013], 0xc0, Some(0)),
    ];
    let mut wrong = Vec::new();
    for (name, body, mask, want) in cases {
        let got = r0(&a32(body, &read_masks(mask)));
        if got != want {
            wrong.push(format!("{name}; mrs & {mask:#x}: {got:x?}, want {want:x?}"));
        }
    }
    // Thumb: <body>; mrs r0, cpsr; and r0, r0, #mask
    for (name, body, mask, want) in [
        ("nothing", &[][..], 0x80, None),
        ("cpsid i", &[0xb672][..], 0x80, Some(0x80)),
        ("cpsie i", &[0xb662][..], 0x80, Some(0)),
        (
            "cpsid i; cpsie i, #19",
            &[0xb672, 0xf3af, 0x8553][..],
            0x80,
            Some(0),
        ),
        ("cpsid f, #19", &[0xf3af, 0x8733][..], 0x40, Some(0x40)),
        (
            "cpsid f; cpsie f, #19",
            &[0xb671, 0xf3af, 0x8533][..],
            0x40,
            Some(0),
        ),
    ] {
        let tail = [0xf3ef, 0x8000, 0xf000, mask];
        let got = r0(&thumb(body, &tail));
        if got != want {
            wrong.push(format!(
                "thumb {name}; mrs & {mask:#x}: {got:x?}, want {want:x?}"
            ));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// `irqs_disabled_flags(flags)` after `mrs`: both paths stay while the I bit
/// is unknown, and the test folds once a `cpsid i` set it.
#[test]
fn a_branch_on_the_saved_i_bit_folds_only_when_it_is_known() {
    // mrs r0, cpsr; tst r0, #0x80; bne 1f; mov r0, #0; bx r4; 1: mov r0, #1
    let tail = [
        0xe10f_0000,
        0xe310_0080,
        0x1a00_0001,
        0xe3a0_0000,
        RETURN,
        0xe3a0_0001,
    ];
    let returns = |f: &Function| {
        f.walk()
            .filter(|&n| matches!(f.node_kind(n), NodeKind::Return))
            .count()
    };
    assert_eq!(returns(&a32(&[], &tail)), 2, "unknown I bit");
    assert_eq!(returns(&a32(&[0xf10c_0080], &tail)), 1, "after cpsid i");
}

/// A call may change the masks and the flags; only the mode survives it.
#[test]
fn a_call_clobbers_the_interrupt_masks() {
    // cpsid i; bl .+0x100; mrs r0, cpsr; and r0, r0, #0x80
    let f = a32(&[0xf10c_0080, 0xeb00_003e], &read_masks(0x80));
    assert_eq!(r0(&f), None);
}

/// `mov r0, #0; adc r0, r0, #0`, so `r0` is C.
const READ_C: [u32; 2] = [0xe3a0_0000, 0xe2a0_0000];
/// `mov r1, #0; cmp r1, #0`: C set. `cmp r1, #1` instead: C clear.
const C_SET: [u32; 2] = [0xe3a0_1000, 0xe351_0000];
const C_CLEAR: [u32; 2] = [0xe3a0_1000, 0xe351_0001];

/// An `msr` with the flags field takes N, Z, C, V, Q from its source; any
/// other `msr` keeps them.
#[test]
fn msr_writes_the_flags_only_through_the_flags_field() {
    let cases: [(&str, [u32; 2], &[u32], u128); 7] = [
        (
            "mov r1, #0x20000000; msr cpsr_f, r1",
            C_CLEAR,
            &[0xe3a0_1202, 0xe128_f001],
            1,
        ),
        ("msr cpsr_f, #0x20000000", C_CLEAR, &[0xe328_f202], 1),
        ("msr cpsr_f, #0", C_SET, &[0xe328_f000], 0),
        ("msr cpsr_x, r1", C_SET, &[0xe122_f001], 1),
        ("msr cpsr_c, r1", C_SET, &[0xe121_f001], 1),
        // The control and flags fields together, as a full restore writes them.
        ("msr cpsr_fc, #0x20000003", C_CLEAR, &[0xe329_f232], 1),
        (
            "mov r1, #0x20000000; orr r1, r1, #0xd3; msr cpsr_fsxc, r1",
            C_CLEAR,
            &[0xe3a0_1202, 0xe381_10d3, 0xe12f_f001],
            1,
        ),
    ];
    let mut wrong = Vec::new();
    for (name, prime, msr, want) in cases {
        let mut body = prime.to_vec();
        body.extend_from_slice(msr);
        let got = r0(&a32(&body, &READ_C));
        if got != Some(want) {
            wrong.push(format!("{name}: C after it is {got:?}, want {want}"));
        }
    }
    // Thumb: movs r3, #0; cmp r3, #0|#1; <msr>; mov.w r0, #0; adc r0, r0, #0
    let read_c = [0xf04f, 0x0000, 0xf140, 0x0000];
    for (name, prime, msr, want) in [
        ("msr cpsr_x, r1", 0x2b00, &[0xf381, 0x8200][..], 1),
        (
            "mov.w r1, #0x20000000; msr apsr_nzcvq, r1",
            0x2b01,
            &[0xf04f, 0x5100, 0xf381, 0x8800][..],
            1,
        ),
        (
            "mov.w r1, #0x20000000; orr r1, r1, #0xd3; msr cpsr_fc, r1",
            0x2b01,
            &[0xf04f, 0x5100, 0xf041, 0x01d3, 0xf381, 0x8900][..],
            1,
        ),
        (
            "movs r1, #0; msr apsr_nzcvq, r1",
            0x2b00,
            &[0x2100, 0xf381, 0x8800][..],
            0,
        ),
    ] {
        let mut body = vec![0x2300, prime];
        body.extend_from_slice(msr);
        let got = r0(&thumb(&body, &read_c));
        if got != Some(want) {
            wrong.push(format!("thumb {name}: C after it is {got:?}, want {want}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// `mrs` reads back the mode an `msr` to the control field wrote, the
/// register form's where the claim does not take it to keep the mode.
#[test]
fn mrs_reads_the_mode_msr_wrote() {
    let opts = mode_from_the_register();
    // mrs r0, cpsr; and r0, r0, #0x1f
    let a32_tail = [0xe10f_0000, 0xe200_001f];
    let mut wrong = Vec::new();
    for (name, body) in [
        ("msr cpsr_c, #0xd1", &[0xe321_f0d1][..]),
        (
            "mov r1, #0xd1; msr cpsr_c, r1",
            &[0xe3a0_10d1, 0xe121_f001][..],
        ),
    ] {
        let got = r0(&a32_with(body, &a32_tail, &opts));
        if got != Some(0x11) {
            wrong.push(format!("{name}: mode {got:x?}"));
        }
    }
    // movs r1, #0xd1; msr cpsr_c, r1; mrs r0, cpsr; and r0, r0, #0x1f
    let got = r0(&thumb_with(
        &[0x21d1, 0xf381, 0x8100],
        &[0xf3ef, 0x8000, 0xf000, 0x001f],
        &opts,
    ));
    if got != Some(0x11) {
        wrong.push(format!("thumb msr cpsr_c, r1: mode {got:x?}"));
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// A Thumb-2 `cps` records the mode it enters, as the A32 one does.
#[test]
fn mrs_reads_the_mode_a_thumb_cps_entered() {
    // cpsid i, #17; mrs r0, cpsr; and r0, r0, #0x1f
    let f = thumb(&[0xf3af, 0x8751], &[0xf3ef, 0x8000, 0xf000, 0x001f]);
    assert_eq!(r0(&f), Some(0x11));
}
