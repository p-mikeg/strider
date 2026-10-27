use std::ops::RangeInclusive;

use crate::node::{FloatBinaryOp, FloatCmpOp, IntBinaryOp, IntCmpOp};
use crate::node_signature::{Conversion, ExpectedValueKind, OperandWidth};
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

    #[inline]
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// A `Switch`'s case-address table. Resolve via
/// [`crate::Function::switch_table`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SwitchTableId(u32);
entity_impl!(SwitchTableId, "switch_table");

/// A `Call`'s override calling convention, deduped by value. Resolve via
/// [`crate::Function::cc`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CcId(u32);
entity_impl!(CcId, "cc");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeKind {
    /// Outputs `[Control]`.
    Entry,
    /// Outputs `[Memory]`.
    InitialMemory,
    /// Entry-time value of the tracked varnode at this id. Outputs one integer
    /// value.
    InitialVar(InitialVnId),

    /// Inputs: one `Control` per predecessor. Outputs: `[Control, PhiToken]`.
    Region,
    /// Selects the live memory token at a join.
    MemPhi,
    /// Inputs: `[phi_token, val_0, val_1, ...]`, one value per predecessor in
    /// the same order as the joining `Region`'s `Control` inputs. Output:
    /// `[value]`.
    ///
    /// A lifter-emitted phi for a register-aliased read carries a source
    /// varnode tag in the `value_vn` side-table; anonymous phis have no entry.
    Phi,

    /// Inputs: `(control, cond:I1)`. Outputs: `[Control, Control]`, index 0
    /// true, index 1 false.
    If,
    /// Resolved jump table. Inputs: `(control, address)`. Outputs: one
    /// `Control` per target, output `i` taken when `address == targets[i]` of
    /// the table this carries.
    /// No default arm: a target the resolver could not prove is reported as
    /// an unresolved branch, not modelled as an arm.
    Switch(SwitchTableId),

    /// Clobbers caller-saved registers and the memory token. `cc` is `None`
    /// under the function's default convention.
    Call {
        cc: Option<CcId>,
    },
    /// Consumes the outgoing control and memory edges plus any return values.
    Return,
    /// Placeholder for a branch the CFG could not resolve. Inputs:
    /// `[control, memory, target_value]`, plus the optional interworking
    /// ISA-mode bit at slot 3. Outputs: `[]`. Surviving the pipeline means
    /// classification failed; the IR stays valid.
    IndirectBranch,
    /// Control sink for a no-return trap (`ud2`, `abort`, `BUG_ON`).
    /// Inputs: `[control]`, plus the optional memory chain at slot 1, which an
    /// exit-free-cycle sink carries so its stores survive compaction.
    /// Outputs: `[]`.
    Unreachable,

    Load(rsleigh::VnSpace),
    Store(rsleigh::VnSpace),

    /// Read the value via `IRViewer::int_const_u128`, never by matching the
    /// payload.
    IntConst(crate::node::const_value::ConstId),
    /// Negation only. Bitwise complement is `Xor(x, all_ones)`.
    IntUnaryOp(crate::node::IntUnaryOp),
    IntBinaryOp(crate::node::IntBinaryOp),
    /// Outputs `I1`. Boolean logic reuses `IntBinaryOp::{And,Or,Xor}` at `I1`;
    /// logical NOT is `Xor(_, IntConst(1)):I1`.
    IntCmpOp(crate::node::IntCmpOp),
    /// Drop high bits.
    Truncate,
    Popcount,
    /// Leading zero count.
    Lzcount,
    Extend(crate::node::ExtendOp),

    /// Raw IEEE 754 bit pattern, masked to the declared output width: upper 32
    /// bits are zero for `F32`. `build_float_const` masks, `validate` re-checks.
    FloatConst(u64),
    FloatBinaryOp(crate::node::FloatBinaryOp),
    FloatUnaryOp(crate::node::FloatUnaryOp),
    /// Outputs `I1`.
    FloatCmpOp(crate::node::FloatCmpOp),

    /// Signed integer to the nearest representable float, like C's `(float)n`.
    IntToFloat,
    /// Float to integer, truncating toward zero, like C's `(int)f`.
    FloatToInt,
    /// Reprecision between float types.
    FloatToFloat,

    /// Same-size bitcast, integer to float. Bits unchanged.
    IntBitsToFloat,
    /// Same-size bitcast, float to integer. Bits unchanged.
    FloatBitsToInt,

    /// Sleigh `CallOther`: CPU intrinsics such as `cpuid`, `rdtsc`, `syscall`,
    /// x87 transcendentals. Also the lifter's `read_volatile`, a read of a
    /// register that changes with nothing writing it.
    ///
    /// Inputs: `[control, memory, arg0, arg1, ...]`. Outputs:
    /// `[Control, Memory]`, then a `Typed` slot for the result when the
    /// instruction has an output varnode, then one per implicit-write clobber.
    /// The `Memory` output is always present; only a memory-clobbering op
    /// advances the region's chain through it.
    CallOther {
        user_op_id: u64,
    },

    /// Sleigh `CPoolRef`: Java constant-pool lookup. Resolution can have
    /// observable side effects (class loading).
    ///
    /// Inputs: `[ref0, ref1, ...]`. Outputs: `[Typed]`.
    CPoolRef,

    /// Sleigh `New`: Java object allocation. Each allocation yields a distinct
    /// object.
    ///
    /// Inputs: `[size, ...]`. Outputs: `[Typed]` (pointer-sized).
    New,
}

