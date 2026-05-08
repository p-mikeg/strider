//! [`CastMask`] — a bitset selector for value-passthrough cast `NodeKind`
//! variants the matcher should walk through transparently.
//!
//! See `docs/superpowers/specs/2026-04-27-cast-mask-design.md`.

use bitflags::bitflags;
use ir::ExtendOp;
use ir::node::NodeKind;

bitflags! {
    /// Bitset selecting which value-passthrough cast `NodeKind`s the
    /// matcher walks through transparently.  Pass to
    /// [`Matcher::ignore_casts_mask`](crate::Matcher::ignore_casts_mask)
    /// to enable selective walk-through; [`Matcher::ignore_casts`](
    /// crate::Matcher::ignore_casts) is shorthand for `ignore_casts_mask(
    /// CastMask::all())`.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
    pub struct CastMask: u32 {
        const ZERO_EXTEND       = 1 << 0;
        const SIGN_EXTEND       = 1 << 1;
        const TRUNCATE          = 1 << 2;
        const CAST_TO_INT       = 1 << 3;
        const CAST_TO_FLOAT     = 1 << 4;
        const CAST_TO_BOOL      = 1 << 5;
        const INT_BITS_TO_FLOAT = 1 << 6;
        const FLOAT_BITS_TO_INT = 1 << 7;

        /// Both extension flavours: `ZERO_EXTEND | SIGN_EXTEND`.
        const EXTEND = Self::ZERO_EXTEND.bits() | Self::SIGN_EXTEND.bits();
    }
}

/// Returns the single-bit [`CastMask`] for value-passthrough cast
/// `NodeKind`s, or [`CastMask::empty()`] for any other kind.
///
/// The match is deliberately exhaustive (no `_` arm).  Adding a new
/// cast-like `NodeKind` variant is a compile error here, forcing the
/// author to classify it.
#[must_use]
pub(crate) const fn cast_mask_of(kind: &NodeKind) -> CastMask {
    match kind {
        NodeKind::Extend(ExtendOp::ZeroExtend) => CastMask::ZERO_EXTEND,
        NodeKind::Extend(ExtendOp::SignExtend) => CastMask::SIGN_EXTEND,
        NodeKind::Truncate => CastMask::TRUNCATE,
        NodeKind::CastToInt => CastMask::CAST_TO_INT,
        NodeKind::CastToFloat => CastMask::CAST_TO_FLOAT,
        NodeKind::CastToBool => CastMask::CAST_TO_BOOL,
        NodeKind::IntBitsToFloat => CastMask::INT_BITS_TO_FLOAT,
        NodeKind::FloatBitsToInt => CastMask::FLOAT_BITS_TO_INT,

        // Explicit non-cast list: every other NodeKind variant.  The
        // exhaustive `match` (no `_`) catches future cast-like additions
        // at compile time so a new cast-like kind doesn't silently miss
        // the walk-through.
        NodeKind::Entry
        | NodeKind::InitialMemory
        | NodeKind::InitialVar(_)
        | NodeKind::FunctionArg { .. }
        | NodeKind::ControlState
        | NodeKind::VarPhi(_)
        | NodeKind::MemPhi
        | NodeKind::ValuePhi
        | NodeKind::If
        | NodeKind::Call
        | NodeKind::CallOther { .. }
        | NodeKind::Return
        | NodeKind::IndirectBranch
        | NodeKind::Load(_)
        | NodeKind::Store(_)
        | NodeKind::StackStore { .. }
        | NodeKind::StackStorePhi { .. }
        | NodeKind::IntConst(_)
        | NodeKind::IntConstWide(_)
        | NodeKind::IntUnaryOp(_)
        | NodeKind::IntBinaryOp(_)
        | NodeKind::IntCmpOp(_)
        | NodeKind::Popcount
        | NodeKind::Lzcount
        | NodeKind::BoolConst(_)
        | NodeKind::BoolUnaryOp(_)
        | NodeKind::BoolBinaryOp(_)
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
