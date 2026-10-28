//! AArch64 float-to-integer conversions (Arm ARM DDI0487 C7.2, `FPToFixed`):
//! `fcvt{n,a,m,p,z}{s,u}` round to nearest even, to nearest away from zero,
//! toward -inf, toward +inf and toward zero, whatever FPCR.RMode holds; the
//! `#fbits` forms scale by 2^fbits first. A result past the destination's
//! range saturates to its bound, and a NaN gives 0. `fjcvtzs` truncates
//! modulo 2^32 and sets Z alone of NZCV when the result is exact.
//!
//! Each function is `ldr {s,d,q}1, <lit>; <conv>; [move <dest> to x0];
//! ret`, the literal 48 bytes in, at a 64-byte stride; every encoding is
//! llvm-mc's (`-mattr=+v8.3a,+fullfp16,+jsconv`).

use crate::common;

use crate::common::Arch;
use crate::common::cone::f16_to_f64;
use strider_orchestrator::opt::AssumptionOptions;

const BASE: u64 = 0x10000;
const STRIDE: usize = 64;
const LIT: usize = 48;
const RET: u32 = 0xd65f_03c0;

const GPR: &[(u32, &str)] = &[
    (0x1ee00020, "fcvtns w0, h1"),
    (0x1e200020, "fcvtns w0, s1"),
    (0x1e600020, "fcvtns w0, d1"),
    (0x9ee00020, "fcvtns x0, h1"),
    (0x9e200020, "fcvtns x0, s1"),
    (0x9e600020, "fcvtns x0, d1"),
    (0x1ee10020, "fcvtnu w0, h1"),
    (0x1e210020, "fcvtnu w0, s1"),
    (0x1e610020, "fcvtnu w0, d1"),
    (0x9ee10020, "fcvtnu x0, h1"),
    (0x9e210020, "fcvtnu x0, s1"),
    (0x9e610020, "fcvtnu x0, d1"),
    (0x1ee40020, "fcvtas w0, h1"),
    (0x1e240020, "fcvtas w0, s1"),
    (0x1e640020, "fcvtas w0, d1"),
    (0x9ee40020, "fcvtas x0, h1"),
    (0x9e240020, "fcvtas x0, s1"),
    (0x9e640020, "fcvtas x0, d1"),
    (0x1ee50020, "fcvtau w0, h1"),
    (0x1e250020, "fcvtau w0, s1"),
    (0x1e650020, "fcvtau w0, d1"),
    (0x9ee50020, "fcvtau x0, h1"),
    (0x9e250020, "fcvtau x0, s1"),
    (0x9e650020, "fcvtau x0, d1"),
    (0x1ef00020, "fcvtms w0, h1"),
    (0x1e300020, "fcvtms w0, s1"),
    (0x1e700020, "fcvtms w0, d1"),
    (0x9ef00020, "fcvtms x0, h1"),
    (0x9e300020, "fcvtms x0, s1"),
    (0x9e700020, "fcvtms x0, d1"),
    (0x1ef10020, "fcvtmu w0, h1"),
    (0x1e310020, "fcvtmu w0, s1"),
    (0x1e710020, "fcvtmu w0, d1"),
    (0x9ef10020, "fcvtmu x0, h1"),
    (0x9e310020, "fcvtmu x0, s1"),
    (0x9e710020, "fcvtmu x0, d1"),
    (0x1ee80020, "fcvtps w0, h1"),
    (0x1e280020, "fcvtps w0, s1"),
    (0x1e680020, "fcvtps w0, d1"),
    (0x9ee80020, "fcvtps x0, h1"),
    (0x9e280020, "fcvtps x0, s1"),
    (0x9e680020, "fcvtps x0, d1"),
    (0x1ee90020, "fcvtpu w0, h1"),
    (0x1e290020, "fcvtpu w0, s1"),
    (0x1e690020, "fcvtpu w0, d1"),
    (0x9ee90020, "fcvtpu x0, h1"),
    (0x9e290020, "fcvtpu x0, s1"),
    (0x9e690020, "fcvtpu x0, d1"),
    (0x1ef80020, "fcvtzs w0, h1"),
    (0x1e380020, "fcvtzs w0, s1"),
    (0x1e780020, "fcvtzs w0, d1"),
    (0x9ef80020, "fcvtzs x0, h1"),
    (0x9e380020, "fcvtzs x0, s1"),
    (0x9e780020, "fcvtzs x0, d1"),
    (0x1ef90020, "fcvtzu w0, h1"),
    (0x1e390020, "fcvtzu w0, s1"),
    (0x1e790020, "fcvtzu w0, d1"),
    (0x9ef90020, "fcvtzu x0, h1"),
    (0x9e390020, "fcvtzu x0, s1"),
    (0x9e790020, "fcvtzu x0, d1"),
];

