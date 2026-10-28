//! x86 conversions to an integer give the integer indefinite, the sign bit
//! alone, for a NaN, an infinity or a result past the destination (SDM
//! Vol. 1 4.8.3.7 and Vol. 2 FIST, FISTTP, CVTSD2SI and kin): `fist`,
//! `fistp` and `fisttp`, and every SSE and VEX `cvt` and `cvtt` to a
//! general, MMX or XMM register. The rounding ones round by RC (the x87
//! control word's bits 10-11, or MXCSR's bits 13-14: 0 nearest even, 1
//! down, 2 up, 3 toward zero), the `t` ones toward zero. `fst` and `fstp` to
//! a single round by RC through `x87RoundingControl`, taken as 0 under
//! `fp_rounding_mode_is_nearest`.
//!
//! Every encoding is llvm-mc's. The x87 functions are `movsd [rsp-16],
//! xmm1; fld qword [rsp-16]; <store> [rsp-8]; <load> eax/rax, [rsp-8];
//! [fstp st0;] ret`, the SSE ones `<conv> <dst>, xmm1; ret`, each lifted once
//! with the source, the control word and MXCSR as inputs.

use crate::common;

use crate::common::Arch;
use crate::common::cone::{Lifted, Val, lift_image};
use crate::common::round::{DOUBLE, Mode, SINGLE, X86_MODES};
use strider_orchestrator::opt::AssumptionOptions;

const MOVSD_FLD: &[u8] = &[0xf2, 0x0f, 0x11, 0x4c, 0x24, 0xf0, 0xdd, 0x44, 0x24, 0xf0];
const MOVSX_EAX: &[u8] = &[0x0f, 0xbf, 0x44, 0x24, 0xf8];
const MOV_EAX: &[u8] = &[0x8b, 0x44, 0x24, 0xf8];
const MOV_RAX: &[u8] = &[0x48, 0x8b, 0x44, 0x24, 0xf8];
const POP: &[u8] = &[0xdd, 0xd8];
const RET: u8 = 0xc3;

fn lift(code: Vec<u8>, ret: &str, claims: &AssumptionOptions) -> Lifted {
    lift_image(
        Arch::X64,
        code,
        &[ret],
        &["XMM1", "FPUControlWord", "MXCSR"],
        claims,
    )
}

fn int(v: &Val) -> u128 {
    match v {
        Val::Int(x) => *x,
        other => panic!("the result is {other:?}"),
    }
}

/// `x` rounded to an integer under `mode`, in a `bits`-wide slot, or the
/// integer indefinite.
fn to_int(x: f64, mode: Mode, bits: u32) -> u128 {
    let r = match mode {
        Mode::Even => x.round_ties_even(),
        Mode::Down => x.floor(),
        Mode::Up => x.ceil(),
        Mode::Zero => x.trunc(),
        Mode::Odd => unreachable!(),
    };
    let bound = 2f64.powi(bits as i32 - 1);
    if r.is_nan() || r < -bound || r >= bound {
        1 << (bits - 1)
    } else {
        (r as i128) as u128 & ((1 << bits) - 1)
    }
}

/// Ties and their neighbours, both sides of each destination's bounds,
/// values no destination holds, signed zeros, denormals, infinities and
/// NaNs.
fn values() -> Vec<f64> {
    let mut out = vec![
        0.0,
        -0.0,
        5e-324,
        -5e-324,
        1e300,
        -1e300,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NAN,
        -f64::NAN,
    ];
    for k in 0..6 {
        let k = f64::from(k);
        out.extend([k + 0.5, -(k + 0.5), k + 0.25, -(k + 0.75)]);
    }
    for n in [15, 16, 31, 32, 63, 64] {
        let b = 2f64.powi(n);
        for d in [-1.5, -1.0, -0.5, 0.0, 0.5, 1.0, 1.5] {
            for v in [b + d, -b + d] {
                out.extend([v, v.next_up(), v.next_down()]);
            }
        }
    }
    out
}

