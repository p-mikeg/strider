use ir::node::NodeOutputType;
use ir::{FloatBinaryOp, FloatCmpOp, FloatUnaryOp};

// ── float constant evaluation ─────────────────────────────────────────────────

/// Evaluates a float binary op on raw bit patterns.  Returns the result as a
/// raw bit pattern, or `None` for undefined operations (should not occur in
/// IEEE 754, but we keep the Option for consistency with the int version).
pub(super) fn eval_float_binary(
    op: FloatBinaryOp,
    bits_l: u64,
    bits_r: u64,
    ty: NodeOutputType,
) -> Option<u64> {
    match ty {
        NodeOutputType::F32 => {
            let l = f32::from_bits(bits_l as u32);
            let r = f32::from_bits(bits_r as u32);
            let result = match op {
                FloatBinaryOp::Add => l + r,
                FloatBinaryOp::Sub => l - r,
                FloatBinaryOp::Mul => l * r,
                FloatBinaryOp::Div => l / r,
            };
            Some(result.to_bits() as u64)
        }
        NodeOutputType::F64 => {
            let l = f64::from_bits(bits_l);
            let r = f64::from_bits(bits_r);
            let result = match op {
                FloatBinaryOp::Add => l + r,
                FloatBinaryOp::Sub => l - r,
                FloatBinaryOp::Mul => l * r,
                FloatBinaryOp::Div => l / r,
            };
            Some(result.to_bits())
        }
        _ => None,
    }
}

/// Evaluates a float comparison on raw bit patterns.
pub(super) fn eval_float_cmp(
    op: FloatCmpOp,
    bits_l: u64,
    bits_r: u64,
    ty: NodeOutputType,
) -> Option<bool> {
    match ty {
        NodeOutputType::F32 => {
            let l = f32::from_bits(bits_l as u32);
            let r = f32::from_bits(bits_r as u32);
            Some(match op {
                FloatCmpOp::Equal => l == r,
                FloatCmpOp::NotEqual => l != r,
                FloatCmpOp::Less => l < r,
                FloatCmpOp::LessEqual => l <= r,
            })
        }
        NodeOutputType::F64 => {
            let l = f64::from_bits(bits_l);
            let r = f64::from_bits(bits_r);
            Some(match op {
                FloatCmpOp::Equal => l == r,
                FloatCmpOp::NotEqual => l != r,
                FloatCmpOp::Less => l < r,
                FloatCmpOp::LessEqual => l <= r,
            })
        }
        _ => None,
    }
}

/// Evaluates a float unary op on a raw bit pattern.
pub(super) fn eval_float_unary(op: FloatUnaryOp, bits: u64, ty: NodeOutputType) -> Option<u64> {
    match ty {
        NodeOutputType::F32 => {
            let v = f32::from_bits(bits as u32);
            let result = match op {
                FloatUnaryOp::Neg => -v,
                FloatUnaryOp::Abs => v.abs(),
                FloatUnaryOp::Sqrt => v.sqrt(),
                FloatUnaryOp::Ceil => v.ceil(),
                FloatUnaryOp::Floor => v.floor(),
                // IEEE 754 / hardware default: ties-to-even, not Rust's
                // ties-away-from-zero `round`.
                FloatUnaryOp::Round => v.round_ties_even(),
            };
            Some(result.to_bits() as u64)
        }
        NodeOutputType::F64 => {
            let v = f64::from_bits(bits);
            let result = match op {
                FloatUnaryOp::Neg => -v,
                FloatUnaryOp::Abs => v.abs(),
                FloatUnaryOp::Sqrt => v.sqrt(),
                FloatUnaryOp::Ceil => v.ceil(),
                FloatUnaryOp::Floor => v.floor(),
                FloatUnaryOp::Round => v.round_ties_even(),
            };
            Some(result.to_bits())
        }
        // F80 (and all non-float types) fall through.  Rust has no native
        // 80-bit float type, so opt rules can't constant-fold F80 ops —
        // the rule sees `None` and skips, leaving the F80 node in the IR
        // for pattern-matching workloads.  Bit-exact F80 emulation is out
        // of scope; pattern queries care about graph shape, not values.
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
        for op in [
            FloatBinaryOp::Add,
            FloatBinaryOp::Sub,
            FloatBinaryOp::Mul,
            FloatBinaryOp::Div,
        ] {
            assert_eq!(eval_float_binary(op, zero, zero, NodeOutputType::F80), None);
        }
    }

    #[test]
    fn eval_f80_cmp_returns_none() {
        let zero = 0u64;
        for op in [
            FloatCmpOp::Equal,
            FloatCmpOp::NotEqual,
            FloatCmpOp::Less,
            FloatCmpOp::LessEqual,
        ] {
            assert_eq!(eval_float_cmp(op, zero, zero, NodeOutputType::F80), None);
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
            assert_eq!(eval_float_unary(op, 0, NodeOutputType::F80), None);
        }
    }
}