const SCALAR: &[(u32, &str)] = &[
    (0x5e79a820, "fcvtns h0, h1"),
    (0x5e21a820, "fcvtns s0, s1"),
    (0x5e61a820, "fcvtns d0, d1"),
    (0x7e79a820, "fcvtnu h0, h1"),
    (0x7e21a820, "fcvtnu s0, s1"),
    (0x7e61a820, "fcvtnu d0, d1"),
    (0x5e79c820, "fcvtas h0, h1"),
    (0x5e21c820, "fcvtas s0, s1"),
    (0x5e61c820, "fcvtas d0, d1"),
    (0x7e79c820, "fcvtau h0, h1"),
    (0x7e21c820, "fcvtau s0, s1"),
    (0x7e61c820, "fcvtau d0, d1"),
    (0x5e79b820, "fcvtms h0, h1"),
    (0x5e21b820, "fcvtms s0, s1"),
    (0x5e61b820, "fcvtms d0, d1"),
    (0x7e79b820, "fcvtmu h0, h1"),
    (0x7e21b820, "fcvtmu s0, s1"),
    (0x7e61b820, "fcvtmu d0, d1"),
    (0x5ef9a820, "fcvtps h0, h1"),
    (0x5ea1a820, "fcvtps s0, s1"),
    (0x5ee1a820, "fcvtps d0, d1"),
    (0x7ef9a820, "fcvtpu h0, h1"),
    (0x7ea1a820, "fcvtpu s0, s1"),
    (0x7ee1a820, "fcvtpu d0, d1"),
    (0x5ef9b820, "fcvtzs h0, h1"),
    (0x5ea1b820, "fcvtzs s0, s1"),
    (0x5ee1b820, "fcvtzs d0, d1"),
    (0x7ef9b820, "fcvtzu h0, h1"),
    (0x7ea1b820, "fcvtzu s0, s1"),
    (0x7ee1b820, "fcvtzu d0, d1"),
];

