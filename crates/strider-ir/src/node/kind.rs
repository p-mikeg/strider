use std::ops::RangeInclusive;

use super::CastMask;
use crate::node::signature::{Conversion, ExpectedValueKind, OperandWidth};
use crate::node::{ExtendOp, FloatBinaryOp, FloatCmpOp, IntBinaryOp, IntCmpOp};
use cranelift_entity::entity_impl;

/// Where a function argument is passed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FunctionArgSource {
    /// Always the full-width container register (`RDI`, never `EDI`).
    Register(rsleigh::Vn),
    /// Byte `offset` from the entry-time stack pointer (`InitialVar(sp)`), in
    /// the stack's address `space` (usually RAM).
    Stack {
        space: rsleigh::VnSpace,
        offset: i128,
    },
}

/// Dense id of a tracked varnode, assigned in `(space, offset, size)` order.
/// Resolve via [`crate::Function::initial_vn`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct InitialVnId(u32);
cranelift_entity::entity_impl!(InitialVnId);

impl InitialVnId {
    #[inline]
    pub fn from_index(index: usize) -> Self {
        Self(index as u32)
    }
}

/// A `Switch`'s case-address table; read it through
/// [`crate::Function::switch_targets`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SwitchTableId(u32);
entity_impl!(SwitchTableId, "switch_table");

/// A `Call`'s override calling convention, deduped by value. Resolve via
/// [`crate::Function::cc`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CcId(u32);
entity_impl!(CcId, "cc");

/// An interned integer constant. Opaque; read the value through
/// [`crate::IRViewer::int_const`] or its width-aware forms
/// [`crate::IRViewer::int_const_u128`] and [`crate::IRViewer::int_const_i128`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct IntConstId(u32);
entity_impl!(IntConstId, "const");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeKind {
    /// Outputs `[Control]`.
    Entry,
    /// Outputs `[Memory]`.
    InitialMemory,
    /// Entry-time value of a tracked varnode.
    InitialVar(InitialVnId),

    /// Inputs: one `Control` per predecessor. Outputs: `[Control, PhiToken]`.
    Region,
    MemPhi,
    /// Inputs: `[phi_token, val_0, val_1, ...]`, one value per predecessor in
    /// the joining `Region`'s `Control` input order. A lifted phi's varnode is
    /// in the `value_vn` side table.
    Phi,

    /// Inputs: `(control, cond:I1)`. Outputs: `[true, false]` controls.
    If,
    /// Resolved jump table. Inputs: `(control, address)`, plus an optional
    /// ISA-mode bit. Output `i` is taken when `address == targets[i]`. No
    /// default arm: an unproven target is reported as an unresolved branch.
    Switch(SwitchTableId),

    /// Inputs: `[control, memory, target, sp, args...]`. Outputs:
    /// `[control, memory]`, then the return registers and the clobbers.
    /// `cc: None` is the function's default convention.
    Call {
        cc: Option<CcId>,
    },
    /// Inputs: `[control, memory, return values...]`.
    Return,
    /// Unresolved branch. Inputs: `[control, memory, target]`, plus an
    /// optional interworking ISA-mode bit at slot 3. No outputs.
    IndirectBranch,
    /// No-return sink (`ud2`, `abort`). Inputs: `[control]`, plus an optional
    /// memory chain at slot 1 that keeps an exit-free cycle's stores alive.
    Unreachable,

    Load(rsleigh::VnSpace),
    Store(rsleigh::VnSpace),

    /// Read via `IRViewer::int_const`, not the payload.
    IntConst(crate::node::IntConstId),
    /// Negation only; complement is `Xor(x, all_ones)`.
    IntUnaryOp(crate::node::IntUnaryOp),
    IntBinaryOp(crate::node::IntBinaryOp),
    /// Outputs `I1`. Boolean logic is `IntBinaryOp::{And,Or,Xor}` at `I1`.
    IntCmpOp(crate::node::IntCmpOp),
    Truncate,
    Popcount,
    Lzcount,
    Extend(crate::node::ExtendOp),

    /// Raw IEEE 754 bits, masked to the output width (upper 32 bits zero for
    /// `F32`).
    FloatConst(u64),
    FloatBinaryOp(crate::node::FloatBinaryOp),
    FloatUnaryOp(crate::node::FloatUnaryOp),
    /// Outputs `I1`.
    FloatCmpOp(crate::node::FloatCmpOp),

    /// Signed integer to nearest float.
    IntToFloat,
    /// Float to integer, truncating toward zero.
    FloatToInt,
    FloatToFloat,

    /// Same-size bitcast.
    IntBitsToFloat,
    /// Same-size bitcast.
    FloatBitsToInt,

    /// Sleigh `CallOther` (CPU intrinsics), and the lifter's volatile register
    /// reads. Inputs: `[control, memory, args...]`. Outputs:
    /// `[Control, Memory]`, then the result slot if the instruction has an
    /// output varnode, then one per implicit-write clobber. Only a
    /// memory-clobbering op advances the memory chain.
    CallOther {
        user_op_id: u64,
    },

    /// Sleigh `CPoolRef` (Java constant pool; may load classes). Inputs:
    /// `[refs...]`. Outputs: one integer.
    CPoolRef,

    /// Sleigh `New` (Java allocation, distinct per occurrence). Inputs: the
    /// p-code operands (class reference, optional count). Outputs: one
    /// pointer-sized integer.
    New,
}

