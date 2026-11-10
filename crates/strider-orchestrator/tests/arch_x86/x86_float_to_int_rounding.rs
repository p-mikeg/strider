//! x86 SSE and VEX float-to-integer conversions that round (`cvtsd2si`,
//! `cvtss2si`, their VEX forms, `cvtpd2dq`, `cvtps2dq`, `cvtpd2pi`,
//! `cvtps2pi`) round to nearest with ties to EVEN, the MXCSR.RC default
//! `fp_rounding_mode_is_nearest` claims (`sse_rounding_control.rs` drops the
//! claim). p-code `FLOAT_ROUND` alone
//! breaks a tie away from zero, so `2.5` must not become 3, and the packed
//! forms must round rather than truncate. A NaN or a result out of range is
//! the integer indefinite, the sign bit alone. The x87 stores, which read the
//! control word, are in `x87_rounding_control.rs`.
//!
//! Every expected value was checked by running the same bytes natively on
//! x86-64.
//!
//! The optimiser does not fold `FloatToInt`, so the returned value's cone is
//! evaluated by [`common::cone`].

use crate::common;

const BASE: u64 = 0x1000;
/// Where the operand vector sits, reached rip-relative.
const DATA: usize = 0x40;

/// `movdqu xmm0, [rip+disp32]`.
const LOAD_XMM0: &[u8] = &[0xf3, 0x0f, 0x6f, 0x05];
const MOVQ_RAX_XMM1: &[u8] = &[0x66, 0x48, 0x0f, 0x7e, 0xc8];
const PEXTRQ_RAX_XMM1_1: &[u8] = &[0x66, 0x48, 0x0f, 0x3a, 0x16, 0xc8, 0x01];
/// `movq rax, mm0` / `emms`.
const MOVQ_RAX_MM0_EMMS: &[u8] = &[0x48, 0x0f, 0x7e, 0xc0, 0x0f, 0x77];
const RET: u8 = 0xc3;

/// Ties both ways, each sign, plain fractions either side of a half, and
/// values whose nearest integer a 32-bit (or any) destination cannot hold.
const SCALARS: [f64; 19] = [
    2.5,
    -2.5,
    3.5,
    -3.5,
    0.5,
    -0.5,
    1.5,
    2.75,
    -2.75,
    1.25,
    -1.25,
    2.0,
    -7.0,
    2147483647.5,
    -2147483648.5,
    3e9,
    1e19,
    f64::NAN,
    f64::INFINITY,
];

/// `head`, a rip-relative disp32 reaching [`DATA`], then `tail`, placed at
/// `pos`.
fn rip_relative(head: &[u8], tail: &[u8], pos: usize) -> Vec<u8> {
    let next = pos + head.len() + 4 + tail.len();
    let disp = i32::try_from(DATA).unwrap() - i32::try_from(next).unwrap();
    let mut v = head.to_vec();
    v.extend_from_slice(&disp.to_le_bytes());
    v.extend_from_slice(tail);
    v
}

fn finish(mut code: Vec<u8>, data: &[u8]) -> Vec<u8> {
    code.push(RET);
    assert!(code.len() <= DATA, "code overruns the operand pool");
    code.resize(DATA, 0);
    code.extend_from_slice(data);
    code
}

/// How a conversion reads its source.
#[derive(Clone, Copy, Debug)]
enum Form {
    /// From `xmm0`, loaded from [`DATA`] first.
    Reg,
    /// Straight from [`DATA`].
    Mem,
}

/// One conversion: its register-source encoding, its memory-source encoding
/// as the bytes before the disp32, and how the result reaches `rax`.
struct Conversion {
    name: &'static str,
    reg: &'static [u8],
    mem: &'static [u8],
    extract: &'static [u8],
}

impl Conversion {
    fn image(&self, form: Form, data: &[u8]) -> Vec<u8> {
        let mut code = match form {
            Form::Reg => {
                let mut c = rip_relative(LOAD_XMM0, &[], 0);
                c.extend_from_slice(self.reg);
                c
            }
            Form::Mem => rip_relative(self.mem, &[], 0),
        };
        code.extend_from_slice(self.extract);
        finish(code, data)
    }
}

