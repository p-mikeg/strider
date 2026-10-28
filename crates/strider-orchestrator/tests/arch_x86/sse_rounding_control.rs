//! x86 SSE and VEX conversions that round by MXCSR.RC (bits 13-14: 0 nearest
//! even, 1 down, 2 up, 3 toward zero). Under `fp_rounding_mode_is_nearest` the
//! lift takes RC to be 0; without it each conversion reads RC and selects
//! among the four modes, so a mode `ldmxcsr` sets folds and the mode the
//! function was entered with stays a select on the initial MXCSR.
//!
//! Float-to-integer conversions follow `x87_rounding_control.rs`; integer to
//! float and double to single step the nearest answer one ulp toward the
//! mode's direction when it lies on the wrong side of the exact value.
//!
//! The optimiser does not fold `FloatToInt`, so each returned value's cone is
//! evaluated by [`common::cone`].

use crate::common;

use strider_orchestrator::opt::AssumptionOptions;

const BASE: u64 = 0x1000;
/// Where the operand sits, reached rip-relative.
const DATA: usize = 0x60;
const RET: u8 = 0xc3;

/// `stmxcsr [rsp-8]; and dword [rsp-8], ~0x6000; or dword [rsp-8], rc << 13;
/// ldmxcsr [rsp-8]`.
fn set_rc(rc: u32) -> Vec<u8> {
    let mut v = vec![0x0f, 0xae, 0x5c, 0x24, 0xf8];
    v.extend_from_slice(&[0x81, 0x64, 0x24, 0xf8, 0xff, 0x9f, 0xff, 0xff]);
    v.extend_from_slice(&[0x81, 0x4c, 0x24, 0xf8]);
    v.extend_from_slice(&(rc << 13).to_le_bytes());
    v.extend_from_slice(&[0x0f, 0xae, 0x54, 0x24, 0xf8]);
    v
}

/// `movq rax, xmm1`.
const MOVQ_RAX_XMM1: &[u8] = &[0x66, 0x48, 0x0f, 0x7e, 0xc8];
/// `movd eax, xmm1`.
const MOVD_EAX_XMM1: &[u8] = &[0x66, 0x0f, 0x7e, 0xc8];
/// `pextrq rax, xmm1, 1`.
const PEXTRQ_RAX_XMM1_1: &[u8] = &[0x66, 0x48, 0x0f, 0x3a, 0x16, 0xc8, 0x01];
/// `movq rax, mm1` / `emms`.
const MOVQ_RAX_MM1_EMMS: &[u8] = &[0x48, 0x0f, 0x7e, 0xc8, 0x0f, 0x77];

/// One conversion reading `[rip+disp32]` into `xmm1`, `mm1` or `eax`/`rax`:
/// the bytes before the disp32, and how the result reaches `rax`.
struct Op {
    name: &'static str,
    head: &'static [u8],
    extract: &'static [u8],
}

impl Op {
    /// The conversion of `data`, under `rc` set by `ldmxcsr`, or under the
    /// initial MXCSR when `rc` is `None`.
    fn image(&self, rc: Option<u32>, data: &[u8]) -> Vec<u8> {
        let mut code = rc.map(set_rc).unwrap_or_default();
        code.extend_from_slice(self.head);
        let next = code.len() + 4;
        let disp = i32::try_from(DATA).unwrap() - i32::try_from(next).unwrap();
        code.extend_from_slice(&disp.to_le_bytes());
        code.extend_from_slice(self.extract);
        code.push(RET);
        assert!(
            code.len() <= DATA,
            "{}: code overruns the operand",
            self.name
        );
        code.resize(DATA, 0);
        code.extend_from_slice(data);
        code
    }

    /// `rax` under each RC: once set by `ldmxcsr` (which must fold, so it
    /// may not depend on the initial registers), once read from the initial
    /// MXCSR.
    fn under(&self, rc: u32, data: &[u8]) -> [u64; 2] {
        let claims = AssumptionOptions::none();
        let set = common::cone::returned_rax_under(self.image(Some(rc), data), BASE, &claims);
        let init = u128::from(rc << 13);
        let entered =
            common::cone::returned_rax_from_under(self.image(None, data), BASE, init, &claims);
        [set, entered]
    }
}