bitflags::bitflags! {
    /// Per-[`NodeKind`] properties.
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub(crate) struct KindFlags: u64 {
        const CACHEABLE = 1 << 0;
        /// Synthesised without a contributing machine instruction.
        const FINGERPRINT_EXEMPT = 1 << 1;
        /// Carries a `Control` input or output.
        const CONTROL = 1 << 2;
        /// Must survive unused although it carries no control.
        const EFFECTFUL = 1 << 3;
        const COMMUTATIVE = 1 << 4;
        /// Consumes control and produces none.
        const TERMINATOR = 1 << 5;
        // A cast kind's `CastMask`, shifted by `CAST_SHIFT`.
        const ZERO_EXTEND = (CastMask::ZERO_EXTEND.bits() as u64) << Self::CAST_SHIFT;
        const SIGN_EXTEND = (CastMask::SIGN_EXTEND.bits() as u64) << Self::CAST_SHIFT;
        const TRUNCATE = (CastMask::TRUNCATE.bits() as u64) << Self::CAST_SHIFT;
        const INT_BITS_TO_FLOAT = (CastMask::INT_BITS_TO_FLOAT.bits() as u64) << Self::CAST_SHIFT;
        const FLOAT_BITS_TO_INT = (CastMask::FLOAT_BITS_TO_INT.bits() as u64) << Self::CAST_SHIFT;
    }
}

impl KindFlags {
    pub(crate) const CAST_SHIFT: u32 = 32;
}

impl NodeKind {
    /// `Load` / `Store` address input.
    pub const ACCESS_ADDR_SLOT: usize = 1;
    pub const STORE_DATA_SLOT: usize = 2;
    pub const CALL_TARGET_SLOT: usize = 2;
    /// `Call`'s stack-pointer anchor input.
    pub const CALL_SP_SLOT: usize = 3;
    pub const IF_COND_SLOT: usize = 1;
    /// Control input of `If`, `Switch`, `Call`, `CallOther`, `Return`,
    /// `IndirectBranch`, `Unreachable`.
    pub const CTRL_SLOT: usize = 0;
    /// Memory input of `Call`, `CallOther`, `Return`, `IndirectBranch`,
    /// `Unreachable`.
    pub const CTRL_MEM_SLOT: usize = 1;
    /// `Load` / `Store` memory input.
    pub const ACCESS_MEM_SLOT: usize = 0;
    pub const INDIRECT_BRANCH_TARGET_SLOT: usize = 2;
    pub const SWITCH_SELECTOR_SLOT: usize = 1;
    /// `Phi` / `MemPhi` input from the owning `Region`.
    pub const PHI_TOKEN_SLOT: usize = 0;
    /// Control output of `Entry`, `Region`, `Call`, `CallOther`.
    pub const CTRL_OUT_SLOT: usize = 0;
    /// Memory output of `Call`, `CallOther`.
    pub const CTRL_MEM_OUT_SLOT: usize = 1;
    /// Memory output of `InitialMemory`, `Store`, `MemPhi`.
    pub const MEM_OUT_SLOT: usize = 0;
    pub const REGION_PHI_OUT_SLOT: usize = 1;
    pub const IF_TRUE_OUT_SLOT: usize = 0;
    pub const IF_FALSE_OUT_SLOT: usize = 1;

    #[inline]
    pub fn is_const(self) -> bool {
        matches!(self, Self::IntConst(..) | Self::FloatConst(..))
    }

    /// Fixed input slots before the variadic tail.
    #[must_use]
    pub fn input_head_len(&self) -> usize {
        crate::node::signature::expected_signature(self)
            .inputs
            .head_len()
    }

