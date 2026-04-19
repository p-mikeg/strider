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
                FloatUnaryOp::Round => v.round(),
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
                FloatUnaryOp::Round => v.round(),
            };
            Some(result.to_bits())
        }
        _ => None,
    }
}