const VECTOR: &[(u32, &str)] = &[
    (0x0e79a820, "fcvtns v0.4h, v1.4h"),
    (0x4e79a820, "fcvtns v0.8h, v1.8h"),
    (0x0e21a820, "fcvtns v0.2s, v1.2s"),
    (0x4e21a820, "fcvtns v0.4s, v1.4s"),
    (0x4e61a820, "fcvtns v0.2d, v1.2d"),
    (0x2e79a820, "fcvtnu v0.4h, v1.4h"),
    (0x6e79a820, "fcvtnu v0.8h, v1.8h"),
    (0x2e21a820, "fcvtnu v0.2s, v1.2s"),
    (0x6e21a820, "fcvtnu v0.4s, v1.4s"),
    (0x6e61a820, "fcvtnu v0.2d, v1.2d"),
    (0x0e79c820, "fcvtas v0.4h, v1.4h"),
    (0x4e79c820, "fcvtas v0.8h, v1.8h"),
    (0x0e21c820, "fcvtas v0.2s, v1.2s"),
    (0x4e21c820, "fcvtas v0.4s, v1.4s"),
    (0x4e61c820, "fcvtas v0.2d, v1.2d"),
    (0x2e79c820, "fcvtau v0.4h, v1.4h"),
    (0x6e79c820, "fcvtau v0.8h, v1.8h"),
    (0x2e21c820, "fcvtau v0.2s, v1.2s"),
    (0x6e21c820, "fcvtau v0.4s, v1.4s"),
    (0x6e61c820, "fcvtau v0.2d, v1.2d"),
    (0x0e79b820, "fcvtms v0.4h, v1.4h"),
    (0x4e79b820, "fcvtms v0.8h, v1.8h"),
    (0x0e21b820, "fcvtms v0.2s, v1.2s"),
    (0x4e21b820, "fcvtms v0.4s, v1.4s"),
    (0x4e61b820, "fcvtms v0.2d, v1.2d"),
    (0x2e79b820, "fcvtmu v0.4h, v1.4h"),
    (0x6e79b820, "fcvtmu v0.8h, v1.8h"),
    (0x2e21b820, "fcvtmu v0.2s, v1.2s"),
    (0x6e21b820, "fcvtmu v0.4s, v1.4s"),
    (0x6e61b820, "fcvtmu v0.2d, v1.2d"),
    (0x0ef9a820, "fcvtps v0.4h, v1.4h"),
    (0x4ef9a820, "fcvtps v0.8h, v1.8h"),
    (0x0ea1a820, "fcvtps v0.2s, v1.2s"),
    (0x4ea1a820, "fcvtps v0.4s, v1.4s"),
    (0x4ee1a820, "fcvtps v0.2d, v1.2d"),
    (0x2ef9a820, "fcvtpu v0.4h, v1.4h"),
    (0x6ef9a820, "fcvtpu v0.8h, v1.8h"),
    (0x2ea1a820, "fcvtpu v0.2s, v1.2s"),
    (0x6ea1a820, "fcvtpu v0.4s, v1.4s"),
    (0x6ee1a820, "fcvtpu v0.2d, v1.2d"),
    (0x0ef9b820, "fcvtzs v0.4h, v1.4h"),
    (0x4ef9b820, "fcvtzs v0.8h, v1.8h"),
    (0x0ea1b820, "fcvtzs v0.2s, v1.2s"),
    (0x4ea1b820, "fcvtzs v0.4s, v1.4s"),
    (0x4ee1b820, "fcvtzs v0.2d, v1.2d"),
    (0x2ef9b820, "fcvtzu v0.4h, v1.4h"),
    (0x6ef9b820, "fcvtzu v0.8h, v1.8h"),
    (0x2ea1b820, "fcvtzu v0.2s, v1.2s"),
    (0x6ea1b820, "fcvtzu v0.4s, v1.4s"),
    (0x6ee1b820, "fcvtzu v0.2d, v1.2d"),
];