/// The x87 integer stores: name, store encoding, the load back, whether ST0
/// is left to pop, width, and whether it truncates.
type X87Store = (&'static str, &'static [u8], &'static [u8], bool, u32, bool);

const X87_STORES: [X87Store; 8] = [
    (
        "fist m16",
        &[0xdf, 0x54, 0x24, 0xf8],
        MOVSX_EAX,
        true,
        16,
        false,
    ),
    (
        "fist m32",
        &[0xdb, 0x54, 0x24, 0xf8],
        MOV_EAX,
        true,
        32,
        false,
    ),
    (
        "fistp m16",
        &[0xdf, 0x5c, 0x24, 0xf8],
        MOVSX_EAX,
        false,
        16,
        false,
    ),
    (
        "fistp m32",
        &[0xdb, 0x5c, 0x24, 0xf8],
        MOV_EAX,
        false,
        32,
        false,
    ),
    (
        "fistp m64",
        &[0xdf, 0x7c, 0x24, 0xf8],
        MOV_RAX,
        false,
        64,
        false,
    ),
    (
        "fisttp m16",
        &[0xdf, 0x4c, 0x24, 0xf8],
        MOVSX_EAX,
        false,
        16,
        true,
    ),
    (
        "fisttp m32",
        &[0xdb, 0x4c, 0x24, 0xf8],
        MOV_EAX,
        false,
        32,
        true,
    ),
    (
        "fisttp m64",
        &[0xdd, 0x4c, 0x24, 0xf8],
        MOV_RAX,
        false,
        64,
        true,
    ),
];

fn x87(store: &[u8], load: &[u8], pop: bool) -> Vec<u8> {
    let mut code = MOVSD_FLD.to_vec();
    code.extend_from_slice(store);
    code.extend_from_slice(load);
    if pop {
        code.extend_from_slice(POP);
    }
    code.push(RET);
    code
}

#[test]
fn x87_integer_stores_give_the_indefinite() {
    let none = AssumptionOptions::none();
    for (name, store, load, pop, bits, trunc) in X87_STORES {
        let l = lift(x87(store, load, pop), "RAX", &none);
        for rc in 0..4u32 {
            for x in values() {
                let cw = u128::from((0x037f & !0xc00) | (rc << 10));
                let got =
                    int(&l.run(&[u128::from(x.to_bits()), cw, 0x1f80], 0)[0]) & ((1 << bits) - 1);
                let mode = if trunc {
                    Mode::Zero
                } else {
                    X86_MODES[rc as usize]
                };
                assert_eq!(
                    got,
                    to_int(x, mode, bits),
                    "{name} rc {rc} {x:e}: got {got:#x}"
                );
            }
        }
    }
}

/// The SSE and VEX conversions: name, encoding, result register, source
/// lanes as singles (else doubles), lane count, width, truncating. The VEX
/// packed forms zero the upper ZMM lanes, past the cone's 128-bit integers.
type SseConv = (
    &'static str,
    &'static [u8],
    &'static str,
    bool,
    u32,
    u32,
    bool,
);

const SSE: [SseConv; 24] = [
    (
        "cvtsd2si eax",
        &[0xf2, 0x0f, 0x2d, 0xc1],
        "RAX",
        false,
        1,
        32,
        false,
    ),
    (
        "cvtss2si eax",
        &[0xf3, 0x0f, 0x2d, 0xc1],
        "RAX",
        true,
        1,
        32,
        false,
    ),
    (
        "cvttsd2si eax",
        &[0xf2, 0x0f, 0x2c, 0xc1],
        "RAX",
        false,
        1,
        32,
        true,
    ),
    (
        "cvttss2si eax",
        &[0xf3, 0x0f, 0x2c, 0xc1],
        "RAX",
        true,
        1,
        32,
        true,
    ),
    (
        "cvtsd2si rax",
        &[0xf2, 0x48, 0x0f, 0x2d, 0xc1],
        "RAX",
        false,
        1,
        64,
        false,
    ),
    (
        "cvtss2si rax",
        &[0xf3, 0x48, 0x0f, 0x2d, 0xc1],
        "RAX",
        true,
        1,
        64,
        false,
    ),
    (
        "cvttsd2si rax",
        &[0xf2, 0x48, 0x0f, 0x2c, 0xc1],
        "RAX",
        false,
        1,
        64,
        true,
    ),
    (
        "cvttss2si rax",
        &[0xf3, 0x48, 0x0f, 0x2c, 0xc1],
        "RAX",
        true,
        1,
        64,
        true,
    ),
    (
        "vcvtsd2si eax",
        &[0xc5, 0xfb, 0x2d, 0xc1],
        "RAX",
        false,
        1,
        32,
        false,
    ),
    (
        "vcvtss2si eax",
        &[0xc5, 0xfa, 0x2d, 0xc1],
        "RAX",
        true,
        1,
        32,
        false,
    ),
    (
        "vcvttsd2si eax",
        &[0xc5, 0xfb, 0x2c, 0xc1],
        "RAX",
        false,
        1,
        32,
        true,
    ),
    (
        "vcvttss2si eax",
        &[0xc5, 0xfa, 0x2c, 0xc1],
        "RAX",
        true,
        1,
        32,
        true,
    ),
    (
        "vcvtsd2si rax",
        &[0xc4, 0xe1, 0xfb, 0x2d, 0xc1],
        "RAX",
        false,
        1,
        64,
        false,
    ),
    (
        "vcvtss2si rax",
        &[0xc4, 0xe1, 0xfa, 0x2d, 0xc1],
        "RAX",
        true,
        1,
        64,
        false,
    ),
    (
        "vcvttsd2si rax",
        &[0xc4, 0xe1, 0xfb, 0x2c, 0xc1],
        "RAX",
        false,
        1,
        64,
        true,
    ),
    (
        "vcvttss2si rax",
        &[0xc4, 0xe1, 0xfa, 0x2c, 0xc1],
        "RAX",
        true,
        1,
        64,
        true,
    ),
    (
        "cvtpd2dq",
        &[0xf2, 0x0f, 0xe6, 0xc1],
        "XMM0",
        false,
        2,
        32,
        false,
    ),
    (
        "cvttpd2dq",
        &[0x66, 0x0f, 0xe6, 0xc1],
        "XMM0",
        false,
        2,
        32,
        true,
    ),
    (
        "cvtps2dq",
        &[0x66, 0x0f, 0x5b, 0xc1],
        "XMM0",
        true,
        4,
        32,
        false,
    ),
    (
        "cvttps2dq",
        &[0xf3, 0x0f, 0x5b, 0xc1],
        "XMM0",
        true,
        4,
        32,
        true,
    ),
    (
        "cvtpd2pi",
        &[0x66, 0x0f, 0x2d, 0xc1],
        "MM0",
        false,
        2,
        32,
        false,
    ),
    (
        "cvttpd2pi",
        &[0x66, 0x0f, 0x2c, 0xc1],
        "MM0",
        false,
        2,
        32,
        true,
    ),
    ("cvtps2pi", &[0x0f, 0x2d, 0xc1], "MM0", true, 2, 32, false),
    ("cvttps2pi", &[0x0f, 0x2c, 0xc1], "MM0", true, 2, 32, true),
];

#[test]
fn sse_and_vex_conversions_give_the_indefinite() {
    let none = AssumptionOptions::none();
    for (name, enc, ret, single, lanes, bits, trunc) in SSE {
        let mut code = enc.to_vec();
        code.push(RET);
        let l = lift(code, ret, &none);
        let xs: Vec<f64> = values()
            .into_iter()
            .map(|x| if single { f64::from(x as f32) } else { x })
            .collect();
        let width = if single { 32 } else { 64 };
        for rc in 0..4u32 {
            for group in xs.chunks(lanes as usize) {
                let xmm = group.iter().enumerate().fold(0u128, |acc, (k, &x)| {
                    let b = if single {
                        u128::from((x as f32).to_bits())
                    } else {
                        u128::from(x.to_bits())
                    };
                    acc | b << (width * k as u32)
                });
                let mxcsr = u128::from((0x1f80 & !0x6000) | (rc << 13));
                let got = int(&l.run(&[xmm, 0x037f, mxcsr], 0)[0]);
                let mode = if trunc {
                    Mode::Zero
                } else {
                    X86_MODES[rc as usize]
                };
                for (k, &x) in group.iter().enumerate() {
                    let lane = got >> (bits * k as u32) & ((1 << bits) - 1);
                    assert_eq!(
                        lane,
                        to_int(x, mode, bits),
                        "{name} rc {rc} lane {k} {x:e}: got {lane:#x}"
                    );
                }
            }
        }
    }
}

const FST_M32: [(&str, &[u8], bool); 2] = [
    ("fst m32", &[0xd9, 0x54, 0x24, 0xf8], true),
    ("fstp m32", &[0xd9, 0x5c, 0x24, 0xf8], false),
];

fn check_fst(l: &Lifted, rc: u32, mode: Mode, what: &str) {
    for b in common::round::narrowing_inputs(DOUBLE, SINGLE) {
        let cw = u128::from((0x037f & !0xc00) | (rc << 10));
        let got = int(&l.run(&[b, cw, 0x1f80], 0)[0]) & 0xffff_ffff;
        match SINGLE.convert(DOUBLE, b, mode) {
            Some(w) => assert_eq!(got, w, "{what} rc {rc} {b:#x}: got {got:#x}"),
            None => assert!(SINGLE.is_nan(got), "{what} {b:#x}: {got:#x} is no NaN"),
        }
    }
}

#[test]
fn fst_to_a_single_rounds_by_rc() {
    let none = AssumptionOptions::none();
    for (name, store, pop) in FST_M32 {
        let l = lift(x87(store, MOV_EAX, pop), "RAX", &none);
        for rc in 0..4 {
            check_fst(&l, rc, X86_MODES[rc as usize], name);
        }
    }
}

/// Under the claim `fst` reads RC as 0 whatever the control word holds;
/// `fist` reads it whatever the claim says.
#[test]
fn the_claim_rounds_fst_to_nearest_and_leaves_fist_alone() {
    let claims = AssumptionOptions::default();
    assert!(claims.fp_rounding_mode_is_nearest);
    for (name, store, pop) in FST_M32 {
        let l = lift(x87(store, MOV_EAX, pop), "RAX", &claims);
        for rc in 1..4 {
            check_fst(&l, rc, Mode::Even, name);
        }
    }
    let (name, store, load, pop, bits, _) = X87_STORES[1];
    let l = lift(x87(store, load, pop), "RAX", &claims);
    for rc in 0..4u32 {
        for x in [2.5f64, -2.5, 1.5, 0.7] {
            let cw = u128::from((0x037f & !0xc00) | (rc << 10));
            let got = int(&l.run(&[u128::from(x.to_bits()), cw, 0x1f80], 0)[0]) & ((1 << bits) - 1);
            assert_eq!(
                got,
                to_int(x, X86_MODES[rc as usize], bits),
                "{name} rc {rc} {x}"
            );
        }
    }
}

/// `x87RoundingControl` is the last user op on both x86 presets, so no
/// other id moved.
#[test]
fn the_x87_rounding_mode_op_is_the_last_user_op() {
    for arch in [Arch::X86, Arch::X64] {
        let (strider, _) = common::strider_over_bytes(arch, vec![0xc3], 0x1000, None);
        let names = strider.user_op_names();
        assert_eq!(
            names.last().map(String::as_str),
            Some("x87RoundingControl"),
            "{arch:?}"
        );
    }
}
