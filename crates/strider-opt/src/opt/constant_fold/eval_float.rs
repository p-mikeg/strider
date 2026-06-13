use strider_ir::node::ValueType;
use strider_ir::{FloatBinaryOp, FloatCmpOp, FloatUnaryOp};

// ── float constant evaluation ─────────────────────────────────────────────────

// Each helper expects `$ty` to be `f32` or `f64` and `$bits_to_lo` to widen the
// final result to a u64 lane.  Both branches differ only in which
// `from_bits`/`to_bits` width to call; the operation match itself is identical.
macro_rules! eval_binary {
    ($ty:ty, $op:expr, $bits_l:expr, $bits_r:expr) => {{
        let l = <$ty>::from_bits($bits_l as _);
        let r = <$ty>::from_bits($bits_r as _);
        let result: $ty = match $op {
            FloatBinaryOp::Add => l + r,
            FloatBinaryOp::Mul => l * r,
            FloatBinaryOp::Div => l / r,
        };
        // A NaN result has a target-dependent bit pattern (quiet bit / payload
        // / sign are not fixed by IEEE 754), so folding it would bake in this
        // host's encoding rather than the target's.  Non-NaN results are
        // bit-portable in the default rounding mode, so only NaN is withheld.
        if result.is_nan() {
            None
        } else {
            Some(result.to_bits() as u64)
        }
    }};
}

macro_rules! eval_cmp {
    ($ty:ty, $op:expr, $bits_l:expr, $bits_r:expr) => {{
        let l = <$ty>::from_bits($bits_l as _);
        let r = <$ty>::from_bits($bits_r as _);
        match $op {
            FloatCmpOp::Equal => l == r,
            FloatCmpOp::Less => l < r,
        }
    }};
}

macro_rules! eval_unary {
    ($ty:ty, $op:expr, $bits:expr) => {{
        let v = <$ty>::from_bits($bits as _);
        let result: $ty = match $op {
            FloatUnaryOp::Neg => -v,
            FloatUnaryOp::Abs => v.abs(),
            FloatUnaryOp::Sqrt => v.sqrt(),
            FloatUnaryOp::Ceil => v.ceil(),
            FloatUnaryOp::Floor => v.floor(),
            // IEEE 754 / hardware default: ties-to-even, not Rust's
            // ties-away-from-zero `round`.
            FloatUnaryOp::Round => v.round_ties_even(),
        };
        // See `eval_binary!`: a NaN result's bit pattern is target-dependent,
        // so it is left unfolded.
        if result.is_nan() {
            None
        } else {
            Some(result.to_bits() as u64)
        }
    }};
}

/// Evaluates a float binary op on raw bit patterns.  Returns the result as a
/// raw bit pattern, or `None` when the type is unsupported (F80) or the result
/// is NaN (whose bit pattern is target-dependent, so folding it would bake in
/// the host encoding — see `eval_binary!`).
pub(crate) fn eval_float_binary(
    op: FloatBinaryOp,
    bits_l: u64,
    bits_r: u64,
    ty: ValueType,
) -> Option<u64> {
    match ty {
        ValueType::F32 => eval_binary!(f32, op, bits_l as u32, bits_r as u32),
        ValueType::F64 => eval_binary!(f64, op, bits_l, bits_r),
        // F80 (and all non-float types) fall through.  Rust has no native
        // 80-bit float type, so opt rules can't constant-fold F80 ops —
        // the rule sees `None` and skips, leaving the F80 node in the IR
        // for pattern-matching workloads.  Bit-exact F80 emulation is out
        // of scope; pattern queries care about graph shape, not values.
        _ => None,
    }
}

/// Evaluates a float comparison on raw bit patterns.
pub(crate) fn eval_float_cmp(
    op: FloatCmpOp,
    bits_l: u64,
    bits_r: u64,
    ty: ValueType,
) -> Option<bool> {
    match ty {
        ValueType::F32 => Some(eval_cmp!(f32, op, bits_l as u32, bits_r as u32)),
        ValueType::F64 => Some(eval_cmp!(f64, op, bits_l, bits_r)),
        _ => None,
    }
}