fn rounded(rc: u32, x: f64) -> f64 {
    match rc {
        0 => x.round_ties_even(),
        1 => x.floor(),
        2 => x.ceil(),
        _ => x.trunc(),
    }
}

/// What a `width`-bit integer conversion of `x` under `rc` yields.
fn to_int(rc: u32, x: f64, width: u32) -> u64 {
    let r = rounded(rc, x);
    let bound = 2f64.powi(width as i32 - 1);
    if r.is_nan() || r < -bound || r >= bound {
        1 << (width - 1)
    } else {
        (r as i64 as u64) & (u64::MAX >> (64 - width))
    }
}

/// The float `rc` picks from the floats bracketing an exact value: `floor`
/// the greatest at or below it, `ceil` the least at or above, `nearest` the
/// nearest even. Toward zero takes `floor` for a non-negative value.
fn pick<F>(rc: u32, nearest: F, floor: F, ceil: F, neg: bool) -> F {
    match rc {
        0 => nearest,
        1 => floor,
        2 => ceil,
        _ if neg => ceil,
        _ => floor,
    }
}

/// `x` as a double under `rc`: the bracketing doubles found by comparing the
/// exact `x` with the nearest one and its neighbours.
fn i_to_f64(rc: u32, x: i64) -> f64 {
    let x = i128::from(x);
    let n = x as f64;
    let exact = |f: f64| f as i128;
    let floor = if exact(n) <= x { n } else { n.next_down() };
    let ceil = if exact(n) >= x { n } else { n.next_up() };
    assert!(exact(floor) <= x && x <= exact(ceil));
    pick(rc, n, floor, ceil, x < 0)
}

fn i_to_f32(rc: u32, x: i64) -> f32 {
    let x = i128::from(x);
    let n = x as f32;
    let exact = |f: f32| f as i128;
    let floor = if exact(n) <= x { n } else { n.next_down() };
    let ceil = if exact(n) >= x { n } else { n.next_up() };
    assert!(exact(floor) <= x && x <= exact(ceil));
    pick(rc, n, floor, ceil, x < 0)
}

/// `x` narrowed to a single under `rc`, bracketed in double precision, where
/// every single is exact.
fn narrowed(rc: u32, x: f64) -> f32 {
    let n = x as f32;
    if x.is_nan() {
        return n;
    }
    let floor = if f64::from(n) <= x { n } else { n.next_down() };
    let ceil = if f64::from(n) >= x { n } else { n.next_up() };
    pick(rc, n, floor, ceil, x < 0.0)
}

/// Ties each way and sign, fractions either side of a half, and values no
/// 32-bit destination holds.
const DOUBLES: [f64; 14] = [
    2.5,
    -2.5,
    3.5,
    -3.5,
    0.5,
    -0.5,
    2.7,
    -2.7,
    0.4,
    -0.4,
    2147483647.5,
    -2147483648.5,
    3e9,
    f64::NAN,
];

/// `(name, head, wide)`: scalar double-to-integer conversions from memory
/// into `eax` (32-bit) or `rax`.
const SD2SI: [(&str, &[u8], bool); 4] = [
    ("cvtsd2si eax", &[0xf2, 0x0f, 0x2d, 0x05], false),
    ("cvtsd2si rax", &[0xf2, 0x48, 0x0f, 0x2d, 0x05], true),
    ("vcvtsd2si eax", &[0xc5, 0xfb, 0x2d, 0x05], false),
    ("vcvtsd2si rax", &[0xc4, 0xe1, 0xfb, 0x2d, 0x05], true),
];

const SS2SI: [(&str, &[u8], bool); 4] = [
    ("cvtss2si eax", &[0xf3, 0x0f, 0x2d, 0x05], false),
    ("cvtss2si rax", &[0xf3, 0x48, 0x0f, 0x2d, 0x05], true),
    ("vcvtss2si eax", &[0xc5, 0xfa, 0x2d, 0x05], false),
    ("vcvtss2si rax", &[0xc4, 0xe1, 0xfa, 0x2d, 0x05], true),
];

