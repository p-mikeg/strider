//! Expected input/output signatures of every [`NodeKind`] variant.
//!
//! This module is the single source of truth for every node's input and
//! output slot shape: slot kind (for validation), slot name (for dot
//! labels), and slot role (for dot colors and IR-aware rendering).
//!
//! Slots are described by [`Slot`] carrying [`ExpectedOutputKind`] —
//! a coarser classification than the concrete [`NodeOutputKind`] stored on
//! actual outputs.  Integer slots accept any width via
//! [`ExpectedOutputKind::AnyInt`], float slots accept F32 or F64 via
//! [`ExpectedOutputKind::AnyFloat`].  Bool remains distinct.
//!
//! Variadic arity is modelled by [`SlotList::tail`]: a `None` tail means
//! the slot list is fixed-arity (equal to `head.len()`), while `Some(tail)`
//! means any index past the head repeats `tail`.  Variadic kinds include
//! [`NodeKind::ControlState`], [`NodeKind::MemPhi`],
//! [`NodeKind::ControlPhi`], [`NodeKind::Call`], [`NodeKind::CallOther`],
//! [`NodeKind::Return`], [`NodeKind::CPoolRef`], and [`NodeKind::New`].

use crate::node::NodeKind;

/// The expected kind of an input or output slot of a [`NodeKind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpectedOutputKind {
    /// A `Control` token.
    Control,
    /// A `Memory` token.
    Memory,
    /// A `ControlPhi` dispatch token.
    ControlPhi,
    /// A `Bool` value.
    Bool,
    /// Any integer-typed value (U8, U16, U32, U64, U128, U256).
    AnyInt,
    /// Any float-typed value (F32, F64).
    AnyFloat,
    /// Any value-typed output: `Bool`, `AnyInt`, or `AnyFloat`.  Used by the
    /// type-polymorphic cast ops (`CastToInt`, `CastToBool`, `CastToFloat`).
    AnyValue,
}

/// Semantic role of a slot, independent of its kind.  Drives label colors
/// in dot rendering and could be used by future IR-aware consumers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotRole {
    Control,
    Memory,
    Phi,
    Lhs,
    Rhs,
    Val,
    Addr,
    Data,
    Sp,
    Target,
    Arg,
    Cond,
    Ref,
    Seg,
    Off,
    Ret,
    In,
}

/// A single input or output slot.
#[derive(Debug, Clone, Copy)]
pub struct Slot {
    pub kind: ExpectedOutputKind,
    pub name: &'static str,
    pub role: SlotRole,
}

/// A list of slots, possibly with a variadic repeating tail.
///
/// `head` describes the fixed-arity prefix.  If `tail` is `Some`, any index
/// `>= head.len()` repeats `tail`; otherwise the list is exactly
/// `head.len()` slots long.
#[derive(Debug, Clone, Copy)]
pub struct SlotList {
    pub head: &'static [Slot],
    pub tail: Option<Slot>,
}

impl SlotList {
    pub const fn fixed(head: &'static [Slot]) -> Self {
        Self { head, tail: None }
    }

    pub const fn variadic(head: &'static [Slot], tail: Slot) -> Self {
        Self {
            head,
            tail: Some(tail),
        }
    }

    pub fn is_variadic(&self) -> bool {
        self.tail.is_some()
    }

    /// Fixed-prefix length.  Callers validating a variadic tail must read
    /// indices `>= head_len()` via [`SlotList::at`] (or against
    /// [`SlotList::tail`] directly).
    pub fn head_len(&self) -> usize {
        self.head.len()
    }

    /// Slot at index `idx`.  For fixed-arity lists returns `None` past the
    /// head; for variadic lists returns the tail for any past-head index.
    pub fn at(&self, idx: usize) -> Option<Slot> {
        if let Some(s) = self.head.get(idx) {
            Some(*s)
        } else if idx >= self.head.len() {
            self.tail
        } else {
            None
        }
    }

}

/// Full input/output signature of a [`NodeKind`].
#[derive(Debug, Clone, Copy)]
pub struct Signature {
    pub inputs: SlotList,
    pub outputs: SlotList,
}

// ── Slot constants ────────────────────────────────────────────────────────────

