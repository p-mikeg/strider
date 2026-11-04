//! Single source of truth for every node's slot shape: kind (validation),
//! name (dot labels), and role (dot colors).

use crate::node::{NodeKind, ValueKind, ValueType};

/// A slot's admissible value kinds, coarser than the concrete
/// [`ValueKind`] stored on real outputs.
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

const _: () = assert!(size_of::<ExpectedValueKind>() == 1);

impl ExpectedValueKind {
    /// Whether a value of kind `actual` may fill a slot of this class.
    pub fn admits(self, actual: ValueKind) -> bool {
        match (self, actual) {
            (Self::Control, ValueKind::Control)
            | (Self::Memory, ValueKind::Memory)
            | (Self::PhiToken, ValueKind::PhiToken)
            | (Self::AnyValue, ValueKind::Typed(_)) => true,
            (Self::Bool, ValueKind::Typed(t)) => t.is_bool(),
            (Self::AnyInt, ValueKind::Typed(t)) => t.is_integer(),
            (Self::AnyFloat, ValueKind::Typed(t)) => t.is_float(),
            (Self::Control | Self::Memory | Self::PhiToken, ValueKind::Typed(_))
            | (Self::Control, ValueKind::Memory | ValueKind::PhiToken)
            | (Self::Memory, ValueKind::Control | ValueKind::PhiToken)
            | (Self::PhiToken, ValueKind::Control | ValueKind::Memory)
            | (
                Self::Bool | Self::AnyInt | Self::AnyFloat | Self::AnyValue,
                ValueKind::Control | ValueKind::Memory | ValueKind::PhiToken,
            ) => false,
        }
    }
}

/// The width an input slot owes its node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperandWidth {
    /// The node's output width.
    OfOutput,
    /// Input slot 0's width; the output is the `I1` verdict.
    OfFirstOperand,
    /// A width the conversion relates to the output's.
    Conversion(Conversion),
    /// No width rule here (a `Phi`'s inputs are held to its type by the phi
    /// check).
    Free,
}

/// How a conversion's input width relates to its output's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conversion {
    /// `Extend`: strictly wider.
    Widen,
    /// `Truncate`: strictly narrower.
    Narrow,
    /// A bitcast: the same size.
    SameSize,
}

impl Conversion {
    /// Whether `input` converts to `output` under this rule. A bitcast
    /// compares bytes: every float width is whole bytes, and none is one byte,
    /// so `I1` cannot pass for a float.
    #[must_use]
    pub fn admits(self, input: ValueType, output: ValueType) -> bool {
        match self {
            Self::Widen => output.bit_width() > input.bit_width(),
            Self::Narrow => output.bit_width() < input.bit_width(),
            Self::SameSize => input.byte_size() == output.byte_size(),
        }
    }
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

    /// Fixed slots before any tail; the index the variadic tail starts at.
    pub(crate) fn head_len(&self) -> usize {
        self.head.len()
    }

    /// Inclusive arity bound; `usize::MAX` for a repeating tail.
    pub(crate) fn max_len(&self) -> usize {
        self.max_len
    }

    /// Past the head: the tail slot while within `max_len`, else `None`.
    pub(crate) fn at(&self, idx: usize) -> Option<Slot> {
        if idx >= self.max_len {
            return None;
        }
        self.head.get(idx).copied().or(self.tail)
    }