#[test]
fn scalar_float_to_int_conversions_round_by_mxcsr() {
    for (double, table) in [(true, &SD2SI), (false, &SS2SI)] {
        for &(name, head, wide) in table {
            let op = Op {
                name,
                head,
                extract: &[],
            };
            let width = if wide { 64 } else { 32 };
            for x in DOUBLES {
                let (data, seen) = if double {
                    (x.to_le_bytes().to_vec(), x)
                } else {
                    let s = x as f32;
                    (s.to_le_bytes().to_vec(), f64::from(s))
                };
                for rc in 0..4 {
                    for (how, got) in ["ldmxcsr", "initial"].into_iter().zip(op.under(rc, &data)) {
                        let got = got & (u64::MAX >> (64 - width));
                        assert_eq!(got, to_int(rc, seen, width), "{name} {how} rc {rc} {x}");
                    }
                }
            }
        }
    }
}

#[test]
fn packed_float_to_int_conversions_round_by_mxcsr() {
    let pd = [2.5f64, -2.7];
    let ps = [2.5f32, -3.5, 0.4, -0.5];
    let pd_data: Vec<u8> = pd.iter().flat_map(|x| x.to_le_bytes()).collect();
    let ps_data: Vec<u8> = ps.iter().flat_map(|x| x.to_le_bytes()).collect();
    let lanes = |r: u64| [r & 0xffff_ffff, r >> 32];
    for rc in 0..4 {
        let want = |xs: &[f64]| [to_int(rc, xs[0], 32), to_int(rc, xs[1], 32)];
        let ps_wide = ps.map(f64::from);
        let cases: [(Op, &[u8], [u64; 2]); 5] = [
            (
                Op {
                    name: "cvtpd2dq",
                    head: &[0xf2, 0x0f, 0xe6, 0x0d],
                    extract: MOVQ_RAX_XMM1,
                },
                &pd_data,
                want(&pd),
            ),
            (
                Op {
                    name: "cvtpd2pi",
                    head: &[0x66, 0x0f, 0x2d, 0x0d],
                    extract: MOVQ_RAX_MM1_EMMS,
                },
                &pd_data,
                want(&pd),
            ),
            (
                Op {
                    name: "cvtps2dq lanes 0-1",
                    head: &[0x66, 0x0f, 0x5b, 0x0d],
                    extract: MOVQ_RAX_XMM1,
                },
                &ps_data,
                want(&ps_wide[..2]),
            ),
            (
                Op {
                    name: "cvtps2dq lanes 2-3",
                    head: &[0x66, 0x0f, 0x5b, 0x0d],
                    extract: PEXTRQ_RAX_XMM1_1,
                },
                &ps_data,
                want(&ps_wide[2..]),
            ),
            (
                Op {
                    name: "cvtps2pi",
                    head: &[0x0f, 0x2d, 0x0d],
                    extract: MOVQ_RAX_MM1_EMMS,
                },
                &ps_data,
                want(&ps_wide[..2]),
            ),
        ];
        for (op, data, want) in &cases {
            for (how, got) in ["ldmxcsr", "initial"].into_iter().zip(op.under(rc, data)) {
                assert_eq!(lanes(got), *want, "{} {how} rc {rc}", op.name);
            }
        }
    }
}

/// Integers past each float's exact range, ties of each sign, and the
/// extremes, whose nearest single or double can be `2^(n-1)`, past the range.
const I64S: [i64; 10] = [
    0,
    -1,
    (1 << 53) + 1,
    (1 << 53) + 3,
    -(1 << 53) - 1,
    (1 << 62) + 1,
    12_345_678_901_234_567,
    -12_345_678_901_234_567,
    i64::MAX,
    i64::MIN,
];

const I32S: [i32; 8] = [
    0,
    -1,
    (1 << 24) + 1,
    (1 << 24) + 3,
    -(1 << 24) - 1,
    123_456_789,
    i32::MAX,
    i32::MIN,
];

