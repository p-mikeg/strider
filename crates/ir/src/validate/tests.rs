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
        NodeKind::IntUnaryOp(IntUnaryOp::BitNot),
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
        NodeKind::IntUnaryOp(IntUnaryOp::BitNot),
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
        NodeKind::IntUnaryOp(IntUnaryOp::BitNot),
        [a_out],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );

    // Retarget the op's input at idx 0 to `b_out` without updating any
    // use-list.  `a_out`'s use-list still references this input, but the
    // input itself now points at `b_out` — that's a stale entry.
    let input_id = graph.node_input_id_at(neg, 0).unwrap();
    graph.test_only_retarget_input(input_id, b_out);

    let errs = validate(&graph, entry).unwrap_err();
    assert!(
        errs.0
            .iter()
            .any(|e| matches!(e, ValidationError::UseListContainsStaleInput { .. })),
        "expected UseListContainsStaleInput, got: {errs:?}"
    );
}

/// Layer B's forward check must still flag missing-from-use-list cases
/// at non-zero input slots (covers the O(E) refactor — the existing
/// `layer_b_input_missing_from_use_list` only covers slot 0).
#[test]
fn layer_b_forward_check_catches_missing_at_non_zero_slot() {
    use crate::node::NodeOutputType;
    use crate::ops::IntBinaryOp;

    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let _mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);

    let a = graph.create_node(
        NodeKind::IntConst(11),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let a_out = graph.node_outputs(a).into_iter().next().unwrap();

    let b = graph.create_node(
        NodeKind::IntConst(13),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let b_out = graph.node_outputs(b).into_iter().next().unwrap();

    // Add(a, b) — a at slot 0, b at slot 1.
    let _add = graph.create_node(
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        [a_out, b_out],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );

    // Corrupt only b's use-list head, leaving a's intact.  Only the
    // slot-1 input should be flagged as missing.
    graph.test_only_clear_first_use(b_out);

    let errs = validate(&graph, entry).unwrap_err();
    let missing: Vec<_> = errs
        .0
        .iter()
        .filter_map(|e| match e {
            ValidationError::InputMissingFromUseList { input_idx, .. } => Some(*input_idx),
            _ => None,
        })
        .collect();
    assert_eq!(
        missing,
        vec![1],
        "only slot-1 input must be flagged; got: {errs:?}"
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
        [NodeOutputKind::Control, NodeOutputKind::PhiToken],
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
        addr_off: 0x20,
        addr_space: rsleigh::VnSpace::REGISTER,
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
        [NodeOutputKind::Control, NodeOutputKind::PhiToken],
    );
    let cs_control_out = graph.node_outputs(cs).into_iter().next().unwrap(); // index 0 = Control
    let vn = test_vn();
    let _phi = graph.create_node(
        NodeKind::VarPhi(vn),
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
        [NodeOutputKind::Control, NodeOutputKind::PhiToken],
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
        NodeKind::VarPhi(vn),
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
    // per-predecessor arity rule that applies to VarPhi/MemPhi must
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
        [NodeOutputKind::Control, NodeOutputKind::PhiToken],
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
        addr_off: 0x38,
        addr_space: rsleigh::VnSpace::REGISTER,
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
        [NodeOutputKind::Control, NodeOutputKind::PhiToken],
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

#[test]
fn layer_a_accepts_bool_value_phi_inputs() {
    // VarPhi / ValuePhi value inputs (the IN_PHI variadic tail) must
    // accept Bool-typed values: real binaries phi-merge x86 flag registers
    // (CF/ZF/SF), which the IR models as Bool. Same rationale as ARG/RET/CALL_OUT.
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let init_mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let entry_ctrl = graph.node_outputs(entry).into_iter().next().unwrap();
    let mem = graph.node_outputs(init_mem).into_iter().next().unwrap();

    let cs = graph.create_node(
        NodeKind::ControlState,
        [entry_ctrl],
        [NodeOutputKind::Control, NodeOutputKind::PhiToken],
    );
    let cs_ctrl = graph.node_outputs(cs).into_iter().next().unwrap();
    let phi_token = graph.node_outputs(cs).into_iter().nth(1).unwrap();

    let bc = graph.create_node(
        NodeKind::BoolConst(true),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::Bool)],
    );
    let bc_out = graph.node_outputs(bc).into_iter().next().unwrap();

    // ValuePhi taking [phi_token, bool_value] — the Bool flows through IN_PHI.
    let vp = graph.create_node(
        NodeKind::ValuePhi,
        [phi_token, bc_out],
        [NodeOutputKind::OutputType(NodeOutputType::Bool)],
    );
    let vp_out = graph.node_outputs(vp).into_iter().next().unwrap();

    // Use the phi'd value so the validator's reachability walk hits it.
    graph.create_node(NodeKind::Return, [cs_ctrl, mem, vp_out], []);

    validate(&graph, entry).expect("Bool-typed value phi inputs must validate");
}

#[test]
fn layer_c_mem_phi_arity_mismatch() {
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let init_mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let entry_out = graph.node_outputs(entry).into_iter().next().unwrap();
    let init_mem_out = graph.node_outputs(init_mem).into_iter().next().unwrap();

    let cs = graph.create_node(
        NodeKind::ControlState,
        [entry_out],
        [NodeOutputKind::Control, NodeOutputKind::PhiToken],
    );
    let cs_phi_out = graph.node_outputs(cs).into_iter().nth(1).unwrap();
    let cs_ctrl_out = graph.node_outputs(cs).into_iter().next().unwrap();

    // MemPhi with two memory inputs but the owning ControlState has one predecessor.
    let mem_phi = graph.create_node(
        NodeKind::MemPhi,
        [cs_phi_out, init_mem_out, init_mem_out],
        [NodeOutputKind::Memory],
    );
    let mem_phi_out = graph.node_outputs(mem_phi).into_iter().next().unwrap();
    graph.create_node(NodeKind::Return, [cs_ctrl_out, mem_phi_out], []);

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
fn layer_c_value_phi_arity_mismatch() {
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let init_mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let entry_out = graph.node_outputs(entry).into_iter().next().unwrap();
    let init_mem_out = graph.node_outputs(init_mem).into_iter().next().unwrap();

    let cs = graph.create_node(
        NodeKind::ControlState,
        [entry_out],
        [NodeOutputKind::Control, NodeOutputKind::PhiToken],
    );
    let cs_phi_out = graph.node_outputs(cs).into_iter().nth(1).unwrap();
    let cs_ctrl_out = graph.node_outputs(cs).into_iter().next().unwrap();

    let c1 = graph.create_node(
        NodeKind::IntConst(1),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let c1_out = graph.node_outputs(c1).into_iter().next().unwrap();

    // ValuePhi with two value inputs but the owning ControlState has one predecessor.
    let vp = graph.create_node(
        NodeKind::ValuePhi,
        [cs_phi_out, c1_out, c1_out],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let vp_out = graph.node_outputs(vp).into_iter().next().unwrap();
    graph.create_node(NodeKind::Return, [cs_ctrl_out, init_mem_out, vp_out], []);

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
fn layer_a_rejects_wrong_output_count() {
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let _mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    // IntConst expects exactly one output but we give it two.
    let bad = graph.create_node(
        NodeKind::IntConst(0),
        [],
        [
            NodeOutputKind::OutputType(NodeOutputType::U64),
            NodeOutputKind::OutputType(NodeOutputType::U64),
        ],
    );
    let bad_out0 = graph.node_outputs(bad).into_iter().next().unwrap();
    let entry_ctrl = graph.node_outputs(entry).into_iter().next().unwrap();
    let mem = graph.node_outputs(_mem).into_iter().next().unwrap();
    graph.create_node(NodeKind::Return, [entry_ctrl, mem, bad_out0], []);

    let errs = validate(&graph, entry).unwrap_err();
    assert!(
        errs.0.iter().any(|e|
            matches!(e, ValidationError::NodeOutputCountMismatch { node, expected: 1, actual: 2 } if *node == bad)
        ),
        "got: {errs:?}"
    );
}

#[test]
fn layer_c_rejects_control_state_with_zero_predecessors() {
    // ControlState has a variadic head_len of 0, so Layer A's count check
    // (>= 0) accepts zero inputs and Layer C's per-predecessor loop is a
    // no-op. Without an explicit check, a *reachable* zero-pred
    // ControlState slips through validation entirely.
    //
    // Walk semantics: graph_walk_succs follows forward-control + backward-data,
    // so we make the zero-pred ControlState reachable by having a downstream
    // Return consume *both* Entry's control (so walk reaches Return) and the
    // ControlState's control (so walking back from Return hits the CS).
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let init_mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let entry_ctrl = graph.node_outputs(entry).into_iter().next().unwrap();
    let mem = graph.node_outputs(init_mem).into_iter().next().unwrap();
    let cs = graph.create_node(
        NodeKind::ControlState,
        [],
        [NodeOutputKind::Control, NodeOutputKind::PhiToken],
    );
    let cs_ctrl = graph.node_outputs(cs).into_iter().next().unwrap();
    // Return consumes entry's control (reaches Return via cfg_succs of Entry)
    // and cs_ctrl as a "ret value" (reaches CS via Return's backward-data).
    graph.create_node(NodeKind::Return, [entry_ctrl, mem, cs_ctrl], []);

    let errs = validate(&graph, entry).unwrap_err();
    assert!(
        errs.0.iter().any(|e| matches!(
            e,
            ValidationError::EmptyControlStatePredecessors { control_state } if *control_state == cs
        )),
        "expected EmptyControlStatePredecessors, got: {errs:?}"
    );
}

#[test]
fn layer_c_tolerates_unreachable_zero_predecessor_control_state() {
    // Zombie ControlState with zero inputs left behind by RedundantPhis is
    // expected; the validator must not flag it (this happens routinely on
    // real binaries after dead-branch elimination).
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let init_mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let entry_ctrl = graph.node_outputs(entry).into_iter().next().unwrap();
    let mem = graph.node_outputs(init_mem).into_iter().next().unwrap();
    // Zombie CS that nothing references — not reachable from entry.
    let _zombie_cs = graph.create_node(
        NodeKind::ControlState,
        [],
        [NodeOutputKind::Control, NodeOutputKind::PhiToken],
    );
    graph.create_node(NodeKind::Return, [entry_ctrl, mem], []);

    validate(&graph, entry).expect("zombie ControlState must not trigger validation error");
}

/// IndirectBranch consumes (control, memory, target_value) and produces no
/// outputs; the validator must accept this exact shape.  IndirectBranch is
/// the lifter's placeholder for `RegionTerminator::UnresolvedIndirectBranch`
/// — it's mutated in-place by the indirect-branch resolver into a real
/// `Return` (LinkRegister) or replaced by a `Call+Return` pair (tail call).
#[test]
fn asm_fingerprint_check_off_by_default_accepts_empty_fingerprints() {
    // Opt-in is off → fully-empty fingerprints on a non-exempt node are OK.
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let _mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let _const_node = graph.create_node(
        NodeKind::IntConst(7),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    // The IntConst is unreachable from entry; default validate ignores it.
    validate(&graph, entry).expect("default validate is unaffected");
}

#[test]
fn asm_fingerprint_check_flags_reachable_non_exempt_empty() {
    // Opt-in is on → a reachable IntConst with no fingerprint is an error.
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let init_mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let entry_ctrl = graph.node_outputs(entry).into_iter().next().unwrap();
    let mem_out = graph.node_outputs(init_mem).into_iter().next().unwrap();
    let int_const = graph.create_node(
        NodeKind::IntConst(7),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let const_out = graph.node_outputs(int_const).into_iter().next().unwrap();
    // Return takes [ctrl, mem, ...values].
    let _ret = graph.create_node(NodeKind::Return, [entry_ctrl, mem_out, const_out], []);
    let opts = ValidateOptions { check_asm_fingerprints: true };
    let errs = validate_with_options(&graph, entry, opts).unwrap_err();
    assert!(
        errs.0.iter().any(|e| matches!(
            e,
            ValidationError::MissingAsmFingerprint { kind: NodeKind::IntConst(_), .. }
        )),
        "expected MissingAsmFingerprint for the IntConst, got: {errs:?}"
    );
    assert!(
        errs.0.iter().any(|e| matches!(
            e,
            ValidationError::MissingAsmFingerprint { kind: NodeKind::Return, .. }
        )),
        "expected MissingAsmFingerprint for Return, got: {errs:?}"
    );
}

#[test]
fn asm_fingerprint_check_accepts_when_fingerprint_present() {
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let init_mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let entry_ctrl = graph.node_outputs(entry).into_iter().next().unwrap();
    let mem_out = graph.node_outputs(init_mem).into_iter().next().unwrap();
    let int_const = graph.create_node(
        NodeKind::IntConst(7),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let const_out = graph.node_outputs(int_const).into_iter().next().unwrap();
    let ret = graph.create_node(NodeKind::Return, [entry_ctrl, mem_out, const_out], []);
    graph.set_asm_fingerprint(int_const, vec![0x1000]);
    graph.set_asm_fingerprint(ret, vec![0x1004]);
    let opts = ValidateOptions { check_asm_fingerprints: true };
    validate_with_options(&graph, entry, opts).expect("populated fingerprints validate");
}

#[test]
fn asm_fingerprint_check_exempts_phis_and_initials() {
    // Build a tiny join: Entry → ControlState ← (mem? no, just one pred);
    // verify that ControlState/InitialMemory are exempt from the check.
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let init_mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let entry_ctrl = graph.node_outputs(entry).into_iter().next().unwrap();
    let cs = graph.create_node(
        NodeKind::ControlState,
        [entry_ctrl],
        [NodeOutputKind::Control, NodeOutputKind::PhiToken],
    );
    let cs_ctrl = graph.node_outputs(cs).into_iter().next().unwrap();
    let mem_out = graph.node_outputs(init_mem).into_iter().next().unwrap();
    let _ret = graph.create_node(NodeKind::Return, [cs_ctrl, mem_out], []);
    let opts = ValidateOptions { check_asm_fingerprints: true };
    let res = validate_with_options(&graph, entry, opts);
    // The Return is reachable and non-exempt — it must be flagged.  But
    // ControlState / Entry / InitialMemory must NOT be flagged.
    let errs = res.unwrap_err();
    for e in &errs.0 {
        if let ValidationError::MissingAsmFingerprint { kind, .. } = e {
            assert!(
                !matches!(
                    kind,
                    NodeKind::Entry
                        | NodeKind::InitialMemory
                        | NodeKind::ControlState
                ),
                "exempt kind {kind:?} was flagged"
            );
        }
    }
    // Sanity: at least one MissingAsmFingerprint for the Return.
    assert!(
        errs.0
            .iter()
            .any(|e| matches!(e, ValidationError::MissingAsmFingerprint { kind: NodeKind::Return, .. })),
        "expected Return to be flagged"
    );
}

#[test]
fn indirect_branch_with_control_memory_and_value_validates() {
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let init_mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let entry_ctrl = graph.node_outputs(entry).into_iter().next().unwrap();
    let mem = graph.node_outputs(init_mem).into_iter().next().unwrap();
    let target = graph.create_node(
        NodeKind::IntConst(0x1234),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let target_val = graph.node_outputs(target).into_iter().next().unwrap();
    let _ib = graph.create_node(
        NodeKind::IndirectBranch,
        [entry_ctrl, mem, target_val],
        [],
    );
    validate(&graph, entry).expect("IndirectBranch with [ctrl, mem, target] must validate");
}