    /// Fixed output slots before the variadic tail.
    #[must_use]
    pub fn output_head_len(&self) -> usize {
        crate::node::signature::expected_signature(self)
            .outputs
            .head_len()
    }

    /// `None` past the arity bound.
    #[must_use]
    pub fn expected_output_kind(&self, idx: usize) -> Option<ExpectedValueKind> {
        crate::node::signature::expected_signature(self)
            .outputs
            .at(idx)
            .map(|s| s.kind)
    }

    /// `None` past the arity bound.
    #[must_use]
    pub fn expected_input_kind(&self, idx: usize) -> Option<ExpectedValueKind> {
        crate::node::signature::expected_signature(self)
            .inputs
            .at(idx)
            .map(|s| s.kind)
    }

    /// A repeating tail ends at `usize::MAX`.
    #[must_use]
    pub fn input_arity(&self) -> RangeInclusive<usize> {
        let inputs = crate::node::signature::expected_signature(self).inputs;
        inputs.head_len()..=inputs.max_len()
    }

    /// The width `validate` requires of input `slot`.
    #[must_use]
    pub fn operand_width(&self, slot: usize) -> OperandWidth {
        match self {
            // p-code leaves a shift COUNT any width.
            Self::IntBinaryOp(op) => {
                if op.is_shift() && slot > 0 {
                    OperandWidth::Free
                } else {
                    OperandWidth::OfOutput
                }
            }
            Self::IntUnaryOp(..) | Self::FloatBinaryOp(..) | Self::FloatUnaryOp(..) => {
                OperandWidth::OfOutput
            }
            Self::IntCmpOp(..) | Self::FloatCmpOp(..) => OperandWidth::OfFirstOperand,
            Self::Extend(..) => OperandWidth::Conversion(Conversion::Widen),
            Self::Truncate => OperandWidth::Conversion(Conversion::Narrow),
            Self::IntBitsToFloat | Self::FloatBitsToInt => {
                OperandWidth::Conversion(Conversion::SameSize)
            }
            Self::Entry
            | Self::InitialMemory
            | Self::InitialVar(..)
            | Self::Region
            | Self::MemPhi
            | Self::Phi
            | Self::If
            | Self::Switch(..)
            | Self::Call { .. }
            | Self::Return
            | Self::IndirectBranch
            | Self::Unreachable
            | Self::Load(..)
            | Self::Store(..)
            | Self::IntConst(..)
            | Self::Popcount
            | Self::Lzcount
            | Self::FloatConst(..)
            | Self::IntToFloat
            | Self::FloatToInt
            | Self::FloatToFloat
            | Self::CallOther { .. }
            | Self::CPoolRef
            | Self::New => OperandWidth::Free,
        }
    }

    /// Consumes control and produces none.
    #[must_use]
    pub fn is_terminator(&self) -> bool {
        self.flags().contains(KindFlags::TERMINATOR)
    }

    #[inline]
    pub(crate) fn is_cacheable(&self) -> bool {
        self.flags().contains(KindFlags::CACHEABLE)
    }

    /// Exempt from the non-empty asm-fingerprint invariant: synthesised
    /// without a contributing machine instruction.
    #[inline]
    pub fn asm_fingerprint_exempt(&self) -> bool {
        self.flags().contains(KindFlags::FINGERPRINT_EXEMPT)
    }

    #[inline]
    pub(crate) fn has_control_flow(&self) -> bool {
        self.flags().contains(KindFlags::CONTROL)
    }

    /// Must survive even with all outputs unused. `Store` counts (removing
    /// one needs alias reasoning); `Load` and `MemPhi` do not.
    #[inline]
    pub(crate) fn has_side_effects(&self) -> bool {
        self.flags()
            .intersects(KindFlags::CONTROL.union(KindFlags::EFFECTFUL))
    }

