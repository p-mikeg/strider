use crate::node::{FloatBinaryOp, FloatCmpOp, IntBinaryOp, IntCmpOp};

/// Not a `NodeKind` payload: arg tracking lives in
/// `Function::arg_index_to_values`. This only exists so pattern builders can
/// filter matches by ABI source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FunctionArgSource {
    /// Always the full-width container register (`RDI`, never `EDI`), so a
    /// sub-register read is expressible as `Truncate(InitialVar(rdi))`.
    Register(rsleigh::Vn),
    /// Byte `offset` from the entry-time stack pointer (`InitialVar(sp)`), in
    /// the stack's address `space` (usually RAM).
    Stack {
        space: rsleigh::VnSpace,
        offset: i128,
    },
}

/// Dense id of a tracked varnode, interned in `Function::vn_interner`. One
/// identity serves both as the [`NodeKind::InitialVar`] payload and as the
/// builder's per-region SSA-variable key.
///
/// Interned rather than inlining `rsleigh::Vn` so the largest `NodeKind`
/// payload stays 4 bytes instead of 16. Ids are assigned in `(space, offset,
/// size)` order, so assignment is deterministic across runs. Resolve via
/// [`crate::Function::initial_vn`].
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
    /// value. Resolve the varnode via [`crate::Function::initial_vn`].
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
    /// varnode tag in the `value_vn` side-table, keyed by the phi's output
    /// `ValueId` (see [`crate::Function::get_vn_for_value`]); anonymous phis
    /// have no entry. Non-cacheable: phi identity matters.
    Phi,

    /// Inputs: `(control, cond:I1)`. Outputs: `[Control, Control]`, index 0
    /// true, index 1 false.
    If,
    /// Resolved jump table. Inputs: `(control, address)`. Outputs: one
    /// `Control` per target, output `i` taken when `address ==
    /// switch_targets[i]` (case addresses live in `Function::switch_targets`).
    /// Exhaustive: no default arm.
    Switch,

    /// Clobbers caller-saved registers and the memory token.
    Call,
    /// Consumes the outgoing control edge plus any return values.
    Return,
    /// Placeholder for a branch the CFG could not resolve. Inputs:
    /// `[control, memory, target_value]`. Outputs: `[]`.
    ///
    /// The resolver classifies the producer of `target_value` once the
    /// optimiser has converged, then rewrites this into a `Return` (link
    /// register) or a `Call`+`Return` pair (tail call). Surviving the pipeline
    /// just means classification failed; the IR stays valid.
    ///
    /// `memory` is anchored here alongside `control` so the resolver can wire
    /// the replacement at the same program point without re-walking the CFG
    /// for the live memory token.
    IndirectBranch,
    /// Control sink for a no-return trap (`ud2`, `int3`, `abort`, `BUG_ON`).
    /// Inputs: `[control]`. Outputs: `[]`. Consumes the dangling `Control`
    /// edge a NoReturn `CallOther` leaves behind, so "every control edge
    /// reaches a terminator" holds and the validator can enforce the
    /// single-successor control invariant.
    Unreachable,

    Load(rsleigh::VnSpace),
    Store(rsleigh::VnSpace),

    /// Value is interned in `Function::const_interner`; read it via
    /// `IRViewer::int_const_u128`, never by matching the payload.
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

    /// Raw IEEE 754 bit pattern; upper 32 bits are zero for `F32`.
    FloatConst(u64),
    FloatBinaryOp(crate::node::FloatBinaryOp),
    FloatUnaryOp(crate::node::FloatUnaryOp),
    /// Outputs `I1`.
    FloatCmpOp(crate::node::FloatCmpOp),

    /// Integer to nearest representable float, like C's `(float)n`.
    IntToFloat,
    /// Float to integer, truncating toward zero, like C's `(int)f`.
    FloatToInt,
    /// Reprecision between float types.
    FloatToFloat,

    /// Same-size bitcast, `I32` to `F32` or `I64` to `F64`. Bits unchanged.
    IntBitsToFloat,
    /// Same-size bitcast, `F32` to `I32` or `F64` to `I64`. Bits unchanged.
    FloatBitsToInt,

    /// Sleigh `CallOther`: CPU intrinsics such as `cpuid`, `rdtsc`, `syscall`,
    /// x87 transcendentals.
    ///
    /// Inputs: `[control, memory, arg0, arg1, ...]`. Outputs:
    /// `[Control, Memory]`, plus a `Typed` slot when the instruction has an
    /// output varnode. Memory is always clobbered, so downstream loads must
    /// depend on the new token. Non-cacheable.
    CallOther {
        user_op_id: u64,
    },

    /// Sleigh `SegmentOp`: resolves a (segment, offset) pair to a flat
    /// pointer. Pure, so cacheable.
    ///
    /// Inputs: `[segment, offset]`. Outputs: `[Typed]` (pointer-sized).
    SegmentOp {
        op_id: u64,
    },

    /// Sleigh `CPoolRef`: Java constant-pool lookup.
    ///
    /// Inputs: `[ref0, ref1, ...]`. Outputs: `[Typed]`. Non-cacheable:
    /// resolution can have observable side effects (class loading).
    CPoolRef,

    /// Sleigh `New`: Java object allocation.
    ///
    /// Inputs: `[size, ...]`. Outputs: `[Typed]` (pointer-sized).
    /// Non-cacheable: each allocation yields a distinct object.
    New,
}

