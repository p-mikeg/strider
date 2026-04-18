//! Expected input/output signatures of every [`NodeKind`] variant.
//!
//! This module is the single source of truth for what input and output slots
//! a given [`NodeKind`] is expected to have.  It is used by the whole-graph
//! validator (see `validate` module) and any future `node_view()`
//! reconstruction.
//!
//! Slots are described by [`ExpectedOutputKind`], a coarser classification
//! than the concrete [`NodeOutputKind`] stored on actual outputs: integer
//! slots accept any width (U8..U256) via [`ExpectedOutputKind::AnyInt`], and
//! float slots accept F32 or F64 via [`ExpectedOutputKind::AnyFloat`].  Bool
//! remains a distinct kind.  Width-level checks, if ever needed, live
//! elsewhere.
//!
//! Variable-arity nodes ([`NodeKind::ControlState`], [`NodeKind::MemPhi`],
//! [`NodeKind::ControlPhi`], [`NodeKind::Call`], [`NodeKind::CallOther`],
//! [`NodeKind::Return`], [`NodeKind::CPoolRef`], [`NodeKind::New`]) return
//! only the **minimum fixed prefix** of inputs / outputs.  Callers that need
//! to validate the variadic tail must do it themselves.

use crate::node::NodeKind;

/// The expected kind of an input or output slot of a [`NodeKind`].
///
/// This is the type used by [`expected_signature`] and the validator to
/// describe what a slot should carry, without over-committing to a specific
/// integer or float width.
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

