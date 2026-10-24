//! MIPS conversions that round by FCSR.RM (bits 0-1: 0 nearest even, 1
//! toward zero, 2 toward +inf, 3 toward -inf; MIPS32/MIPS64 Vol. II CVT.S
//! and CVT.D): `cvt.s.d`, `cvt.s.w`, `cvt.s.l` and `cvt.d.l`.
//! `fp_rounding_mode_is_nearest` takes RM to be 0; without it each reads
//! FCSR.
//!
//! Every encoding is llvm-mc's (`mips32r2,+fp64`, `mips64r2`). Each is one
//! instruction then `jr $ra; nop`, lifted once with $f2 returned and $f4
//! and FCSR as inputs (the f2_3 and f4_5 pairs for a doubleword on a 32-bit
//! FPU), then evaluated per input against `common::round`.

mod common;

use common::Arch;
use common::cone::{Lifted, Val, lift_regs};
use common::round::{DOUBLE, Fmt, MIPS_MODES, Mode, SINGLE};
use strider_orchestrator::opt::AssumptionOptions;

const ARCHES: [Arch; 4] = [
    Arch::Mips32be,
    Arch::Mips32le,
    Arch::Mips64be,
    Arch::Mips64le,
];

#[derive(Clone, Copy)]
enum Src {
    Double,
    Word,
    Long,
}

struct Conv {
    asm: &'static str,
    word: u32,
    src: Src,
    dst: Fmt,
}

const CONVS: [Conv; 4] = [
    Conv {
        asm: "cvt.s.d $f2, $f4",
        word: 0x4620_20a0,
        src: Src::Double,
        dst: SINGLE,
    },
    Conv {
        asm: "cvt.s.w $f2, $f4",
        word: 0x4680_20a0,
        src: Src::Word,
        dst: SINGLE,
    },
    Conv {
        asm: "cvt.s.l $f2, $f4",
        word: 0x46a0_20a0,
        src: Src::Long,
        dst: SINGLE,
    },
    Conv {
        asm: "cvt.d.l $f2, $f4",
        word: 0x46a0_20a1,
        src: Src::Long,
        dst: DOUBLE,
    },
];

fn is64(arch: Arch) -> bool {
    matches!(arch, Arch::Mips64be | Arch::Mips64le)
}

fn lift(arch: Arch, c: &Conv, claims: &AssumptionOptions) -> Lifted {
    let pair =
        |r: &'static str, p: &'static str, wide: bool| if wide && !is64(arch) { p } else { r };
    let input = pair("f4", "f4_5", !matches!(c.src, Src::Word));
    let ret = pair("f2", "f2_3", c.dst == DOUBLE);
    lift_regs(
        arch,
        &[c.word, 0x03e0_0008, 0],
        &[ret],
        &[input, "fcsr"],
        claims,
    )
}

fn int(v: &Val) -> u128 {
    match v {
        Val::Int(x) => *x,
        other => panic!("$f2 holds {other:?}"),
    }
}

fn check(c: &Conv, l: &Lifted, rm: u32, mode: Mode, what: &str) {
    let ins: Vec<(u128, Option<u128>)> = match c.src {
        Src::Double => common::round::narrowing_inputs(DOUBLE, SINGLE)
            .into_iter()
            .map(|b| (b, SINGLE.convert(DOUBLE, b, mode)))
            .collect(),
        Src::Word | Src::Long => {
            let bits = if matches!(c.src, Src::Word) { 32 } else { 64 };
            common::round::integer_inputs(bits, true, c.dst)
                .into_iter()
                .map(|x| {
                    (
                        x as u128 & ((1 << bits) - 1),
                        Some(c.dst.round_fixed(x, 0, mode)),
                    )
                })
                .collect()
        }
    };
    let width = c.dst.bits();
    for (b, want) in ins {
        for fill in [0, u128::MAX] {
            // the other FCSR bits set or clear with the fill
            let fcsr = (fill & 0xffff_fffc) | u128::from(rm);
            let got = int(&l.run(&[b, fcsr], fill)[0]) & ((1 << width) - 1);
            match want {
                Some(w) => assert_eq!(got, w, "{what} {} rm {rm} {b:#x}: got {got:#x}", c.asm),
                None => assert!(
                    c.dst.is_nan(got),
                    "{what} {} {b:#x}: {got:#x} is no NaN",
                    c.asm
                ),
            }
        }
    }
}

#[test]
fn narrowing_and_integer_to_float_round_by_fcsr_rm() {
    let none = AssumptionOptions::none();
    for arch in ARCHES {
        for c in &CONVS {
            let l = lift(arch, c, &none);
            for rm in 0..4 {
                check(c, &l, rm, MIPS_MODES[rm as usize], &format!("{arch:?}"));
            }
        }
    }
}

/// Under the claim each reads RM as 0 whatever FCSR holds.
#[test]
fn the_claim_rounds_them_to_nearest_whatever_fcsr_holds() {
    let claims = AssumptionOptions::default();
    assert!(claims.fp_rounding_mode_is_nearest);
    for arch in [Arch::Mips32be, Arch::Mips64le] {
        for c in &CONVS {
            let l = lift(arch, c, &claims);
            for rm in 1..4 {
                check(c, &l, rm, Mode::Even, &format!("{arch:?} under the claim"));
            }
        }
    }
}

/// A conversion onto its own source register rounds the value it read.
#[test]
fn a_conversion_onto_its_source_reads_it_first() {
    let none = AssumptionOptions::none();
    let forms = [
        ("cvt.d.l $f4, $f4", 0x46a0_2121, Src::Long, DOUBLE),
        ("cvt.s.l $f4, $f4", 0x46a0_2120, Src::Long, SINGLE),
        ("cvt.s.d $f4, $f4", 0x4620_2120, Src::Double, SINGLE),
    ];
    for arch in [Arch::Mips64be, Arch::Mips64le] {
        for (asm, word, src, dst) in forms {
            let c = Conv {
                asm,
                word,
                src,
                dst,
            };
            let l = lift_regs(
                arch,
                &[word, 0x03e0_0008, 0],
                &["f4"],
                &["f4", "fcsr"],
                &none,
            );
            for rm in 0..4 {
                check(&c, &l, rm, MIPS_MODES[rm as usize], &format!("{arch:?}"));
            }
        }
    }
}
