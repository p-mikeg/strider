//! x86 float-to-integer conversions that round (`cvtsd2si`, `cvtss2si`, their
//! VEX forms, `cvtpd2dq`, `cvtps2dq`, `cvtpd2pi`, `cvtps2pi`, `fist`, `fistp`)
//! round to nearest with ties to EVEN, the MXCSR.RC / x87 CW.RC default.
//! p-code `FLOAT_ROUND` alone breaks a tie away from zero, so `2.5` must not
//! become 3, and the packed forms must round rather than truncate.
//!
//! Every expected value was checked by running the same bytes natively on
//! x86-64: they are `f64::round_ties_even` throughout.
//!
//! The optimiser does not fold `FloatToInt`, so the returned value's cone is
//! evaluated by [`common::cone`].

mod common;

const BASE: u64 = 0x1000;
/// Where the operand vector sits, reached rip-relative.
const DATA: usize = 0x40;

/// `movdqu xmm0, [rip+disp32]`.
const LOAD_XMM0: &[u8] = &[0xf3, 0x0f, 0x6f, 0x05];
/// `fld qword [rip+disp32]`.
const FLD_M64: &[u8] = &[0xdd, 0x05];
const MOVQ_RAX_XMM1: &[u8] = &[0x66, 0x48, 0x0f, 0x7e, 0xc8];
const PEXTRQ_RAX_XMM1_1: &[u8] = &[0x66, 0x48, 0x0f, 0x3a, 0x16, 0xc8, 0x01];
/// `movq rax, mm0` / `emms`.
const MOVQ_RAX_MM0_EMMS: &[u8] = &[0x48, 0x0f, 0x7e, 0xc0, 0x0f, 0x77];
const FSTP_ST0: &[u8] = &[0xdd, 0xd8];
const RET: u8 = 0xc3;

/// Ties both ways, each sign, and plain fractions either side of a half.
const SCALARS: [f64; 13] = [
    2.5, -2.5, 3.5, -3.5, 0.5, -0.5, 1.5, 2.75, -2.75, 1.25, -1.25, 2.0, -7.0,
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
    for (op, double, wide) in ops {
        for x in SCALARS {
            let data = if *double {
                f64_data(&[x])
            } else {
                f32_data(&[x as f32])
            };
            for form in [Form::Reg, Form::Mem] {
                let r = returned(op.image(form, &data));
                let got = if *wide { r as i64 } else { low32(r) };
                assert_eq!(got, nearest_even(x), "{} {form:?} {x}", op.name);
            }
        }
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

/// `fld qword [x]` / `<store> [rsp-16]` / `<load back>` / [`fstp st0`] /
/// `ret`.
fn x87_image(store: &[u8], load: &[u8], pop: bool, x: f64) -> Vec<u8> {
    let mut code = rip_relative(FLD_M64, &[], 0);
    code.extend_from_slice(store);
    code.extend_from_slice(load);
    if pop {
        code.extend_from_slice(FSTP_ST0);
    }
    finish(code, &f64_data(&[x]))
}

#[test]
fn x87_integer_stores_round_half_to_even() {
    const MOVSX_EAX_M16: &[u8] = &[0x0f, 0xbf, 0x44, 0x24, 0xf0];
    const MOV_EAX_M32: &[u8] = &[0x8b, 0x44, 0x24, 0xf0];
    const MOV_RAX_M64: &[u8] = &[0x48, 0x8b, 0x44, 0x24, 0xf0];
    /// (name, store to [rsp-16], load it back, `fstp st0` after a `fist`,
    /// 64-bit result)
    type Case = (&'static str, &'static [u8], &'static [u8], bool, bool);
    let cases: [Case; 5] = [
        (
            "fist m16",
            &[0xdf, 0x54, 0x24, 0xf0],
            MOVSX_EAX_M16,
            true,
            false,
        ),
        (
            "fist m32",
            &[0xdb, 0x54, 0x24, 0xf0],
            MOV_EAX_M32,
            true,
            false,
        ),
        (
            "fistp m16",
            &[0xdf, 0x5c, 0x24, 0xf0],
            MOVSX_EAX_M16,
            false,
            false,
        ),
        (
            "fistp m32",
            &[0xdb, 0x5c, 0x24, 0xf0],
            MOV_EAX_M32,
            false,
            false,
        ),
        (
            "fistp m64",
            &[0xdf, 0x7c, 0x24, 0xf0],
            MOV_RAX_M64,
            false,
            true,
        ),
    ];
    for (name, store, load, pop, wide) in cases {
        for x in SCALARS {
            let r = returned(x87_image(store, load, pop, x));
            let got = if wide { r as i64 } else { low32(r) };
            assert_eq!(got, nearest_even(x), "{name} {x}");
        }
    }
}
