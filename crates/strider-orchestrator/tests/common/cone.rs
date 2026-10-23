//! Evaluates a returned value's cone by each node kind's p-code meaning, for
//! tests whose answer passes through a node the optimiser does not fold
//! (`FloatToInt`, any F80 arithmetic). Float lanes are carried as `f64`,
//! which is exact for the values these tests use.

use strider_ir::node::{NodeKind, ValueId};
use strider_ir::{
    ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, Function, IRViewer, IRWalker, ValueType,
};

use super::{inputs_of, producer_kind};

#[derive(Clone, Copy, Debug)]
pub(crate) enum Val {
    Int(u128),
    Float(f64),
}

fn width(ty: ValueType) -> u16 {
    match ty {
        ValueType::Int(w) | ValueType::Float(w) => w,
    }
}

fn mask(ty: ValueType) -> u128 {
    match width(ty) {
        w if w >= 128 => u128::MAX,
        w => (1u128 << w) - 1,
    }
}

/// An x87 extended-precision bit pattern as an `f64`; exact for the values
/// these tests use.
fn f80_to_f64(bits: u128) -> f64 {
    let sign = if bits >> 79 & 1 == 1 { -1.0 } else { 1.0 };
    let exp = i32::try_from(bits >> 64 & 0x7fff).unwrap();
    let mant = (bits & u128::from(u64::MAX)) as u64;
    if exp == 0 && mant == 0 {
        return sign * 0.0;
    }
    sign * (mant as f64) * 2f64.powi(exp - 16383 - 63)
}

/// An `f64` as an x87 extended-precision bit pattern (no NaN payloads).
fn f64_to_f80(x: f64) -> u128 {
    let sign = u128::from(x.is_sign_negative()) << 79;
    if x == 0.0 {
        return sign;
    }
    if x.is_infinite() {
        return sign | 0x7fff << 64 | 1 << 63;
    }
    assert!(x.is_normal(), "{x} has no exact F80 here");
    let bits = x.to_bits();
    let exp = (bits >> 52 & 0x7ff) as i64 - 1023 + 16383;
    let mant = (bits & ((1 << 52) - 1) | 1 << 52) << 11;
    sign | u128::try_from(exp).unwrap() << 64 | u128::from(mant)
}

/// Rounds an `f64` result to `ty`'s precision, F80 kept at `f64`.
fn narrow(ty: ValueType, x: f64) -> f64 {
    match width(ty) {
        32 => f64::from(x as f32),
        64 | 80 => x,
        w => panic!("no float of width {w} in these conversions"),
    }
}

