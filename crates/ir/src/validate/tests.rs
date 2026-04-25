use super::*;
use crate::node::{NodeKind, NodeOutputKind, NodeOutputType};

#[test]
fn empty_graph_with_entry_only() {
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let _mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    assert!(validate(&graph, entry).is_ok());
}

#[test]
fn layer_a_wrong_input_kind_on_int_unary_op() {
    use crate::node::NodeOutputType;
    use crate::ops::IntUnaryOp;

    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let _mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);

    // IntUnaryOp expects an OutputType input, but we feed it a Control output.
    let control_out = graph.node_outputs(entry).into_iter().next().unwrap();
    let _bad = graph.create_node(
        NodeKind::IntUnaryOp(IntUnaryOp::Neg),
        [control_out],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );

    let errs = validate(&graph, entry).unwrap_err();
    assert!(
        errs.0.iter().any(|e| matches!(
            e,
            ValidationError::NodeInputKindMismatch { input_idx: 0, .. }
        )),
        "expected a NodeInputKindMismatch, got: {errs:?}"
    );
}

#[test]
fn layer_a_wrong_output_kind() {
    let mut graph = Graph::new();
    // Entry should produce Control, we make it produce Memory instead.
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Memory]);
    let _mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);

    let errs = validate(&graph, entry).unwrap_err();
    assert!(
        errs.0.iter().any(|e| matches!(
            e,
            ValidationError::NodeOutputKindMismatch { output_idx: 0, .. }
        )),
        "got: {errs:?}"
    );
}

#[test]
fn layer_b_input_missing_from_use_list() {
    use crate::node::NodeOutputType;
    use crate::ops::IntUnaryOp;

    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let _mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);

    let c = graph.create_node(
        NodeKind::IntConst(3),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let c_out = graph.node_outputs(c).into_iter().next().unwrap();

    let _neg = graph.create_node(
        NodeKind::IntUnaryOp(IntUnaryOp::Neg),
        [c_out],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );

    // Corrupt the forward link: clear the IntConst output's head-of-use
    // pointer.  The op's input is still recorded, but the producer no
    // longer admits it as a consumer.
    graph.test_only_clear_first_use(c_out);

    let errs = validate(&graph, entry).unwrap_err();
    assert!(
        errs.0.iter().any(|e| matches!(
            e,
            ValidationError::InputMissingFromUseList { input_idx: 0, .. }
        )),
        "expected InputMissingFromUseList, got: {errs:?}"
    );
}

#[test]
fn layer_b_stale_input_in_use_list() {
    use crate::node::NodeOutputType;
    use crate::ops::IntUnaryOp;

    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let _mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);

    let a = graph.create_node(
        NodeKind::IntConst(1),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let a_out = graph.node_outputs(a).into_iter().next().unwrap();

    let b = graph.create_node(
        NodeKind::IntConst(2),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let b_out = graph.node_outputs(b).into_iter().next().unwrap();

    let neg = graph.create_node(
        NodeKind::IntUnaryOp(IntUnaryOp::Neg),
        [a_out],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );

    // Retarget the op's input at idx 0 to `b_out` without updating any
    // use-list.  `a_out`'s use-list still references this input, but the
    // input itself now points at `b_out` — that's a stale entry.
    let input_id = graph.node_input_id_at(neg, 0);
    graph.test_only_retarget_input(input_id, b_out);

    let errs = validate(&graph, entry).unwrap_err();
    assert!(
        errs.0
            .iter()
            .any(|e| matches!(e, ValidationError::UseListContainsStaleInput { .. })),
        "expected UseListContainsStaleInput, got: {errs:?}"
    );
}

#[test]
fn layer_c_missing_initial_memory() {
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);

    let errs = validate(&graph, entry).unwrap_err();
    assert!(
        errs.0
            .iter()
            .any(|e| matches!(e, ValidationError::MissingInitialMemoryNode)),
        "expected MissingInitialMemoryNode, got: {errs:?}"
    );
}

#[test]
fn layer_c_duplicate_entry() {
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let _entry2 = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let _mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);

    let errs = validate(&graph, entry).unwrap_err();
    assert!(
        errs.0
            .iter()
            .any(|e| matches!(e, ValidationError::MultipleEntryNodes { .. })),
        "expected MultipleEntryNodes, got: {errs:?}"
    );
}

#[test]
fn layer_c_duplicate_initial_memory() {
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let _mem1 = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let _mem2 = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);

    let errs = validate(&graph, entry).unwrap_err();
    assert!(
        errs.0
            .iter()
            .any(|e| matches!(e, ValidationError::MultipleInitialMemoryNodes { .. })),
        "expected MultipleInitialMemoryNodes, got: {errs:?}"
    );
}

