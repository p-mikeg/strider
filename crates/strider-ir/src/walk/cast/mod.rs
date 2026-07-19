//! Which `NodeKind` variants are value-passthrough casts. The structural
//! classification lives here as the single source of truth; the pattern
//! semantics (when a consumer walks through one) stay in the matcher.

use bitflags::bitflags;

use crate::ExtendOp;
use crate::node::NodeKind;

bitflags! {
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
    pub struct CastMask: u32 {
        const ZERO_EXTEND       = 1 << 0;
        const SIGN_EXTEND       = 1 << 1;
        const TRUNCATE          = 1 << 2;
        const INT_BITS_TO_FLOAT = 1 << 6;
        const FLOAT_BITS_TO_INT = 1 << 7;

        const EXTEND = Self::ZERO_EXTEND.bits() | Self::SIGN_EXTEND.bits();
    }
}

/// Empty for non-cast kinds. The match is exhaustive on purpose: a new
/// cast-like `NodeKind` fails to compile until someone classifies it.
pub const fn cast_mask_of(kind: &NodeKind) -> CastMask {
    match kind {
        NodeKind::Extend(ExtendOp::ZeroExtend) => CastMask::ZERO_EXTEND,
        NodeKind::Extend(ExtendOp::SignExtend) => CastMask::SIGN_EXTEND,
        NodeKind::Truncate => CastMask::TRUNCATE,
        NodeKind::IntBitsToFloat => CastMask::INT_BITS_TO_FLOAT,
        NodeKind::FloatBitsToInt => CastMask::FLOAT_BITS_TO_INT,

        NodeKind::Entry
        | NodeKind::InitialMemory
        | NodeKind::InitialVar(_)
        | NodeKind::Region
        | NodeKind::Phi
        | NodeKind::MemPhi
        | NodeKind::If
        | NodeKind::Switch
        | NodeKind::Call
        | NodeKind::CallOther { .. }
        | NodeKind::Return
        | NodeKind::IndirectBranch
        | NodeKind::Unreachable
        | NodeKind::Load(_)
        | NodeKind::Store(_)
        | NodeKind::IntConst(_)
        | NodeKind::IntUnaryOp(_)
        | NodeKind::IntBinaryOp(_)
        | NodeKind::IntCmpOp(_)
        | NodeKind::Popcount
        | NodeKind::Lzcount
        | NodeKind::FloatConst(_)
        | NodeKind::FloatUnaryOp(_)
        | NodeKind::FloatBinaryOp(_)
        | NodeKind::FloatCmpOp(_)
        | NodeKind::IntToFloat
        | NodeKind::FloatToInt
        | NodeKind::FloatToFloat
        | NodeKind::SegmentOp { .. }
        | NodeKind::CPoolRef
        | NodeKind::New => CastMask::empty(),
    }
}

#[cfg(test)]
mod tests;
