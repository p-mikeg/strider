//! ARM VFP conversions that round by FPSCR.RMode (bits 22-23: 0 nearest
//! even, 1 toward +inf, 2 toward -inf, 3 toward zero; Arm ARM FPConvert and
//! FixedToFP): `vcvt.f32.f64`, `vcvt.f32.s32`, `vcvt.f32.u32`, and `vcvtb` and
//! `vcvtt` to half, which leave the other half of the destination alone.
//! `fp_rounding_mode_is_nearest` takes RMode to be 0; without it each reads
//! FPSCR. `vrintr` and `vrintx` hand their opaque op RMode, bits 22-23.
//!
//! Every encoding is llvm-mc's (`armv8a`, `+fp-armv8`; the Thumb ones are the
//! same words as two halfwords). Each conversion is one instruction and
//! `bx lr`, lifted once with s0 returned and its source, s0 and FPSCR as
//! inputs, then evaluated per input against `common::round`.

use crate::common;

use crate::common::Arch;
use crate::common::cone::{IMAGE_BASE, Lifted, Val, lift_image_with};
use crate::common::round::{ARM_MODES, DOUBLE, Fmt, HALF, Mode, SINGLE};
use strider_ir::{IRViewer, IRWalker};
use strider_orchestrator::opt::AssumptionOptions;

const ARCHES: [Arch; 4] = [Arch::Arm, Arch::ArmBe, Arch::ArmBe8, Arch::ArmThumb];

/// `insn` (an A32 VFP word; in Thumb its two halfwords) then `bx lr`.
fn image(arch: Arch, insn: u32) -> Vec<u8> {
    match arch {
        Arch::ArmThumb => {
            let mut v = Vec::new();
            for h in [(insn >> 16) as u16, insn as u16, 0x4770] {
                v.extend_from_slice(&h.to_le_bytes());
            }
            v
        }
        Arch::ArmBe => common::words_image(&[insn, 0xe12f_ff1e], strider_target::Endianness::Big),
        _ => common::words_image(&[insn, 0xe12f_ff1e], strider_target::Endianness::Little),
    }
}

fn lift(arch: Arch, insn: u32, input: &str, claims: &AssumptionOptions) -> Lifted {
    // vrintr's and vrintx's ops, which no ABI row names
    let overrides = strider_target::call_other_abi::CallOtherOverrides::new(
        ["FPRoundInt", "FPConvertInexact"]
            .map(|n| {
                (
                    n.to_owned(),
                    strider_target::call_other_abi::CallOtherClass::PURE.into(),
                )
            })
            .to_vec(),
    )
    .expect("unique override names");
    let opts = strider_orchestrator::AnalyzeOptions {
        cfg: strider_cfg::CfgOptions {
            call_other_overrides: &overrides,
            ..Default::default()
        },
        assumptions: claims.clone(),
        ..Default::default()
    };
    let entry = IMAGE_BASE | u64::from(arch == Arch::ArmThumb);
    lift_image_with(
        arch,
        image(arch, insn),
        entry,
        &["s0"],
        &[input, "s0", "fpscr"],
        &opts,
    )
}

fn int(v: &Val) -> u128 {
    match v {
        Val::Int(x) => *x,
        Val::Float(x) => u128::from((*x as f32).to_bits()),
        Val::Quad(q) => panic!("s0 holds a quad {q:#x}"),
    }
}

/// FPSCR holding RMode `rm` among other bits, bit 21 set.
fn fpscr(rm: u32) -> u128 {
    u128::from(0x0320_009f | rm << 22)
}

#[derive(Clone, Copy)]
enum Src {
    Float(Fmt),
    Int(bool),
}

struct Conv {
    asm: &'static str,
    word: u32,
    src: Src,
    input: &'static str,
    /// The half of s0 a half result lands in (`Some(true)` the top one), or
    /// `None` for a whole single.
    half: Option<bool>,
}