#[test]
fn layer_c_control_state_bad_predecessor() {
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let mem_out = graph.node_outputs(mem).into_iter().next().unwrap();

    // ControlState with a Memory predecessor instead of Control.
    let _bad_cs = graph.create_node(
        NodeKind::ControlState,
        [mem_out],
        [NodeOutputKind::Control, NodeOutputKind::ControlPhi],
    );

    let errs = validate(&graph, entry).unwrap_err();
    assert!(
        errs.0.iter().any(|e| matches!(
            e,
            ValidationError::ControlStateNonControlPredecessor { input_idx: 0, .. }
        )),
        "got: {errs:?}"
    );
}

fn test_vn() -> rsleigh::Vn {
    rsleigh::Vn {
        addr: rsleigh::VnAddr {
            space: rsleigh::VnSpace::REGISTER,
            off: 0x20,
        },
        size: 4,
    }
}

#[test]
fn layer_c_phi_token_from_wrong_node() {
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let _mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let entry_out = graph.node_outputs(entry).into_iter().next().unwrap();
    let cs = graph.create_node(
        NodeKind::ControlState,
        [entry_out],
        [NodeOutputKind::Control, NodeOutputKind::ControlPhi],
    );
    let cs_control_out = graph.node_outputs(cs).into_iter().next().unwrap(); // index 0 = Control
    let vn = test_vn();
    let _phi = graph.create_node(
        NodeKind::ControlPhi(vn),
        [cs_control_out],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );

    let errs = validate(&graph, entry).unwrap_err();
    assert!(
        errs.0
            .iter()
            .any(|e| matches!(e, ValidationError::PhiTokenNotFromControlState { .. })),
        "got: {errs:?}"
    );
}