fn returned(bytes: Vec<u8>) -> u64 {
    common::cone::returned_rax(bytes, BASE)
}

/// `x` rounded as the hardware does by default.
fn nearest_even(x: f64) -> i64 {
    x.round_ties_even() as i64
}

/// `x` converted to a `width`-bit integer under the default mode: the
/// nearest even integer, or the indefinite when that does not fit.
fn converted(x: f64, width: u32) -> u64 {
    let indefinite = 1u64 << (width - 1);
    let r = x.round_ties_even();
    let bound = 2f64.powi(width as i32 - 1);
    if r.is_nan() || r < -bound || r >= bound {
        indefinite
    } else {
        (r as i64 as u64) & (u64::MAX >> (64 - width))
    }
}

fn low32(r: u64) -> i64 {
    i64::from(r as u32 as i32)
}

fn high32(r: u64) -> i64 {
    i64::from((r >> 32) as u32 as i32)
}

fn f64_data(lanes: &[f64]) -> Vec<u8> {
    let mut v: Vec<u8> = lanes.iter().flat_map(|x| x.to_le_bytes()).collect();
    v.resize(16, 0);
    v
}

fn f32_data(lanes: &[f32]) -> Vec<u8> {
    let mut v: Vec<u8> = lanes.iter().flat_map(|x| x.to_le_bytes()).collect();
    v.resize(16, 0);
    v
}

/// Scalar conversions into a 32- or 64-bit register.
fn check_scalar(ops: &[(Conversion, bool, bool)]) {
    let mut cases = Vec::new();
    for (op, double, wide) in ops {
        let width = if *wide { 64 } else { 32 };
        for x in SCALARS {
            // The value the instruction sees, after a float source's rounding.
            let (data, seen) = if *double {
                (f64_data(&[x]), x)
            } else {
                (f32_data(&[x as f32]), f64::from(x as f32))
            };
            for form in [Form::Reg, Form::Mem] {
                cases.push((op, width, x, seen, form, op.image(form, &data)));
            }
        }
    }
    let images: Vec<Vec<u8>> = cases.iter().map(|c| c.5.clone()).collect();
    let got = common::cone::returned_rax_each_under(&images, BASE, &Default::default());
    for (&(op, width, x, seen, form, _), r) in cases.iter().zip(got) {
        let r = r & (u64::MAX >> (64 - width));
        assert_eq!(r, converted(seen, width), "{} {form:?} {x}", op.name);
    }
}

const fn scalar(name: &'static str, reg: &'static [u8], mem: &'static [u8]) -> Conversion {
    Conversion {
        name,
        reg,
        mem,
        extract: &[],
    }
}

#[test]
fn sse_scalar_conversions_round_half_to_even() {
    check_scalar(&[
        (
            scalar(
                "cvtsd2si eax",
                &[0xf2, 0x0f, 0x2d, 0xc0],
                &[0xf2, 0x0f, 0x2d, 0x05],
            ),
            true,
            false,
        ),
        (
            scalar(
                "cvtsd2si rax",
                &[0xf2, 0x48, 0x0f, 0x2d, 0xc0],
                &[0xf2, 0x48, 0x0f, 0x2d, 0x05],
            ),
            true,
            true,
        ),
        (
            scalar(
                "cvtss2si eax",
                &[0xf3, 0x0f, 0x2d, 0xc0],
                &[0xf3, 0x0f, 0x2d, 0x05],
            ),
            false,
            false,
        ),
        (
            scalar(
                "cvtss2si rax",
                &[0xf3, 0x48, 0x0f, 0x2d, 0xc0],
                &[0xf3, 0x48, 0x0f, 0x2d, 0x05],
            ),
            false,
            true,
        ),
    ]);
}

#[test]
fn vex_scalar_conversions_round_half_to_even() {
    check_scalar(&[
        (
            scalar(
                "vcvtsd2si eax",
                &[0xc5, 0xfb, 0x2d, 0xc0],
                &[0xc5, 0xfb, 0x2d, 0x05],
            ),
            true,
            false,
        ),
        (
            scalar(
                "vcvtss2si eax",
                &[0xc5, 0xfa, 0x2d, 0xc0],
                &[0xc5, 0xfa, 0x2d, 0x05],
            ),
            false,
            false,
        ),
        (
            scalar(
                "vcvtss2si rax",
                &[0xc4, 0xe1, 0xfa, 0x2d, 0xc0],
                &[0xc4, 0xe1, 0xfa, 0x2d, 0x05],
            ),
            false,
            true,
        ),
    ]);
}

