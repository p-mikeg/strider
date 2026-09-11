use strider_ir::node::ValueType;
use strider_ir::{FloatBinaryOp, FloatCmpOp, FloatUnaryOp};

// `$ty` must be `f32` or `f64`; the result widens into a u64 lane.
macro_rules! eval_binary {
    ($ty:ty, $op:expr, $bits_l:expr, $bits_r:expr) => {{
        let l = <$ty>::from_bits($bits_l as _);
        let r = <$ty>::from_bits($bits_r as _);
        let result: $ty = match $op {
            FloatBinaryOp::Add => l + r,
            FloatBinaryOp::Mul => l * r,
            FloatBinaryOp::Div => l / r,
        };
        // IEEE 754 does not fix a NaN's quiet bit, payload or sign, so folding
        // one would bake in this host's encoding instead of the target's.
        // Non-NaN results are bit-portable under default rounding.
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
            // `FLOAT_ROUND` is round-half-away-from-zero, not mode-dependent.
            FloatUnaryOp::Round => v.round(),
        };
        // NaN withheld, as in `eval_binary!`.
        if result.is_nan() {
            None
        } else {
            Some(result.to_bits() as u64)
        }
    }};
}

/// Operates on raw bit patterns. `None` when the type is unsupported (F80) or
/// the result is NaN (see `eval_binary!`).
pub(crate) fn eval_float_binary(
    op: FloatBinaryOp,
    bits_l: u64,
    bits_r: u64,
    ty: ValueType,
) -> Option<u64> {
    match ty {
        ValueType::F32 => eval_binary!(f32, op, bits_l as u32, bits_r as u32),
        ValueType::F64 => eval_binary!(f64, op, bits_l, bits_r),
        // Rust has no native 80-bit float, so F80 never folds; the rule skips
        // and the node survives for pattern queries, which care about graph
        // shape rather than values.
        _ => None,
    }
}

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

    /// Pinned against a partial F80 path via `f64`, which would silently
    /// truncate.
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

    #[test]
    fn eval_binary_withholds_nan_result() {
        let zero = 0.0f64.to_bits();
        assert_eq!(
            eval_float_binary(FloatBinaryOp::Div, zero, zero, ValueType::F64),
            None
        );
        let inf = f64::INFINITY.to_bits();
        let neg_inf = (f64::NEG_INFINITY).to_bits();
        assert_eq!(
            eval_float_binary(FloatBinaryOp::Add, inf, neg_inf, ValueType::F64),
            None
        );
        let nan = f64::NAN.to_bits();
        assert_eq!(
            eval_float_binary(FloatBinaryOp::Mul, nan, 2.0f64.to_bits(), ValueType::F64),
            None
        );
        // A non-NaN result still folds.
        let three = eval_float_binary(
            FloatBinaryOp::Add,
            1.0f64.to_bits(),
            2.0f64.to_bits(),
            ValueType::F64,
        );
        assert_eq!(three.map(f64::from_bits), Some(3.0));
        assert_eq!(
            eval_float_binary(
                FloatBinaryOp::Div,
                0.0f32.to_bits() as u64,
                0.0f32.to_bits() as u64,
                ValueType::F32
            ),
            None
        );
    }

    #[test]
    fn eval_unary_withholds_nan_result() {
        assert_eq!(
            eval_float_unary(FloatUnaryOp::Sqrt, (-1.0f64).to_bits(), ValueType::F64),
            None
        );
        let two = eval_float_unary(FloatUnaryOp::Sqrt, 4.0f64.to_bits(), ValueType::F64);
        assert_eq!(two.map(f64::from_bits), Some(2.0));
    }

    /// Comparisons still fold on NaN: unordered is a portable boolean, unlike a
    /// NaN bit pattern.
    #[test]
    fn eval_cmp_folds_nan_operands() {
        let nan = f64::NAN.to_bits();
        assert_eq!(
            eval_float_cmp(FloatCmpOp::Equal, nan, nan, ValueType::F64),
            Some(false)
        );
        assert_eq!(
            eval_float_cmp(FloatCmpOp::Less, nan, 1.0f64.to_bits(), ValueType::F64),
            Some(false)
        );
    }
}