use ExpectedOutputKind::*;
use SlotRole as R;

const CTRL: Slot = Slot {
    kind: Control,
    name: "ctrl",
    role: R::Control,
};
const MEM: Slot = Slot {
    kind: Memory,
    name: "mem",
    role: R::Memory,
};
const PHI: Slot = Slot {
    kind: ControlPhi,
    name: "phi",
    role: R::Phi,
};
const COND: Slot = Slot {
    kind: Bool,
    name: "cond",
    role: R::Cond,
};
const LHS: Slot = Slot {
    kind: AnyInt,
    name: "lhs",
    role: R::Lhs,
};
const RHS: Slot = Slot {
    kind: AnyInt,
    name: "rhs",
    role: R::Rhs,
};
const FLHS: Slot = Slot {
    kind: AnyFloat,
    name: "lhs",
    role: R::Lhs,
};
const FRHS: Slot = Slot {
    kind: AnyFloat,
    name: "rhs",
    role: R::Rhs,
};
const BLHS: Slot = Slot {
    kind: Bool,
    name: "lhs",
    role: R::Lhs,
};
const BRHS: Slot = Slot {
    kind: Bool,
    name: "rhs",
    role: R::Rhs,
};
const INT_VAL: Slot = Slot {
    kind: AnyInt,
    name: "val",
    role: R::Val,
};
const FLOAT_VAL: Slot = Slot {
    kind: AnyFloat,
    name: "val",
    role: R::Val,
};
const BOOL_VAL: Slot = Slot {
    kind: Bool,
    name: "val",
    role: R::Val,
};
const ANY_VAL: Slot = Slot {
    kind: AnyValue,
    name: "val",
    role: R::Val,
};
const ADDR: Slot = Slot {
    kind: AnyInt,
    name: "addr",
    role: R::Addr,
};
const DATA: Slot = Slot {
    kind: AnyInt,
    name: "data",
    role: R::Data,
};
const SP: Slot = Slot {
    kind: AnyInt,
    name: "sp",
    role: R::Sp,
};
const TARGET: Slot = Slot {
    kind: AnyInt,
    name: "target",
    role: R::Target,
};
const ARG: Slot = Slot {
    kind: AnyInt,
    name: "arg",
    role: R::Arg,
};
const RET: Slot = Slot {
    kind: AnyInt,
    name: "ret",
    role: R::Ret,
};
const SEG: Slot = Slot {
    kind: AnyInt,
    name: "seg",
    role: R::Seg,
};
const OFF: Slot = Slot {
    kind: AnyInt,
    name: "off",
    role: R::Off,
};
const REF: Slot = Slot {
    kind: AnyInt,
    name: "ref",
    role: R::Ref,
};
const IN_PHI: Slot = Slot {
    kind: AnyInt,
    name: "in",
    role: R::In,
};

// ── Signatures ────────────────────────────────────────────────────────────────

