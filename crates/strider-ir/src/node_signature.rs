//! Single source of truth for every node's slot shape: kind (validation),
//! name (dot labels), and role (dot colors).

use crate::node::NodeKind;

/// A slot's admissible value kinds, coarser than the concrete
/// [`ValueKind`](crate::node::ValueKind) stored on real outputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpectedValueKind {
    Control,
    Memory,
    PhiToken,
    /// Exactly `I1`.
    Bool,
    AnyInt,
    AnyFloat,
    /// `AnyInt` or `AnyFloat`.
    AnyValue,
}

/// Drives label colors in dot rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SlotRole {
    Control,
    Memory,
    Phi,
    Lhs,
    Rhs,
    Val,
    Addr,
    Data,
    Target,
    Sp,
    Arg,
    Cond,
    Ref,
    Seg,
    Off,
    Ret,
    In,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Slot {
    pub(crate) kind: ExpectedValueKind,
    pub(crate) name: &'static str,
    pub(crate) role: SlotRole,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SlotList {
    head: &'static [Slot],
    tail: Option<Slot>,
    /// Inclusive arity bound. `usize::MAX` for a repeating tail.
    max_len: usize,
}

impl SlotList {
    pub(crate) const fn fixed(head: &'static [Slot]) -> Self {
        Self {
            head,
            tail: None,
            max_len: head.len(),
        }
    }

    /// `tail` repeats without bound.
    pub(crate) const fn variadic(head: &'static [Slot], tail: Slot) -> Self {
        Self {
            head,
            tail: Some(tail),
            max_len: usize::MAX,
        }
    }

    /// `tail` occupies at most one slot past the head.
    pub(crate) const fn optional(head: &'static [Slot], tail: Slot) -> Self {
        Self {
            head,
            tail: Some(tail),
            max_len: head.len() + 1,
        }
    }

    /// `None` when `len` is admissible, else the bound it violates.
    pub(crate) fn arity_violation(&self, len: usize) -> Option<usize> {
        if len < self.head.len() {
            Some(self.head.len())
        } else if len > self.max_len {
            Some(self.max_len)
        } else {
            None
        }
    }

    /// Past the head: the tail slot while within `max_len`, else `None`.
    pub(crate) fn at(&self, idx: usize) -> Option<Slot> {
        if idx >= self.max_len {
            return None;
        }
        self.head.get(idx).copied().or(self.tail)
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Signature {
    pub(crate) inputs: SlotList,
    pub(crate) outputs: SlotList,
}

use ExpectedValueKind::*;
use SlotRole as R;

const fn slot(kind: ExpectedValueKind, name: &'static str, role: SlotRole) -> Slot {
    Slot { kind, name, role }
}

const CTRL: Slot = slot(Control, "ctrl", R::Control);
const MEM: Slot = slot(Memory, "mem", R::Memory);
const PHI: Slot = slot(PhiToken, "phi", R::Phi);
const COND: Slot = slot(Bool, "cond", R::Cond);
const LHS: Slot = slot(AnyInt, "lhs", R::Lhs);
const RHS: Slot = slot(AnyInt, "rhs", R::Rhs);
const FLHS: Slot = slot(AnyFloat, "lhs", R::Lhs);
const FRHS: Slot = slot(AnyFloat, "rhs", R::Rhs);
const INT_VAL: Slot = slot(AnyInt, "val", R::Val);
const FLOAT_VAL: Slot = slot(AnyFloat, "val", R::Val);
const BOOL_VAL: Slot = slot(Bool, "val", R::Val);
const ANY_VAL: Slot = slot(AnyValue, "val", R::Val);
const ADDR: Slot = slot(AnyInt, "addr", R::Addr);
const DATA: Slot = slot(AnyInt, "data", R::Data);
const TARGET: Slot = slot(AnyInt, "target", R::Target);
// Optional trailing `IndirectBranch` input: the ISA-mode bit the branch
// instruction commits, taken from the write to the `ISAModeSwitch` register
// (ARM `SetThumbMode`, MIPS `JXWritePC`).
const ISA_MODE: Slot = slot(AnyInt, "isa", R::Cond);
const SP: Slot = slot(AnyInt, "sp", R::Sp);
// Argument and return registers hold floats as well as integers, as do the
// clobbered registers a `Call` outputs (`ANY_VAL`).
const ARG: Slot = slot(AnyValue, "arg", R::Arg);
const RET: Slot = slot(AnyValue, "ret", R::Ret);
const SEG: Slot = slot(AnyInt, "seg", R::Seg);
const OFF: Slot = slot(AnyInt, "off", R::Off);
const REF: Slot = slot(AnyInt, "ref", R::Ref);
// Per-predecessor Phi input.
const IN_PHI: Slot = slot(AnyValue, "in", R::In);

pub(crate) fn expected_signature(kind: &NodeKind) -> Signature {
    macro_rules! sig {
        (inputs: [$($i:expr),* $(,)?], outputs: [$($o:expr),* $(,)?] $(,)?) => {
            Signature {
                inputs: SlotList::fixed(&[$($i),*]),
                outputs: SlotList::fixed(&[$($o),*]),
            }
        };
        (inputs: [$($i:expr),* $(,)?]; in_tail: $it:expr, outputs: [$($o:expr),* $(,)?] $(,)?) => {
            Signature {
                inputs: SlotList::variadic(&[$($i),*], $it),
                outputs: SlotList::fixed(&[$($o),*]),
            }
        };
        (inputs: [$($i:expr),* $(,)?]; in_opt: $it:expr, outputs: [$($o:expr),* $(,)?] $(,)?) => {
            Signature {
                inputs: SlotList::optional(&[$($i),*], $it),
                outputs: SlotList::fixed(&[$($o),*]),
            }
        };
        (inputs: [$($i:expr),* $(,)?]; in_tail: $it:expr, outputs: [$($o:expr),* $(,)?]; out_tail: $ot:expr $(,)?) => {
            Signature {
                inputs: SlotList::variadic(&[$($i),*], $it),
                outputs: SlotList::variadic(&[$($o),*], $ot),
            }
        };
        (inputs: [$($i:expr),* $(,)?], outputs: [$($o:expr),* $(,)?]; out_tail: $ot:expr $(,)?) => {
            Signature {
                inputs: SlotList::fixed(&[$($i),*]),
                outputs: SlotList::variadic(&[$($o),*], $ot),
            }
        };
    }

    match kind {
        NodeKind::Entry => sig!(inputs: [], outputs: [CTRL]),
        NodeKind::InitialMemory => sig!(inputs: [], outputs: [MEM]),
        NodeKind::InitialVar(_) | NodeKind::IntConst(_) => sig!(inputs: [], outputs: [INT_VAL]),

        // One Control input per predecessor.
        NodeKind::Region => sig!(inputs: []; in_tail: CTRL, outputs: [CTRL, PHI]),
        NodeKind::MemPhi => sig!(inputs: [PHI]; in_tail: MEM, outputs: [MEM]),
        // Tagged and anonymous phis share this shape.
        NodeKind::Phi => sig!(inputs: [PHI]; in_tail: IN_PHI, outputs: [ANY_VAL]),

        NodeKind::If => sig!(inputs: [CTRL, COND], outputs: [CTRL, CTRL]),
        // One Control output per target region, in target order. No default
        // arm, and the arms need not cover every dispatch value: a site the
        // resolver seated early can hold one arm and widen later.
        NodeKind::Switch => sig!(inputs: [CTRL, INT_VAL], outputs: [CTRL]; out_tail: CTRL),

        // SP is an input-only anchor; the outputs are the clobbered varnodes.
        NodeKind::Call => sig!(
            inputs: [CTRL, MEM, TARGET, SP]; in_tail: ARG,
            outputs: [CTRL, MEM]; out_tail: ANY_VAL,
        ),
        NodeKind::Return => sig!(inputs: [CTRL, MEM]; in_tail: RET, outputs: []),
        // Placeholder for an unresolved branch. The optional trailing input is
        // the interworking ISA-mode bit (absent for a non-switching branch); it
        // is a real input so the optimizer keeps its cone live for the resolver.
        NodeKind::IndirectBranch => {
            sig!(inputs: [CTRL, MEM, TARGET]; in_opt: ISA_MODE, outputs: [])
        }
        // Control sink for a no-return trap. The optional trailing input is the
        // memory chain: control alone anchors no liveness, so an exit-free
        // control cycle would otherwise lose its stores to compaction.
        NodeKind::Unreachable => sig!(inputs: [CTRL]; in_opt: MEM, outputs: []),

        NodeKind::Load(_) => sig!(inputs: [MEM, ADDR], outputs: [INT_VAL]),
        NodeKind::Store(_) => sig!(inputs: [MEM, ADDR, DATA], outputs: [MEM]),

        // `IntConst` shares the input-less arm above.
        NodeKind::IntUnaryOp(_)
        | NodeKind::Truncate
        | NodeKind::Popcount
        | NodeKind::Lzcount
        | NodeKind::Extend(_) => sig!(inputs: [INT_VAL], outputs: [INT_VAL]),
        NodeKind::IntBinaryOp(_) => sig!(inputs: [LHS, RHS], outputs: [INT_VAL]),
        NodeKind::IntCmpOp(_) => sig!(inputs: [LHS, RHS], outputs: [BOOL_VAL]),

        NodeKind::FloatConst(_) => sig!(inputs: [], outputs: [FLOAT_VAL]),
        NodeKind::FloatBinaryOp(_) => sig!(inputs: [FLHS, FRHS], outputs: [FLOAT_VAL]),
        NodeKind::FloatUnaryOp(_) | NodeKind::FloatToFloat => {
            sig!(inputs: [FLOAT_VAL], outputs: [FLOAT_VAL])
        }
        NodeKind::FloatCmpOp(_) => sig!(inputs: [FLHS, FRHS], outputs: [BOOL_VAL]),
        NodeKind::IntToFloat | NodeKind::IntBitsToFloat => {
            sig!(inputs: [INT_VAL], outputs: [FLOAT_VAL])
        }
        NodeKind::FloatToInt | NodeKind::FloatBitsToInt => {
            sig!(inputs: [FLOAT_VAL], outputs: [INT_VAL])
        }

        NodeKind::CallOther { .. } => sig!(
            inputs: [CTRL, MEM]; in_tail: ARG,
            outputs: [CTRL, MEM]; out_tail: ANY_VAL,
        ),
        NodeKind::SegmentOp { .. } => sig!(inputs: [SEG, OFF], outputs: [INT_VAL]),
        NodeKind::CPoolRef => sig!(inputs: []; in_tail: REF, outputs: [INT_VAL]),
        NodeKind::New => sig!(inputs: []; in_tail: ARG, outputs: [INT_VAL]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::NodeKind;
    use cranelift_entity::EntityRef;

    fn kinds(kind: &NodeKind) -> (Vec<ExpectedValueKind>, Vec<ExpectedValueKind>) {
        let sig = expected_signature(kind);
        (
            sig.inputs.head.iter().map(|s| s.kind).collect(),
            sig.outputs.head.iter().map(|s| s.kind).collect(),
        )
    }

    #[test]
    fn expected_signature_int_const() {
        let (inputs, outputs) = kinds(&NodeKind::IntConst(crate::node::const_value::ConstId::new(
            42_usize,
        )));
        assert_eq!(inputs, vec![]);
        assert_eq!(outputs, vec![ExpectedValueKind::AnyInt]);
    }

    #[test]
    fn expected_signature_entry() {
        let (inputs, outputs) = kinds(&NodeKind::Entry);
        assert_eq!(inputs, vec![]);
        assert_eq!(outputs, vec![ExpectedValueKind::Control]);
    }

    #[test]
    fn expected_signature_if() {
        let (inputs, outputs) = kinds(&NodeKind::If);
        assert_eq!(
            inputs,
            vec![ExpectedValueKind::Control, ExpectedValueKind::Bool]
        );
        assert_eq!(
            outputs,
            vec![ExpectedValueKind::Control, ExpectedValueKind::Control]
        );
    }

    #[test]
    fn expected_signature_initial_memory() {
        let (inputs, outputs) = kinds(&NodeKind::InitialMemory);
        assert_eq!(inputs, vec![]);
        assert_eq!(outputs, vec![ExpectedValueKind::Memory]);
    }

    #[test]
    fn expected_signature_load() {
        let space = rsleigh::VnSpace::RAM;
        let (inputs, outputs) = kinds(&NodeKind::Load(space));
        assert_eq!(
            inputs,
            vec![ExpectedValueKind::Memory, ExpectedValueKind::AnyInt]
        );
        assert_eq!(outputs, vec![ExpectedValueKind::AnyInt]);
    }

    #[test]
    fn expected_signature_store() {
        let space = rsleigh::VnSpace::RAM;
        let (inputs, outputs) = kinds(&NodeKind::Store(space));
        assert_eq!(
            inputs,
            vec![
                ExpectedValueKind::Memory,
                ExpectedValueKind::AnyInt,
                ExpectedValueKind::AnyInt,
            ]
        );
        assert_eq!(outputs, vec![ExpectedValueKind::Memory]);
    }

    #[test]
    fn expected_signature_return() {
        let (inputs, outputs) = kinds(&NodeKind::Return);
        assert_eq!(
            inputs,
            vec![ExpectedValueKind::Control, ExpectedValueKind::Memory]
        );
        assert_eq!(outputs, vec![]);
    }

    #[test]
    fn expected_signature_call() {
        let (inputs, outputs) = kinds(&NodeKind::Call);
        assert_eq!(
            inputs,
            vec![
                ExpectedValueKind::Control,
                ExpectedValueKind::Memory,
                ExpectedValueKind::AnyInt, // target
                ExpectedValueKind::AnyInt, // sp
            ]
        );
        assert_eq!(
            outputs,
            vec![ExpectedValueKind::Control, ExpectedValueKind::Memory]
        );
    }

    #[test]
    fn expected_signature_int_binary_op() {
        use crate::node::IntBinaryOp;
        let (inputs, outputs) = kinds(&NodeKind::IntBinaryOp(IntBinaryOp::Add));
        assert_eq!(
            inputs,
            vec![ExpectedValueKind::AnyInt, ExpectedValueKind::AnyInt]
        );
        assert_eq!(outputs, vec![ExpectedValueKind::AnyInt]);
    }

    #[test]
    fn expected_signature_int_cmp_op() {
        use crate::node::IntCmpOp;
        let (inputs, outputs) = kinds(&NodeKind::IntCmpOp(IntCmpOp::Equal));
        assert_eq!(
            inputs,
            vec![ExpectedValueKind::AnyInt, ExpectedValueKind::AnyInt]
        );
        assert_eq!(outputs, vec![ExpectedValueKind::Bool]);
    }

    #[test]
    fn expected_signature_region() {
        let (inputs, outputs) = kinds(&NodeKind::Region);
        assert_eq!(inputs, vec![]);
        assert_eq!(
            outputs,
            vec![ExpectedValueKind::Control, ExpectedValueKind::PhiToken]
        );
    }

    #[test]
    fn expected_signature_mem_phi() {
        let (inputs, outputs) = kinds(&NodeKind::MemPhi);
        assert_eq!(inputs, vec![ExpectedValueKind::PhiToken]);
        assert_eq!(outputs, vec![ExpectedValueKind::Memory]);
    }

    #[test]
    fn expected_signature_float_const() {
        let (inputs, outputs) = kinds(&NodeKind::FloatConst(0));
        assert_eq!(inputs, vec![]);
        assert_eq!(outputs, vec![ExpectedValueKind::AnyFloat]);
    }

    #[test]
    fn expected_signature_float_binary_op() {
        use crate::node::FloatBinaryOp;
        let (inputs, outputs) = kinds(&NodeKind::FloatBinaryOp(FloatBinaryOp::Add));
        assert_eq!(
            inputs,
            vec![ExpectedValueKind::AnyFloat, ExpectedValueKind::AnyFloat]
        );
        assert_eq!(outputs, vec![ExpectedValueKind::AnyFloat]);
    }

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
        assert!(sig.inputs.tail.is_some());
        assert_eq!(sig.inputs.head.len(), 4);
        assert_eq!(sig.inputs.at(0).unwrap().name, "ctrl");
        assert_eq!(sig.inputs.at(1).unwrap().name, "mem");
        assert_eq!(sig.inputs.at(2).unwrap().name, "target");
        assert_eq!(sig.inputs.at(3).unwrap().name, "sp");
        assert_eq!(sig.inputs.at(3).unwrap().role, SlotRole::Sp);
        assert_eq!(sig.inputs.at(4).unwrap().role, SlotRole::Arg);
        assert_eq!(sig.inputs.at(999).unwrap().role, SlotRole::Arg);
    }

    #[test]
    fn return_input_tail_is_ret() {
        let sig = expected_signature(&NodeKind::Return);
        assert_eq!(sig.inputs.head.len(), 2);
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
        use crate::node::IntBinaryOp;
        let sig = expected_signature(&NodeKind::IntBinaryOp(IntBinaryOp::Add));
        assert_eq!(sig.inputs.at(0).unwrap().role, SlotRole::Lhs);
        assert_eq!(sig.inputs.at(1).unwrap().role, SlotRole::Rhs);
    }

    /// One representative per [`NodeKind`] variant, in declaration order.
    fn every_node_kind() -> Vec<NodeKind> {
        use crate::node::{
            ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp, IntUnaryOp,
        };
        let space = rsleigh::VnSpace::RAM;
        vec![
            NodeKind::Entry,
            NodeKind::InitialMemory,
            NodeKind::InitialVar(crate::node::InitialVnId::from_index(0)),
            NodeKind::Region,
            NodeKind::MemPhi,
            NodeKind::Phi,
            NodeKind::If,
            NodeKind::Switch,
            NodeKind::Call,
            NodeKind::Return,
            NodeKind::IndirectBranch,
            NodeKind::Unreachable,
            NodeKind::Load(space),
            NodeKind::Store(space),
            NodeKind::IntConst(crate::node::const_value::ConstId::new(0_usize)),
            NodeKind::IntUnaryOp(IntUnaryOp::Neg),
            NodeKind::IntBinaryOp(IntBinaryOp::Add),
            NodeKind::IntCmpOp(IntCmpOp::Equal),
            NodeKind::Truncate,
            NodeKind::Popcount,
            NodeKind::Lzcount,
            NodeKind::Extend(ExtendOp::ZeroExtend),
            NodeKind::FloatConst(0),
            NodeKind::FloatBinaryOp(FloatBinaryOp::Add),
            NodeKind::FloatUnaryOp(FloatUnaryOp::Neg),
            NodeKind::FloatCmpOp(FloatCmpOp::Equal),
            NodeKind::IntToFloat,
            NodeKind::FloatToInt,
            NodeKind::FloatToFloat,
            NodeKind::IntBitsToFloat,
            NodeKind::FloatBitsToInt,
            NodeKind::CallOther { user_op_id: 0 },
            NodeKind::SegmentOp { op_id: 0 },
            NodeKind::CPoolRef,
            NodeKind::New,
        ]
    }

    /// Position of `kind` in [`every_node_kind`]'s declaration order. The match
    /// has no `_` arm, so a new [`NodeKind`] variant does not compile until it
    /// is numbered here, and `every_node_kind_is_exhaustive` then fails until it
    /// also has a representative.
    fn declaration_index(kind: &NodeKind) -> usize {
        match kind {
            NodeKind::Entry => 0,
            NodeKind::InitialMemory => 1,
            NodeKind::InitialVar(_) => 2,
            NodeKind::Region => 3,
            NodeKind::MemPhi => 4,
            NodeKind::Phi => 5,
            NodeKind::If => 6,
            NodeKind::Switch => 7,
            NodeKind::Call => 8,
            NodeKind::Return => 9,
            NodeKind::IndirectBranch => 10,
            NodeKind::Unreachable => 11,
            NodeKind::Load(_) => 12,
            NodeKind::Store(_) => 13,
            NodeKind::IntConst(_) => 14,
            NodeKind::IntUnaryOp(_) => 15,
            NodeKind::IntBinaryOp(_) => 16,
            NodeKind::IntCmpOp(_) => 17,
            NodeKind::Truncate => 18,
            NodeKind::Popcount => 19,
            NodeKind::Lzcount => 20,
            NodeKind::Extend(_) => 21,
            NodeKind::FloatConst(_) => 22,
            NodeKind::FloatBinaryOp(_) => 23,
            NodeKind::FloatUnaryOp(_) => 24,
            NodeKind::FloatCmpOp(_) => 25,
            NodeKind::IntToFloat => 26,
            NodeKind::FloatToInt => 27,
            NodeKind::FloatToFloat => 28,
            NodeKind::IntBitsToFloat => 29,
            NodeKind::FloatBitsToInt => 30,
            NodeKind::CallOther { .. } => 31,
            NodeKind::SegmentOp { .. } => 32,
            NodeKind::CPoolRef => 33,
            NodeKind::New => 34,
        }
    }

    #[test]
    fn every_node_kind_is_exhaustive() {
        let kinds = every_node_kind();
        let seen: std::collections::BTreeSet<usize> = kinds.iter().map(declaration_index).collect();
        let expected: std::collections::BTreeSet<usize> = (0..kinds.len()).collect();
        assert_eq!(
            seen, expected,
            "every_node_kind must hold one representative per variant, in declaration order"
        );
    }

    #[test]
    fn expected_signature_covers_every_node_kind() {
        for k in &every_node_kind() {
            let sig = expected_signature(k);
            for i in 0..sig.inputs.head.len() {
                assert!(sig.inputs.at(i).is_some(), "input.at({i}) for {k:?}");
            }
            for i in 0..sig.outputs.head.len() {
                assert!(sig.outputs.at(i).is_some(), "output.at({i}) for {k:?}");
            }
            if sig.inputs.tail.is_some() {
                let tail = sig.inputs.at(sig.inputs.head.len());
                assert!(tail.is_some(), "variadic input tail for {k:?}");
            }
            if sig.outputs.tail.is_some() {
                let tail = sig.outputs.at(sig.outputs.head.len());
                assert!(tail.is_some(), "variadic output tail for {k:?}");
            }
        }
    }

    /// Guards against an integer-only kind creeping into a tail that must
    /// admit Bool flag-register values.
    #[test]
    fn variadic_tail_kinds_match_intent() {
        use ExpectedValueKind as K;
        let cases: &[(NodeKind, K)] = &[
            (NodeKind::Region, K::Control),
            (NodeKind::MemPhi, K::Memory),
            (NodeKind::Phi, K::AnyValue),
            (NodeKind::Call, K::AnyValue),
            (NodeKind::CallOther { user_op_id: 0 }, K::AnyValue),
            (NodeKind::Return, K::AnyValue),
            (NodeKind::CPoolRef, K::AnyInt),
            (NodeKind::New, K::AnyValue),
        ];
        for (k, expected) in cases {
            let sig = expected_signature(k);
            let tail = sig
                .inputs
                .tail
                .unwrap_or_else(|| panic!("input tail for {k:?}"));
            assert_eq!(tail.kind, *expected, "input tail kind for {k:?}");
        }

        let sig = expected_signature(&NodeKind::Call);
        let tail = sig
            .outputs
            .tail
            .expect("Call output tail is variadic (clobbered registers)");
        assert_eq!(tail.kind, K::AnyValue);
    }

    #[test]
    fn expected_signature_switch_is_ctrl_val_in_variadic_ctrl_out() {
        let sig = expected_signature(&NodeKind::Switch);
        assert_eq!(sig.inputs.head.len(), 2);
        assert!(sig.inputs.tail.is_none(), "switch inputs are fixed-arity");
        assert!(
            sig.outputs.tail.is_some(),
            "switch has variadic control outputs"
        );
    }
}