#[test]
fn scalar_int_to_float_conversions_round_by_mxcsr() {
    let to_sd = |name, head| Op {
        name,
        head,
        extract: MOVQ_RAX_XMM1,
    };
    for op in [to_sd("cvtsi2sd xmm1, r64", &[0xf2, 0x48, 0x0f, 0x2a, 0x0d])] {
        for x in I64S {
            for rc in 0..4 {
                for (how, got) in ["ldmxcsr", "initial"]
                    .into_iter()
                    .zip(op.under(rc, &x.to_le_bytes()))
                {
                    let want = i_to_f64(rc, x);
                    assert_eq!(
                        got,
                        want.to_bits(),
                        "{} {how} rc {rc} {x}: want {want}",
                        op.name
                    );
                }
            }
        }
    }
    for (name, head) in [("cvtsi2ss xmm1, r64", &[0xf3, 0x48, 0x0f, 0x2a, 0x0d][..])] {
        let op = Op {
            name,
            head,
            extract: MOVD_EAX_XMM1,
        };
        for x in I64S {
            for rc in 0..4 {
                for (how, got) in ["ldmxcsr", "initial"]
                    .into_iter()
                    .zip(op.under(rc, &x.to_le_bytes()))
                {
                    let want = i_to_f32(rc, x);
                    assert_eq!(
                        got & 0xffff_ffff,
                        u64::from(want.to_bits()),
                        "{name} {how} rc {rc} {x}"
                    );
                }
            }
        }
    }
    for (name, head) in [("cvtsi2ss xmm1, r32", &[0xf3, 0x0f, 0x2a, 0x0d][..])] {
        let op = Op {
            name,
            head,
            extract: MOVD_EAX_XMM1,
        };
        for x in I32S {
            for rc in 0..4 {
                for (how, got) in ["ldmxcsr", "initial"]
                    .into_iter()
                    .zip(op.under(rc, &x.to_le_bytes()))
                {
                    let want = i_to_f32(rc, i64::from(x));
                    assert_eq!(
                        got & 0xffff_ffff,
                        u64::from(want.to_bits()),
                        "{name} {how} rc {rc} {x}"
                    );
                }
            }
        }
    }
}

#[test]
fn packed_int_to_float_conversions_round_by_mxcsr() {
    let lanes: [i32; 4] = [(1 << 24) + 1, -(1 << 24) - 3, i32::MAX, 7];
    let data: Vec<u8> = lanes.iter().flat_map(|x| x.to_le_bytes()).collect();
    for rc in 0..4 {
        let want = |xs: &[i32]| {
            u64::from(i_to_f32(rc, i64::from(xs[0])).to_bits())
                | u64::from(i_to_f32(rc, i64::from(xs[1])).to_bits()) << 32
        };
        for (op, want) in [
            (
                Op {
                    name: "cvtdq2ps lanes 0-1",
                    head: &[0x0f, 0x5b, 0x0d],
                    extract: MOVQ_RAX_XMM1,
                },
                want(&lanes[..2]),
            ),
            (
                Op {
                    name: "cvtdq2ps lanes 2-3",
                    head: &[0x0f, 0x5b, 0x0d],
                    extract: PEXTRQ_RAX_XMM1_1,
                },
                want(&lanes[2..]),
            ),
            (
                Op {
                    name: "cvtpi2ps",
                    head: &[0x0f, 0x2a, 0x0d],
                    extract: MOVQ_RAX_XMM1,
                },
                want(&lanes[..2]),
            ),
        ] {
            for (how, got) in ["ldmxcsr", "initial"].into_iter().zip(op.under(rc, &data)) {
                assert_eq!(got, want, "{} {how} rc {rc}", op.name);
            }
        }
    }
}

/// Doubles between two singles, a tie, the edges of the single range and
/// values that underflow it.
const NARROWED: [f64; 12] = [
    1.0 + 1.0 / (1u64 << 30) as f64,
    -1.0 - 1.0 / (1u64 << 30) as f64,
    1.0 + 1.0 / (1u64 << 24) as f64,
    0.1,
    -0.1,
    1e39,
    -1e39,
    1e-46,
    -1e-46,
    0.5,
    -0.0,
    f64::INFINITY,
];