    /// [`Self::at`] with the slot's ordinal within a repeating tail, `None`
    /// for a fixed head slot, so no caller re-derives the head length.
    pub(crate) fn at_indexed(&self, idx: usize) -> Option<(Slot, Option<usize>)> {
        Some((self.at(idx)?, idx.checked_sub(self.head.len())))
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
// Optional trailing `IndirectBranch` / `Switch` input: the ISA-mode bit the
// branch instruction commits, taken from the write to the `ISAModeSwitch`
// register (ARM `SetThumbMode`, MIPS `JXWritePC`).
const ISA_MODE: Slot = slot(AnyInt, "isa", R::Cond);
const SP: Slot = slot(AnyInt, "sp", R::Sp);
// Argument and return registers hold floats as well as integers, as do the
// clobbered registers a `Call` outputs (`ANY_VAL`).
const ARG: Slot = slot(AnyValue, "arg", R::Arg);
const RET: Slot = slot(AnyValue, "ret", R::Ret);
const REF: Slot = slot(AnyInt, "ref", R::Ref);
// Per-predecessor Phi input.
const IN_PHI: Slot = slot(AnyValue, "in", R::In);

pub(crate) fn expected_signature(kind: &NodeKind) -> Signature {
    const fn sig(inputs: SlotList, outputs: SlotList) -> Signature {
        Signature { inputs, outputs }
    }
    use SlotList as L;

    match kind {
        NodeKind::Entry => sig(L::fixed(&[]), L::fixed(&[CTRL])),
        NodeKind::InitialMemory => sig(L::fixed(&[]), L::fixed(&[MEM])),
        NodeKind::InitialVar(_) | NodeKind::IntConst(_) => sig(L::fixed(&[]), L::fixed(&[INT_VAL])),

        // One Control input per predecessor.
        NodeKind::Region => sig(L::variadic(&[], CTRL), L::fixed(&[CTRL, PHI])),
        NodeKind::MemPhi => sig(L::variadic(&[PHI], MEM), L::fixed(&[MEM])),
        // Tagged and anonymous phis share this shape.
        NodeKind::Phi => sig(L::variadic(&[PHI], IN_PHI), L::fixed(&[ANY_VAL])),

        NodeKind::If => sig(L::fixed(&[CTRL, COND]), L::fixed(&[CTRL, CTRL])),
        // One Control output per target region, in target order. No default
        // arm, and the arms need not cover every dispatch value: a site the
        // resolver seated early can hold one arm and widen later.
        NodeKind::Switch(_) => sig(
            L::optional(&[CTRL, INT_VAL], ISA_MODE),
            L::variadic(&[CTRL], CTRL),
        ),

        // SP is an input-only anchor; the outputs are the clobbered varnodes.
        NodeKind::Call { .. } => sig(
            L::variadic(&[CTRL, MEM, TARGET, SP], ARG),
            L::variadic(&[CTRL, MEM], ANY_VAL),
        ),
        NodeKind::Return => sig(L::variadic(&[CTRL, MEM], RET), L::fixed(&[])),
        // Placeholder for an unresolved branch. The optional trailing input is
        // the interworking ISA-mode bit (absent for a non-switching branch); it
        // is a real input so the optimizer keeps its cone live for the resolver.
        NodeKind::IndirectBranch => sig(L::optional(&[CTRL, MEM, TARGET], ISA_MODE), L::fixed(&[])),
        // Control sink for a no-return trap. The optional trailing input is the
        // memory chain: control alone anchors no liveness, so an exit-free
        // control cycle would otherwise lose its stores to compaction.
        NodeKind::Unreachable => sig(L::optional(&[CTRL], MEM), L::fixed(&[])),

        NodeKind::Load(_) => sig(L::fixed(&[MEM, ADDR]), L::fixed(&[INT_VAL])),
        NodeKind::Store(_) => sig(L::fixed(&[MEM, ADDR, DATA]), L::fixed(&[MEM])),

        // `IntConst` shares the input-less arm above.
        NodeKind::IntUnaryOp(_)
        | NodeKind::Truncate
        | NodeKind::Popcount
        | NodeKind::Lzcount
        | NodeKind::Extend(_) => sig(L::fixed(&[INT_VAL]), L::fixed(&[INT_VAL])),
        NodeKind::IntBinaryOp(_) => sig(L::fixed(&[LHS, RHS]), L::fixed(&[INT_VAL])),
        NodeKind::IntCmpOp(_) => sig(L::fixed(&[LHS, RHS]), L::fixed(&[BOOL_VAL])),

        NodeKind::FloatConst(_) => sig(L::fixed(&[]), L::fixed(&[FLOAT_VAL])),
        NodeKind::FloatBinaryOp(_) => sig(L::fixed(&[FLHS, FRHS]), L::fixed(&[FLOAT_VAL])),
        NodeKind::FloatUnaryOp(_) | NodeKind::FloatToFloat => {
            sig(L::fixed(&[FLOAT_VAL]), L::fixed(&[FLOAT_VAL]))
        }
        NodeKind::FloatCmpOp(_) => sig(L::fixed(&[FLHS, FRHS]), L::fixed(&[BOOL_VAL])),
        NodeKind::IntToFloat | NodeKind::IntBitsToFloat => {
            sig(L::fixed(&[INT_VAL]), L::fixed(&[FLOAT_VAL]))
        }
        NodeKind::FloatToInt | NodeKind::FloatBitsToInt => {
            sig(L::fixed(&[FLOAT_VAL]), L::fixed(&[INT_VAL]))
        }

        NodeKind::CallOther { .. } => sig(
            L::variadic(&[CTRL, MEM], ARG),
            L::variadic(&[CTRL, MEM], ANY_VAL),
        ),
        NodeKind::CPoolRef => sig(L::variadic(&[], REF), L::fixed(&[INT_VAL])),
        NodeKind::New => sig(L::variadic(&[], ARG), L::fixed(&[INT_VAL])),
    }
}

/// One representative per [`NodeKind`] variant, in declaration order. Its
/// ids are dangling: it is for slot-shape tables, not for a graph.
#[cfg(test)]
pub(crate) fn every_node_kind() -> Vec<NodeKind> {
    use crate::node::{
        ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp, IntUnaryOp,
    };
    use cranelift_entity::EntityRef;
    let space = rsleigh::VnSpace::RAM;
    vec![
        NodeKind::Entry,
        NodeKind::InitialMemory,
        NodeKind::InitialVar(crate::node::InitialVnId::from_index(0)),
        NodeKind::Region,
        NodeKind::MemPhi,
        NodeKind::Phi,
        NodeKind::If,
        NodeKind::Switch(crate::node::SwitchTableId::from_u32(0)),
        NodeKind::Call { cc: None },
        NodeKind::Return,
        NodeKind::IndirectBranch,
        NodeKind::Unreachable,
        NodeKind::Load(space),
        NodeKind::Store(space),
        NodeKind::IntConst(crate::node::IntConstId::new(0_usize)),
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
        NodeKind::CPoolRef,
        NodeKind::New,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use cranelift_entity::EntityRef;

    fn kinds(kind: &NodeKind) -> (Vec<ExpectedValueKind>, Vec<ExpectedValueKind>) {
        let sig = expected_signature(kind);
        (
            sig.inputs.head.iter().map(|s| s.kind).collect(),
            sig.outputs.head.iter().map(|s| s.kind).collect(),
        )
    }

    #[test]
    fn expected_signature_rows() {
        use crate::node::{FloatBinaryOp, IntBinaryOp, IntCmpOp};
        use ExpectedValueKind::{AnyFloat, AnyInt, Bool, Control, Memory, PhiToken};
        let ram = rsleigh::VnSpace::RAM;
        let rows: [(NodeKind, &[ExpectedValueKind], &[ExpectedValueKind]); 14] = [
            (
                NodeKind::IntConst(crate::node::IntConstId::new(42_usize)),
                &[],
                &[AnyInt],
            ),
            (NodeKind::Entry, &[], &[Control]),
            (NodeKind::If, &[Control, Bool], &[Control, Control]),
            (NodeKind::InitialMemory, &[], &[Memory]),
            (NodeKind::Load(ram), &[Memory, AnyInt], &[AnyInt]),
            (NodeKind::Store(ram), &[Memory, AnyInt, AnyInt], &[Memory]),
            (NodeKind::Return, &[Control, Memory], &[]),
            // ctrl, mem, target, sp
            (
                NodeKind::Call { cc: None },
                &[Control, Memory, AnyInt, AnyInt],
                &[Control, Memory],
            ),
            (
                NodeKind::IntBinaryOp(IntBinaryOp::Add),
                &[AnyInt, AnyInt],
                &[AnyInt],
            ),
            (
                NodeKind::IntCmpOp(IntCmpOp::Equal),
                &[AnyInt, AnyInt],
                &[Bool],
            ),
            (NodeKind::Region, &[], &[Control, PhiToken]),
            (NodeKind::MemPhi, &[PhiToken], &[Memory]),
            (NodeKind::FloatConst(0), &[], &[AnyFloat]),
            (
                NodeKind::FloatBinaryOp(FloatBinaryOp::Add),
                &[AnyFloat, AnyFloat],
                &[AnyFloat],
            ),
        ];
        for (kind, inputs, outputs) in rows {
            assert_eq!(
                kinds(&kind),
                (inputs.to_vec(), outputs.to_vec()),
                "{kind:?}"
            );
        }
    }

    #[test]
    fn expected_signature_slot_roles() {
        use crate::node::IntBinaryOp;
        let rows: [(NodeKind, usize, &str, SlotRole); 11] = [
            (
                NodeKind::Load(rsleigh::VnSpace::RAM),
                0,
                "mem",
                SlotRole::Memory,
            ),
            (
                NodeKind::Load(rsleigh::VnSpace::RAM),
                1,
                "addr",
                SlotRole::Addr,
            ),
            (NodeKind::Call { cc: None }, 0, "ctrl", SlotRole::Control),
            (NodeKind::Call { cc: None }, 1, "mem", SlotRole::Memory),
            (NodeKind::Call { cc: None }, 2, "target", SlotRole::Target),
            (NodeKind::Call { cc: None }, 3, "sp", SlotRole::Sp),
            (NodeKind::Return, 0, "ctrl", SlotRole::Control),
            (NodeKind::Return, 1, "mem", SlotRole::Memory),
            (NodeKind::If, 1, "cond", SlotRole::Cond),
            (
                NodeKind::IntBinaryOp(IntBinaryOp::Add),
                0,
                "lhs",
                SlotRole::Lhs,
            ),
            (
                NodeKind::IntBinaryOp(IntBinaryOp::Add),
                1,
                "rhs",
                SlotRole::Rhs,
            ),
        ];
        for (kind, at, name, role) in rows {
            let slot = expected_signature(&kind).inputs.at(at).unwrap();
            assert_eq!((slot.name, slot.role), (name, role), "{kind:?} slot {at}");
        }
        let call = expected_signature(&NodeKind::Call { cc: None });
        assert_eq!(call.inputs.head.len(), 4);
        assert_eq!(call.inputs.at(4).unwrap().role, SlotRole::Arg);
        assert_eq!(call.inputs.at(999).unwrap().role, SlotRole::Arg);
        let ret = expected_signature(&NodeKind::Return);
        assert_eq!(ret.inputs.head.len(), 2);
        assert_eq!(ret.inputs.at(2).unwrap().role, SlotRole::Ret);
        assert_eq!(ret.inputs.at(99).unwrap().role, SlotRole::Ret);
    }

    /// One pattern per [`NodeKind`] variant, in declaration order: yields
    /// `declaration_index` and `KIND_COUNT` from the one list.
    macro_rules! declaration_order {
        ($($kind:pat),* $(,)?) => {
            const DECLARED: &[&str] = &[$(stringify!($kind)),*];
            const KIND_COUNT: usize = DECLARED.len();

            /// Position of `kind` in the list. The match has no `_` arm, so a
            /// new variant does not compile until it is listed, and
            /// `every_node_kind_is_exhaustive` then fails until
            /// [`every_node_kind`] holds it too.
            fn declaration_index(kind: &NodeKind) -> usize {
                let name = match kind {
                    $($kind => stringify!($kind),)*
                };
                DECLARED.iter().position(|&d| d == name).expect("listed")
            }
        };
    }

    declaration_order!(
        NodeKind::Entry,
        NodeKind::InitialMemory,
        NodeKind::InitialVar(_),
        NodeKind::Region,
        NodeKind::MemPhi,
        NodeKind::Phi,
        NodeKind::If,
        NodeKind::Switch(_),
        NodeKind::Call { .. },
        NodeKind::Return,
        NodeKind::IndirectBranch,
        NodeKind::Unreachable,
        NodeKind::Load(_),
        NodeKind::Store(_),
        NodeKind::IntConst(_),
        NodeKind::IntUnaryOp(_),
        NodeKind::IntBinaryOp(_),
        NodeKind::IntCmpOp(_),
        NodeKind::Truncate,
        NodeKind::Popcount,
        NodeKind::Lzcount,
        NodeKind::Extend(_),
        NodeKind::FloatConst(_),
        NodeKind::FloatBinaryOp(_),
        NodeKind::FloatUnaryOp(_),
        NodeKind::FloatCmpOp(_),
        NodeKind::IntToFloat,
        NodeKind::FloatToInt,
        NodeKind::FloatToFloat,
        NodeKind::IntBitsToFloat,
        NodeKind::FloatBitsToInt,
        NodeKind::CallOther { .. },
        NodeKind::CPoolRef,
        NodeKind::New,
    );

    #[test]
    fn every_node_kind_is_exhaustive() {
        let kinds = every_node_kind();
        assert_eq!(kinds.len(), KIND_COUNT, "one representative per variant");
        let order: Vec<usize> = kinds.iter().map(declaration_index).collect();
        assert_eq!(
            order,
            (0..KIND_COUNT).collect::<Vec<_>>(),
            "every_node_kind must list the variants in declaration order"
        );
    }

    /// `EditFunction::canonicalize_node` merges a twin through its only output.
    #[test]
    fn every_cacheable_kind_has_one_output() {
        for k in every_node_kind().iter().filter(|k| k.is_cacheable()) {
            let sig = expected_signature(k);
            assert_eq!(sig.outputs.max_len, 1, "{k:?} is cacheable");
        }
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

    /// `has_control_flow` and `is_terminator` are hand-written; the
    /// signature is what they restate.
    #[test]
    fn control_predicates_agree_with_the_signature() {
        let has_control = |l: &SlotList| {
            l.head
                .iter()
                .chain(&l.tail)
                .any(|s| s.kind == ExpectedValueKind::Control)
        };
        for k in &every_node_kind() {
            let sig = expected_signature(k);
            let (control_in, control_out) = (has_control(&sig.inputs), has_control(&sig.outputs));
            assert_eq!(
                k.has_control_flow(),
                control_in || control_out,
                "has_control_flow for {k:?}"
            );
            // The doc's "produces no control"; a terminator has no outputs at all.
            assert_eq!(
                k.is_terminator(),
                control_in && !control_out,
                "is_terminator for {k:?}"
            );
            if k.is_terminator() {
                assert_eq!(sig.outputs.max_len, 0, "{k:?} has outputs");
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
            (NodeKind::Call { cc: None }, K::AnyValue),
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

        let sig = expected_signature(&NodeKind::Call { cc: None });
        let tail = sig
            .outputs
            .tail
            .expect("Call output tail is variadic (clobbered registers)");
        assert_eq!(tail.kind, K::AnyValue);
    }

    #[test]
    fn expected_signature_switch_is_ctrl_val_in_variadic_ctrl_out() {
        let sig = expected_signature(&NodeKind::Switch(crate::node::SwitchTableId::from_u32(0)));
        assert_eq!(sig.inputs.head.len(), 2);
        assert_eq!(sig.inputs.at(2).map(|slot| slot.name), Some("isa"));
        assert!(
            sig.inputs.at(3).is_none(),
            "switch takes at most an ISA-mode input past its head"
        );
        assert!(
            sig.outputs.tail.is_some(),
            "switch has variadic control outputs"
        );
    }

    /// Which value kinds each slot class admits, both directions.
    #[test]
    fn admits_is_the_slot_class_table() {
        use crate::node::{ValueKind as V, ValueType};
        let actual = [
            V::Control,
            V::Memory,
            V::PhiToken,
            V::Typed(ValueType::I1),
            V::Typed(ValueType::int(32)),
            V::Typed(ValueType::float(64)),
        ];
        use ExpectedValueKind as E;
        let rows: [(E, [bool; 6]); 7] = [
            (E::Control, [true, false, false, false, false, false]),
            (E::Memory, [false, true, false, false, false, false]),
            (E::PhiToken, [false, false, true, false, false, false]),
            (E::Bool, [false, false, false, true, false, false]),
            (E::AnyInt, [false, false, false, true, true, false]),
            (E::AnyFloat, [false, false, false, false, false, true]),
            (E::AnyValue, [false, false, false, true, true, true]),
        ];
        for (want, row) in rows {
            for (got, admitted) in actual.into_iter().zip(row) {
                assert_eq!(want.admits(got), admitted, "{want:?} admits {got:?}");
            }
        }
    }

    /// The width each slot owes, one row per rule.
    #[test]
    fn operand_width_rows() {
        use crate::node::{
            Conversion, ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp,
            IntUnaryOp, OperandWidth as W,
        };
        let space = rsleigh::VnSpace::RAM;
        let rows = [
            (NodeKind::IntBinaryOp(IntBinaryOp::Add), 1, W::OfOutput),
            (NodeKind::IntUnaryOp(IntUnaryOp::Neg), 0, W::OfOutput),
            (NodeKind::FloatBinaryOp(FloatBinaryOp::Mul), 1, W::OfOutput),
            (NodeKind::FloatUnaryOp(FloatUnaryOp::Sqrt), 0, W::OfOutput),
            (NodeKind::FloatCmpOp(FloatCmpOp::Less), 1, W::OfFirstOperand),
            (
                NodeKind::IntBinaryOp(IntBinaryOp::ShiftLeft),
                0,
                W::OfOutput,
            ),
            (NodeKind::IntBinaryOp(IntBinaryOp::SShiftRight), 1, W::Free),
            (NodeKind::IntCmpOp(IntCmpOp::Sless), 1, W::OfFirstOperand),
            (
                NodeKind::Extend(ExtendOp::SignExtend),
                0,
                W::Conversion(Conversion::Widen),
            ),
            (NodeKind::Truncate, 0, W::Conversion(Conversion::Narrow)),
            (
                NodeKind::FloatBitsToInt,
                0,
                W::Conversion(Conversion::SameSize),
            ),
            (NodeKind::Load(space), 1, W::Free),
            (NodeKind::Popcount, 0, W::Free),
        ];
        for (kind, slot, want) in rows {
            assert_eq!(kind.operand_width(slot), want, "{kind:?} slot {slot}");
        }
        assert_eq!(NodeKind::Store(space).input_arity(), 3..=3);
        assert_eq!(NodeKind::Unreachable.input_arity(), 1..=2);
        assert_eq!(NodeKind::Region.input_arity(), 0..=usize::MAX);
        assert_eq!(
            NodeKind::Store(space).expected_input_kind(2),
            Some(ExpectedValueKind::AnyInt)
        );
        assert_eq!(NodeKind::Store(space).expected_input_kind(3), None);
    }

    /// `operand_width` is a second per-kind table beside the signature: a slot
    /// it ties to a width is a value slot there, and so is the output it reads.
    #[test]
    fn operand_widths_name_value_slots_of_the_signature() {
        use crate::node::{IntBinaryOp, OperandWidth as W};
        use ExpectedValueKind as E;
        let is_value =
            |k: Option<E>| matches!(k, Some(E::Bool | E::AnyInt | E::AnyFloat | E::AnyValue));
        let kinds = every_node_kind()
            .into_iter()
            .chain(IntBinaryOp::ALL.map(NodeKind::IntBinaryOp));
        for k in kinds {
            let inputs = expected_signature(&k).inputs;
            for slot in 0..inputs.max_len.min(inputs.head.len() + 1) {
                let rule = k.operand_width(slot);
                if rule == W::Free {
                    continue;
                }
                let input = k.expected_input_kind(slot);
                assert!(is_value(input), "{k:?} slot {slot}: {rule:?} on {input:?}");
                let output = k.expected_output_kind(0);
                match rule {
                    W::OfFirstOperand => assert_eq!(output, Some(E::Bool), "{k:?}"),
                    W::OfOutput | W::Conversion(_) => assert!(is_value(output), "{k:?}"),
                    W::Free => unreachable!(),
                }
            }
        }
    }

    /// Each named input slot sits where the signature names it.
    #[test]
    fn slot_consts_name_their_signature_slots() {
        let space = rsleigh::VnSpace::RAM;
        let name =
            |k: NodeKind, slot: usize| expected_signature(&k).inputs.at(slot).map(|s| s.name);
        let call = NodeKind::Call { cc: None };
        assert_eq!(
            name(NodeKind::Load(space), NodeKind::ACCESS_ADDR_SLOT),
            Some("addr")
        );
        assert_eq!(
            name(NodeKind::Store(space), NodeKind::ACCESS_ADDR_SLOT),
            Some("addr")
        );
        assert_eq!(
            name(NodeKind::Store(space), NodeKind::STORE_DATA_SLOT),
            Some("data")
        );
        assert_eq!(name(call, NodeKind::CALL_TARGET_SLOT), Some("target"));
        assert_eq!(name(call, NodeKind::CALL_SP_SLOT), Some("sp"));
        assert_eq!(name(NodeKind::If, NodeKind::IF_COND_SLOT), Some("cond"));

        let switch = NodeKind::Switch(crate::node::SwitchTableId::from_u32(0));
        let call_other = NodeKind::CallOther { user_op_id: 0 };
        let ctrl_then_mem = [
            call,
            call_other,
            NodeKind::Return,
            NodeKind::IndirectBranch,
            NodeKind::Unreachable,
        ];
        for kind in ctrl_then_mem.into_iter().chain([NodeKind::If, switch]) {
            assert_eq!(name(kind, NodeKind::CTRL_SLOT), Some("ctrl"), "{kind:?}");
        }
        for kind in ctrl_then_mem {
            assert_eq!(name(kind, NodeKind::CTRL_MEM_SLOT), Some("mem"), "{kind:?}");
        }
        // And no kind outside those lists has a fixed `ctrl` at the head, or a
        // `mem` after it.
        let listed = |kinds: &[NodeKind], k: NodeKind| {
            kinds
                .iter()
                .any(|l| std::mem::discriminant(l) == std::mem::discriminant(&k))
        };
        for kind in every_node_kind() {
            // A repeating tail (`Region`'s predecessors) holds no fixed slot.
            let tail_repeats = *kind.input_arity().end() == usize::MAX;
            let fixed = |slot: usize| {
                (slot < kind.input_head_len() || !tail_repeats).then(|| name(kind, slot))
            };
            let ctrl = fixed(NodeKind::CTRL_SLOT) == Some(Some("ctrl"));
            let ctrl_mem = ctrl && fixed(NodeKind::CTRL_MEM_SLOT) == Some(Some("mem"));
            assert_eq!(
                ctrl,
                listed(&ctrl_then_mem, kind) || listed(&[NodeKind::If, switch], kind),
                "{kind:?}"
            );
            assert_eq!(ctrl_mem, listed(&ctrl_then_mem, kind), "{kind:?}");
        }
        for kind in [NodeKind::Load(space), NodeKind::Store(space)] {
            assert_eq!(
                name(kind, NodeKind::ACCESS_MEM_SLOT),
                Some("mem"),
                "{kind:?}"
            );
        }
        assert_eq!(
            name(
                NodeKind::IndirectBranch,
                NodeKind::INDIRECT_BRANCH_TARGET_SLOT
            ),
            Some("target")
        );
        assert_eq!(name(switch, NodeKind::SWITCH_SELECTOR_SLOT), Some("val"));
        for kind in [NodeKind::Phi, NodeKind::MemPhi] {
            assert_eq!(
                name(kind, NodeKind::PHI_TOKEN_SLOT),
                Some("phi"),
                "{kind:?}"
            );
        }
    }

    /// Each named output slot holds the kind the signature gives it, and the
    /// value outputs start at `output_head_len`.
    #[test]
    fn output_slot_consts_name_their_signature_slots() {
        use ExpectedValueKind as E;
        let out =
            |k: NodeKind, slot: usize| expected_signature(&k).outputs.at(slot).map(|s| s.kind);
        let call = NodeKind::Call { cc: None };
        let call_other = NodeKind::CallOther { user_op_id: 0 };
        for kind in [NodeKind::Entry, NodeKind::Region, call, call_other] {
            assert_eq!(
                out(kind, NodeKind::CTRL_OUT_SLOT),
                Some(E::Control),
                "{kind:?}"
            );
        }
        for kind in [call, call_other] {
            assert_eq!(
                out(kind, NodeKind::CTRL_MEM_OUT_SLOT),
                Some(E::Memory),
                "{kind:?}"
            );
            assert_eq!(kind.output_head_len(), 2, "{kind:?}");
            assert_eq!(
                out(kind, kind.output_head_len()),
                Some(E::AnyValue),
                "{kind:?}"
            );
        }
        let space = rsleigh::VnSpace::RAM;
        for kind in [
            NodeKind::InitialMemory,
            NodeKind::Store(space),
            NodeKind::MemPhi,
        ] {
            assert_eq!(
                out(kind, NodeKind::MEM_OUT_SLOT),
                Some(E::Memory),
                "{kind:?}"
            );
            assert_eq!(expected_signature(&kind).outputs.max_len(), 1, "{kind:?}");
        }
        assert_eq!(
            out(NodeKind::Region, NodeKind::REGION_PHI_OUT_SLOT),
            Some(E::PhiToken)
        );
        assert_eq!(
            out(NodeKind::If, NodeKind::IF_TRUE_OUT_SLOT),
            Some(E::Control)
        );
        assert_eq!(
            out(NodeKind::If, NodeKind::IF_FALSE_OUT_SLOT),
            Some(E::Control)
        );
        assert_ne!(NodeKind::IF_TRUE_OUT_SLOT, NodeKind::IF_FALSE_OUT_SLOT);
    }
}