const FIXED: &[(u32, &str)] = &[
    (0x1ed8fc20, "fcvtzs w0, h1, #1"),
    (0x1ed8f420, "fcvtzs w0, h1, #3"),
    (0x1ed88020, "fcvtzs w0, h1, #32"),
    (0x1e18fc20, "fcvtzs w0, s1, #1"),
    (0x1e18f420, "fcvtzs w0, s1, #3"),
    (0x1e188020, "fcvtzs w0, s1, #32"),
    (0x1e58fc20, "fcvtzs w0, d1, #1"),
    (0x1e58f420, "fcvtzs w0, d1, #3"),
    (0x1e588020, "fcvtzs w0, d1, #32"),
    (0x9ed8fc20, "fcvtzs x0, h1, #1"),
    (0x9ed8f420, "fcvtzs x0, h1, #3"),
    (0x9ed80020, "fcvtzs x0, h1, #64"),
    (0x9e18fc20, "fcvtzs x0, s1, #1"),
    (0x9e18f420, "fcvtzs x0, s1, #3"),
    (0x9e180020, "fcvtzs x0, s1, #64"),
    (0x9e58fc20, "fcvtzs x0, d1, #1"),
    (0x9e58f420, "fcvtzs x0, d1, #3"),
    (0x9e580020, "fcvtzs x0, d1, #64"),
    (0x5f1ffc20, "fcvtzs h0, h1, #1"),
    (0x5f1dfc20, "fcvtzs h0, h1, #3"),
    (0x5f10fc20, "fcvtzs h0, h1, #16"),
    (0x5f3ffc20, "fcvtzs s0, s1, #1"),
    (0x5f3dfc20, "fcvtzs s0, s1, #3"),
    (0x5f20fc20, "fcvtzs s0, s1, #32"),
    (0x5f7ffc20, "fcvtzs d0, d1, #1"),
    (0x5f7dfc20, "fcvtzs d0, d1, #3"),
    (0x5f40fc20, "fcvtzs d0, d1, #64"),
    (0x1ed9fc20, "fcvtzu w0, h1, #1"),
    (0x1ed9f420, "fcvtzu w0, h1, #3"),
    (0x1ed98020, "fcvtzu w0, h1, #32"),
    (0x1e19fc20, "fcvtzu w0, s1, #1"),
    (0x1e19f420, "fcvtzu w0, s1, #3"),
    (0x1e198020, "fcvtzu w0, s1, #32"),
    (0x1e59fc20, "fcvtzu w0, d1, #1"),
    (0x1e59f420, "fcvtzu w0, d1, #3"),
    (0x1e598020, "fcvtzu w0, d1, #32"),
    (0x9ed9fc20, "fcvtzu x0, h1, #1"),
    (0x9ed9f420, "fcvtzu x0, h1, #3"),
    (0x9ed90020, "fcvtzu x0, h1, #64"),
    (0x9e19fc20, "fcvtzu x0, s1, #1"),
    (0x9e19f420, "fcvtzu x0, s1, #3"),
    (0x9e190020, "fcvtzu x0, s1, #64"),
    (0x9e59fc20, "fcvtzu x0, d1, #1"),
    (0x9e59f420, "fcvtzu x0, d1, #3"),
    (0x9e590020, "fcvtzu x0, d1, #64"),
    (0x7f1ffc20, "fcvtzu h0, h1, #1"),
    (0x7f1dfc20, "fcvtzu h0, h1, #3"),
    (0x7f10fc20, "fcvtzu h0, h1, #16"),
    (0x7f3ffc20, "fcvtzu s0, s1, #1"),
    (0x7f3dfc20, "fcvtzu s0, s1, #3"),
    (0x7f20fc20, "fcvtzu s0, s1, #32"),
    (0x7f7ffc20, "fcvtzu d0, d1, #1"),
    (0x7f7dfc20, "fcvtzu d0, d1, #3"),
    (0x7f40fc20, "fcvtzu d0, d1, #64"),
];

/// Doubles at each rounding's ties and sign, every destination's bounds and
/// the doubles either side of them, and values none holds.
const VALUES: [f64; 40] = [
    0.0,
    -0.0,
    0.5,
    -0.5,
    1.5,
    -1.5,
    2.5,
    -2.5,
    3.5,
    2.7,
    -2.7,
    5e-324,
    -5e-324,
    32767.5,
    -32768.5,
    65535.5,
    65536.0,
    2_147_483_647.5,
    2_147_483_648.0,
    -2_147_483_648.5,
    -2_147_483_649.0,
    4_294_967_295.5,
    4_294_967_296.0,
    3e9,
    -3e9,
    9_223_372_036_854_774_784.0,
    9_223_372_036_854_775_808.0,
    -9_223_372_036_854_775_808.0,
    -9_223_372_036_854_777_856.0,
    18_446_744_073_709_549_568.0,
    18_446_744_073_709_551_616.0,
    1e19,
    -1e19,
    1e300,
    -1e300,
    f64::NAN,
    -f64::NAN,
    f64::INFINITY,
    f64::NEG_INFINITY,
    1.0e-3,
];