/// Evaluates `v`'s cone by each node's p-code meaning, every initial
/// register reading as `init` (masked to its width).
pub(crate) fn eval(f: &Function, v: ValueId, init: u128) -> Val {
    let ty = f.value_type(v).expect("typed");
    let ins = inputs_of(f, v);
    let int = |i: usize| match eval(f, ins[i], init) {
        Val::Int(x) => x,
        Val::Float(x) => panic!("operand {i} of {:?} is the float {x}", producer_kind(f, v)),
    };
    let float = |i: usize| match eval(f, ins[i], init) {
        Val::Float(x) => x,
        Val::Int(x) => panic!("operand {i} of {:?} is the int {x:#x}", producer_kind(f, v)),
    };
    let in_ty = |i: usize| f.value_type(ins[i]).expect("typed");
    match producer_kind(f, v) {
        NodeKind::IntConst(_) => Val::Int(f.int_const_u128(v).expect("int const")),
        NodeKind::InitialVar(_) => Val::Int(init & mask(ty)),
        NodeKind::FloatConst(bits) => Val::Float(match width(ty) {
            32 => f64::from(f32::from_bits(*bits as u32)),
            64 => f64::from_bits(*bits),
            w => panic!("FloatConst of width {w}"),
        }),
        NodeKind::IntBitsToFloat => Val::Float(match width(ty) {
            32 => f64::from(f32::from_bits(int(0) as u32)),
            64 => f64::from_bits(int(0) as u64),
            80 => f80_to_f64(int(0)),
            w => panic!("IntBitsToFloat of width {w}"),
        }),
        NodeKind::FloatBitsToInt => Val::Int(match width(in_ty(0)) {
            32 => u128::from((float(0) as f32).to_bits()),
            64 => u128::from(float(0).to_bits()),
            80 => f64_to_f80(float(0)),
            w => panic!("FloatBitsToInt of width {w}"),
        }),
        NodeKind::FloatToFloat => Val::Float(narrow(ty, float(0))),
        NodeKind::IntToFloat => {
            let x = strider_ir::eval::sign_extend(int(0), in_ty(0), ValueType::int(128))
                .expect("sign extend") as i128;
            // Rounded once, to the destination's own precision.
            Val::Float(match width(ty) {
                32 => f64::from(x as f32),
                _ => narrow(ty, x as f64),
            })
        }
        NodeKind::FloatToInt => {
            // Out of range or NaN gives the x86 integer indefinite, the sign
            // bit alone.
            let x = float(0).trunc();
            let bound = 2f64.powi(i32::from(width(ty)) - 1);
            Val::Int(if x.is_nan() || x < -bound || x >= bound {
                1 << (width(ty) - 1)
            } else {
                (x as i128) as u128 & mask(ty)
            })
        }
        NodeKind::FloatUnaryOp(op) => {
            let x = float(0);
            Val::Float(narrow(
                ty,
                match op {
                    FloatUnaryOp::Neg => -x,
                    FloatUnaryOp::Abs => x.abs(),
                    FloatUnaryOp::Sqrt => x.sqrt(),
                    FloatUnaryOp::Ceil => x.ceil(),
                    FloatUnaryOp::Floor => x.floor(),
                    // FLOAT_ROUND: nearest, ties away from zero.
                    FloatUnaryOp::Round => x.round(),
                },
            ))
        }
        NodeKind::FloatBinaryOp(op) => {
            let (l, r) = (float(0), float(1));
            Val::Float(narrow(
                ty,
                match op {
                    FloatBinaryOp::Add => l + r,
                    FloatBinaryOp::Mul => l * r,
                    FloatBinaryOp::Div => l / r,
                },
            ))
        }
        NodeKind::FloatCmpOp(op) => {
            let (l, r) = (float(0), float(1));
            Val::Int(u128::from(match op {
                FloatCmpOp::Equal => l == r,
                FloatCmpOp::Less => l < r,
            }))
        }
        NodeKind::IntBinaryOp(op) => {
            Val::Int(strider_ir::eval::int_binary(*op, int(0), int(1), ty).expect("int binary"))
        }
        NodeKind::IntCmpOp(op) => Val::Int(u128::from(
            strider_ir::eval::int_cmp(*op, int(0), int(1), in_ty(0)).expect("int cmp"),
        )),
        NodeKind::IntUnaryOp(op) => {
            Val::Int(strider_ir::eval::int_unary(*op, int(0), ty).expect("int unary"))
        }
        NodeKind::Extend(ExtendOp::ZeroExtend) => Val::Int(int(0)),
        NodeKind::Extend(ExtendOp::SignExtend) => {
            Val::Int(strider_ir::eval::sign_extend(int(0), in_ty(0), ty).expect("sign extend"))
        }
        NodeKind::Truncate => Val::Int(int(0) & mask(ty)),
        NodeKind::Phi => {
            let preds = inputs_of(f, ins[0]);
            let live: Vec<usize> = (0..preds.len())
                .filter(|&i| control_live(f, preds[i], init))
                .collect();
            let [i] = live[..] else {
                panic!("{} live predecessors of a Phi", live.len());
            };
            eval(f, ins[i + 1], init)
        }
        other => panic!("{other:?} in a conversion of constants"),
    }
}

