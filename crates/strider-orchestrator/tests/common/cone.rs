//! Evaluates a returned value's cone by each node kind's p-code meaning, for
//! tests whose answer passes through a node the optimiser does not fold
//! (`FloatToInt`, any F80 or F128 arithmetic). Float lanes are carried as
//! `f64`, which is exact for the values these tests use, and 128-bit floats
//! as their bits ([`super::quad`]). A `FloatToInt` past its range or of a NaN
//! has no defined answer in p-code, so it evaluates to an arbitrary pattern
//! that a cone relying on it gets wrong.

use strider_ir::node::{NodeKind, ValueId};
use strider_ir::{
    ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, Function, IRViewer, IRWalker, UBig,
    ValueType,
};

use super::quad::{self, QUAD};
use super::round::{DOUBLE, Fmt, HALF, SINGLE};
use super::{inputs_of, producer_kind};

#[derive(Clone, Copy, Debug)]
pub(crate) enum Val {
    Int(u128),
    Float(f64),
    Quad(u128),
}

/// An `eval` answer in the `u128` this interpreter carries.
fn ev(v: Option<UBig>) -> u128 {
    u128::try_from(&v.expect("eval must answer")).expect("past the carrier")
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

/// An IEEE half-precision bit pattern as an `f64`, exactly.
pub(crate) fn f16_to_f64(bits: u16) -> f64 {
    let sign = if bits >> 15 == 1 { -1.0 } else { 1.0 };
    let exp = i32::from(bits >> 10 & 0x1f);
    let frac = f64::from(bits & 0x3ff);
    sign * match exp {
        0 => frac * 2f64.powi(-24),
        0x1f if frac == 0.0 => f64::INFINITY,
        0x1f => f64::NAN,
        _ => (1024.0 + frac) * 2f64.powi(exp - 25),
    }
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

/// An `f64` rounded to a half's bits, nearest even.
fn f64_to_f16(x: f64) -> u16 {
    use super::round::Mode;
    match HALF.convert(DOUBLE, u128::from(x.to_bits()), Mode::Even) {
        Some(b) => b as u16,
        None => 0x7e00 | (u16::from(x.is_sign_negative()) << 15),
    }
}

/// Rounds an `f64` result to `ty`'s precision, F80 kept at `f64`.
fn narrow(ty: ValueType, x: f64) -> f64 {
    match width(ty) {
        16 => f16_to_f64(f64_to_f16(x)),
        32 => f64::from(x as f32),
        64 | 80 => x,
        w => panic!("no float of width {w} in these conversions"),
    }
}

/// The format of a float of `ty`.
fn format(ty: ValueType) -> Fmt {
    match width(ty) {
        16 => HALF,
        32 => SINGLE,
        64 => DOUBLE,
        128 => QUAD,
        w => panic!("no float format of width {w} here"),
    }
}

/// A float `Val` of `ty` as its bits.
fn float_bits(val: Val, ty: ValueType) -> u128 {
    match (val, width(ty)) {
        (Val::Quad(q), _) => q,
        (Val::Float(x), 16) => u128::from(f64_to_f16(x)),
        (Val::Float(x), 32) => u128::from((x as f32).to_bits()),
        (Val::Float(x), 64) => u128::from(x.to_bits()),
        (val, w) => panic!("{val:?} is no float of width {w}"),
    }
}

/// The float `Val` of `ty` with `bits`.
fn float_val(bits: u128, ty: ValueType) -> Val {
    match width(ty) {
        16 => Val::Float(f16_to_f64(bits as u16)),
        32 => Val::Float(f64::from(f32::from_bits(bits as u32))),
        64 => Val::Float(f64::from_bits(bits as u64)),
        128 => Val::Quad(bits),
        w => panic!("no float of width {w} here"),
    }
}

/// What an undefined `FloatToInt` evaluates to.
const POISON: u128 = 0x5a5a_5a5a_5a5a_5a5a_5a5a_5a5a_5a5a_5a5a;

/// Evaluates `v`'s cone by each node's p-code meaning, every initial
/// register reading as `init` (masked to its width).
pub(crate) fn eval(f: &Function, v: ValueId, init: u128) -> Val {
    eval_env(f, v, &|_| init)
}

/// [`eval`] with each initial register reading as `env` gives it.
pub(crate) fn eval_env(f: &Function, v: ValueId, env: &dyn Fn(&rsleigh::Vn) -> u128) -> Val {
    eval_cx(
        f,
        v,
        &Cx {
            init: env,
            opaque: &|_| None,
        },
    )
}

/// What a cone evaluation reads: each initial register, and the value an
/// opaque `CallOther` of a name yields (`None` for one it cannot evaluate).
struct Cx<'a> {
    init: &'a dyn Fn(&rsleigh::Vn) -> u128,
    opaque: &'a dyn Fn(&str) -> Option<u128>,
}

fn eval_cx(f: &Function, v: ValueId, cx: &Cx<'_>) -> Val {
    MEMO.with(|m| m.borrow_mut().clear());
    eval_rec(f, v, cx)
}

thread_local! {
    /// Values already evaluated in the current [`eval`]: a cone shares
    /// subexpressions, which a plain recursion re-walks once per use.
    static MEMO: std::cell::RefCell<std::collections::HashMap<ValueId, Val>> =
        std::cell::RefCell::default();
}

fn eval_rec(f: &Function, v: ValueId, cx: &Cx<'_>) -> Val {
    if let Some(val) = MEMO.with(|m| m.borrow().get(&v).copied()) {
        return val;
    }
    let val = eval_node(f, v, cx);
    MEMO.with(|m| m.borrow_mut().insert(v, val));
    val
}

fn eval_node(f: &Function, v: ValueId, cx: &Cx<'_>) -> Val {
    let ty = f.value_type(v).expect("typed");
    let ins = inputs_of(f, v);
    let int = |i: usize| match eval_rec(f, ins[i], cx) {
        Val::Int(x) => x,
        other => panic!("operand {i} of {:?} is {other:?}", producer_kind(f, v)),
    };
    let float = |i: usize| match eval_rec(f, ins[i], cx) {
        Val::Float(x) => x,
        other => panic!("operand {i} of {:?} is {other:?}", producer_kind(f, v)),
    };
    let in_ty = |i: usize| f.value_type(ins[i]).expect("typed");
    let bits = |i: usize| float_bits(eval_rec(f, ins[i], cx), in_ty(i));
    let wide = width(ty) == 128
        || ins.first().is_some_and(|&i| {
            f.value_type(i)
                .is_ok_and(|t| t.is_float() && width(t) == 128)
        });
    match producer_kind(f, v) {
        NodeKind::IntConst(_) => Val::Int(f.int_const_u128(v).expect("int const")),
        NodeKind::InitialVar(id) => Val::Int((cx.init)(&f.initial_vn(*id)) & mask(ty)),
        NodeKind::FloatConst(bits) => Val::Float(match width(ty) {
            16 => f16_to_f64(*bits as u16),
            32 => f64::from(f32::from_bits(*bits as u32)),
            64 => f64::from_bits(*bits),
            w => panic!("FloatConst of width {w}"),
        }),
        NodeKind::IntBitsToFloat => Val::Float(match width(ty) {
            16 => f16_to_f64(int(0) as u16),
            32 => f64::from(f32::from_bits(int(0) as u32)),
            64 => f64::from_bits(int(0) as u64),
            80 => f80_to_f64(int(0)),
            128 => return Val::Quad(int(0)),
            w => panic!("IntBitsToFloat of width {w}"),
        }),
        NodeKind::FloatBitsToInt => Val::Int(match width(in_ty(0)) {
            16 => u128::from(f64_to_f16(float(0))),
            32 => u128::from((float(0) as f32).to_bits()),
            128 => bits(0),
            64 => u128::from(float(0).to_bits()),
            80 => f64_to_f80(float(0)),
            w => panic!("FloatBitsToInt of width {w}"),
        }),
        NodeKind::FloatToFloat if wide => {
            float_val(quad::convert(format(in_ty(0)), format(ty), bits(0)), ty)
        }
        NodeKind::FloatToFloat => Val::Float(narrow(ty, float(0))),
        NodeKind::IntToFloat => {
            let x = ev(strider_ir::eval::sign_extend(
                &UBig::from(int(0)),
                in_ty(0),
                ValueType::int(128),
            )) as i128;
            // Rounded once, to the destination's own precision.
            Val::Float(match width(ty) {
                128 => return Val::Quad(quad::from_int(QUAD, x)),
                32 => f64::from(x as f32),
                _ => narrow(ty, x as f64),
            })
        }
        NodeKind::FloatToInt => {
            let x = float(0).trunc();
            let bound = 2f64.powi(i32::from(width(ty)) - 1);
            Val::Int(if x.is_nan() || x < -bound || x >= bound {
                POISON & mask(ty)
            } else {
                (x as i128) as u128 & mask(ty)
            })
        }
        NodeKind::FloatUnaryOp(FloatUnaryOp::Neg) if wide => Val::Quad(quad::neg(QUAD, bits(0))),
        NodeKind::FloatBinaryOp(op) if wide => Val::Quad(match op {
            FloatBinaryOp::Add => quad::add(QUAD, bits(0), bits(1)),
            FloatBinaryOp::Mul => quad::mul(QUAD, bits(0), bits(1)),
            FloatBinaryOp::Div => quad::div(QUAD, bits(0), bits(1)),
        }),
        NodeKind::FloatCmpOp(op) if wide => {
            let ord = quad::cmp(QUAD, bits(0), bits(1));
            Val::Int(u128::from(match op {
                FloatCmpOp::Equal => ord == Some(std::cmp::Ordering::Equal),
                FloatCmpOp::Less => ord == Some(std::cmp::Ordering::Less),
            }))
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
        NodeKind::IntBinaryOp(op) => Val::Int(ev(strider_ir::eval::int_binary(
            *op,
            &UBig::from(int(0)),
            &UBig::from(int(1)),
            ty,
        ))),
        NodeKind::IntCmpOp(op) => Val::Int(u128::from(
            strider_ir::eval::int_cmp(*op, &UBig::from(int(0)), &UBig::from(int(1)), in_ty(0))
                .expect("int cmp"),
        )),
        NodeKind::IntUnaryOp(op) => Val::Int(ev(strider_ir::eval::int_unary(
            *op,
            &UBig::from(int(0)),
            ty,
        ))),
        NodeKind::Extend(ExtendOp::ZeroExtend) => Val::Int(int(0)),
        NodeKind::Extend(ExtendOp::SignExtend) => Val::Int(ev(strider_ir::eval::sign_extend(
            &UBig::from(int(0)),
            in_ty(0),
            ty,
        ))),
        NodeKind::Truncate => Val::Int(int(0) & mask(ty)),
        NodeKind::Load(space) => {
            let addr = int(NodeKind::ACCESS_ADDR_SLOT);
            let big = f.byte_order().of(*space) == strider_target::Endianness::Big;
            let size = usize::from(width(ty) / 8);
            let mut out = 0u128;
            for i in 0..size {
                let place = if big { size - 1 - i } else { i };
                let byte =
                    stored_byte(f, ins[NodeKind::ACCESS_MEM_SLOT], addr + i as u128, big, cx);
                out |= u128::from(byte) << (8 * place);
            }
            Val::Int(out)
        }
        NodeKind::Phi => {
            let preds = inputs_of(f, ins[0]);
            let live: Vec<usize> = (0..preds.len())
                .filter(|&i| control_live(f, preds[i], cx))
                .collect();
            let [i] = live[..] else {
                panic!("{} live predecessors of a Phi", live.len());
            };
            eval_rec(f, ins[i + 1], cx)
        }
        // A volatile register reads as its initial value.
        NodeKind::CallOther { .. } if f.call_other_name(f.producer(v)) == Some("read_volatile") => {
            let vn = f
                .get_vn_for_value(v)
                .expect("a volatile read names its register");
            Val::Int((cx.init)(&vn) & mask(ty))
        }
        NodeKind::CallOther { .. }
            if f.call_other_name(f.producer(v))
                .and_then(cx.opaque)
                .is_some() =>
        {
            let name = f.call_other_name(f.producer(v)).expect("named");
            Val::Int((cx.opaque)(name).expect("valued") & mask(ty))
        }
        other => panic!("{other:?} in a conversion of constants"),
    }
}

/// The byte at `addr` of the memory `mem`, which only stores this cone
/// evaluates may have written.
fn stored_byte(f: &Function, mem: ValueId, addr: u128, big: bool, cx: &Cx<'_>) -> u8 {
    let mut mem = mem;
    loop {
        let ins = inputs_of(f, mem);
        match producer_kind(f, mem) {
            NodeKind::Store(_) => {
                let at = match eval_rec(f, ins[NodeKind::ACCESS_ADDR_SLOT], cx) {
                    Val::Int(x) => x,
                    other => panic!("a store address is {other:?}"),
                };
                let data = ins[NodeKind::STORE_DATA_SLOT];
                let ty = f.value_type(data).expect("typed");
                let size = u128::from(width(ty) / 8);
                if (at..at + size).contains(&addr) {
                    let bits = match eval_rec(f, data, cx) {
                        Val::Int(x) => x,
                        val => float_bits(val, ty),
                    };
                    let k = addr - at;
                    let place = if big { size - 1 - k } else { k };
                    return (bits >> (8 * place)) as u8;
                }
                mem = ins[NodeKind::ACCESS_MEM_SLOT];
            }
            other => panic!("a load reaches {other:?} at {addr:#x}"),
        }
    }
}

/// Whether control reaches `c` under `cx`.
fn control_live(f: &Function, c: ValueId, cx: &Cx<'_>) -> bool {
    let node = f.producer(c);
    let ins = inputs_of(f, c);
    match f.node_kind(node) {
        NodeKind::Entry => true,
        NodeKind::Region => ins.into_iter().any(|p| control_live(f, p, cx)),
        NodeKind::If => {
            let Val::Int(cond) = eval_rec(f, ins[1], cx) else {
                panic!("a float If condition");
            };
            let taken = f.node_outputs(node)[usize::from(cond == 0)];
            taken == c && control_live(f, ins[0], cx)
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
    analyze_rets(arch, bytes, base, &[base], claims)
        .pop()
        .expect("one entry")
}

/// [`analyze_ret`] at each of `entries`, one image and one engine for all.
fn analyze_rets(
    arch: super::Arch,
    bytes: Vec<u8>,
    base: u64,
    entries: &[u64],
    claims: &strider_orchestrator::opt::AssumptionOptions,
) -> Vec<(Function, ValueId)> {
    let rom: Box<dyn strider_orchestrator::opt::ReadOnlyMemory> = Box::new(
        strider_ir_test_utils::MockRom::raw_bytes(base, bytes.clone()),
    );
    let (mut strider, cc) = super::strider_over_bytes(arch, bytes, base, Some(rom));
    let opts = strider_orchestrator::AnalyzeOptions {
        assumptions: claims.clone(),
        ..Default::default()
    };
    entries
        .iter()
        .map(|&entry| {
            let f = strider
                .analyze(entry, &cc, &opts, None)
                .expect("analyze")
                .function;
            let v = returned(&f);
            (f, v)
        })
        .collect()
}

/// The value in the first return slot of `f`'s one `Return`.
fn returned(f: &Function) -> ValueId {
    let ret = f
        .walk()
        .find(|&n| matches!(f.node_kind(n), NodeKind::Return))
        .expect("one Return");
    f.node_inputs(ret).into_iter().nth(2).expect("return slot")
}

fn as_rax(val: Val) -> u64 {
    match val {
        Val::Int(x) => u64::try_from(x).expect("rax is 64 bits"),
        other => panic!("rax holds {other:?}"),
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
    returned_rax_each_under(&[bytes], base, claims)[0]
}

/// [`returned_rax_under`] of each of `images`, laid out by
/// [`super::concat_images`] and lifted through one engine.
pub(crate) fn returned_rax_each_under(
    images: &[Vec<u8>],
    base: u64,
    claims: &strider_orchestrator::opt::AssumptionOptions,
) -> Vec<u64> {
    let (bytes, entries) = super::concat_images(images, base);
    analyze_rets(super::Arch::X64, bytes, base, &entries, claims)
        .iter()
        .map(|(f, v)| {
            let [zeros, ones] = [0, u128::MAX].map(|init| as_rax(eval(f, *v, init)));
            assert_eq!(zeros, ones, "rax depends on an initial register");
            zeros
        })
        .collect()
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

/// [`returned_rax_from_under`] of each `(image, init)`, the images laid out
/// by [`super::concat_images`] and lifted through one engine.
pub(crate) fn returned_rax_each_from_under(
    images: &[(Vec<u8>, u128)],
    base: u64,
    claims: &strider_orchestrator::opt::AssumptionOptions,
) -> Vec<u64> {
    let bytes: Vec<Vec<u8>> = images.iter().map(|(b, _)| b.clone()).collect();
    let (bytes, entries) = super::concat_images(&bytes, base);
    analyze_rets(super::Arch::X64, bytes, base, &entries, claims)
        .iter()
        .zip(images)
        .map(|((f, v), &(_, init))| as_rax(eval(f, *v, init)))
        .collect()
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
    returned_from_with(arch, bytes, base, init, claims, &|_| None)
}

/// [`returned_from_under`] with each opaque `CallOther` named `n` yielding
/// `opaque(n)`.
pub(crate) fn returned_from_with(
    arch: super::Arch,
    bytes: Vec<u8>,
    base: u64,
    init: u128,
    claims: &strider_orchestrator::opt::AssumptionOptions,
    opaque: &dyn Fn(&str) -> Option<u128>,
) -> u64 {
    let (f, v) = analyze_ret(arch, bytes, base, claims);
    let cx = Cx {
        init: &|_| init,
        opaque,
    };
    as_rax(eval_cx(&f, v, &cx))
}

/// [`returned_from_under`] for the function at each of `entries`, all in one
/// image.
pub(crate) fn returned_at_under(
    arch: super::Arch,
    bytes: Vec<u8>,
    base: u64,
    entries: &[u64],
    init: u128,
    claims: &strider_orchestrator::opt::AssumptionOptions,
) -> Vec<u64> {
    returned_at_each_init_under(arch, bytes, base, entries, &[init], claims).remove(0)
}

/// [`returned_at_under`] under each of `inits`, each function lifted once.
pub(crate) fn returned_at_each_init_under(
    arch: super::Arch,
    bytes: Vec<u8>,
    base: u64,
    entries: &[u64],
    inits: &[u128],
    claims: &strider_orchestrator::opt::AssumptionOptions,
) -> Vec<Vec<u64>> {
    let lifted = analyze_rets(arch, bytes, base, entries, claims);
    inits
        .iter()
        .map(|&init| {
            lifted
                .iter()
                .map(|(f, v)| as_rax(eval(f, *v, init)))
                .collect()
        })
        .collect()
}

/// Where [`lift_image`] maps its image.
pub(crate) const IMAGE_BASE: u64 = 0x10000;

/// A function lifted with chosen registers returned and chosen initial
/// registers to set.
pub(crate) struct Lifted {
    pub(crate) f: Function,
    rets: Vec<ValueId>,
    inputs: Vec<rsleigh::Vn>,
    /// The register space is big-endian.
    big: bool,
}

impl Lifted {
    /// The register `inputs[i]` names.
    pub(crate) fn input(&self, i: usize) -> rsleigh::Vn {
        self.inputs[i]
    }

    /// The returned registers with `inputs[i]` reading `vals[i]`, every other
    /// initial register byte reading `fill`'s byte at its place.
    pub(crate) fn run(&self, vals: &[u128], fill: u128) -> Vec<Val> {
        self.run_with(vals, fill, &|_| None)
    }

    /// [`Self::run`] with each opaque `CallOther` named `n` yielding
    /// `opaque(n)`.
    pub(crate) fn run_with(
        &self,
        vals: &[u128],
        fill: u128,
        opaque: &dyn Fn(&str) -> Option<u128>,
    ) -> Vec<Val> {
        self.rets
            .iter()
            .map(|&v| self.eval_with(v, vals, fill, opaque))
            .collect()
    }

    /// Returned register `i` when the lift folds it to an integer constant.
    pub(crate) fn folded(&self, i: usize) -> Option<u128> {
        self.f.int_const_u128(self.rets[i])
    }

    /// Any value `v` of the function, initial registers as [`Self::run`] has
    /// them.
    pub(crate) fn eval(&self, v: ValueId, vals: &[u128], fill: u128) -> Val {
        self.eval_with(v, vals, fill, &|_| None)
    }

    /// [`Self::eval`] with opaque `CallOther`s as [`Self::run_with`] has them.
    fn eval_with(
        &self,
        v: ValueId,
        vals: &[u128],
        fill: u128,
        opaque: &dyn Fn(&str) -> Option<u128>,
    ) -> Val {
        let env = |vn: &rsleigh::Vn| -> u128 {
            let size = vn.size as usize;
            let mut out = fill & mask(ValueType::Int(u16::try_from(8 * size).expect("width")));
            for i in 0..size {
                let at = vn.addr_off + i as u64;
                let place = if self.big { size - 1 - i } else { i };
                let hit = self.inputs.iter().zip(vals).find(|(r, _)| {
                    r.addr_space == vn.addr_space
                        && (r.addr_off..r.addr_off + u64::from(r.size)).contains(&at)
                });
                if let Some((r, &val)) = hit {
                    let k = (at - r.addr_off) as usize;
                    let from = if self.big { r.size as usize - 1 - k } else { k };
                    let byte = val >> (8 * from) & 0xff;
                    out = out & !(0xff << (8 * place)) | byte << (8 * place);
                }
            }
            out
        };
        eval_cx(&self.f, v, &Cx { init: &env, opaque })
    }
}

/// `words` (then nothing: the last must end the function) lifted on `arch`
/// under `claims`, returning the registers `rets`, with `inputs` resolved
/// for [`Lifted::run`].
pub(crate) fn lift_regs(
    arch: super::Arch,
    words: &[u32],
    rets: &[&str],
    inputs: &[&str],
    claims: &strider_orchestrator::opt::AssumptionOptions,
) -> Lifted {
    let bytes = super::words_image(words, arch.sleigh().endianness());
    lift_image(arch, bytes, rets, inputs, claims)
}

/// [`lift_regs`] over an image mapped at [`IMAGE_BASE`], read-only, with
/// the function at its start.
pub(crate) fn lift_image(
    arch: super::Arch,
    bytes: Vec<u8>,
    rets: &[&str],
    inputs: &[&str],
    claims: &strider_orchestrator::opt::AssumptionOptions,
) -> Lifted {
    let opts = strider_orchestrator::AnalyzeOptions {
        assumptions: claims.clone(),
        ..Default::default()
    };
    lift_image_with(arch, bytes, IMAGE_BASE, rets, inputs, &opts)
}

/// [`lift_image`] analysed from `entry` (`IMAGE_BASE | 1` for Thumb code)
/// under `opts`.
pub(crate) fn lift_image_with(
    arch: super::Arch,
    bytes: Vec<u8>,
    entry: u64,
    rets: &[&str],
    inputs: &[&str],
    opts: &strider_orchestrator::AnalyzeOptions<'_>,
) -> Lifted {
    const BASE: u64 = IMAGE_BASE;
    let rom: Box<dyn strider_orchestrator::opt::ReadOnlyMemory> = Box::new(
        strider_ir_test_utils::MockRom::raw_bytes(BASE, bytes.clone()),
    );
    let (mut strider, mut cc) = super::strider_over_bytes(arch, bytes, BASE, Some(rom));
    let vn = |name: &str| {
        strider_target::calling_convention::vn_for_name(strider.sleigh_regs(), name)
            .expect("a register name")
    };
    cc.ret_val_regs = rets.iter().map(|r| vn(r)).collect();
    cc.ret_val_regs_float = Vec::new();
    // A returned register is no longer one the function preserves.
    let rets_vns = cc.ret_val_regs.clone();
    cc.callee_saved_regs.retain(|s| {
        !rets_vns.iter().any(|r| {
            r.addr_space == s.addr_space
                && r.addr_off < s.addr_off + u64::from(s.size)
                && s.addr_off < r.addr_off + u64::from(r.size)
        })
    });
    let inputs = inputs.iter().map(|r| vn(r)).collect();
    let f = strider
        .analyze(entry, &cc, opts, None)
        .expect("analyze")
        .function;
    let rets = super::returned(&f);
    assert_eq!(rets.len(), cc.ret_val_regs.len(), "one value per register");
    let big = f.byte_order().register == strider_target::Endianness::Big;
    Lifted {
        f,
        rets,
        inputs,
        big,
    }
}