/// Evaluates a float unary op on a raw bit pattern.
pub(crate) fn eval_float_unary(op: FloatUnaryOp, bits: u64, ty: ValueType) -> Option<u64> {
    match ty {
        ValueType::F32 => eval_unary!(f32, op, bits as u32),
        ValueType::F64 => eval_unary!(f64, op, bits),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// F80 binary op evaluation must return `None` so the const-fold rule
    /// skips and the F80 node remains in the graph.  Pinned to prevent
    /// future contributors from adding a partial F80 path that loses
    /// precision (Rust's `f64`-via-conversion would silently truncate).
    #[test]
    fn eval_f80_binary_returns_none() {
        let zero = 0u64;
        for op in [FloatBinaryOp::Add, FloatBinaryOp::Mul, FloatBinaryOp::Div] {
            assert_eq!(eval_float_binary(op, zero, zero, ValueType::F80), None);
        }
    }

    #[test]
    fn eval_f80_cmp_returns_none() {
        let zero = 0u64;
        for op in [FloatCmpOp::Equal, FloatCmpOp::Less] {
            assert_eq!(eval_float_cmp(op, zero, zero, ValueType::F80), None);
        }
    }

    #[test]
    fn eval_f80_unary_returns_none() {
        for op in [
            FloatUnaryOp::Neg,
            FloatUnaryOp::Abs,
            FloatUnaryOp::Sqrt,
            FloatUnaryOp::Ceil,
            FloatUnaryOp::Floor,
            FloatUnaryOp::Round,
        ] {
            assert_eq!(eval_float_unary(op, 0, ValueType::F80), None);
        }
    }

    /// A NaN-producing fold is withheld (returns `None`) so a host-specific
    /// NaN encoding is never baked into the IR.  Non-NaN results still fold.
    #[test]
    fn eval_binary_withholds_nan_result() {
        let zero = 0.0f64.to_bits();
        // 0.0 / 0.0 = NaN, inf + (-inf) = NaN → not folded.
        assert_eq!(eval_float_binary(FloatBinaryOp::Div, zero, zero, ValueType::F64), None);
        let inf = f64::INFINITY.to_bits();
        let neg_inf = (f64::NEG_INFINITY).to_bits();
        assert_eq!(eval_float_binary(FloatBinaryOp::Add, inf, neg_inf, ValueType::F64), None);
        // NaN operand propagates to a NaN result → not folded.
        let nan = f64::NAN.to_bits();
        assert_eq!(eval_float_binary(FloatBinaryOp::Mul, nan, 2.0f64.to_bits(), ValueType::F64), None);
        // A normal (non-NaN) result still folds.
        let three = eval_float_binary(FloatBinaryOp::Add, 1.0f64.to_bits(), 2.0f64.to_bits(), ValueType::F64);
        assert_eq!(three.map(f64::from_bits), Some(3.0));
        // F32 mirror.
        assert_eq!(
            eval_float_binary(FloatBinaryOp::Div, 0.0f32.to_bits() as u64, 0.0f32.to_bits() as u64, ValueType::F32),
            None
        );
    }

    #[test]
    fn eval_unary_withholds_nan_result() {
        // sqrt(-1.0) = NaN → not folded; sqrt(4.0) = 2.0 still folds.
        assert_eq!(eval_float_unary(FloatUnaryOp::Sqrt, (-1.0f64).to_bits(), ValueType::F64), None);
        let two = eval_float_unary(FloatUnaryOp::Sqrt, 4.0f64.to_bits(), ValueType::F64);
        assert_eq!(two.map(f64::from_bits), Some(2.0));
    }

    /// Comparisons are unaffected: NaN compares unordered (a portable boolean),
    /// so they still fold.
    #[test]
    fn eval_cmp_folds_nan_operands() {
        let nan = f64::NAN.to_bits();
        assert_eq!(eval_float_cmp(FloatCmpOp::Equal, nan, nan, ValueType::F64), Some(false));
        assert_eq!(eval_float_cmp(FloatCmpOp::Less, nan, 1.0f64.to_bits(), ValueType::F64), Some(false));
    }
}