    #[inline]
    pub(crate) const fn flags(&self) -> KindFlags {
        use KindFlags as F;
        match self {
            Self::Entry => F::CACHEABLE.union(F::FINGERPRINT_EXEMPT).union(F::CONTROL),
            Self::InitialMemory | Self::InitialVar(..) => F::CACHEABLE.union(F::FINGERPRINT_EXEMPT),
            Self::Region => F::FINGERPRINT_EXEMPT.union(F::CONTROL),
            // Inputs grow after construction, so identity is not fixed.
            Self::MemPhi | Self::Phi => F::FINGERPRINT_EXEMPT,
            // Each occurrence is a distinct event.
            Self::If | Self::Switch(_) | Self::Call { .. } | Self::CallOther { .. } => F::CONTROL,
            Self::Return | Self::IndirectBranch | Self::Unreachable => {
                F::CONTROL.union(F::TERMINATOR)
            }
            Self::Store(..) => F::CACHEABLE.union(F::EFFECTFUL),
            // Opaque per-occurrence identity.
            Self::CPoolRef | Self::New => F::EFFECTFUL,
            Self::IntBinaryOp(
                IntBinaryOp::Add
                | IntBinaryOp::Mul
                | IntBinaryOp::And
                | IntBinaryOp::Or
                | IntBinaryOp::Xor,
            )
            | Self::FloatBinaryOp(FloatBinaryOp::Add | FloatBinaryOp::Mul)
            | Self::IntCmpOp(IntCmpOp::Equal | IntCmpOp::Carry | IntCmpOp::Scarry)
            | Self::FloatCmpOp(FloatCmpOp::Equal) => F::CACHEABLE.union(F::COMMUTATIVE),
            Self::Load(..)
            | Self::IntConst(..)
            | Self::IntUnaryOp(..)
            | Self::IntBinaryOp(..)
            | Self::IntCmpOp(..)
            | Self::Popcount
            | Self::Lzcount
            | Self::FloatConst(..)
            | Self::FloatBinaryOp(..)
            | Self::FloatUnaryOp(..)
            | Self::FloatCmpOp(..)
            | Self::IntToFloat
            | Self::FloatToInt
            | Self::FloatToFloat => F::CACHEABLE,
            Self::Extend(ExtendOp::ZeroExtend) => F::CACHEABLE.union(F::ZERO_EXTEND),
            Self::Extend(ExtendOp::SignExtend) => F::CACHEABLE.union(F::SIGN_EXTEND),
            Self::Truncate => F::CACHEABLE.union(F::TRUNCATE),
            Self::IntBitsToFloat => F::CACHEABLE.union(F::INT_BITS_TO_FLOAT),
            Self::FloatBitsToInt => F::CACHEABLE.union(F::FLOAT_BITS_TO_INT),
        }
    }

    /// Whether a pattern may try both operand orders. Float `Add` / `Mul`
    /// commute up to NaN payload; `Carry` / `Scarry` test `l + r`.
    #[inline]
    pub fn is_commutative(&self) -> bool {
        self.flags().contains(KindFlags::COMMUTATIVE)
    }
}

// The largest inline payload is a u64 (FloatConst / CallOther).
const _: () = assert!(size_of::<NodeKind>() == 16);

#[cfg(test)]
mod tests {
    use super::NodeKind;

    #[test]
    fn commutative_kinds_are_exactly_the_symmetric_ops() {
        use crate::node::{FloatBinaryOp, FloatCmpOp, IntBinaryOp, IntCmpOp};
        let mut kinds = crate::node::signature::every_node_kind();
        kinds.extend(IntBinaryOp::ALL.map(NodeKind::IntBinaryOp));
        kinds.extend(IntCmpOp::ALL.map(NodeKind::IntCmpOp));
        kinds.extend(FloatBinaryOp::ALL.map(NodeKind::FloatBinaryOp));
        kinds.extend(FloatCmpOp::ALL.map(NodeKind::FloatCmpOp));
        let commutative = [
            NodeKind::IntBinaryOp(IntBinaryOp::Add),
            NodeKind::IntBinaryOp(IntBinaryOp::Mul),
            NodeKind::IntBinaryOp(IntBinaryOp::And),
            NodeKind::IntBinaryOp(IntBinaryOp::Or),
            NodeKind::IntBinaryOp(IntBinaryOp::Xor),
            NodeKind::FloatBinaryOp(FloatBinaryOp::Add),
            NodeKind::FloatBinaryOp(FloatBinaryOp::Mul),
            NodeKind::IntCmpOp(IntCmpOp::Equal),
            NodeKind::IntCmpOp(IntCmpOp::Carry),
            NodeKind::IntCmpOp(IntCmpOp::Scarry),
            NodeKind::FloatCmpOp(FloatCmpOp::Equal),
        ];
        for k in kinds {
            assert_eq!(k.is_commutative(), commutative.contains(&k), "{k:?}");
        }
    }

    #[test]
    fn terminators_are_return_indirect_branch_and_unreachable() {
        let terminators = [
            NodeKind::Return,
            NodeKind::IndirectBranch,
            NodeKind::Unreachable,
        ];
        for k in crate::node::signature::every_node_kind() {
            assert_eq!(k.is_terminator(), terminators.contains(&k), "{k:?}");
        }
    }
}
