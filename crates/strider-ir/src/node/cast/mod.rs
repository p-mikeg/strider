use bitflags::bitflags;

use super::{KindFlags, NodeKind};

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

/// Empty for non-cast kinds.
pub const fn cast_mask_of(kind: &NodeKind) -> CastMask {
    CastMask::from_bits_retain((kind.flags().bits() >> KindFlags::CAST_SHIFT) as u32)
}

#[cfg(test)]
mod tests;
