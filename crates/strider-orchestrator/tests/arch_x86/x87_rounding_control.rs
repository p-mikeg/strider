//! x87 `fist`, `fistp` and `frndint` round by the control word's RC (bits
//! 10-11): 0 nearest even, 1 down, 2 up, 3 toward zero. A NaN or a result out
//! of the destination's range stores the integer indefinite, and `frndint`
//! keeps the sign of a zero result.
//!
//! Every case was also run natively on x86-64 from the same bytes, agreeing
//! with [`rounded`], [`stored`] and [`frndint`] on all of them.

use crate::common;

use strider_ir::node::NodeKind;
use strider_ir::{FloatUnaryOp, IRViewer, IRWalker};

const BASE: u64 = 0x1000;
/// Where the operand double sits, reached rip-relative.
const DATA: usize = 0x60;

const RET: u8 = 0xc3;
/// `fnstcw [rsp-24]`.
const SAVE_CW: &[u8] = &[0xd9, 0x7c, 0x24, 0xe8];
/// `fldcw [rsp-24]`.
const RESTORE_CW: &[u8] = &[0xd9, 0x6c, 0x24, 0xe8];
/// `fld qword [rip+disp32]`.
const FLD_M64: &[u8] = &[0xdd, 0x05];
/// `fstp st0`.
const POP: &[u8] = &[0xdd, 0xd8];

/// `movzx eax, word [rsp-24]` / `and eax, ~0xc00` / `or eax, rc << 10` /
/// `mov [rsp-22], ax` / `fldcw [rsp-22]`, after [`SAVE_CW`].
fn set_rc(rc: u32) -> Vec<u8> {
    let mut v = SAVE_CW.to_vec();
    v.extend_from_slice(&[0x0f, 0xb7, 0x44, 0x24, 0xe8]);
    v.extend_from_slice(&[0x25, 0xff, 0xf3, 0xff, 0xff]);
    v.push(0x0d);
    v.extend_from_slice(&(rc << 10).to_le_bytes());
    v.extend_from_slice(&[0x66, 0x89, 0x44, 0x24, 0xea, 0xd9, 0x6c, 0x24, 0xea]);
    v
}

/// One x87 operation on `fld qword [x]`: what it does to `[rsp-16]`, how the
/// result comes back into `rax`, whether ST0 must be popped after, and the
/// integer width (`None` for `frndint`, whose double comes back whole).
struct Op {
    name: &'static str,
    store: &'static [u8],
    load: &'static [u8],
    pop: bool,
    width: Option<u32>,
}

const MOVSX_EAX_M16: &[u8] = &[0x0f, 0xbf, 0x44, 0x24, 0xf0];
const MOV_EAX_M32: &[u8] = &[0x8b, 0x44, 0x24, 0xf0];
const MOV_RAX_M64: &[u8] = &[0x48, 0x8b, 0x44, 0x24, 0xf0];

const STORES: [Op; 5] = [
    Op {
        name: "fist m16",
        store: &[0xdf, 0x54, 0x24, 0xf0],
        load: MOVSX_EAX_M16,
        pop: true,
        width: Some(16),
    },
    Op {
        name: "fist m32",
        store: &[0xdb, 0x54, 0x24, 0xf0],
        load: MOV_EAX_M32,
        pop: true,
        width: Some(32),
    },
    Op {
        name: "fistp m16",
        store: &[0xdf, 0x5c, 0x24, 0xf0],
        load: MOVSX_EAX_M16,
        pop: false,
        width: Some(16),
    },
    Op {
        name: "fistp m32",
        store: &[0xdb, 0x5c, 0x24, 0xf0],
        load: MOV_EAX_M32,
        pop: false,
        width: Some(32),
    },
    Op {
        name: "fistp m64",
        store: &[0xdf, 0x7c, 0x24, 0xf0],
        load: MOV_RAX_M64,
        pop: false,
        width: Some(64),
    },
];

/// `frndint` / `fstp qword [rsp-16]`.
const FRNDINT: Op = Op {
    name: "frndint",
    store: &[0xd9, 0xfc, 0xdd, 0x5c, 0x24, 0xf0],
    load: MOV_RAX_M64,
    pop: false,
    width: None,
};

/// Ties of each sign, fractions either side of a half, signed zeros, the
/// edges of the 16- and 32-bit ranges, and values no destination holds.
const VALUES: [f64; 21] = [
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
    -0.0,
    -7.0,
    32767.5,
    -32768.5,
    40000.0,
    2147483647.5,
    -2147483648.5,
    3e9,
    1e19,
    f64::NAN,
    f64::INFINITY,
];

/// The x87 operation under `rc`, or under the initial control word when
/// `rc` is `None`.
fn image(op: &Op, rc: Option<u32>, x: f64) -> Vec<u8> {
    let mut code = rc.map(set_rc).unwrap_or_default();
    code.extend_from_slice(FLD_M64);
    let next = code.len() + 4;
    let disp = i32::try_from(DATA).unwrap() - i32::try_from(next).unwrap();
    code.extend_from_slice(&disp.to_le_bytes());
    code.extend_from_slice(op.store);
    if rc.is_some() {
        code.extend_from_slice(RESTORE_CW);
    }
    code.extend_from_slice(op.load);
    if op.pop {
        code.extend_from_slice(POP);
    }
    code.push(RET);
    assert!(code.len() <= DATA, "code overruns the operand");
    code.resize(DATA, 0);
    code.extend_from_slice(&x.to_le_bytes());
    code
}

