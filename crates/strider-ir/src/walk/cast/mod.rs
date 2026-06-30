//! [`CastMask`] — a bitset selector for value-passthrough cast `NodeKind`
//! variants a consumer (notably the strider-orchestrator pattern matcher) should
//! walk through transparently.
//!
//! Lives in `strider-ir::walk` so the structural classification — which
//! `NodeKind` variants are value-passthrough casts — has a single source of
//! truth alongside the other structural traversal primitives.  Pattern
//! semantics (when to walk through them) stays in the analyzer.
//!
//! See `docs/superpowers/specs/2026-04-27-cast-mask-design.md`.

use bitflags::bitflags;

use crate::ExtendOp;
use crate::node::NodeKind;

bitflags! {
    /// Bitset selecting which value-passthrough cast `NodeKind`s a
    /// consumer (notably the matcher) walks through transparently.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
    pub struct CastMask: u32 {
        const ZERO_EXTEND       = 1 << 0;
        const SIGN_EXTEND       = 1 << 1;
        const TRUNCATE          = 1 << 2;
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
pub const fn cast_mask_of(kind: &NodeKind) -> CastMask {
    match kind {
        NodeKind::Extend(ExtendOp::ZeroExtend) => CastMask::ZERO_EXTEND,
        NodeKind::Extend(ExtendOp::SignExtend) => CastMask::SIGN_EXTEND,
        NodeKind::Truncate => CastMask::TRUNCATE,
        NodeKind::IntBitsToFloat => CastMask::INT_BITS_TO_FLOAT,
        NodeKind::FloatBitsToInt => CastMask::FLOAT_BITS_TO_INT,

        // Explicit non-cast list: every other NodeKind variant.  The
        // exhaustive `match` (no `_`) catches future cast-like additions
        // at compile time so a new cast-like kind doesn't silently miss
        // the walk-through.
        NodeKind::Entry
        | NodeKind::InitialMemory
        | NodeKind::InitialVar(_)
        | NodeKind::Region
        | NodeKind::Phi
        | NodeKind::MemPhi
        | NodeKind::If
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
