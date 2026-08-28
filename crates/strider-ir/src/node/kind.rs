use crate::node::{FloatBinaryOp, FloatCmpOp, IntBinaryOp, IntCmpOp};
use crate::node_signature::ExpectedValueKind;

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
    /// `Control` per target, output `i` taken when `address ==
    /// switch_targets[i]` (case addresses live in the `switch_targets` side-table).
    /// No default arm: a target the resolver could not prove is reported in
    /// `unresolved_indirect_branches`, not modelled as an arm.
    Switch,

    /// Clobbers caller-saved registers and the memory token.
    Call,
    /// Consumes the outgoing control and memory edges plus any return values.
    Return,
    /// Placeholder for a branch the CFG could not resolve. Inputs:
    /// `[control, memory, target_value]`, plus the optional interworking
    /// ISA-mode bit at slot 3. Outputs: `[]`. Surviving the pipeline means
    /// classification failed; the IR stays valid.
    IndirectBranch,
    /// Control sink for a no-return trap (`ud2`, `int3`, `abort`, `BUG_ON`).
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
    /// x87 transcendentals.
    ///
    /// Inputs: `[control, memory, arg0, arg1, ...]`. Outputs:
    /// `[Control, Memory]`, then a `Typed` slot for the result when the
    /// instruction has an output varnode, then one per implicit-write clobber.
    /// The `Memory` output is always present; only a memory-clobbering op
    /// advances the region's chain through it.
    CallOther {
        user_op_id: u64,
    },

    /// Sleigh `SegmentOp`: resolves a (segment, offset) pair to a flat
    /// pointer. Pure.
    ///
    /// Inputs: `[segment, offset]`. Outputs: `[Typed]` (pointer-sized).
    SegmentOp {
        op_id: u64,
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

impl NodeKind {
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

    /// Fixed output slots before the variadic tail. See
    /// [`input_head_len`](Self::input_head_len).
    #[must_use]
    pub fn output_head_len(&self) -> usize {
        crate::node_signature::expected_signature(self)
            .outputs
            .head_len()
    }

    /// The value kind input slot `idx` admits, `None` past the arity bound.
    #[must_use]
    pub fn expected_input_kind(&self, idx: usize) -> Option<ExpectedValueKind> {
        crate::node_signature::expected_signature(self)
            .inputs
            .at(idx)
            .map(|s| s.kind)
    }

    /// The value kind output slot `idx` admits, `None` past the arity bound.
    #[must_use]
    pub fn expected_output_kind(&self, idx: usize) -> Option<ExpectedValueKind> {
        crate::node_signature::expected_signature(self)
            .outputs
            .at(idx)
            .map(|s| s.kind)
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
        match self {
            // Identity is fully determined by the payload plus the inputs.
            Self::Entry
            | Self::InitialMemory
            | Self::InitialVar(..)
            | Self::If
            | Self::Load(..)
            | Self::Store(..)
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
            | Self::FloatBitsToInt
            | Self::SegmentOp { .. } => true,

            // Inputs grow after construction.
            Self::Region
            | Self::Phi
            | Self::MemPhi
            // Each occurrence is a distinct event.
            | Self::Return
            | Self::IndirectBranch
            | Self::Unreachable
            | Self::Switch
            | Self::Call
            | Self::CallOther { .. }
            // Opaque per-occurrence identity.
            | Self::CPoolRef
            | Self::New => false,
        }
    }

    /// Exempt from the non-empty asm-fingerprint invariant: region headers,
    /// initial state and phis are synthesised without a contributing machine
    /// instruction.
    #[inline]
    pub fn asm_fingerprint_exempt(&self) -> bool {
        match self {
            Self::Entry
            | Self::InitialMemory
            | Self::InitialVar(..)
            | Self::Region
            | Self::Phi
            | Self::MemPhi => true,

            Self::If
            | Self::Switch
            | Self::Load(..)
            | Self::Store(..)
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
            | Self::FloatBitsToInt
            | Self::SegmentOp { .. }
            | Self::Return
            | Self::IndirectBranch
            | Self::Unreachable
            | Self::Call
            | Self::CallOther { .. }
            | Self::CPoolRef
            | Self::New => false,
        }
    }

    /// Whether the node carries a `Control` input or output.
    #[inline]
    pub fn has_control_flow(&self) -> bool {
        match self {
            Self::Entry
            | Self::Region
            | Self::If
            | Self::Switch
            | Self::Return
            | Self::Call
            | Self::CallOther { .. }
            | Self::IndirectBranch
            | Self::Unreachable => true,

            Self::InitialMemory
            | Self::InitialVar(..)
            | Self::MemPhi
            | Self::Phi
            | Self::Load(..)
            | Self::Store(..)
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
            | Self::FloatBitsToInt
            | Self::SegmentOp { .. }
            | Self::CPoolRef
            | Self::New => false,
        }
    }

    /// Whether the node must survive even with all outputs unused.
    ///
    /// `Store` counts: removing it is dead-store elimination, which needs
    /// aliasing reasoning this does not do. `Load` and `MemPhi` do not.
    #[inline]
    pub fn has_side_effects(&self) -> bool {
        self.has_control_flow()
            || matches!(
                self,
                NodeKind::Store(_) | NodeKind::CPoolRef | NodeKind::New
            )
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

// The largest inline payload is a u64 (FloatConst / CallOther / SegmentOp).
const _: () = assert!(
    std::mem::size_of::<NodeKind>() <= 16,
    "NodeKind must stay <= 16 bytes (no inline payload may exceed 8 bytes)"
);

#[cfg(test)]
mod tests {
    use super::NodeKind;
    use crate::node::const_value::ConstId;
    use cranelift_entity::EntityRef;

    #[test]
    fn has_side_effects_is_control_flow_plus_memory_writes_and_opaque() {
        for k in [
            NodeKind::Entry,
            NodeKind::Region,
            NodeKind::If,
            NodeKind::Switch,
            NodeKind::Return,
            NodeKind::Call,
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