/// Expected (input kinds, output kinds) for a given [`NodeKind`].
///
/// For variable-arity kinds, the returned vectors describe only the fixed
/// prefix of the signature; see the module-level docs.
pub(crate) fn expected_signature(
    kind: &NodeKind,
) -> (Vec<ExpectedOutputKind>, Vec<ExpectedOutputKind>) {
    use ExpectedOutputKind::*;

    match kind {
        // ── Initial state ───────────────────────────────────────────────────
        NodeKind::Entry => (vec![], vec![Control]),
        NodeKind::InitialMemory => (vec![], vec![Memory]),
        NodeKind::InitialVar(_) => (vec![], vec![AnyInt]),

        // ── Region / join nodes (variadic inputs) ───────────────────────────
        // ControlState: one Control input per predecessor (variadic).
        NodeKind::ControlState => (vec![], vec![Control, ControlPhi]),
        // MemPhi: [phi_token, ...per-predecessor Memory tokens].
        NodeKind::MemPhi => (vec![ControlPhi], vec![Memory]),
        // ControlPhi: [phi_token, ...per-predecessor values].
        NodeKind::ControlPhi(_) => (vec![ControlPhi], vec![AnyInt]),

        // ── Conditional branch ──────────────────────────────────────────────
        NodeKind::If => (vec![Control, Bool], vec![Control, Control]),
        NodeKind::IfCase(_) => (vec![Control], vec![Control]),

        // ── Calls and returns ───────────────────────────────────────────────
        // Call: [control, memory, call_address, ...args].
        // Outputs: [Control, Memory, ...clobbered varnode values].
        NodeKind::Call => (vec![Control, Memory, AnyInt], vec![Control, Memory]),
        NodeKind::PostCallMemState => (vec![Control], vec![Memory]),
        NodeKind::PostCallVarState(_) => (vec![Control], vec![AnyInt]),
        // Return: [control, ...return values]. No memory input.
        NodeKind::Return => (vec![Control], vec![]),

        // ── Memory operations ───────────────────────────────────────────────
        NodeKind::Load(_) => (vec![Memory, AnyInt], vec![AnyInt]),
        NodeKind::Store(_) => (vec![Memory, AnyInt, AnyInt], vec![Memory]),
        // StackStore: [memory, base, data].
        NodeKind::StackStore { .. } => (vec![Memory, AnyInt, AnyInt], vec![Memory]),
        // StackStorePhi: [phi_token, memory, data].
        NodeKind::StackStorePhi { .. } => (vec![ControlPhi, Memory, AnyInt], vec![Memory]),

        // ── Integer constants and operations ────────────────────────────────
        NodeKind::IntConst(_) => (vec![], vec![AnyInt]),
        NodeKind::IntUnaryOp(_) => (vec![AnyInt], vec![AnyInt]),
        NodeKind::IntBinaryOp(_) => (vec![AnyInt, AnyInt], vec![AnyInt]),
        NodeKind::IntCmpOp(_) => (vec![AnyInt, AnyInt], vec![Bool]),
        NodeKind::CastToInt => (vec![AnyValue], vec![AnyInt]),
        NodeKind::Truncate => (vec![AnyInt], vec![AnyInt]),
        NodeKind::Popcount => (vec![AnyInt], vec![AnyInt]),
        NodeKind::Lzcount => (vec![AnyInt], vec![AnyInt]),
        NodeKind::Piece => (vec![AnyInt, AnyInt], vec![AnyInt]),
        NodeKind::Insert { .. } => (vec![AnyInt, AnyInt], vec![AnyInt]),
        NodeKind::Extend(_) => (vec![AnyInt], vec![AnyInt]),

        // ── Boolean constants and operations ────────────────────────────────
        NodeKind::BoolConst(_) => (vec![], vec![Bool]),
        NodeKind::BoolUnaryOp(_) => (vec![Bool], vec![Bool]),
        NodeKind::BoolBinaryOp(_) => (vec![Bool, Bool], vec![Bool]),
        NodeKind::CastToBool => (vec![AnyValue], vec![Bool]),

        // ── Float constants and operations ──────────────────────────────────
        NodeKind::FloatConst(_) => (vec![], vec![AnyFloat]),
        NodeKind::FloatBinaryOp(_) => (vec![AnyFloat, AnyFloat], vec![AnyFloat]),
        NodeKind::FloatUnaryOp(_) => (vec![AnyFloat], vec![AnyFloat]),
        NodeKind::FloatCmpOp(_) => (vec![AnyFloat, AnyFloat], vec![Bool]),
        NodeKind::IntToFloat => (vec![AnyInt], vec![AnyFloat]),
        NodeKind::FloatToInt => (vec![AnyFloat], vec![AnyInt]),
        NodeKind::FloatToFloat => (vec![AnyFloat], vec![AnyFloat]),
        NodeKind::IntBitsToFloat => (vec![AnyInt], vec![AnyFloat]),
        NodeKind::FloatBitsToInt => (vec![AnyFloat], vec![AnyInt]),
        NodeKind::CastToFloat => (vec![AnyValue], vec![AnyFloat]),

        // ── User-defined / opaque opcodes ───────────────────────────────────
        // CallOther: [control, memory, ...args].
        // Outputs: [Control, Memory] or [Control, Memory, OutputType].
        NodeKind::CallOther { .. } => (vec![Control, Memory], vec![Control, Memory]),
        NodeKind::SegmentOp { .. } => (vec![AnyInt, AnyInt], vec![AnyInt]),
        // CPoolRef: [...refs] (variadic).
        NodeKind::CPoolRef => (vec![], vec![AnyInt]),
        // New: [...args] (variadic, typically a size).
        NodeKind::New => (vec![], vec![AnyInt]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::NodeKind;

    #[test]
    fn expected_signature_int_const() {
        let (inputs, outputs) = expected_signature(&NodeKind::IntConst(42));
        assert_eq!(inputs, vec![]);
        assert_eq!(outputs, vec![ExpectedOutputKind::AnyInt]);
    }

    #[test]
    fn expected_signature_entry() {
        let (inputs, outputs) = expected_signature(&NodeKind::Entry);
        assert_eq!(inputs, vec![]);
        assert_eq!(outputs, vec![ExpectedOutputKind::Control]);
    }

    #[test]
    fn expected_signature_if() {
        let (inputs, outputs) = expected_signature(&NodeKind::If);
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
        let (inputs, outputs) = expected_signature(&NodeKind::InitialMemory);
        assert_eq!(inputs, vec![]);
        assert_eq!(outputs, vec![ExpectedOutputKind::Memory]);
    }

    #[test]
    fn expected_signature_load() {
        let space = rsleigh::VnSpace::RAM;
        let (inputs, outputs) = expected_signature(&NodeKind::Load(space));
        assert_eq!(
            inputs,
            vec![ExpectedOutputKind::Memory, ExpectedOutputKind::AnyInt]
        );
        assert_eq!(outputs, vec![ExpectedOutputKind::AnyInt]);
    }

    #[test]
    fn expected_signature_store() {
        let space = rsleigh::VnSpace::RAM;
        let (inputs, outputs) = expected_signature(&NodeKind::Store(space));
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
        let (inputs, outputs) = expected_signature(&NodeKind::StackStore { space, offset: -4 });
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
        let (inputs, outputs) = expected_signature(&NodeKind::StackStorePhi { space });
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
        // Return's fixed prefix is just [Control]; the return values are variadic.
        let (inputs, outputs) = expected_signature(&NodeKind::Return);
        assert_eq!(inputs, vec![ExpectedOutputKind::Control]);
        assert_eq!(outputs, vec![]);
    }

    #[test]
    fn expected_signature_call() {
        // Call's fixed prefix is [Control, Memory, call_address]; args are variadic.
        let (inputs, outputs) = expected_signature(&NodeKind::Call);
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
        let (inputs, outputs) = expected_signature(&NodeKind::IntBinaryOp(IntBinaryOp::Add));
        assert_eq!(
            inputs,
            vec![ExpectedOutputKind::AnyInt, ExpectedOutputKind::AnyInt]
        );
        assert_eq!(outputs, vec![ExpectedOutputKind::AnyInt]);
    }

    #[test]
    fn expected_signature_int_cmp_op() {
        use crate::ops::IntCmpOp;
        let (inputs, outputs) = expected_signature(&NodeKind::IntCmpOp(IntCmpOp::Equal));
        assert_eq!(
            inputs,
            vec![ExpectedOutputKind::AnyInt, ExpectedOutputKind::AnyInt]
        );
        assert_eq!(outputs, vec![ExpectedOutputKind::Bool]);
    }

    #[test]
    fn expected_signature_bool_const() {
        let (inputs, outputs) = expected_signature(&NodeKind::BoolConst(true));
        assert_eq!(inputs, vec![]);
        assert_eq!(outputs, vec![ExpectedOutputKind::Bool]);
    }

    #[test]
    fn expected_signature_cast_to_bool() {
        let (inputs, outputs) = expected_signature(&NodeKind::CastToBool);
        assert_eq!(inputs, vec![ExpectedOutputKind::AnyValue]);
        assert_eq!(outputs, vec![ExpectedOutputKind::Bool]);
    }

    #[test]
    fn expected_signature_control_state() {
        // ControlState has variadic Control inputs (one per predecessor), so the
        // fixed prefix is empty.  Outputs are [Control, ControlPhi].
        let (inputs, outputs) = expected_signature(&NodeKind::ControlState);
        assert_eq!(inputs, vec![]);
        assert_eq!(
            outputs,
            vec![ExpectedOutputKind::Control, ExpectedOutputKind::ControlPhi]
        );
    }

    #[test]
    fn expected_signature_mem_phi() {
        let (inputs, outputs) = expected_signature(&NodeKind::MemPhi);
        assert_eq!(inputs, vec![ExpectedOutputKind::ControlPhi]);
        assert_eq!(outputs, vec![ExpectedOutputKind::Memory]);
    }

    #[test]
    fn expected_signature_float_const() {
        let (inputs, outputs) = expected_signature(&NodeKind::FloatConst(0));
        assert_eq!(inputs, vec![]);
        assert_eq!(outputs, vec![ExpectedOutputKind::AnyFloat]);
    }

    #[test]
    fn expected_signature_float_binary_op() {
        use crate::ops::FloatBinaryOp;
        let (inputs, outputs) = expected_signature(&NodeKind::FloatBinaryOp(FloatBinaryOp::Add));
        assert_eq!(
            inputs,
            vec![ExpectedOutputKind::AnyFloat, ExpectedOutputKind::AnyFloat]
        );
        assert_eq!(outputs, vec![ExpectedOutputKind::AnyFloat]);
    }
}