#[test]
fn double_to_single_conversions_round_by_mxcsr() {
    let movd = MOVD_EAX_XMM1;
    for (name, head, extract, two) in [
        ("cvtsd2ss", &[0xf2, 0x0f, 0x5a, 0x0d][..], movd, false),
        (
            "cvtpd2ps",
            &[0x66, 0x0f, 0x5a, 0x0d][..],
            MOVQ_RAX_XMM1,
            true,
        ),
    ] {
        let op = Op {
            name,
            head,
            extract,
        };
        for x in NARROWED {
            // The packed form's second lane holds the negation.
            let mut data = x.to_le_bytes().to_vec();
            data.extend_from_slice(&(-x).to_le_bytes());
            for rc in 0..4 {
                let mut want = u64::from(narrowed(rc, x).to_bits());
                if two {
                    want |= u64::from(narrowed(rc, -x).to_bits()) << 32;
                }
                for (how, got) in ["ldmxcsr", "initial"].into_iter().zip(op.under(rc, &data)) {
                    let got = if two { got } else { got & 0xffff_ffff };
                    assert_eq!(got, want, "{name} {how} rc {rc} {x}");
                }
            }
        }
    }
}

/// Under the claim the lift reads RC as 0 whatever MXCSR holds: an `ldmxcsr`
/// to round down leaves `cvtsd2si` of 2.7 at 3.
#[test]
fn the_claim_rounds_to_nearest_whatever_mxcsr_holds() {
    let op = Op {
        name: "cvtsd2si eax",
        head: &[0xf2, 0x0f, 0x2d, 0x05],
        extract: &[],
    };
    let data = 2.7f64.to_le_bytes();
    let claims = AssumptionOptions::default();
    assert!(claims.fp_rounding_mode_is_nearest);
    let got = common::cone::returned_rax_under(op.image(Some(1), &data), BASE, &claims);
    assert_eq!(got & 0xffff_ffff, 3);
    assert_eq!(
        op.under(1, &data),
        [2, 2],
        "rounding down without the claim"
    );
}

/// The VEX forms writing an XMM register share the legacy forms' macros. Their
/// result passes through the 512-bit ZMM register, which the cone evaluator
/// does not carry, so each is checked to read MXCSR exactly when the claim is
/// off.
#[test]
fn vex_conversions_into_xmm_read_mxcsr_only_without_the_claim() {
    for (name, head, data) in [
        (
            "vcvtsi2sd xmm1, r64",
            &[0xc4, 0xe1, 0xf3, 0x2a, 0x0d][..],
            &[1u8; 8][..],
        ),
        (
            "vcvtsi2ss xmm1, r64",
            &[0xc4, 0xe1, 0xf2, 0x2a, 0x0d][..],
            &[1u8; 8][..],
        ),
        (
            "vcvtsi2ss xmm1, r32",
            &[0xc5, 0xf2, 0x2a, 0x0d][..],
            &[1u8; 8][..],
        ),
        (
            "vcvtsd2ss",
            &[0xc5, 0xf3, 0x5a, 0x0d][..],
            &0.1f64.to_le_bytes()[..],
        ),
    ] {
        let op = Op {
            name,
            head,
            extract: MOVQ_RAX_XMM1,
        };
        for (claims, reads) in [
            (AssumptionOptions::none(), true),
            (AssumptionOptions::default(), false),
        ] {
            let bytes = op.image(None, data);
            let rom: Box<dyn strider_orchestrator::opt::ReadOnlyMemory> = Box::new(
                strider_ir_test_utils::MockRom::raw_bytes(BASE, bytes.clone()),
            );
            let (mut strider, cc) =
                common::strider_over_bytes(common::Arch::X64, bytes, BASE, Some(rom));
            let mxcsr = strider.sleigh_regs().name_to_vn("MXCSR").expect("MXCSR");
            let opts = strider_orchestrator::AnalyzeOptions {
                assumptions: claims,
                ..Default::default()
            };
            let f = strider
                .analyze(BASE, &cc, &opts, None)
                .expect("analyze")
                .function;
            assert_eq!(
                f.initial_var_value(&mxcsr).is_some(),
                reads,
                "{name}: reads MXCSR\n{}",
                f.to_text(false)
            );
        }
    }
}