bitflags::bitflags! {
    /// What a [`NodeKind`] is, as one row per variant in `NodeKind::flags`.
    #[derive(Clone, Copy, PartialEq, Eq)]
    struct KindFlags: u8 {
        /// The graph may dedup nodes of this kind.
        const CACHEABLE = 1 << 0;
        /// Synthesised without a contributing machine instruction.
        const FINGERPRINT_EXEMPT = 1 << 1;
        /// Carries a `Control` input or output.
        const CONTROL = 1 << 2;
        /// Must survive unused although it carries no control.
        const EFFECTFUL = 1 << 3;
    }
}

impl NodeKind {
    /// Input slot of a `Load` / `Store` address.
    pub const ACCESS_ADDR_SLOT: usize = 1;
    /// Input slot of a `Store`'s data.
    pub const STORE_DATA_SLOT: usize = 2;
    /// Input slot of a `Call`'s target.
    pub const CALL_TARGET_SLOT: usize = 2;
    /// Input slot of a `Call`'s stack-pointer anchor.
    pub const CALL_SP_SLOT: usize = 3;
    /// Input slot of an `If`'s condition.
    pub const IF_COND_SLOT: usize = 1;
    /// Input slot of the control predecessor on every kind with a fixed one
    /// (`If`, `Switch`, `Call`, `CallOther`, `Return`, `IndirectBranch`,
    /// `Unreachable`).
    pub const CTRL_SLOT: usize = 0;
    /// Input slot of the memory token after the control predecessor
    /// (`Call`, `CallOther`, `Return`, `IndirectBranch`, `Unreachable`).
    pub const CTRL_MEM_SLOT: usize = 1;
    /// Input slot of a `Load` / `Store`'s memory token.
    pub const ACCESS_MEM_SLOT: usize = 0;
    /// Input slot of an `IndirectBranch`'s target.
    pub const INDIRECT_BRANCH_TARGET_SLOT: usize = 2;
    /// Input slot of a `Switch`'s selector.
    pub const SWITCH_SELECTOR_SLOT: usize = 1;
    /// Input slot of a `Phi` / `MemPhi`'s phi token, the owning `Region`'s.
    pub const PHI_TOKEN_SLOT: usize = 0;
    /// Output slot of the control successor on every kind with one fixed
    /// (`Entry`, `Region`, `Call`, `CallOther`).
    pub const CTRL_OUT_SLOT: usize = 0;
    /// Output slot of the memory token after the control successor (`Call`,
    /// `CallOther`).
    pub const CTRL_MEM_OUT_SLOT: usize = 1;
    /// Output slot of the memory token of a kind with no other output
    /// (`InitialMemory`, `Store`, `MemPhi`).
    pub const MEM_OUT_SLOT: usize = 0;
    /// Output slot of a `Region`'s phi token.
    pub const REGION_PHI_OUT_SLOT: usize = 1;
    /// Output slot of an `If`'s arm for a true condition.
    pub const IF_TRUE_OUT_SLOT: usize = 0;
    /// Output slot of an `If`'s arm for a false condition.
    pub const IF_FALSE_OUT_SLOT: usize = 1;