/// Whether control reaches `c` with every initial register reading `init`.
fn control_live(f: &Function, c: ValueId, init: u128) -> bool {
    let node = f.producer(c);
    let ins = inputs_of(f, c);
    match f.node_kind(node) {
        NodeKind::Entry => true,
        NodeKind::Region => ins.into_iter().any(|p| control_live(f, p, init)),
        NodeKind::If => {
            let Val::Int(cond) = eval(f, ins[1], init) else {
                panic!("a float If condition");
            };
            let taken = f.node_outputs(node)[usize::from(cond == 0)];
            taken == c && control_live(f, ins[0], init)
        }
        other => panic!("{other:?} on a control path"),
    }
}

/// The first integer register an `arch` function over `bytes` at `base`
/// returns (`rax`, `r3`), lifted under `claims`, with the function to
/// evaluate it in.
fn analyze_ret(
    arch: super::Arch,
    bytes: Vec<u8>,
    base: u64,
    claims: &strider_orchestrator::opt::AssumptionOptions,
) -> (Function, ValueId) {
    let rom: Box<dyn strider_orchestrator::opt::ReadOnlyMemory> = Box::new(
        strider_ir_test_utils::MockRom::raw_bytes(base, bytes.clone()),
    );
    let (mut strider, cc) = super::strider_over_bytes(arch, bytes, base, Some(rom));
    let opts = strider_orchestrator::AnalyzeOptions {
        assumptions: claims.clone(),
        ..Default::default()
    };
    let f = strider
        .analyze(base, &cc, &opts, None)
        .expect("analyze")
        .function;
    let ret = f
        .walk()
        .find(|&n| matches!(f.node_kind(n), NodeKind::Return))
        .expect("one Return");
    let v = f.node_inputs(ret).into_iter().nth(2).expect("return slot");
    (f, v)
}

fn as_rax(val: Val) -> u64 {
    match val {
        Val::Int(x) => u64::try_from(x).expect("rax is 64 bits"),
        Val::Float(x) => panic!("rax holds the float {x}"),
    }
}

/// The returned `rax`, evaluated. A partial register write leaves the
/// destination's other bits masked in, so the value must not depend on the
/// initial registers.
pub(crate) fn returned_rax(bytes: Vec<u8>, base: u64) -> u64 {
    returned_rax_under(bytes, base, &Default::default())
}

/// [`returned_rax`], lifted under `claims`.
pub(crate) fn returned_rax_under(
    bytes: Vec<u8>,
    base: u64,
    claims: &strider_orchestrator::opt::AssumptionOptions,
) -> u64 {
    let (f, v) = analyze_ret(super::Arch::X64, bytes, base, claims);
    let [zeros, ones] = [0, u128::MAX].map(|init| as_rax(eval(&f, v, init)));
    assert_eq!(zeros, ones, "rax depends on an initial register");
    zeros
}

/// The returned `rax` with every initial register reading `init`.
pub(crate) fn returned_rax_from(bytes: Vec<u8>, base: u64, init: u128) -> u64 {
    returned_rax_from_under(bytes, base, init, &Default::default())
}

/// [`returned_rax_from`], lifted under `claims`.
pub(crate) fn returned_rax_from_under(
    bytes: Vec<u8>,
    base: u64,
    init: u128,
    claims: &strider_orchestrator::opt::AssumptionOptions,
) -> u64 {
    returned_from_under(super::Arch::X64, bytes, base, init, claims)
}

/// The first integer register an `arch` function returns, every initial
/// register reading `init`, lifted under `claims`.
pub(crate) fn returned_from_under(
    arch: super::Arch,
    bytes: Vec<u8>,
    base: u64,
    init: u128,
    claims: &strider_orchestrator::opt::AssumptionOptions,
) -> u64 {
    let (f, v) = analyze_ret(arch, bytes, base, claims);
    as_rax(eval(&f, v, init))
}
