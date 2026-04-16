//! Expected input/output signatures of every [`NodeKind`] variant.
//!
//! This module is the single source of truth for what input and output
//! [`NodeOutputKind`]s a given [`NodeKind`] is expected to have.  It is used
//! both by the (planned) `node_view()` reconstruction and by the whole-graph
//! validator (see `validate` module, to be added).
//!
//! Variable-arity nodes ([`NodeKind::ControlState`], [`NodeKind::MemPhi`],
//! [`NodeKind::ControlPhi`], [`NodeKind::Call`], [`NodeKind::CallOther`],
//! [`NodeKind::Return`], [`NodeKind::CPoolRef`], [`NodeKind::New`]) return
//! only the **minimum fixed prefix** of inputs / outputs.  Callers that need
//! to validate the variadic tail must do it themselves.

use crate::node::{NodeKind, NodeOutputKind, NodeOutputType};

/// Expected (input kinds, output kinds) for a given [`NodeKind`].
///
/// Integer-typed slots are reported as `OutputType(U64)` as a placeholder —
/// this helper only distinguishes the coarse [`NodeOutputKind`] (Control /
/// Memory / ControlPhi / OutputType) and does NOT carry the concrete
/// integer width.  Width-level checks live elsewhere.
///
/// For variable-arity kinds, the returned vectors describe only the fixed
/// prefix of the signature; see the module-level docs.
#[allow(dead_code)] // Used by the validator added in a later task.
pub(crate) fn expected_signature(
    kind: &NodeKind,
) -> (Vec<NodeOutputKind>, Vec<NodeOutputKind>) {
    use NodeOutputKind::*;
    use NodeOutputType::*;

    match kind {
        // ── Initial state ───────────────────────────────────────────────────
        NodeKind::Entry => (vec![], vec![Control]),
        NodeKind::InitialMemory => (vec![], vec![Memory]),
        NodeKind::InitialVar(_) => (vec![], vec![OutputType(U64)]),

        // ── Region / join nodes (variadic inputs) ───────────────────────────
        // ControlState: one Control input per predecessor (variadic).
        NodeKind::ControlState => (vec![], vec![Control, ControlPhi]),
        // MemPhi: [phi_token, ...per-predecessor Memory tokens].
        NodeKind::MemPhi => (vec![ControlPhi], vec![Memory]),
        // ControlPhi: [phi_token, ...per-predecessor values].
        NodeKind::ControlPhi(_) => (vec![ControlPhi], vec![OutputType(U64)]),

        // ── Conditional branch ──────────────────────────────────────────────
        NodeKind::If => (
            vec![Control, OutputType(Bool)],
            vec![Control, Control],
        ),
        NodeKind::IfCase(_) => (vec![Control], vec![Control]),

        // ── Calls and returns ───────────────────────────────────────────────
        // Call: [control, memory, call_address, ...args].
        // Outputs: [Control, Memory, ...clobbered varnode values].
        NodeKind::Call => (
            vec![Control, Memory, OutputType(U64)],
            vec![Control, Memory],
        ),
        NodeKind::PostCallMemState => (vec![Control], vec![Memory]),
        NodeKind::PostCallVarState(_) => (vec![Control], vec![OutputType(U64)]),
        // Return: [control, ...return values]. No memory input.
        NodeKind::Return => (vec![Control], vec![]),

        // ── Memory operations ───────────────────────────────────────────────
        NodeKind::Load(_) => (
            vec![Memory, OutputType(U64)],
            vec![OutputType(U64)],
        ),
        NodeKind::Store(_) => (
            vec![Memory, OutputType(U64), OutputType(U64)],
            vec![Memory],
        ),
        // StackStore: [memory, base, data].
        NodeKind::StackStore { .. } => (
            vec![Memory, OutputType(U64), OutputType(U64)],
            vec![Memory],
        ),
        // StackStorePhi: [phi_token, memory, data].
        NodeKind::StackStorePhi { .. } => (
            vec![ControlPhi, Memory, OutputType(U64)],
            vec![Memory],
        ),

        // ── Integer constants and operations ────────────────────────────────
        NodeKind::IntConst(_) => (vec![], vec![OutputType(U64)]),
        NodeKind::IntUnaryOp(_) => (vec![OutputType(U64)], vec![OutputType(U64)]),
        NodeKind::IntBinaryOp(_) => (
            vec![OutputType(U64), OutputType(U64)],
            vec![OutputType(U64)],
        ),
        NodeKind::IntCmpOp(_) => (
            vec![OutputType(U64), OutputType(U64)],
            vec![OutputType(Bool)],
        ),
        NodeKind::CastToInt => (vec![OutputType(Bool)], vec![OutputType(U64)]),
        NodeKind::Truncate => (vec![OutputType(U64)], vec![OutputType(U64)]),
        NodeKind::Popcount => (vec![OutputType(U64)], vec![OutputType(U64)]),
        NodeKind::Lzcount => (vec![OutputType(U64)], vec![OutputType(U64)]),
        NodeKind::Piece => (
            vec![OutputType(U64), OutputType(U64)],
            vec![OutputType(U64)],
        ),
        NodeKind::Extract { .. } => (vec![OutputType(U64)], vec![OutputType(U64)]),
        NodeKind::Insert { .. } => (
            vec![OutputType(U64), OutputType(U64)],
            vec![OutputType(U64)],
        ),
        NodeKind::Extend(_) => (vec![OutputType(U64)], vec![OutputType(U64)]),

        // ── Boolean constants and operations ────────────────────────────────
        NodeKind::BoolConst(_) => (vec![], vec![OutputType(Bool)]),
        NodeKind::BoolUnaryOp(_) => (vec![OutputType(Bool)], vec![OutputType(Bool)]),
        NodeKind::BoolBinaryOp(_) => (
            vec![OutputType(Bool), OutputType(Bool)],
            vec![OutputType(Bool)],
        ),
        NodeKind::CastToBool => (vec![OutputType(U64)], vec![OutputType(Bool)]),

        // ── Float constants and operations ──────────────────────────────────
        NodeKind::FloatConst(_) => (vec![], vec![OutputType(U64)]),
        NodeKind::FloatBinaryOp(_) => (
            vec![OutputType(U64), OutputType(U64)],
            vec![OutputType(U64)],
        ),
        NodeKind::FloatUnaryOp(_) => (vec![OutputType(U64)], vec![OutputType(U64)]),
        NodeKind::FloatCmpOp(_) => (
            vec![OutputType(U64), OutputType(U64)],
            vec![OutputType(Bool)],
        ),
        NodeKind::FloatIsNan => (vec![OutputType(U64)], vec![OutputType(Bool)]),
        NodeKind::IntToFloat => (vec![OutputType(U64)], vec![OutputType(U64)]),
        NodeKind::FloatToInt => (vec![OutputType(U64)], vec![OutputType(U64)]),
        NodeKind::FloatToFloat => (vec![OutputType(U64)], vec![OutputType(U64)]),
        NodeKind::IntBitsToFloat => (vec![OutputType(U64)], vec![OutputType(U64)]),
        NodeKind::FloatBitsToInt => (vec![OutputType(U64)], vec![OutputType(U64)]),
        NodeKind::CastToFloat => (vec![OutputType(U64)], vec![OutputType(U64)]),

        // ── User-defined / opaque opcodes ───────────────────────────────────
        // CallOther: [control, memory, ...args].
        // Outputs: [Control, Memory] or [Control, Memory, OutputType].
        NodeKind::CallOther { .. } => (
            vec![Control, Memory],
            vec![Control, Memory],
        ),
        NodeKind::SegmentOp { .. } => (
            vec![OutputType(U64), OutputType(U64)],
            vec![OutputType(U64)],
        ),
        // CPoolRef: [...refs] (variadic).
        NodeKind::CPoolRef => (vec![], vec![OutputType(U64)]),
        // New: [...args] (variadic, typically a size).
        NodeKind::New => (vec![], vec![OutputType(U64)]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{NodeKind, NodeOutputKind, NodeOutputType};

    #[test]
    fn expected_signature_int_const() {
        let (inputs, outputs) = expected_signature(&NodeKind::IntConst(42));
        assert_eq!(inputs, vec![]);
        assert_eq!(outputs, vec![NodeOutputKind::OutputType(NodeOutputType::U64)]);
    }

    #[test]
    fn expected_signature_entry() {
        let (inputs, outputs) = expected_signature(&NodeKind::Entry);
        assert_eq!(inputs, vec![]);
        assert_eq!(outputs, vec![NodeOutputKind::Control]);
    }

    #[test]
    fn expected_signature_if() {
        let (inputs, outputs) = expected_signature(&NodeKind::If);
        assert_eq!(
            inputs,
            vec![
                NodeOutputKind::Control,
                NodeOutputKind::OutputType(NodeOutputType::Bool),
            ]
        );
        assert_eq!(
            outputs,
            vec![NodeOutputKind::Control, NodeOutputKind::Control]
        );
    }

    #[test]
    fn expected_signature_initial_memory() {
        let (inputs, outputs) = expected_signature(&NodeKind::InitialMemory);
        assert_eq!(inputs, vec![]);
        assert_eq!(outputs, vec![NodeOutputKind::Memory]);
    }

    #[test]
    fn expected_signature_load() {
        let space = rsleigh::VnSpace::RAM;
        let (inputs, outputs) = expected_signature(&NodeKind::Load(space));
        assert_eq!(
            inputs,
            vec![
                NodeOutputKind::Memory,
                NodeOutputKind::OutputType(NodeOutputType::U64),
            ]
        );
        assert_eq!(
            outputs,
            vec![NodeOutputKind::OutputType(NodeOutputType::U64)]
        );
    }

    #[test]
    fn expected_signature_store() {
        let space = rsleigh::VnSpace::RAM;
        let (inputs, outputs) = expected_signature(&NodeKind::Store(space));
        assert_eq!(
            inputs,
            vec![
                NodeOutputKind::Memory,
                NodeOutputKind::OutputType(NodeOutputType::U64),
                NodeOutputKind::OutputType(NodeOutputType::U64),
            ]
        );
        assert_eq!(outputs, vec![NodeOutputKind::Memory]);
    }

    #[test]
    fn expected_signature_stack_store() {
        let space = rsleigh::VnSpace::RAM;
        let (inputs, outputs) =
            expected_signature(&NodeKind::StackStore { space, offset: -4 });
        assert_eq!(
            inputs,
            vec![
                NodeOutputKind::Memory,
                NodeOutputKind::OutputType(NodeOutputType::U64),
                NodeOutputKind::OutputType(NodeOutputType::U64),
            ]
        );
        assert_eq!(outputs, vec![NodeOutputKind::Memory]);
    }

    #[test]
    fn expected_signature_stack_store_phi() {
        let space = rsleigh::VnSpace::RAM;
        let (inputs, outputs) = expected_signature(&NodeKind::StackStorePhi { space });
        assert_eq!(
            inputs,
            vec![
                NodeOutputKind::ControlPhi,
                NodeOutputKind::Memory,
                NodeOutputKind::OutputType(NodeOutputType::U64),
            ]
        );
        assert_eq!(outputs, vec![NodeOutputKind::Memory]);
    }

    #[test]
    fn expected_signature_return() {
        // Return's fixed prefix is just [Control]; the return values are variadic.
        let (inputs, outputs) = expected_signature(&NodeKind::Return);
        assert_eq!(inputs, vec![NodeOutputKind::Control]);
        assert_eq!(outputs, vec![]);
    }

    #[test]
    fn expected_signature_call() {
        // Call's fixed prefix is [Control, Memory, call_address]; args are variadic.
        let (inputs, outputs) = expected_signature(&NodeKind::Call);
        assert_eq!(
            inputs,
            vec![
                NodeOutputKind::Control,
                NodeOutputKind::Memory,
                NodeOutputKind::OutputType(NodeOutputType::U64),
            ]
        );
        assert_eq!(
            outputs,
            vec![NodeOutputKind::Control, NodeOutputKind::Memory]
        );
    }

    #[test]
    fn expected_signature_int_binary_op() {
        use crate::ops::IntBinaryOp;
        let (inputs, outputs) =
            expected_signature(&NodeKind::IntBinaryOp(IntBinaryOp::Add));
        assert_eq!(
            inputs,
            vec![
                NodeOutputKind::OutputType(NodeOutputType::U64),
                NodeOutputKind::OutputType(NodeOutputType::U64),
            ]
        );
        assert_eq!(
            outputs,
            vec![NodeOutputKind::OutputType(NodeOutputType::U64)]
        );
    }

    #[test]
    fn expected_signature_int_cmp_op() {
        use crate::ops::IntCmpOp;
        let (inputs, outputs) =
            expected_signature(&NodeKind::IntCmpOp(IntCmpOp::Equal));
        assert_eq!(
            inputs,
            vec![
                NodeOutputKind::OutputType(NodeOutputType::U64),
                NodeOutputKind::OutputType(NodeOutputType::U64),
            ]
        );
        assert_eq!(
            outputs,
            vec![NodeOutputKind::OutputType(NodeOutputType::Bool)]
        );
    }

    #[test]
    fn expected_signature_bool_const() {
        let (inputs, outputs) = expected_signature(&NodeKind::BoolConst(true));
        assert_eq!(inputs, vec![]);
        assert_eq!(
            outputs,
            vec![NodeOutputKind::OutputType(NodeOutputType::Bool)]
        );
    }

    #[test]
    fn expected_signature_cast_to_bool() {
        let (inputs, outputs) = expected_signature(&NodeKind::CastToBool);
        assert_eq!(
            inputs,
            vec![NodeOutputKind::OutputType(NodeOutputType::U64)]
        );
        assert_eq!(
            outputs,
            vec![NodeOutputKind::OutputType(NodeOutputType::Bool)]
        );
    }

    #[test]
    fn expected_signature_control_state() {
        // ControlState has variadic Control inputs (one per predecessor), so the
        // fixed prefix is empty.  Outputs are [Control, ControlPhi].
        let (inputs, outputs) = expected_signature(&NodeKind::ControlState);
        assert_eq!(inputs, vec![]);
        assert_eq!(
            outputs,
            vec![NodeOutputKind::Control, NodeOutputKind::ControlPhi]
        );
    }

    #[test]
    fn expected_signature_mem_phi() {
        let (inputs, outputs) = expected_signature(&NodeKind::MemPhi);
        assert_eq!(inputs, vec![NodeOutputKind::ControlPhi]);
        assert_eq!(outputs, vec![NodeOutputKind::Memory]);
    }
}