#[test]
fn layer_c_phi_value_arity_mismatch() {
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let _mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let entry_out = graph.node_outputs(entry).into_iter().next().unwrap();

    let cs = graph.create_node(
        NodeKind::ControlState,
        [entry_out],
        [NodeOutputKind::Control, NodeOutputKind::ControlPhi],
    );
    let cs_phi_out = graph.node_outputs(cs).into_iter().nth(1).unwrap();

    let c1 = graph.create_node(
        NodeKind::IntConst(1),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let c2 = graph.create_node(
        NodeKind::IntConst(2),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let c1_out = graph.node_outputs(c1).into_iter().next().unwrap();
    let c2_out = graph.node_outputs(c2).into_iter().next().unwrap();
    let vn = test_vn();
    let _phi = graph.create_node(
        NodeKind::ControlPhi(vn),
        [cs_phi_out, c1_out, c2_out],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );

    let errs = validate(&graph, entry).unwrap_err();
    assert!(
        errs.0.iter().any(|e| matches!(
            e,
            ValidationError::PhiValueArityMismatch {
                expected_predecessors: 1,
                actual_values: 2,
                ..
            }
        )),
        "got: {errs:?}"
    );
}

#[test]
fn layer_c_stack_store_phi_does_not_fire_arity_mismatch() {
    // StackStorePhi has fixed arity [token, memory, data] regardless of
    // how many predecessors the owning ControlState has.  The
    // per-predecessor arity rule that applies to ControlPhi/MemPhi must
    // not fire on it.  Here the owning ControlState has 1 predecessor;
    // before the fix this produced a spurious
    // PhiValueArityMismatch { expected_predecessors: 1, actual_values: 2 }.
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let entry_out = graph.node_outputs(entry).into_iter().next().unwrap();
    let mem_out = graph.node_outputs(mem).into_iter().next().unwrap();

    let cs = graph.create_node(
        NodeKind::ControlState,
        [entry_out],
        [NodeOutputKind::Control, NodeOutputKind::ControlPhi],
    );
    let cs_phi_out = graph.node_outputs(cs).into_iter().nth(1).unwrap();

    let data = graph.create_node(
        NodeKind::IntConst(0),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let data_out = graph.node_outputs(data).into_iter().next().unwrap();

    let _ssp = graph.create_node(
        NodeKind::StackStorePhi {
            space: rsleigh::VnSpace::RAM,
        },
        [cs_phi_out, mem_out, data_out],
        [NodeOutputKind::Memory],
    );

    let res = validate(&graph, entry);
    if let Err(errs) = &res {
        assert!(
            !errs
                .0
                .iter()
                .any(|e| matches!(e, ValidationError::PhiValueArityMismatch { .. })),
            "StackStorePhi must not trigger PhiValueArityMismatch; got: {errs:?}"
        );
    }
}

#[test]
fn layer_c_postcall_mem_state_from_entry_is_error() {
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let _mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let entry_ctrl = graph.node_outputs(entry).into_iter().next().unwrap();

    let _bad = graph.create_node(
        NodeKind::PostCallMemState,
        [entry_ctrl],
        [NodeOutputKind::Memory],
    );

    let errs = validate(&graph, entry).unwrap_err();
    assert!(
        errs.0
            .iter()
            .any(|e| matches!(e, ValidationError::PostCallMemStateNotAfterCall { .. })),
        "got: {errs:?}"
    );
}

#[test]
fn layer_c_postcall_var_state_from_entry_is_error() {
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let _mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let entry_ctrl = graph.node_outputs(entry).into_iter().next().unwrap();

    let vn = test_vn();
    let _bad = graph.create_node(
        NodeKind::PostCallVarState(vn),
        [entry_ctrl],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );

    let errs = validate(&graph, entry).unwrap_err();
    assert!(
        errs.0
            .iter()
            .any(|e| matches!(e, ValidationError::PostCallVarStateNotAfterCall { .. })),
        "got: {errs:?}"
    );
}

// Helpers for constructing a Call node and its outputs.
fn make_call(
    graph: &mut Graph,
    ctrl: NodeOutputId,
    mem: NodeOutputId,
) -> (NodeId, NodeOutputId) {
    let addr = graph.create_node(
        NodeKind::IntConst(0x1000),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let addr_out = graph.node_outputs(addr).into_iter().next().unwrap();
    let call = graph.create_node(
        NodeKind::Call,
        [ctrl, mem, addr_out],
        [NodeOutputKind::Control, NodeOutputKind::Memory],
    );
    let call_ctrl = graph.node_outputs(call).into_iter().next().unwrap();
    (call, call_ctrl)
}

#[test]
fn layer_c_two_postcall_mem_states_on_same_call() {
    // To get two distinct PostCallMemState nodes consuming the same Call
    // ctrl, we create one PostCallMemState for call1 and one for call2,
    // then retarget the second node's input to call1's ctrl.  This
    // corrupts the use-list (Layer B also fires), but the test only checks
    // that DuplicatePostCallMemState is among the errors.
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let mem_node = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let entry_ctrl = graph.node_outputs(entry).into_iter().next().unwrap();
    let mem_out = graph.node_outputs(mem_node).into_iter().next().unwrap();

    let (_, call1_ctrl) = make_call(&mut graph, entry_ctrl, mem_out);
    // call2 needs a different ctrl — reuse entry_ctrl (Layer A may object,
    // but we just need two distinct Call nodes to get two distinct
    // PostCallMemState NodeIds).
    let (_, call2_ctrl) = make_call(&mut graph, entry_ctrl, mem_out);

    let _pcm1 = graph.create_node(
        NodeKind::PostCallMemState,
        [call1_ctrl],
        [NodeOutputKind::Memory],
    );
    let pcm2 = graph.create_node(
        NodeKind::PostCallMemState,
        [call2_ctrl],
        [NodeOutputKind::Memory],
    );

    // Retarget pcm2's input to call1_ctrl so call1 now has two consumers.
    // (Use test_only_retarget_input so the use-list is not updated — that
    // corruption is intentional; we want to verify the uniqueness check.)
    let pcm2_input = graph.node_input_id_at(pcm2, 0);
    graph.test_only_retarget_input(pcm2_input, call1_ctrl);

    let errs = validate(&graph, entry).unwrap_err();
    assert!(
        errs.0
            .iter()
            .any(|e| matches!(e, ValidationError::DuplicatePostCallMemState { .. })),
        "got: {errs:?}"
    );
}

#[test]
fn layer_c_two_postcall_var_states_same_vn() {
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let mem_node = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let entry_ctrl = graph.node_outputs(entry).into_iter().next().unwrap();
    let mem_out = graph.node_outputs(mem_node).into_iter().next().unwrap();

    let (_, call1_ctrl) = make_call(&mut graph, entry_ctrl, mem_out);
    let (_, call2_ctrl) = make_call(&mut graph, entry_ctrl, mem_out);

    let vn = test_vn();
    let v1 = graph.create_node(
        NodeKind::PostCallVarState(vn),
        [call1_ctrl],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let v2 = graph.create_node(
        NodeKind::PostCallVarState(vn),
        [call2_ctrl],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );

    // Retarget v2's input to call1_ctrl — same vn, same call, two nodes.
    let v2_input = graph.node_input_id_at(v2, 0);
    graph.test_only_retarget_input(v2_input, call1_ctrl);

    let errs = validate(&graph, entry).unwrap_err();
    assert!(
        errs.0
            .iter()
            .any(|e| matches!(e, ValidationError::DuplicatePostCallVarState { .. })),
        "got: {errs:?}"
    );

    // v1 still consumes call1 ctrl legitimately.
    let _ = v1;
}

#[test]
fn layer_a_wrong_input_count() {
    use crate::node::NodeOutputType;
    use crate::ops::IntBinaryOp;

    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let _mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let entry_ctrl = graph.node_outputs(entry).into_iter().next().unwrap();
    let c = graph.create_node(
        NodeKind::IntConst(5),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let c_out = graph.node_outputs(c).into_iter().next().unwrap();

    // IntBinaryOp expects 2 inputs; give it 1.
    let bad = graph.create_node(
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        [c_out],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let bad_out = graph.node_outputs(bad).into_iter().next().unwrap();

    // Wire `bad` into the reachable sub-graph so the reachability-scoped
    // Layer A actually inspects it.  A Return consuming entry's Control
    // plus `bad`'s value output is the smallest reachable shape.
    let _ret = graph.create_node(NodeKind::Return, [entry_ctrl, bad_out], []);

    let errs = validate(&graph, entry).unwrap_err();
    assert!(
        errs.0.iter().any(|e| matches!(
            e,
            ValidationError::NodeInputCountMismatch {
                expected: 2,
                actual: 1,
                ..
            }
        )),
        "got: {errs:?}"
    );
}

/// Layer C: two `FunctionArg` nodes sharing the same `index` are a
/// construction/optimization bug — the validator must catch it.
#[test]
fn layer_c_duplicate_function_arg_index_detected() {
    use crate::node::FunctionArgSource;

    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let _mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);

    let reg = rsleigh::Vn {
        addr: rsleigh::VnAddr {
            space: rsleigh::VnSpace::REGISTER,
            off: 0x38,
        },
        size: 8,
    };
    let _a = graph.create_node(
        NodeKind::FunctionArg {
            source: FunctionArgSource::Register(reg),
            index: 0,
        },
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let _b = graph.create_node(
        NodeKind::FunctionArg {
            source: FunctionArgSource::Register(reg),
            index: 0,
        },
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );

    let errs = validate(&graph, entry).unwrap_err();
    assert!(
        errs.0.iter().any(|e| matches!(
            e,
            ValidationError::DuplicateFunctionArg { index: 0, .. }
        )),
        "expected DuplicateFunctionArg, got: {errs:?}"
    );
}

/// Regression: Layer A must check variadic input tails, not just the fixed
/// head prefix. A `MemPhi` whose per-predecessor inputs are not Memory
/// (e.g. a Control token leaks through) used to slip past validation
/// because the variadic-tail kind check was elided.
#[test]
fn layer_a_mem_phi_variadic_tail_must_be_memory() {
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let entry_ctrl = graph.node_outputs(entry).into_iter().next().unwrap();
    let init_mem = graph.node_outputs(mem).into_iter().next().unwrap();

    // ControlState with one valid Control predecessor (entry).
    let cs = graph.create_node(
        NodeKind::ControlState,
        [entry_ctrl],
        [NodeOutputKind::Control, NodeOutputKind::ControlPhi],
    );
    let cs_outputs: Vec<_> = graph.node_outputs(cs).into_iter().collect();
    let cs_ctrl = cs_outputs[0];
    let cs_phi_token = cs_outputs[1];

    // MemPhi with: phi_token (correct PHI kind), then a Control output as
    // its variadic predecessor (WRONG — should be Memory).
    let bad_mem_phi = graph.create_node(
        NodeKind::MemPhi,
        [cs_phi_token, entry_ctrl],
        [NodeOutputKind::Memory],
    );
    let bad_mem_out = graph.node_outputs(bad_mem_phi).into_iter().next().unwrap();
    let _ = init_mem; // unused but kept to satisfy InitialMemory uniqueness

    // Reach the MemPhi via a Return so Layer A walks to it.
    graph.create_node(NodeKind::Return, [cs_ctrl, bad_mem_out], []);

    let errs = validate(&graph, entry).unwrap_err();
    assert!(
        errs.0.iter().any(|e| matches!(
            e,
            ValidationError::NodeInputKindMismatch { input_idx: 1, .. }
        )),
        "expected NodeInputKindMismatch on MemPhi input[1], got: {errs:?}"
    );
}