/// `x` rounded to an integral value under `rc`.
fn rounded(rc: u32, x: f64) -> f64 {
    match rc {
        0 => x.round_ties_even(),
        1 => x.floor(),
        2 => x.ceil(),
        _ => x.trunc(),
    }
}

/// What an integer store of `x` under `rc` leaves in a `width`-bit slot.
fn stored(rc: u32, x: f64, width: u32) -> u64 {
    let r = rounded(rc, x);
    let bound = 2f64.powi(width as i32 - 1);
    if r.is_nan() || r < -bound || r >= bound {
        1 << (width - 1)
    } else {
        (r as i64 as u64) & (u64::MAX >> (64 - width))
    }
}

/// The double `frndint` leaves: integral, the sign of a zero kept.
fn frndint(rc: u32, x: f64) -> u64 {
    rounded(rc, x).to_bits()
}

/// The returned `rax` of each of `images`, lifted through one engine.
fn returned_each(images: &[Vec<u8>]) -> Vec<u64> {
    common::cone::returned_rax_each_under(images, BASE, &Default::default())
}

/// The returned `rax` of each `(image, init)`, lifted through one engine.
fn returned_each_from(images: &[(Vec<u8>, u128)]) -> Vec<u64> {
    common::cone::returned_rax_each_from_under(images, BASE, &Default::default())
}

fn slot(r: u64, width: u32) -> u64 {
    r & (u64::MAX >> (64 - width))
}

#[test]
fn integer_stores_round_by_the_control_word() {
    let mut cases = Vec::new();
    for op in &STORES {
        for rc in 0..4 {
            for x in VALUES {
                cases.push((op, rc, x));
            }
        }
    }
    let images: Vec<Vec<u8>> = cases
        .iter()
        .map(|&(op, rc, x)| image(op, Some(rc), x))
        .collect();
    for (&(op, rc, x), got) in cases.iter().zip(returned_each(&images)) {
        let width = op.width.unwrap();
        assert_eq!(
            slot(got, width),
            stored(rc, x, width),
            "{} rc {rc} {x}",
            op.name
        );
    }
}

#[test]
fn frndint_rounds_by_the_control_word() {
    let mut cases = Vec::new();
    for rc in 0..4 {
        for x in VALUES.into_iter().filter(|x| !x.is_nan()) {
            cases.push((rc, x));
        }
    }
    let images: Vec<Vec<u8>> = cases
        .iter()
        .map(|&(rc, x)| image(&FRNDINT, Some(rc), x))
        .collect();
    for (&(rc, x), got) in cases.iter().zip(returned_each(&images)) {
        assert_eq!(
            f64::from_bits(got).to_bits(),
            frndint(rc, x),
            "frndint rc {rc} {x}: got {}",
            f64::from_bits(got)
        );
    }
}

/// With no `fldcw`, each store and `frndint` select on the control word the
/// function was entered with: each RC the initial register can hold picks its
/// own mode.
#[test]
fn an_unset_control_word_selects_on_the_initial_register() {
    let mut cases = Vec::new();
    for op in &STORES {
        for x in VALUES {
            for rc in 0..4u32 {
                cases.push((op, rc, x));
            }
        }
    }
    let images: Vec<(Vec<u8>, u128)> = cases
        .iter()
        .map(|&(op, rc, x)| (image(op, None, x), u128::from(rc << 10)))
        .collect();
    for (&(op, rc, x), got) in cases.iter().zip(returned_each_from(&images)) {
        let width = op.width.unwrap();
        assert_eq!(
            slot(got, width),
            stored(rc, x, width),
            "{} initial rc {rc} {x}",
            op.name
        );
    }
    let mut cases = Vec::new();
    for x in VALUES.into_iter().filter(|x| !x.is_nan()) {
        for rc in 0..4u32 {
            cases.push((rc, x));
        }
    }
    let images: Vec<(Vec<u8>, u128)> = cases
        .iter()
        .map(|&(rc, x)| (image(&FRNDINT, None, x), u128::from(rc << 10)))
        .collect();
    for (&(rc, x), got) in cases.iter().zip(returned_each_from(&images)) {
        assert_eq!(
            got,
            frndint(rc, x),
            "frndint initial rc {rc} {x}: got {}",
            f64::from_bits(got)
        );
    }
}

/// gcc's i386 `(int)x` is `fnstcw; or $0xc00; fldcw; fistp; fldcw`: the
/// optimiser folds the mode to 3, so each store is a plain truncation.
#[test]
fn gccs_truncating_casts_fold_to_a_plain_truncation() {
    for arch in [common::Arch::X86, common::Arch::X86Kernel] {
        for (name, stores) in [("float_to_int", 1), ("main", 6)] {
            let f = common::analyze(arch, "floats", name);
            let mut conversions = 0;
            for node in f.walk() {
                match f.node_kind(node) {
                    NodeKind::FloatUnaryOp(
                        op @ (FloatUnaryOp::Round | FloatUnaryOp::Floor | FloatUnaryOp::Ceil),
                    ) => panic!("{arch:?} {name}: a {op:?} survives"),
                    NodeKind::FloatToInt => {
                        conversions += 1;
                        let source = f.node_inputs(node).into_iter().next().unwrap();
                        assert!(
                            !matches!(common::producer_kind(&f, source), NodeKind::FloatUnaryOp(_)),
                            "{arch:?} {name}: the conversion takes a rounded value"
                        );
                    }
                    _ => {}
                }
            }
            assert_eq!(conversions, stores, "{arch:?} {name}");
        }
    }
}