/// Halves at the same points: ties, the smallest denormals, the 16-bit
/// bounds (32768 and -32800), the largest finite, infinities and NaNs.
const HALVES: [u16; 24] = [
    0x0000, 0x8000, 0x3800, 0xb800, 0x3e00, 0xbe00, 0x4100, 0xc100, 0x4300, 0x4166, 0xc166, 0x0001,
    0x8001, 0x7800, 0x77ff, 0xf800, 0xf801, 0x7bff, 0xfbff, 0x7c00, 0xfc00, 0x7e00, 0xfe00, 0x1400,
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Prec {
    H,
    S,
    D,
}

impl Prec {
    fn of(reg: &str) -> Self {
        match reg.as_bytes()[0] {
            b'h' => Self::H,
            b's' => Self::S,
            b'd' => Self::D,
            _ => panic!("{reg} is no float register"),
        }
    }

    fn bits(self) -> u32 {
        match self {
            Self::H => 16,
            Self::S => 32,
            Self::D => 64,
        }
    }

    /// The literal load into v1: `ldr s1` carries a half in its low bits.
    fn load(self) -> u32 {
        match self {
            Self::H | Self::S => 0x1c00_0181,
            Self::D => 0x5c00_0181,
        }
    }

    /// The inputs at this precision, as bit patterns.
    fn inputs(self) -> Vec<u64> {
        match self {
            Self::H => HALVES.iter().map(|&b| u64::from(b)).collect(),
            Self::S => VALUES
                .iter()
                .map(|&x| u64::from((x as f32).to_bits()))
                .collect(),
            Self::D => VALUES.iter().map(|&x| x.to_bits()).collect(),
        }
    }

    fn value(self, bits: u64) -> f64 {
        match self {
            Self::H => f16_to_f64(bits as u16),
            Self::S => f64::from(f32::from_bits(bits as u32)),
            Self::D => f64::from_bits(bits),
        }
    }
}

/// A mnemonic's rounding (FPRounding: 0 nearest even, 1 toward +inf, 2
/// toward -inf, 3 toward zero, 4 nearest away) and signedness.
#[derive(Clone, Copy, Debug)]
struct Conv {
    rounding: u8,
    unsigned: bool,
}

impl Conv {
    fn of(mnemonic: &str) -> Self {
        let rounding = match &mnemonic[4..5] {
            "n" => 0,
            "p" => 1,
            "m" => 2,
            "z" => 3,
            "a" => 4,
            m => panic!("fcvt{m}?"),
        };
        Self {
            rounding,
            unsigned: mnemonic.ends_with('u'),
        }
    }
}

/// `FPToFixed(x, fbits, unsigned, rounding)` in the low `bits`.
fn fp_to_fixed(x: f64, fbits: u32, conv: Conv, bits: u32) -> u64 {
    if x.is_nan() {
        return 0;
    }
    // Exact: a power-of-two scale, and an overflow saturates either way.
    let v = x * 2f64.powi(i32::try_from(fbits).unwrap());
    let r = match conv.rounding {
        0 => v.round_ties_even(),
        1 => v.ceil(),
        2 => v.floor(),
        3 => v.trunc(),
        _ => v.round(),
    };
    let (lo, hi): (i128, i128) = if conv.unsigned {
        (0, (1 << bits) - 1)
    } else {
        (-(1 << (bits - 1)), (1 << (bits - 1)) - 1)
    };
    // An infinity casts to the i128 bound.
    ((r as i128).clamp(lo, hi) as u64) & (u64::MAX >> (64 - bits))
}

/// One function per literal, each `load; insn; tail..; ret`.
fn image(load: u32, insn: u32, tail: &[u32], lits: &[[u8; 16]]) -> (Vec<u8>, Vec<u64>) {
    let mut bytes = vec![0; lits.len() * STRIDE];
    let mut entries = Vec::new();
    for (i, lit) in lits.iter().enumerate() {
        let at = i * STRIDE;
        let mut words = vec![load, insn];
        words.extend_from_slice(tail);
        words.push(RET);
        assert!(words.len() * 4 <= LIT);
        for (j, w) in words.iter().enumerate() {
            bytes[at + 4 * j..at + 4 * j + 4].copy_from_slice(&w.to_le_bytes());
        }
        bytes[at + LIT..at + LIT + 16].copy_from_slice(lit);
        entries.push(BASE + at as u64);
    }
    (bytes, entries)
}

fn lit(lo: u64, hi: u64) -> [u8; 16] {
    let mut b = [0; 16];
    b[..8].copy_from_slice(&lo.to_le_bytes());
    b[8..].copy_from_slice(&hi.to_le_bytes());
    b
}

/// x0 at each entry, every initial register reading 0 and then all ones:
/// no conversion reads FPCR, so both agree.
fn run(bytes: Vec<u8>, entries: &[u64]) -> Vec<u64> {
    let claims = AssumptionOptions::none();
    let zeros =
        common::cone::returned_at_under(Arch::Aarch64, bytes.clone(), BASE, entries, 0, &claims);
    let ones =
        common::cone::returned_at_under(Arch::Aarch64, bytes, BASE, entries, u128::MAX, &claims);
    assert_eq!(zeros, ones, "a conversion read an initial register");
    zeros
}

/// The operands of `asm`: mnemonic, destination, source and the `#fbits`.
fn parse(asm: &str) -> (&str, &str, &str, u32) {
    let (mnemonic, ops) = asm.split_once(' ').unwrap();
    let ops: Vec<&str> = ops.split(", ").collect();
    let fbits = ops.get(2).map_or(0, |f| f[1..].parse().unwrap());
    (mnemonic, ops[0], ops[1], fbits)
}

/// Converts every input at `src` through `insn`, x0 read back by `tail`.
fn check(asm: &str, insn: u32, src: Prec, tail: &[u32], bits: u32, fbits: u32) {
    let (mnemonic, ..) = parse(asm);
    let conv = Conv::of(mnemonic);
    let inputs = src.inputs();
    let lits: Vec<[u8; 16]> = inputs.iter().map(|&b| lit(b, 0)).collect();
    let (bytes, entries) = image(src.load(), insn, tail, &lits);
    for (got, bits_in) in run(bytes, &entries).into_iter().zip(inputs) {
        let x = src.value(bits_in);
        let want = fp_to_fixed(x, fbits, conv, bits);
        assert_eq!(got, want, "{asm} of {x:e} ({bits_in:#x})");
    }
}

/// A scalar result's bits into x0, zero-extended: `umov w0, v0.h[0]`,
/// `fmov w0, s0` or `fmov x0, d0`.
fn mover(prec: Prec) -> u32 {
    match prec {
        Prec::H => 0x0e02_3c00,
        Prec::S => 0x1e26_0000,
        Prec::D => 0x9e66_0000,
    }
}

#[test]
fn gpr_conversions_round_by_their_mnemonic_and_saturate() {
    for &(insn, asm) in GPR {
        let (_, dst, src, _) = parse(asm);
        let bits = if dst.starts_with('w') { 32 } else { 64 };
        check(asm, insn, Prec::of(src), &[], bits, 0);
    }
}

#[test]
fn scalar_conversions_round_by_their_mnemonic_and_saturate() {
    for &(insn, asm) in SCALAR {
        let (_, dst, src, _) = parse(asm);
        let prec = Prec::of(src);
        assert_eq!(Prec::of(dst), prec);
        check(asm, insn, prec, &[mover(prec)], prec.bits(), 0);
    }
}

#[test]
fn fixed_point_conversions_scale_round_toward_zero_and_saturate() {
    for &(insn, asm) in FIXED {
        let (_, dst, src, fbits) = parse(asm);
        let prec = Prec::of(src);
        let (tail, bits) = match dst.as_bytes()[0] {
            b'w' => (vec![], 32),
            b'x' => (vec![], 64),
            _ => (vec![mover(prec)], prec.bits()),
        };
        check(asm, insn, prec, &tail, bits, fbits);
    }
}

/// Every lane converts on its own, and a 64-bit vector zeroes the upper
/// half.
#[test]
fn vector_conversions_round_each_lane() {
    for &(insn, asm) in VECTOR {
        let (mnemonic, dst, ..) = parse(asm);
        let conv = Conv::of(mnemonic);
        let (lanes, prec) = match &dst[3..] {
            "4h" => (4, Prec::H),
            "8h" => (8, Prec::H),
            "2s" => (2, Prec::S),
            "4s" => (4, Prec::S),
            "2d" => (2, Prec::D),
            v => panic!("{v}"),
        };
        let w = prec.bits();
        let full = 128 / w;
        let inputs = prec.inputs();
        let groups: Vec<Vec<u64>> = inputs.chunks(full as usize).map(<[u64]>::to_vec).collect();
        let pack = |g: &[u64]| -> u128 {
            g.iter()
                .enumerate()
                .fold(0, |acc, (i, &b)| acc | u128::from(b) << (w * i as u32))
        };
        let lits: Vec<[u8; 16]> = groups.iter().map(|g| pack(g).to_le_bytes()).collect();
        let mut halves = Vec::new();
        for tail in [0x9e66_0000_u32, 0x4e18_3c00] {
            let (bytes, entries) = image(0x9c00_0181, insn, &[tail], &lits);
            halves.push(run(bytes, &entries));
        }
        for (k, g) in groups.iter().enumerate() {
            let got = u128::from(halves[0][k]) | u128::from(halves[1][k]) << 64;
            let mut want = 0u128;
            for (i, &b) in g.iter().enumerate().take(lanes) {
                let x = prec.value(b);
                want |= u128::from(fp_to_fixed(x, 0, conv, w)) << (w * i as u32);
            }
            assert_eq!(
                got, want,
                "{asm} of lanes {g:x?}: got {got:#x} want {want:#x}"
            );
        }
    }
}

/// `fjcvtzs w0, d1` and NZCV in bits 32-35 of x0 (`cset` then `orr`).
#[test]
fn fjcvtzs_truncates_modulo_2_32_and_sets_z_when_exact() {
    let tail = [
        0x9a9f_17e2, // cset x2, eq
        0x9a9f_57e3, // cset x3, mi
        0x9a9f_37e4, // cset x4, hs
        0x9a9f_77e5, // cset x5, vs
        0xaa02_8000, // orr x0, x0, x2, lsl #32
        0xaa03_8400, // orr x0, x0, x3, lsl #33
        0xaa04_8800, // orr x0, x0, x4, lsl #34
        0xaa05_8c00, // orr x0, x0, x5, lsl #35
    ];
    let mut values = VALUES.to_vec();
    values.extend([3.0, -2.0, -2_147_483_648.0, 4_294_967_301.0, 2f64.powi(84)]);
    let lits: Vec<[u8; 16]> = values.iter().map(|x| lit(x.to_bits(), 0)).collect();
    // fjcvtzs w0, d1
    let (bytes, entries) = image(0x5c00_0181, 0x1e7e_0020, &tail, &lits);
    for (got, x) in run(bytes, &entries).into_iter().zip(values) {
        let finite = x.is_finite() && x.abs() < 2f64.powi(124);
        let t = if finite { x.trunc() as i128 } else { 0 };
        let exact = finite
            && x == x.trunc()
            && i32::try_from(t).is_ok()
            && !(x == 0.0 && x.is_sign_negative());
        let want = (t as u64 & 0xffff_ffff) | u64::from(exact) << 32;
        assert_eq!(got, want, "fjcvtzs of {x:e}");
    }
}