/// Expected input/output [`Signature`] for a given [`NodeKind`].
///
/// For variable-arity kinds, the returned [`SlotList`] carries both the
/// fixed prefix in `head` and the repeating slot in `tail`.  See the
/// module-level docs.
pub(crate) fn expected_signature(kind: &NodeKind) -> Signature {
    macro_rules! sig {
        (inputs: [$($i:expr),* $(,)?], outputs: [$($o:expr),* $(,)?] $(,)?) => {
            Signature {
                inputs: SlotList::fixed(&[$($i),*]),
                outputs: SlotList::fixed(&[$($o),*]),
            }
        };
        (inputs: [$($i:expr),* $(,)?], outputs: [$($o:expr),* $(,)?]; out_tail: $ot:expr $(,)?) => {
            Signature {
                inputs: SlotList::fixed(&[$($i),*]),
                outputs: SlotList::variadic(&[$($o),*], $ot),
            }
        };
        (inputs: [$($i:expr),* $(,)?]; in_tail: $it:expr, outputs: [$($o:expr),* $(,)?] $(,)?) => {
            Signature {
                inputs: SlotList::variadic(&[$($i),*], $it),
                outputs: SlotList::fixed(&[$($o),*]),
            }
        };
        (inputs: [$($i:expr),* $(,)?]; in_tail: $it:expr, outputs: [$($o:expr),* $(,)?]; out_tail: $ot:expr $(,)?) => {
            Signature {
                inputs: SlotList::variadic(&[$($i),*], $it),
                outputs: SlotList::variadic(&[$($o),*], $ot),
            }
        };
    }

    match kind {
        // ── Initial state ───────────────────────────────────────────────────
        NodeKind::Entry => sig!(inputs: [], outputs: [CTRL]),
        NodeKind::InitialMemory => sig!(inputs: [], outputs: [MEM]),
        NodeKind::InitialVar(_) => sig!(inputs: [], outputs: [INT_VAL]),
        NodeKind::FunctionArg { .. } => sig!(inputs: [], outputs: [INT_VAL]),

        // ── Region / join nodes (variadic inputs) ───────────────────────────
        // ControlState: one Control input per predecessor (variadic).
        NodeKind::ControlState => sig!(inputs: []; in_tail: CTRL, outputs: [CTRL, PHI]),
        // MemPhi: [phi_token, ...per-predecessor Memory tokens].
        NodeKind::MemPhi => sig!(inputs: [PHI]; in_tail: MEM, outputs: [MEM]),
        // ControlPhi: [phi_token, ...per-predecessor values].
        NodeKind::ControlPhi(_) => sig!(inputs: [PHI]; in_tail: IN_PHI, outputs: [INT_VAL]),
        // ValuePhi: [phi_token, ...per-predecessor values].  Same shape as
        // ControlPhi but not tied to a source varnode.
        NodeKind::ValuePhi => sig!(inputs: [PHI]; in_tail: IN_PHI, outputs: [INT_VAL]),

        // ── Conditional branch ──────────────────────────────────────────────
        NodeKind::If => sig!(inputs: [CTRL, COND], outputs: [CTRL, CTRL]),

        // ── Calls and returns ───────────────────────────────────────────────
        // Call: [control, memory, call_address, ...args].
        // Outputs: [Control, Memory, ...clobbered varnode values].
        NodeKind::Call => sig!(
            inputs: [CTRL, MEM, TARGET]; in_tail: ARG,
            outputs: [CTRL, MEM]; out_tail: INT_VAL,
        ),
        NodeKind::PostCallMemState => sig!(inputs: [CTRL], outputs: [MEM]),
        NodeKind::PostCallVarState(_) => sig!(inputs: [CTRL], outputs: [INT_VAL]),
        // Return: [control, memory, ...return values]. Return values are the
        // calling convention's ret_val_regs when built by the analyzer; synthetic
        // test builds may supply a single explicit value via `build_return`.
        NodeKind::Return => sig!(inputs: [CTRL, MEM]; in_tail: RET, outputs: []),

        // ── Memory operations ───────────────────────────────────────────────
        NodeKind::Load(_) => sig!(inputs: [MEM, ADDR], outputs: [INT_VAL]),
        NodeKind::Store(_) => sig!(inputs: [MEM, ADDR, DATA], outputs: [MEM]),
        // StackStore: [memory, base, data].
        NodeKind::StackStore { .. } => sig!(inputs: [MEM, SP, DATA], outputs: [MEM]),
        // StackStorePhi: [phi_token, memory, data].
        NodeKind::StackStorePhi { .. } => sig!(inputs: [PHI, MEM, DATA], outputs: [MEM]),

        // ── Integer constants and operations ────────────────────────────────
        NodeKind::IntConst(_) => sig!(inputs: [], outputs: [INT_VAL]),
        NodeKind::IntUnaryOp(_) => sig!(inputs: [INT_VAL], outputs: [INT_VAL]),
        NodeKind::IntBinaryOp(_) => sig!(inputs: [LHS, RHS], outputs: [INT_VAL]),
        NodeKind::IntCmpOp(_) => sig!(inputs: [LHS, RHS], outputs: [BOOL_VAL]),
        NodeKind::CastToInt => sig!(inputs: [ANY_VAL], outputs: [INT_VAL]),
        NodeKind::Truncate => sig!(inputs: [INT_VAL], outputs: [INT_VAL]),
        NodeKind::Popcount => sig!(inputs: [INT_VAL], outputs: [INT_VAL]),
        NodeKind::Lzcount => sig!(inputs: [INT_VAL], outputs: [INT_VAL]),
        NodeKind::Extend(_) => sig!(inputs: [INT_VAL], outputs: [INT_VAL]),

        // ── Boolean constants and operations ────────────────────────────────
        NodeKind::BoolConst(_) => sig!(inputs: [], outputs: [BOOL_VAL]),
        NodeKind::BoolUnaryOp(_) => sig!(inputs: [BOOL_VAL], outputs: [BOOL_VAL]),
        NodeKind::BoolBinaryOp(_) => sig!(inputs: [BLHS, BRHS], outputs: [BOOL_VAL]),
        NodeKind::CastToBool => sig!(inputs: [ANY_VAL], outputs: [BOOL_VAL]),

        // ── Float constants and operations ──────────────────────────────────
        NodeKind::FloatConst(_) => sig!(inputs: [], outputs: [FLOAT_VAL]),
        NodeKind::FloatBinaryOp(_) => sig!(inputs: [FLHS, FRHS], outputs: [FLOAT_VAL]),
        NodeKind::FloatUnaryOp(_) => sig!(inputs: [FLOAT_VAL], outputs: [FLOAT_VAL]),
        NodeKind::FloatCmpOp(_) => sig!(inputs: [FLHS, FRHS], outputs: [BOOL_VAL]),
        NodeKind::IntToFloat => sig!(inputs: [INT_VAL], outputs: [FLOAT_VAL]),
        NodeKind::FloatToInt => sig!(inputs: [FLOAT_VAL], outputs: [INT_VAL]),
        NodeKind::FloatToFloat => sig!(inputs: [FLOAT_VAL], outputs: [FLOAT_VAL]),
        NodeKind::IntBitsToFloat => sig!(inputs: [INT_VAL], outputs: [FLOAT_VAL]),
        NodeKind::FloatBitsToInt => sig!(inputs: [FLOAT_VAL], outputs: [INT_VAL]),
        NodeKind::CastToFloat => sig!(inputs: [ANY_VAL], outputs: [FLOAT_VAL]),

        // ── User-defined / opaque opcodes ───────────────────────────────────
        // CallOther: [control, memory, ...args].
        // Outputs: [Control, Memory] or [Control, Memory, OutputType].
        NodeKind::CallOther { .. } => sig!(
            inputs: [CTRL, MEM]; in_tail: ARG,
            outputs: [CTRL, MEM]; out_tail: ANY_VAL,
        ),
        NodeKind::SegmentOp { .. } => sig!(inputs: [SEG, OFF], outputs: [INT_VAL]),
        // CPoolRef: [...refs] (variadic).
        NodeKind::CPoolRef => sig!(inputs: []; in_tail: REF, outputs: [INT_VAL]),
        // New: [...args] (variadic, typically a size).
        NodeKind::New => sig!(inputs: []; in_tail: ARG, outputs: [INT_VAL]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::NodeKind;

    /// Convenience: projects the head slot kinds of a signature into the
    /// `(Vec<Kind>, Vec<Kind>)` shape used by the pre-refactor assertions.
    fn kinds(
        kind: &NodeKind,
    ) -> (Vec<ExpectedOutputKind>, Vec<ExpectedOutputKind>) {
        let sig = expected_signature(kind);
        (
            sig.inputs.head.iter().map(|s| s.kind).collect(),
            sig.outputs.head.iter().map(|s| s.kind).collect(),
        )
    }

    #[test]
    fn expected_signature_int_const() {
        let (inputs, outputs) = kinds(&NodeKind::IntConst(42));
        assert_eq!(inputs, vec![]);
        assert_eq!(outputs, vec![ExpectedOutputKind::AnyInt]);
    }

    #[test]
    fn expected_signature_entry() {
        let (inputs, outputs) = kinds(&NodeKind::Entry);
        assert_eq!(inputs, vec![]);
        assert_eq!(outputs, vec![ExpectedOutputKind::Control]);
    }

    #[test]
    fn expected_signature_if() {
        let (inputs, outputs) = kinds(&NodeKind::If);
        assert_eq!(
            inputs,
            vec![ExpectedOutputKind::Control, ExpectedOutputKind::Bool]
        );
        assert_eq!(
            outputs,
            vec![ExpectedOutputKind::Control, ExpectedOutputKind::Control]
        );
    }

    #[test]
    fn expected_signature_initial_memory() {
        let (inputs, outputs) = kinds(&NodeKind::InitialMemory);
        assert_eq!(inputs, vec![]);
        assert_eq!(outputs, vec![ExpectedOutputKind::Memory]);
    }

    #[test]
    fn expected_signature_load() {
        let space = rsleigh::VnSpace::RAM;
        let (inputs, outputs) = kinds(&NodeKind::Load(space));
        assert_eq!(
            inputs,
            vec![ExpectedOutputKind::Memory, ExpectedOutputKind::AnyInt]
        );
        assert_eq!(outputs, vec![ExpectedOutputKind::AnyInt]);
    }

    #[test]
    fn expected_signature_store() {
        let space = rsleigh::VnSpace::RAM;
        let (inputs, outputs) = kinds(&NodeKind::Store(space));
        assert_eq!(
            inputs,
            vec![
                ExpectedOutputKind::Memory,
                ExpectedOutputKind::AnyInt,
                ExpectedOutputKind::AnyInt,
            ]
        );
        assert_eq!(outputs, vec![ExpectedOutputKind::Memory]);
    }

    #[test]
    fn expected_signature_stack_store() {
        let space = rsleigh::VnSpace::RAM;
        let (inputs, outputs) = kinds(&NodeKind::StackStore { space, offset: -4 });
        assert_eq!(
            inputs,
            vec![
                ExpectedOutputKind::Memory,
                ExpectedOutputKind::AnyInt,
                ExpectedOutputKind::AnyInt,
            ]
        );
        assert_eq!(outputs, vec![ExpectedOutputKind::Memory]);
    }

    #[test]
    fn expected_signature_stack_store_phi() {
        let space = rsleigh::VnSpace::RAM;
        let (inputs, outputs) = kinds(&NodeKind::StackStorePhi { space });
        assert_eq!(
            inputs,
            vec![
                ExpectedOutputKind::ControlPhi,
                ExpectedOutputKind::Memory,
                ExpectedOutputKind::AnyInt,
            ]
        );
        assert_eq!(outputs, vec![ExpectedOutputKind::Memory]);
    }

    #[test]
    fn expected_signature_return() {
        let (inputs, outputs) = kinds(&NodeKind::Return);
        assert_eq!(
            inputs,
            vec![ExpectedOutputKind::Control, ExpectedOutputKind::Memory]
        );
        assert_eq!(outputs, vec![]);
    }

    #[test]
    fn expected_signature_call() {
        let (inputs, outputs) = kinds(&NodeKind::Call);
        assert_eq!(
            inputs,
            vec![
                ExpectedOutputKind::Control,
                ExpectedOutputKind::Memory,
                ExpectedOutputKind::AnyInt,
            ]
        );
        assert_eq!(
            outputs,
            vec![ExpectedOutputKind::Control, ExpectedOutputKind::Memory]
        );
    }

    #[test]
    fn expected_signature_int_binary_op() {
        use crate::ops::IntBinaryOp;
        let (inputs, outputs) = kinds(&NodeKind::IntBinaryOp(IntBinaryOp::Add));
        assert_eq!(
            inputs,
            vec![ExpectedOutputKind::AnyInt, ExpectedOutputKind::AnyInt]
        );
        assert_eq!(outputs, vec![ExpectedOutputKind::AnyInt]);
    }

    #[test]
    fn expected_signature_int_cmp_op() {
        use crate::ops::IntCmpOp;
        let (inputs, outputs) = kinds(&NodeKind::IntCmpOp(IntCmpOp::Equal));
        assert_eq!(
            inputs,
            vec![ExpectedOutputKind::AnyInt, ExpectedOutputKind::AnyInt]
        );
        assert_eq!(outputs, vec![ExpectedOutputKind::Bool]);
    }

    #[test]
    fn expected_signature_bool_const() {
        let (inputs, outputs) = kinds(&NodeKind::BoolConst(true));
        assert_eq!(inputs, vec![]);
        assert_eq!(outputs, vec![ExpectedOutputKind::Bool]);
    }

    #[test]
    fn expected_signature_cast_to_bool() {
        let (inputs, outputs) = kinds(&NodeKind::CastToBool);
        assert_eq!(inputs, vec![ExpectedOutputKind::AnyValue]);
        assert_eq!(outputs, vec![ExpectedOutputKind::Bool]);
    }

    #[test]
    fn expected_signature_control_state() {
        let (inputs, outputs) = kinds(&NodeKind::ControlState);
        assert_eq!(inputs, vec![]);
        assert_eq!(
            outputs,
            vec![ExpectedOutputKind::Control, ExpectedOutputKind::ControlPhi]
        );
    }

    #[test]
    fn expected_signature_mem_phi() {
        let (inputs, outputs) = kinds(&NodeKind::MemPhi);
        assert_eq!(inputs, vec![ExpectedOutputKind::ControlPhi]);
        assert_eq!(outputs, vec![ExpectedOutputKind::Memory]);
    }

    #[test]
    fn expected_signature_float_const() {
        let (inputs, outputs) = kinds(&NodeKind::FloatConst(0));
        assert_eq!(inputs, vec![]);
        assert_eq!(outputs, vec![ExpectedOutputKind::AnyFloat]);
    }

    #[test]
    fn expected_signature_float_binary_op() {
        use crate::ops::FloatBinaryOp;
        let (inputs, outputs) = kinds(&NodeKind::FloatBinaryOp(FloatBinaryOp::Add));
        assert_eq!(
            inputs,
            vec![ExpectedOutputKind::AnyFloat, ExpectedOutputKind::AnyFloat]
        );
        assert_eq!(outputs, vec![ExpectedOutputKind::AnyFloat]);
    }

    // ── Slot-level metadata tests ────────────────────────────────────────────

    #[test]
    fn load_input_slots_have_mem_and_addr_roles() {
        let space = rsleigh::VnSpace::RAM;
        let sig = expected_signature(&NodeKind::Load(space));
        assert_eq!(sig.inputs.at(0).unwrap().role, SlotRole::Memory);
        assert_eq!(sig.inputs.at(0).unwrap().name, "mem");
        assert_eq!(sig.inputs.at(1).unwrap().role, SlotRole::Addr);
        assert_eq!(sig.inputs.at(1).unwrap().name, "addr");
    }

    #[test]
    fn call_is_variadic_in_args() {
        let sig = expected_signature(&NodeKind::Call);
        assert!(sig.inputs.is_variadic());
        // Head: ctrl, mem, target.
        assert_eq!(sig.inputs.head_len(), 3);
        assert_eq!(sig.inputs.at(0).unwrap().name, "ctrl");
        assert_eq!(sig.inputs.at(1).unwrap().name, "mem");
        assert_eq!(sig.inputs.at(2).unwrap().name, "target");
        // Tail: arg.
        assert_eq!(sig.inputs.at(3).unwrap().role, SlotRole::Arg);
        assert_eq!(sig.inputs.at(999).unwrap().role, SlotRole::Arg);
    }

    #[test]
    fn return_input_tail_is_ret() {
        let sig = expected_signature(&NodeKind::Return);
        assert_eq!(sig.inputs.head_len(), 2);
        assert_eq!(sig.inputs.at(0).unwrap().name, "ctrl");
        assert_eq!(sig.inputs.at(1).unwrap().name, "mem");
        assert_eq!(sig.inputs.at(2).unwrap().role, SlotRole::Ret);
        assert_eq!(sig.inputs.at(99).unwrap().role, SlotRole::Ret);
    }

    #[test]
    fn if_cond_slot_role_is_cond() {
        let sig = expected_signature(&NodeKind::If);
        assert_eq!(sig.inputs.at(1).unwrap().role, SlotRole::Cond);
        assert_eq!(sig.inputs.at(1).unwrap().name, "cond");
    }

    #[test]
    fn int_binary_op_slot_roles_are_lhs_rhs() {
        use crate::ops::IntBinaryOp;
        let sig = expected_signature(&NodeKind::IntBinaryOp(IntBinaryOp::Add));
        assert_eq!(sig.inputs.at(0).unwrap().role, SlotRole::Lhs);
        assert_eq!(sig.inputs.at(1).unwrap().role, SlotRole::Rhs);
    }
}