    #[inline]
    pub fn is_const(self) -> bool {
        matches!(self, Self::IntConst(..) | Self::FloatConst(..))
    }

    /// Fixed input slots before the variadic tail, i.e. the slot a caller's
    /// `arg(n)` / `ret_val(n)` / `phi_input(n)` shifts by. Read off
    /// `expected_signature` so a head-slot addition cannot silently mis-wire
    /// the shift.
    #[must_use]
    pub fn input_head_len(&self) -> usize {
        crate::node_signature::expected_signature(self)
            .inputs
            .head_len()
    }

    /// Fixed output slots before the variadic tail: a `Call` / `CallOther`'s
    /// first value output.
    #[must_use]
    pub fn output_head_len(&self) -> usize {
        crate::node_signature::expected_signature(self)
            .outputs
            .head_len()
    }

    /// The value kind output slot `idx` admits, `None` past the arity bound.
    #[must_use]
    pub fn expected_output_kind(&self, idx: usize) -> Option<ExpectedValueKind> {
        crate::node_signature::expected_signature(self)
            .outputs
            .at(idx)
            .map(|s| s.kind)
    }

    /// The value kind input slot `idx` admits, `None` past the arity bound.
    #[must_use]
    pub fn expected_input_kind(&self, idx: usize) -> Option<ExpectedValueKind> {
        crate::node_signature::expected_signature(self)
            .inputs
            .at(idx)
            .map(|s| s.kind)
    }

    /// How many inputs the signature admits; a repeating tail ends at
    /// `usize::MAX`.
    #[must_use]
    pub fn input_arity(&self) -> RangeInclusive<usize> {
        let inputs = crate::node_signature::expected_signature(self).inputs;
        inputs.head_len()..=inputs.max_len()
    }

    /// The width input `slot` owes this node, as `validate` checks it.
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

    /// Consumes control and produces none. The single statement of the
    /// terminator set: a control walk that misses one concludes a live arm
    /// does not escape.
    #[must_use]
    pub fn is_terminator(&self) -> bool {
        matches!(
            self,
            Self::Return | Self::IndirectBranch | Self::Unreachable
        )
    }

    /// Whether the graph may dedup nodes of this kind.
    #[inline]
    pub fn is_cacheable(&self) -> bool {
        self.flags().contains(KindFlags::CACHEABLE)
    }

    /// Exempt from the non-empty asm-fingerprint invariant: region headers,
    /// initial state and phis are synthesised without a contributing machine
    /// instruction.
    #[inline]
    pub fn asm_fingerprint_exempt(&self) -> bool {
        self.flags().contains(KindFlags::FINGERPRINT_EXEMPT)
    }

    /// Whether the node carries a `Control` input or output.
    #[inline]
    pub fn has_control_flow(&self) -> bool {
        self.flags().contains(KindFlags::CONTROL)
    }

    /// Whether the node must survive even with all outputs unused.
    ///
    /// `Store` counts: removing it is dead-store elimination, which needs
    /// aliasing reasoning this does not do. `Load` and `MemPhi` do not.
    #[inline]
    pub fn has_side_effects(&self) -> bool {
        self.flags()
            .intersects(KindFlags::CONTROL.union(KindFlags::EFFECTFUL))
    }