impl NodeKind {
    /// Constants only enter the graph via the const interners, so a
    /// `matches!` is exhaustive in practice and no new variant can become
    /// const without going through `IntConst` / `FloatConst`.
    #[inline]
    pub fn is_const(self) -> bool {
        matches!(self, Self::IntConst(..) | Self::FloatConst(..))
    }

    /// Whether the graph may dedup nodes of this kind.
    ///
    /// Not cacheable: kinds whose inputs grow after construction (`Region`,
    /// `Phi`) and kinds where each occurrence is a distinct event (`Return`,
    /// `Call`). Matched without a `_` arm so a new variant fails to compile
    /// until someone decides.
    #[inline]
    pub fn is_cacheable(&self) -> bool {
        match self {
            // Initial-state singletons: identity is fully determined by the
            // NodeKind payload, so dedup enforces one-per-function.
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

    /// Exempt from the non-empty asm-fingerprint invariant.
    ///
    /// Region headers, initial state and phis are synthesised without a
    /// contributing machine instruction, so an empty fingerprint is legal.
    /// Everything else must carry at least one entry. No `_` arm, so a new
    /// variant fails to compile until someone decides.
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

    /// Whether the node carries a `Control` input or output. No `_` arm, so a
    /// new variant fails to compile until someone decides.
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
    /// aliasing reasoning this does not do. `CPoolRef` / `New` count because
    /// resolution can observe state. `Load` and `MemPhi` do not, and are
    /// culled when unused. New non-control-flow variants with observable
    /// effects must be added to the `matches!` below.
    #[inline]
    pub fn has_side_effects(&self) -> bool {
        self.has_control_flow()
            || matches!(
                self,
                NodeKind::Store(_) | NodeKind::CPoolRef | NodeKind::New
            )
    }

    /// Single source of truth for whether a pattern may try both operand
    /// orderings at this node.
    ///
    /// `FloatAdd` / `FloatMul` count under the IEEE-754
    /// commutativity-up-to-NaN convention the IR uses throughout. `Carry` and
    /// `Scarry` ask whether `l + r` overflows, and addition commutes, so they
    /// do too. `FloatCmpOp::Equal` is symmetric including on NaN.
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

// The largest inline payload is a u64 (FloatConst / CallOther / SegmentOp);
// InitialVar interns its varnode rather than inlining a 16-byte rsleigh::Vn.
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
        // A memory write plus the opaque ops.
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