/// `vcvtsd2si r64` converts: the register receives the integer, not the
/// rounded double's bit pattern.
#[test]
fn vex_vcvtsd2si_into_a_64_bit_register_yields_the_integer() {
    check_scalar(&[(
        scalar(
            "vcvtsd2si rax",
            &[0xc4, 0xe1, 0xfb, 0x2d, 0xc0],
            &[0xc4, 0xe1, 0xfb, 0x2d, 0x05],
        ),
        true,
        true,
    )]);
}

/// Each lane rounds to nearest even; `cvtpd2dq` zeroes the upper two.
#[test]
fn packed_conversions_round_each_lane_half_to_even() {
    const CVTPD2DQ: (&[u8], &[u8]) = (&[0xf2, 0x0f, 0xe6, 0xc8], &[0xf2, 0x0f, 0xe6, 0x0d]);
    const CVTPS2DQ: (&[u8], &[u8]) = (&[0x66, 0x0f, 0x5b, 0xc8], &[0x66, 0x0f, 0x5b, 0x0d]);
    const CVTPD2PI: (&[u8], &[u8]) = (&[0x66, 0x0f, 0x2d, 0xc0], &[0x66, 0x0f, 0x2d, 0x05]);
    const CVTPS2PI: (&[u8], &[u8]) = (&[0x0f, 0x2d, 0xc0], &[0x0f, 0x2d, 0x05]);
    const PD: [f64; 2] = [2.5, -2.75];
    const PS: [[f32; 4]; 2] = [[2.5, -3.5, 0.5, 2.75], [-0.5, 1.5, -2.5, 3.5]];

    let lanes = |name, (reg, mem): (&'static [u8], &'static [u8]), extract, data: &[u8]| {
        let op = Conversion {
            name,
            reg,
            mem,
            extract,
        };
        [Form::Reg, Form::Mem].map(|form| {
            let r = returned(op.image(form, data));
            (form, [low32(r), high32(r)])
        })
    };
    let even = |xs: &[f64]| [nearest_even(xs[0]), nearest_even(xs[1])];

    for (form, got) in lanes("cvtpd2dq", CVTPD2DQ, MOVQ_RAX_XMM1, &f64_data(&PD)) {
        assert_eq!(got, even(&PD), "cvtpd2dq lanes 0-1 {form:?}");
    }
    for (form, got) in lanes("cvtpd2dq", CVTPD2DQ, PEXTRQ_RAX_XMM1_1, &f64_data(&PD)) {
        assert_eq!(got, [0, 0], "cvtpd2dq lanes 2-3 {form:?}");
    }
    for (form, got) in lanes("cvtpd2pi", CVTPD2PI, MOVQ_RAX_MM0_EMMS, &f64_data(&PD)) {
        assert_eq!(got, even(&PD), "cvtpd2pi {form:?}");
    }
    for ps in PS {
        let wide = ps.map(f64::from);
        for (form, got) in lanes("cvtps2dq", CVTPS2DQ, MOVQ_RAX_XMM1, &f32_data(&ps)) {
            assert_eq!(got, even(&wide[..2]), "cvtps2dq {ps:?} lanes 0-1 {form:?}");
        }
        for (form, got) in lanes("cvtps2dq", CVTPS2DQ, PEXTRQ_RAX_XMM1_1, &f32_data(&ps)) {
            assert_eq!(got, even(&wide[2..]), "cvtps2dq {ps:?} lanes 2-3 {form:?}");
        }
        for (form, got) in lanes("cvtps2pi", CVTPS2PI, MOVQ_RAX_MM0_EMMS, &f32_data(&ps)) {
            assert_eq!(got, even(&wide[..2]), "cvtps2pi {ps:?} {form:?}");
        }
    }
}