    /// The one row per variant the four predicates above read.
    #[inline]
    const fn flags(&self) -> KindFlags {
        use KindFlags as F;
        match self {
            Self::Entry => F::CACHEABLE.union(F::FINGERPRINT_EXEMPT).union(F::CONTROL),
            Self::InitialMemory | Self::InitialVar(..) => F::CACHEABLE.union(F::FINGERPRINT_EXEMPT),
            Self::Region => F::FINGERPRINT_EXEMPT.union(F::CONTROL),
            // Inputs grow after construction, so identity is not fixed.
            Self::MemPhi | Self::Phi => F::FINGERPRINT_EXEMPT,
            // Each occurrence is a distinct event.
            Self::If
            | Self::Switch(_)
            | Self::Call { .. }
            | Self::CallOther { .. }
            | Self::Return
            | Self::IndirectBranch
            | Self::Unreachable => F::CONTROL,
            Self::Store(..) => F::CACHEABLE.union(F::EFFECTFUL),
            // Opaque per-occurrence identity.
            Self::CPoolRef | Self::New => F::EFFECTFUL,
            // Identity is fully determined by the payload plus the inputs.
            Self::Load(..)
            | Self::IntConst(..)
            | Self::IntUnaryOp(..)
            | Self::IntBinaryOp(..)
            | Self::IntCmpOp(..)
            | Self::Truncate
            | Self::Popcount
            | Self::Lzcount
            | Self::Extend(..)
            | Self::FloatConst(..)
            | Self::FloatBinaryOp(..)
            | Self::FloatUnaryOp(..)
            | Self::FloatCmpOp(..)
            | Self::IntToFloat
            | Self::FloatToInt
            | Self::FloatToFloat
            | Self::IntBitsToFloat
            | Self::FloatBitsToInt => F::CACHEABLE,
        }
    }

    /// Whether a pattern may try both operand orderings at this node.
    ///
    /// `FloatAdd` / `FloatMul` count under the IEEE-754
    /// commutativity-up-to-NaN convention the IR uses throughout. `Carry` and
    /// `Scarry` ask whether `l + r` overflows, and addition commutes.
    #[inline]
    pub fn is_commutative(&self) -> bool {
        match self {
            Self::IntBinaryOp(op) => matches!(
                op,
                IntBinaryOp::Add
                    | IntBinaryOp::Mul
                    | IntBinaryOp::And
                    | IntBinaryOp::Or
                    | IntBinaryOp::Xor
            ),
            Self::FloatBinaryOp(op) => matches!(op, FloatBinaryOp::Add | FloatBinaryOp::Mul),
            Self::IntCmpOp(op) => {
                matches!(op, IntCmpOp::Equal | IntCmpOp::Carry | IntCmpOp::Scarry)
            }
            Self::FloatCmpOp(op) => matches!(op, FloatCmpOp::Equal),
            _ => false,
        }
    }
}

// The largest inline payload is a u64 (FloatConst / CallOther).
const _: () = assert!(size_of::<NodeKind>() == 16);

#[cfg(test)]
mod tests {
    use super::{NodeKind, SwitchTableId};
    use crate::node::const_value::ConstId;
    use cranelift_entity::EntityRef;

    #[test]
    fn has_side_effects_is_control_flow_plus_memory_writes_and_opaque() {
        for k in [
            NodeKind::Entry,
            NodeKind::Region,
            NodeKind::If,
            NodeKind::Switch(SwitchTableId::new(0)),
            NodeKind::Return,
            NodeKind::Call { cc: None },
            NodeKind::CallOther { user_op_id: 0 },
            NodeKind::IndirectBranch,
        ] {
            assert!(k.has_control_flow(), "{k:?} should be control flow");
            assert!(k.has_side_effects(), "{k:?} should have side effects");
        }
        for k in [
            NodeKind::Store(rsleigh::VnSpace::RAM),
            NodeKind::CPoolRef,
            NodeKind::New,
        ] {
            assert!(!k.has_control_flow(), "{k:?} is not control flow");
            assert!(k.has_side_effects(), "{k:?} should have side effects");
        }
        // Killable when unused, memory reads included.
        for k in [
            NodeKind::IntConst(ConstId::new(0)),
            NodeKind::IntBinaryOp(crate::node::IntBinaryOp::Add),
            NodeKind::Load(rsleigh::VnSpace::RAM),
        ] {
            assert!(!k.has_control_flow(), "{k:?} is not control flow");
            assert!(!k.has_side_effects(), "{k:?} should NOT have side effects");
        }
    }
}