const CONVS: [Conv; 7] = [
    Conv {
        asm: "vcvt.f32.f64 s0, d1",
        word: 0xeeb7_0bc1,
        src: Src::Float(DOUBLE),
        input: "d1",
        half: None,
    },
    Conv {
        asm: "vcvt.f32.s32 s0, s1",
        word: 0xeeb8_0ae0,
        src: Src::Int(true),
        input: "s1",
        half: None,
    },
    Conv {
        asm: "vcvt.f32.u32 s0, s1",
        word: 0xeeb8_0a60,
        src: Src::Int(false),
        input: "s1",
        half: None,
    },
    Conv {
        asm: "vcvtb.f16.f32 s0, s1",
        word: 0xeeb3_0a60,
        src: Src::Float(SINGLE),
        input: "s1",
        half: Some(false),
    },
    Conv {
        asm: "vcvtt.f16.f32 s0, s1",
        word: 0xeeb3_0ae0,
        src: Src::Float(SINGLE),
        input: "s1",
        half: Some(true),
    },
    Conv {
        asm: "vcvtb.f16.f64 s0, d1",
        word: 0xeeb3_0b41,
        src: Src::Float(DOUBLE),
        input: "d1",
        half: Some(false),
    },
    Conv {
        asm: "vcvtt.f16.f64 s0, d1",
        word: 0xeeb3_0bc1,
        src: Src::Float(DOUBLE),
        input: "d1",
        half: Some(true),
    },
];

/// The old s0 a half result is written into.
const OLD: u128 = 0x9abc_def0;

fn check(c: &Conv, l: &Lifted, rm: u32, mode: Mode, what: &str) {
    let dst = if c.half.is_some() { HALF } else { SINGLE };
    let ins: Vec<(u128, Option<u128>)> = match c.src {
        Src::Float(src) => common::round::narrowing_inputs(src, dst)
            .into_iter()
            .map(|b| (b, dst.convert(src, b, mode)))
            .collect(),
        Src::Int(signed) => common::round::integer_inputs(32, signed, SINGLE)
            .into_iter()
            .map(|x| {
                (
                    x as u128 & 0xffff_ffff,
                    Some(SINGLE.round_fixed(x, 0, mode)),
                )
            })
            .collect(),
    };
    for (b, want) in ins {
        for fill in [0, u128::MAX] {
            let s0 = int(&l.run(&[b, OLD, fpscr(rm)], fill)[0]);
            let got = match c.half {
                None => s0,
                Some(top) => {
                    let kept = if top { s0 & 0xffff } else { s0 >> 16 };
                    let old = if top { OLD & 0xffff } else { OLD >> 16 };
                    assert_eq!(kept, old, "{what} {} keeps the other half", c.asm);
                    if top { s0 >> 16 } else { s0 & 0xffff }
                }
            };
            common::round::check_against(
                dst,
                got,
                want,
                &format!("{what} {} rm {rm} {b:#x}", c.asm),
            );
        }
    }
}

#[test]
fn narrowing_and_integer_to_single_round_by_fpscr_rmode() {
    let none = AssumptionOptions::none();
    for arch in ARCHES {
        for c in &CONVS {
            let l = lift(arch, c.word, c.input, &none);
            for rm in 0..4 {
                check(c, &l, rm, ARM_MODES[rm as usize], &format!("{arch:?}"));
            }
        }
    }
}

/// Under the claim each reads RMode as 0 whatever FPSCR holds.
#[test]
fn the_claim_rounds_them_to_nearest_whatever_fpscr_holds() {
    let claims = AssumptionOptions::default();
    assert!(claims.fp_rounding_mode_is_nearest);
    for c in &CONVS {
        let l = lift(Arch::Arm, c.word, c.input, &claims);
        for rm in 1..4 {
            check(c, &l, rm, Mode::Even, "under the claim");
        }
    }
}

/// `vrintr` and `vrintx` pass `FPRoundInt` RMode, bits 22-23, whatever bit
/// 21 holds.
#[test]
fn vrintr_and_vrintx_pass_rmode() {
    let none = AssumptionOptions::none();
    for arch in ARCHES {
        for (asm, word) in [
            ("vrintr.f32 s0, s1", 0xeeb6_0a60),
            ("vrintx.f32 s0, s1", 0xeeb7_0a60),
        ] {
            let l = lift(arch, word, "s1", &none);
            let call =
                l.f.walk()
                    .find(|&n| l.f.call_other_name(n) == Some("FPRoundInt"))
                    .expect("an FPRoundInt");
            // control, memory, then the value, its width, the mode, exact
            let mode = l.f.node_inputs(call).into_iter().nth(4).expect("the mode");
            for rm in 0..4u32 {
                for bit21 in [0, 1 << 21] {
                    let fp = u128::from(0x0000_009f | bit21 | rm << 22);
                    let got = int(&l.eval(mode, &[0, 0, fp], 0));
                    assert_eq!(
                        got,
                        u128::from(rm),
                        "{arch:?} {asm} rm {rm} bit 21 {bit21:#x}"
                    );
                }
            }
        }
    }
}
