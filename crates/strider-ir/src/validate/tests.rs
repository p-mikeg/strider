use super::*;
use crate::node::{NodeKind, NodeOutputKind, NodeOutputType};

/// Sentinel asm-fingerprint base used by [`stamp`] below — distinct from
/// any real machine address.
const SENTINEL: u64 = 0xDEAD_BEEF_0000_0001;

/// Stamp a sentinel asm-fingerprint on `id` so the always-on Layer-C
/// asm-fingerprint check is satisfied for raw `Graph::create_node`-built
/// mock graphs.  Exempt kinds (`Entry`, `InitialMemory`, phis, etc.) can
/// be stamped harmlessly — the check skips them.
fn stamp(graph: &mut Function, id: crate::node::NodeId) {
    graph.set_asm_fingerprint(id, vec![SENTINEL]);
}

#[test]
fn empty_graph_with_entry_only() {
    let mut graph = Function::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let _mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory(None)]);
    assert!(validate(&graph, entry).is_ok());
}

#[test]
fn local_typing_wrong_input_kind_on_int_unary_op() {
    use crate::node::NodeOutputType;
    use crate::ops::IntUnaryOp;

    let mut graph = Function::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let _mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory(None)]);

    // IntUnaryOp expects an OutputType input, but we feed it a Control output.
    let control_out = graph.node_outputs(entry).iter().copied().next().unwrap();
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
fn local_typing_wrong_output_kind() {
    let mut graph = Function::new();
    // Entry should produce Control, we make it produce Memory instead.
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Memory(None)]);
    let _mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory(None)]);

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
fn use_list_input_missing_from_use_list() {
    use crate::node::NodeOutputType;
    use crate::ops::IntUnaryOp;

    let mut graph = Function::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory(None)]);

    let c = graph.create_node(
        NodeKind::IntConst(3),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let c_out = graph.node_outputs(c).iter().copied().next().unwrap();

    let neg = graph.create_node(
        NodeKind::IntUnaryOp(IntUnaryOp::BitNot),
        [c_out],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );

    // Corrupt the forward link: clear the IntConst output's head-of-use
    // pointer.  The op's input is still recorded, but the producer no
    // longer admits it as a consumer.
    graph.test_only_clear_first_use(c_out);

    // use-list consistency is reachability-scoped (matches the local-typing check and
    // check_graph_invariants_phis), so wire `neg` onto the reachable spine via
    // a Return that consumes Control + Memory + the value output.
    let entry_ctrl = graph.node_outputs(entry).iter().copied().next().unwrap();
    let mem_out = graph.node_outputs(mem).iter().copied().next().unwrap();
    let neg_out = graph.node_outputs(neg).iter().copied().next().unwrap();
    let _ret = graph.create_node(NodeKind::Return, [entry_ctrl, mem_out, neg_out], []);

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
fn use_list_stale_input_in_use_list() {
    use crate::node::NodeOutputType;
    use crate::ops::IntUnaryOp;

    let mut graph = Function::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory(None)]);

    let a = graph.create_node(
        NodeKind::IntConst(1),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let a_out = graph.node_outputs(a).iter().copied().next().unwrap();

    let b = graph.create_node(
        NodeKind::IntConst(2),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let b_out = graph.node_outputs(b).iter().copied().next().unwrap();

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

    // use-list consistency is reachability-scoped; wire `neg` AND `a` onto the
    // reachable spine.  `a_out` must be reachable so the use-list sweep
    // visits its (now-stale) head; otherwise the forward check on
    // `neg`'s input fires first as InputMissingFromUseList instead of
    // the intended UseListContainsStaleInput.  Threading both through
    // a 2-value Return keeps both producers in the reachable set.
    let entry_ctrl = graph.node_outputs(entry).iter().copied().next().unwrap();
    let mem_out = graph.node_outputs(mem).iter().copied().next().unwrap();
    let neg_out = graph.node_outputs(neg).iter().copied().next().unwrap();
    let _ret = graph.create_node(
        NodeKind::Return,
        [entry_ctrl, mem_out, neg_out, a_out],
        [],
    );

    let errs = validate(&graph, entry).unwrap_err();
    assert!(
        errs.0
            .iter()
            .any(|e| matches!(e, ValidationError::UseListContainsStaleInput { .. })),
        "expected UseListContainsStaleInput, got: {errs:?}"
    );
}

/// the use-list forward check must still flag missing-from-use-list cases
/// at non-zero input slots (covers the O(E) refactor — the existing
/// `use_list_input_missing_from_use_list` only covers slot 0).
#[test]
fn use_list_forward_check_catches_missing_at_non_zero_slot() {
    use crate::node::NodeOutputType;
    use crate::ops::IntBinaryOp;

    let mut graph = Function::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory(None)]);

    let a = graph.create_node(
        NodeKind::IntConst(11),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let a_out = graph.node_outputs(a).iter().copied().next().unwrap();

    let b = graph.create_node(
        NodeKind::IntConst(13),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let b_out = graph.node_outputs(b).iter().copied().next().unwrap();

    // Add(a, b) — a at slot 0, b at slot 1.
    let add = graph.create_node(
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        [a_out, b_out],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );

    // Corrupt only b's use-list head, leaving a's intact.  Only the
    // slot-1 input should be flagged as missing.
    graph.test_only_clear_first_use(b_out);

    // use-list consistency is reachability-scoped; wire `add` onto the reachable
    // spine via Return[Ctrl, Memory, add_out].
    let entry_ctrl = graph.node_outputs(entry).iter().copied().next().unwrap();
    let mem_out = graph.node_outputs(mem).iter().copied().next().unwrap();
    let add_out = graph.node_outputs(add).iter().copied().next().unwrap();
    let _ret = graph.create_node(NodeKind::Return, [entry_ctrl, mem_out, add_out], []);

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
fn use_list_skips_unreachable_zombie_node() {
    // Pin the use-list reachability scoping (matches the local-typing check and
    // check_graph_invariants_phis): a corrupted use-list on a node that's
    // unreachable from the entry must NOT trip the use-list check.  Opt passes
    // (RedundantPhis, DeadBranchElimination) detach unreachable
    // subgraphs but leave the zombie nodes in the arena; surfacing
    // their use-list inconsistencies is noise, not real bugs.
    use crate::node::NodeOutputType;
    use crate::ops::IntUnaryOp;

    let mut graph = Function::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory(None)]);

    // Detached / unreachable producer + consumer pair.  Corrupt their
    // use-list link so that, were the use-list check graph-wide, it would fire.
    let c = graph.create_node(
        NodeKind::IntConst(7),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let c_out = graph.node_outputs(c).iter().copied().next().unwrap();
    let _zombie_consumer = graph.create_node(
        NodeKind::IntUnaryOp(IntUnaryOp::BitNot),
        [c_out],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    graph.test_only_clear_first_use(c_out); // Would fire the use-list check graph-wide.

    // Minimal reachable spine — entry + memory + a Return that takes
    // no values.  Neither `c` nor `_zombie_consumer` is reachable.
    let entry_ctrl = graph.node_outputs(entry).iter().copied().next().unwrap();
    let mem_out = graph.node_outputs(mem).iter().copied().next().unwrap();
    let ret = graph.create_node(NodeKind::Return, [entry_ctrl, mem_out], []);
    stamp(&mut graph, ret);

    validate(&graph, entry).expect("validator must skip unreachable use-list inconsistencies");
}

#[test]
fn graph_invariants_missing_initial_memory() {
    let mut graph = Function::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);

    let errs = validate(&graph, entry).unwrap_err();
    assert!(
        errs.0
            .iter()
            .any(|e| matches!(e, ValidationError::MissingInitialMemoryNode)),
        "expected MissingInitialMemoryNode, got: {errs:?}"
    );
}

// `MultipleEntryNodes` / `MultipleInitialMemoryNodes` were verified via tests
// that called `create_node` twice and expected the validator to flag the
// duplicate.  Once Entry and InitialMemory became cacheable, dedup makes the
// "duplicate" construction structurally impossible from any code path that
// goes through `create_node`.  The validator checks themselves remain as
// defence-in-depth against future graph-construction bugs (e.g. compact()
// ordering issues that resurrect a stale node).

#[test]
fn graph_invariants_entry_dedupes_on_repeated_create() {
    let mut graph = Function::new();
    let entry1 = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let entry2 = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    assert_eq!(entry1, entry2, "Entry must dedup");
    let _mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory(None)]);
    validate(&graph, entry1).expect("graph with single deduped Entry must validate");
}

#[test]
fn graph_invariants_initial_memory_dedupes_on_repeated_create() {
    let mut graph = Function::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let mem1 = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory(None)]);
    let mem2 = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory(None)]);
    assert_eq!(mem1, mem2, "InitialMemory must dedup");
    validate(&graph, entry).expect("graph with single deduped InitialMemory must validate");
}

#[test]
fn graph_invariants_region_bad_predecessor() {
    // The bad Region must be **reachable** from entry — otherwise
    // the reachability gate in `check_graph_invariants_region`
    // correctly skips it as an unreachable zombie.  Build a 2-predecessor
    // Region: input[0] = entry's Control (well-formed) so the walk
    // reaches it via cfg-succs, input[1] = InitialMemory's Memory (the
    // bad input the test pins).  The Region's Control output then
    // feeds a Return so it stays in the reachable set even after the
    // walk's forward-control phase.
    let mut graph = Function::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory(None)]);
    let entry_ctrl = graph.node_outputs(entry).iter().copied().next().unwrap();
    let mem_out = graph.node_outputs(mem).iter().copied().next().unwrap();

    // Region with [Control, Memory] inputs — input[1] is wrong.
    let bad_cs = graph.create_node(
        NodeKind::Region,
        [entry_ctrl, mem_out],
        [NodeOutputKind::Control, NodeOutputKind::PhiToken],
    );
    let bad_cs_ctrl = graph.node_outputs(bad_cs).iter().copied().next().unwrap();
    let _ret = graph.create_node(NodeKind::Return, [bad_cs_ctrl, mem_out], []);

    let errs = validate(&graph, entry).unwrap_err();
    assert!(
        errs.0.iter().any(|e| matches!(
            e,
            ValidationError::RegionNonControlPredecessor { input_idx: 1, .. }
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
fn graph_invariants_phi_token_from_wrong_node() {
    let mut graph = Function::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let _mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory(None)]);
    let entry_out = graph.node_outputs(entry).iter().copied().next().unwrap();
    let cs = graph.create_node(
        NodeKind::Region,
        [entry_out],
        [NodeOutputKind::Control, NodeOutputKind::PhiToken],
    );
    let cs_control_out = graph.node_outputs(cs).iter().copied().next().unwrap(); // index 0 = Control
    let vn = test_vn();
    let phi = graph.create_node(
        NodeKind::Phi,
        [cs_control_out],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    graph.set_phi_var_tag(phi, vn);

    let errs = validate(&graph, entry).unwrap_err();
    assert!(
        errs.0
            .iter()
            .any(|e| matches!(e, ValidationError::PhiTokenNotFromRegion { .. })),
        "got: {errs:?}"
    );
}

#[test]
fn graph_invariants_phi_value_arity_mismatch() {
    let mut graph = Function::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let _mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory(None)]);
    let entry_out = graph.node_outputs(entry).iter().copied().next().unwrap();

    let cs = graph.create_node(
        NodeKind::Region,
        [entry_out],
        [NodeOutputKind::Control, NodeOutputKind::PhiToken],
    );
    let cs_phi_out = graph.node_outputs(cs).iter().copied().nth(1).unwrap();

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
    let c1_out = graph.node_outputs(c1).iter().copied().next().unwrap();
    let c2_out = graph.node_outputs(c2).iter().copied().next().unwrap();
    let vn = test_vn();
    let phi = graph.create_node(
        NodeKind::Phi,
        [cs_phi_out, c1_out, c2_out],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    graph.set_phi_var_tag(phi, vn);

    // V-2: graph_invariants_phis is reachability-scoped, so the phi must be
    // attached to something reachable from the entry.  Wire its value
    // output through a Return that consumes the Region's Control
    // output too — this puts the phi on the cfg-reachable spine.
    let cs_ctrl_out = graph.node_outputs(cs).iter().copied().next().unwrap();
    let phi_val_out = graph.node_outputs(phi).iter().copied().next().unwrap();
    let ret = graph.create_node(NodeKind::Return, [], []);
    graph.add_node_input(ret, cs_ctrl_out).unwrap();
    graph.add_node_input(ret, phi_val_out).unwrap();

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
fn graph_invariants_phis_skips_unreachable_zombie_phi() {
    // V-2 regression: opt passes (RedundantPhis, DeadBranchElimination)
    // detach phi inputs and leave the zero-input zombie node in the
    // arena.  The validator must not falsely fire
    // PhiTokenNotFromRegion on these — the phi is no longer on
    // the reachable spine.  Exercise the contract by creating a
    // detached Phi (zero inputs) alongside an otherwise-valid
    // function and asserting validate() succeeds.
    let mut graph = Function::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let _mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory(None)]);
    let entry_ctrl = graph.node_outputs(entry).iter().copied().next().unwrap();
    // Return needs Ctrl + Memory inputs (per node_signature: [CTRL, MEM]).
    let mem_node = graph
        .nodes
        .keys()
        .find(|n| matches!(graph.node_kind(*n), NodeKind::InitialMemory))
        .unwrap();
    let mem_out = graph.node_outputs(mem_node).iter().copied().next().unwrap();
    let ret = graph.create_node(NodeKind::Return, [entry_ctrl, mem_out], []);
    stamp(&mut graph, ret);

    // Detached zombie Phi with NO inputs.
    let vn = test_vn();
    let zombie = graph.create_node(
        NodeKind::Phi,
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    graph.set_phi_var_tag(zombie, vn);

    validate(&graph, entry).expect("validator must skip unreachable zombie phis");
}

#[test]
fn local_typing_wrong_input_count() {
    use crate::node::NodeOutputType;
    use crate::ops::IntBinaryOp;

    let mut graph = Function::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let _mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory(None)]);
    let entry_ctrl = graph.node_outputs(entry).iter().copied().next().unwrap();
    let c = graph.create_node(
        NodeKind::IntConst(5),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let c_out = graph.node_outputs(c).iter().copied().next().unwrap();

    // IntBinaryOp expects 2 inputs; give it 1.
    let bad = graph.create_node(
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        [c_out],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let bad_out = graph.node_outputs(bad).iter().copied().next().unwrap();

    // Wire `bad` into the reachable sub-graph so the reachability-scoped
    // the local-typing check actually inspects it.  A Return consuming entry's Control
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

/// Regression: the local-typing check must check variadic input tails, not just the fixed
/// head prefix. A `MemPhi` whose per-predecessor inputs are not Memory
/// (e.g. a Control token leaks through) used to slip past validation
/// because the variadic-tail kind check was elided.
#[test]
fn local_typing_mem_phi_variadic_tail_must_be_memory() {
    let mut graph = Function::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory(None)]);
    let entry_ctrl = graph.node_outputs(entry).iter().copied().next().unwrap();
    let init_mem = graph.node_outputs(mem).iter().copied().next().unwrap();

    // Region with one valid Control predecessor (entry).
    let cs = graph.create_node(
        NodeKind::Region,
        [entry_ctrl],
        [NodeOutputKind::Control, NodeOutputKind::PhiToken],
    );
    let cs_outputs: Vec<_> = graph.node_outputs(cs).to_vec();
    let cs_ctrl = cs_outputs[0];
    let cs_phi_token = cs_outputs[1];

    // MemPhi with: phi_token (correct PHI kind), then a Control output as
    // its variadic predecessor (WRONG — should be Memory).
    let bad_mem_phi = graph.create_node(
        NodeKind::MemPhi,
        [cs_phi_token, entry_ctrl],
        [NodeOutputKind::Memory(None)],
    );
    let bad_mem_out = graph.node_outputs(bad_mem_phi).iter().copied().next().unwrap();
    let _ = init_mem; // unused but kept to satisfy InitialMemory uniqueness

    // Reach the MemPhi via a Return so the local-typing check walks to it.
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
fn local_typing_accepts_bool_value_phi_inputs() {
    // Phi value inputs (the IN_PHI variadic tail) must accept
    // Bool-typed values: real binaries phi-merge x86 flag registers
    // (CF/ZF/SF), which the IR models as Bool. Same rationale as ARG/RET/CALL_OUT.
    let mut graph = Function::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let init_mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory(None)]);
    let entry_ctrl = graph.node_outputs(entry).iter().copied().next().unwrap();
    let mem = graph.node_outputs(init_mem).iter().copied().next().unwrap();

    let cs = graph.create_node(
        NodeKind::Region,
        [entry_ctrl],
        [NodeOutputKind::Control, NodeOutputKind::PhiToken],
    );
    let cs_ctrl = graph.node_outputs(cs).iter().copied().next().unwrap();
    let phi_token = graph.node_outputs(cs).iter().copied().nth(1).unwrap();

    let bc = graph.create_node(
        NodeKind::BoolConst(true),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::Bool)],
    );
    let bc_out = graph.node_outputs(bc).iter().copied().next().unwrap();

    // Anonymous Phi taking [phi_token, bool_value] — the Bool flows through IN_PHI.
    let vp = graph.create_node(
        NodeKind::Phi,
        [phi_token, bc_out],
        [NodeOutputKind::OutputType(NodeOutputType::Bool)],
    );
    let vp_out = graph.node_outputs(vp).iter().copied().next().unwrap();

    // Use the phi'd value so the validator's reachability walk hits it.
    let ret = graph.create_node(NodeKind::Return, [cs_ctrl, mem, vp_out], []);
    stamp(&mut graph, bc);
    stamp(&mut graph, ret);

    validate(&graph, entry).expect("Bool-typed value phi inputs must validate");
}

#[test]
fn graph_invariants_mem_phi_arity_mismatch() {
    let mut graph = Function::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let init_mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory(None)]);
    let entry_out = graph.node_outputs(entry).iter().copied().next().unwrap();
    let init_mem_out = graph.node_outputs(init_mem).iter().copied().next().unwrap();

    let cs = graph.create_node(
        NodeKind::Region,
        [entry_out],
        [NodeOutputKind::Control, NodeOutputKind::PhiToken],
    );
    let cs_phi_out = graph.node_outputs(cs).iter().copied().nth(1).unwrap();
    let cs_ctrl_out = graph.node_outputs(cs).iter().copied().next().unwrap();

    // MemPhi with two memory inputs but the owning Region has one predecessor.
    let mem_phi = graph.create_node(
        NodeKind::MemPhi,
        [cs_phi_out, init_mem_out, init_mem_out],
        [NodeOutputKind::Memory(None)],
    );
    let mem_phi_out = graph.node_outputs(mem_phi).iter().copied().next().unwrap();
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
fn graph_invariants_value_phi_arity_mismatch() {
    let mut graph = Function::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let init_mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory(None)]);
    let entry_out = graph.node_outputs(entry).iter().copied().next().unwrap();
    let init_mem_out = graph.node_outputs(init_mem).iter().copied().next().unwrap();

    let cs = graph.create_node(
        NodeKind::Region,
        [entry_out],
        [NodeOutputKind::Control, NodeOutputKind::PhiToken],
    );
    let cs_phi_out = graph.node_outputs(cs).iter().copied().nth(1).unwrap();
    let cs_ctrl_out = graph.node_outputs(cs).iter().copied().next().unwrap();

    let c1 = graph.create_node(
        NodeKind::IntConst(1),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let c1_out = graph.node_outputs(c1).iter().copied().next().unwrap();

    // Anonymous Phi with two value inputs but the owning Region has one predecessor.
    let vp = graph.create_node(
        NodeKind::Phi,
        [cs_phi_out, c1_out, c1_out],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let vp_out = graph.node_outputs(vp).iter().copied().next().unwrap();
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
fn local_typing_rejects_wrong_output_count() {
    let mut graph = Function::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let _mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory(None)]);
    // IntConst expects exactly one output but we give it two.
    let bad = graph.create_node(
        NodeKind::IntConst(0),
        [],
        [
            NodeOutputKind::OutputType(NodeOutputType::U64),
            NodeOutputKind::OutputType(NodeOutputType::U64),
        ],
    );
    let bad_out0 = graph.node_outputs(bad).iter().copied().next().unwrap();
    let entry_ctrl = graph.node_outputs(entry).iter().copied().next().unwrap();
    let mem = graph.node_outputs(_mem).iter().copied().next().unwrap();
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
fn graph_invariants_rejects_region_with_zero_predecessors() {
    // Region has a variadic head_len of 0, so the local-typing check's count check
    // (>= 0) accepts zero inputs and the graph-invariants check's per-predecessor loop is a
    // no-op. Without an explicit check, a *reachable* zero-pred
    // Region slips through validation entirely.
    //
    // Walk semantics: graph_walk_succs follows forward-control + backward-data,
    // so we make the zero-pred Region reachable by having a downstream
    // Return consume *both* Entry's control (so walk reaches Return) and the
    // Region's control (so walking back from Return hits the CS).
    let mut graph = Function::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let init_mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory(None)]);
    let entry_ctrl = graph.node_outputs(entry).iter().copied().next().unwrap();
    let mem = graph.node_outputs(init_mem).iter().copied().next().unwrap();
    let cs = graph.create_node(
        NodeKind::Region,
        [],
        [NodeOutputKind::Control, NodeOutputKind::PhiToken],
    );
    let cs_ctrl = graph.node_outputs(cs).iter().copied().next().unwrap();
    // Return consumes entry's control (reaches Return via cfg_succs of Entry)
    // and cs_ctrl as a "ret value" (reaches Region via Return's backward-data).
    graph.create_node(NodeKind::Return, [entry_ctrl, mem, cs_ctrl], []);

    let errs = validate(&graph, entry).unwrap_err();
    assert!(
        errs.0.iter().any(|e| matches!(
            e,
            ValidationError::EmptyRegionPredecessors { region } if *region == cs
        )),
        "expected EmptyRegionPredecessors, got: {errs:?}"
    );
}

#[test]
fn graph_invariants_tolerates_unreachable_zero_predecessor_region() {
    // Zombie Region with zero inputs left behind by RedundantPhis is
    // expected; the validator must not flag it (this happens routinely on
    // real binaries after dead-branch elimination).
    let mut graph = Function::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let init_mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory(None)]);
    let entry_ctrl = graph.node_outputs(entry).iter().copied().next().unwrap();
    let mem = graph.node_outputs(init_mem).iter().copied().next().unwrap();
    // Zombie Region that nothing references — not reachable from entry.
    let _zombie_cs = graph.create_node(
        NodeKind::Region,
        [],
        [NodeOutputKind::Control, NodeOutputKind::PhiToken],
    );
    let ret = graph.create_node(NodeKind::Return, [entry_ctrl, mem], []);
    stamp(&mut graph, ret);

    validate(&graph, entry).expect("zombie Region must not trigger validation error");
}

/// IndirectBranch consumes (control, memory, target_value) and produces no
/// outputs; the validator must accept this exact shape.  IndirectBranch is
/// the lifter's placeholder for `RegionTerminator::UnresolvedIndirectBranch`
/// — it's mutated in-place by the indirect-branch resolver into a real
/// `Return` (LinkRegister) or replaced by a `Call+Return` pair (tail call).
#[test]
fn asm_fingerprint_check_off_by_default_accepts_empty_fingerprints() {
    // Opt-in is off → fully-empty fingerprints on a non-exempt node are OK.
    let mut graph = Function::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let _mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory(None)]);
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
    let mut graph = Function::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let init_mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory(None)]);
    let entry_ctrl = graph.node_outputs(entry).iter().copied().next().unwrap();
    let mem_out = graph.node_outputs(init_mem).iter().copied().next().unwrap();
    let int_const = graph.create_node(
        NodeKind::IntConst(7),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let const_out = graph.node_outputs(int_const).iter().copied().next().unwrap();
    // Return takes [ctrl, mem, ...values].
    let _ret = graph.create_node(NodeKind::Return, [entry_ctrl, mem_out, const_out], []);
    let errs = validate(&graph, entry).unwrap_err();
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
    let mut graph = Function::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let init_mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory(None)]);
    let entry_ctrl = graph.node_outputs(entry).iter().copied().next().unwrap();
    let mem_out = graph.node_outputs(init_mem).iter().copied().next().unwrap();
    let int_const = graph.create_node(
        NodeKind::IntConst(7),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let const_out = graph.node_outputs(int_const).iter().copied().next().unwrap();
    let ret = graph.create_node(NodeKind::Return, [entry_ctrl, mem_out, const_out], []);
    graph.set_asm_fingerprint(int_const, vec![0x1000]);
    graph.set_asm_fingerprint(ret, vec![0x1004]);
    validate(&graph, entry).expect("populated fingerprints validate");
}

#[test]
fn asm_fingerprint_check_exempts_phis_and_initials() {
    // Build a tiny join: Entry → Region ← (mem? no, just one pred);
    // verify that Region/InitialMemory are exempt from the check.
    let mut graph = Function::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let init_mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory(None)]);
    let entry_ctrl = graph.node_outputs(entry).iter().copied().next().unwrap();
    let cs = graph.create_node(
        NodeKind::Region,
        [entry_ctrl],
        [NodeOutputKind::Control, NodeOutputKind::PhiToken],
    );
    let cs_ctrl = graph.node_outputs(cs).iter().copied().next().unwrap();
    let mem_out = graph.node_outputs(init_mem).iter().copied().next().unwrap();
    let _ret = graph.create_node(NodeKind::Return, [cs_ctrl, mem_out], []);
    let res = validate(&graph, entry);
    // The Return is reachable and non-exempt — it must be flagged.  But
    // Region / Entry / InitialMemory must NOT be flagged.
    let errs = res.unwrap_err();
    for e in &errs.0 {
        if let ValidationError::MissingAsmFingerprint { kind, .. } = e {
            assert!(
                !matches!(
                    kind,
                    NodeKind::Entry
                        | NodeKind::InitialMemory
                        | NodeKind::Region
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

/// regression: a non-reachable
/// `Region` zombie with stale non-Control inputs must not
/// produce a false-positive `RegionNonControlPredecessor`
/// error.  Pre-fix, the empty-input branch was correctly
/// reachability-gated but the non-empty-input branch was not.
#[test]
fn unreachable_region_with_non_control_input_does_not_fire() {
    let mut graph = Function::new();
    // Reachable spine: Entry → Return.
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let init_mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory(None)]);
    let entry_ctrl = graph.node_outputs(entry).iter().copied().next().unwrap();
    let mem_out = graph.node_outputs(init_mem).iter().copied().next().unwrap();
    let ret = graph.create_node(NodeKind::Return, [entry_ctrl, mem_out], []);
    stamp(&mut graph, ret);

    // Detached zombie: a Region whose input is a non-Control output
    // (an IntConst's value output).  This shape can be left behind by a
    // future pass that surgery-edits without scrubbing inputs.  The
    // node IS in the arena but is NOT reachable from `entry`.
    let int_const = graph.create_node(
        NodeKind::IntConst(0x1234),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let bogus_input = graph.node_outputs(int_const).iter().copied().next().unwrap();
    let _zombie_cs = graph.create_node(
        NodeKind::Region,
        [bogus_input],
        [NodeOutputKind::Control, NodeOutputKind::PhiToken],
    );

    // The unreachable zombie must be skipped by the reachability gate;
    // the validator must not flag a `RegionNonControlPredecessor`
    // error.  (Pre-fix this would have fired.)
    validate(&graph, entry).expect(
        "unreachable Region zombies must not produce \
         RegionNonControlPredecessor errors",
    );
}

#[test]
fn indirect_branch_with_control_memory_and_value_validates() {
    let mut graph = Function::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let init_mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory(None)]);
    let entry_ctrl = graph.node_outputs(entry).iter().copied().next().unwrap();
    let mem = graph.node_outputs(init_mem).iter().copied().next().unwrap();
    let target = graph.create_node(
        NodeKind::IntConst(0x1234),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let target_val = graph.node_outputs(target).iter().copied().next().unwrap();
    let ib = graph.create_node(
        NodeKind::IndirectBranch,
        [entry_ctrl, mem, target_val],
        [],
    );
    stamp(&mut graph, target);
    stamp(&mut graph, ib);
    validate(&graph, entry).expect("IndirectBranch with [ctrl, mem, target] must validate");
}

#[test]
fn graph_invariants_dangling_wide_const_id_detected() {
    use crate::wide_const::WideConstId;
    let mut graph = Function::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory(None)]);
    let entry_ctrl = graph.node_outputs(entry).iter().copied().next().unwrap();
    let mem_out = graph.node_outputs(mem).iter().copied().next().unwrap();
    // Construct an IntConstWide pointing at an id that was never interned.
    let bogus_id = WideConstId::from_u32(99);
    let bogus = graph.create_node(
        NodeKind::IntConstWide(bogus_id),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U256)],
    );
    let bogus_out = graph.node_outputs(bogus).iter().copied().next().unwrap();
    let _ret = graph.create_node(NodeKind::Return, [entry_ctrl, mem_out, bogus_out], []);

    let errs = validate(&graph, entry).unwrap_err();
    assert!(
        errs.0
            .iter()
            .any(|e| matches!(e, ValidationError::DanglingWideConstId { .. })),
        "expected DanglingWideConstId, got: {errs:?}"
    );
}

#[test]
fn graph_invariants_wide_const_width_mismatch_detected() {
    use crate::wide_const::WideConstStorage;
    let mut graph = Function::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory(None)]);
    let entry_ctrl = graph.node_outputs(entry).iter().copied().next().unwrap();
    let mem_out = graph.node_outputs(mem).iter().copied().next().unwrap();
    // Intern a U256 storage but assign it to a U512-typed output.
    let id = graph.intern_wide_const(WideConstStorage::U256([0; 4]));
    let bad = graph.create_node(
        NodeKind::IntConstWide(id),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U512)],
    );
    let bad_out = graph.node_outputs(bad).iter().copied().next().unwrap();
    let _ret = graph.create_node(NodeKind::Return, [entry_ctrl, mem_out, bad_out], []);

    let errs = validate(&graph, entry).unwrap_err();
    assert!(
        errs.0.iter().any(|e| matches!(
            e,
            ValidationError::WideConstWidthMismatch {
                expected_bytes: 64,
                actual_bytes: 32,
                ..
            }
        )),
        "expected WideConstWidthMismatch, got: {errs:?}"
    );
}

/// Build: Entry → InitialMemory → MemProject → (Stack lane, Unknown lane)
///        → MemUnion → Return.  The happy-path chain for the AliasSplit
///        partition-boundary nodes must pass validate without errors.
#[test]
fn validate_accepts_mem_project_and_union_chain() {
    use strider_target::AliasClass;

    let mut graph = Function::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    graph.set_entry(entry);
    let mem_node = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory(None)]);
    let [entry_ctrl] = graph.node_outputs_exact::<1>(entry).unwrap();
    let [mem_out] = graph.node_outputs_exact::<1>(mem_node).unwrap();

    // MemProject: 1 unified memory in → 2 partition lanes out.
    let mp = graph.create_node(
        NodeKind::MemProject,
        [mem_out],
        [
            NodeOutputKind::Memory(Some(AliasClass::Stack)),
            NodeOutputKind::Memory(Some(AliasClass::Unknown)),
        ],
    );
    let mp_outs = graph.node_outputs(mp).to_vec();
    let stack_out = mp_outs[0];
    let unknown_out = mp_outs[1];

    // MemUnion: 2 partition lanes in → 1 unified memory out.
    let mu = graph.create_node(
        NodeKind::MemUnion,
        [stack_out, unknown_out],
        [NodeOutputKind::Memory(None)],
    );
    let [unified_out] = graph.node_outputs_exact::<1>(mu).unwrap();

    // Return consumes the reunified memory.
    let ret = graph.create_node(NodeKind::Return, [entry_ctrl, unified_out], []);
    stamp(&mut graph, ret);

    assert!(
        validate(&graph, entry).is_ok(),
        "MemProject → MemUnion chain must pass validate"
    );
}

// ── CC arity check ───────────────────────────────────────────────────────

/// Build a minimal Function whose `cc_metadata` declares
/// `ret_val_regs = [v1, v2]`.  Used by the cc-arity tests below.
fn fn_with_declared_cc() -> (Function, crate::node::NodeId) {
    use cranelift_entity::PrimaryMap;
    let mut f = Function::new();
    let entry = f.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    stamp(&mut f, entry);
    f.set_entry(entry);
    let mk_vn = |off: u64| rsleigh::Vn {
        addr_off: off,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 8,
    };
    let mut variables: PrimaryMap<crate::builder::VarId, rsleigh::Vn> = PrimaryMap::new();
    variables.push(mk_vn(0x10));
    variables.push(mk_vn(0x18));
    f.set_cc_metadata(crate::graph::CcMetadata {
        variables,
        call_clobbered: Box::new([]),
        ret_val_regs: Box::new([mk_vn(0x10), mk_vn(0x18)]),
        call_other_clobbered: Box::new([]),
        no_memory_clobber: false,
    });
    (f, entry)
}

#[test]
fn cc_arity_catches_return_dropping_a_declared_ret_val_reg() {
    // Function declares ret_val_regs = [v1, v2] (count 2).  We build
    // a Return with only [ctrl, mem, v1_val] — one short.  The
    // validator's cc-arity check must fire with NodeInputCountMismatch.
    // This is the bug class A6-H1 in the multi-round review: a
    // synthesised Return dropping ret_val_regs_float silently produces
    // a too-short Return.
    let (mut f, entry) = fn_with_declared_cc();
    let [ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
    let mem = f.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory(None)]);
    let [mem_out] = f.node_outputs_exact::<1>(mem).unwrap();
    stamp(&mut f, mem);
    let v1 = f.create_node(
        NodeKind::IntConst(7),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let [v1_out] = f.node_outputs_exact::<1>(v1).unwrap();
    stamp(&mut f, v1);
    // Return with only ONE ret-val input — dropping v2's slot.
    let ret = f.create_node(NodeKind::Return, [ctrl, mem_out, v1_out], []);
    stamp(&mut f, ret);

    let err = validate(&f, entry).expect_err("expected cc-arity violation");
    assert!(
        err.0.iter().any(|e| matches!(
            e,
            ValidationError::NodeInputCountMismatch { expected: 4, actual: 3, .. }
        )),
        "expected NodeInputCountMismatch {{ expected: 4, actual: 3 }} for the Return, got: {err:?}"
    );
}

#[test]
fn cc_arity_passes_when_return_matches_declared_ret_val_regs() {
    let (mut f, entry) = fn_with_declared_cc();
    let [ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
    let mem = f.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory(None)]);
    let [mem_out] = f.node_outputs_exact::<1>(mem).unwrap();
    stamp(&mut f, mem);
    let v1 = f.create_node(
        NodeKind::IntConst(7),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let [v1_out] = f.node_outputs_exact::<1>(v1).unwrap();
    stamp(&mut f, v1);
    let v2 = f.create_node(
        NodeKind::IntConst(8),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let [v2_out] = f.node_outputs_exact::<1>(v2).unwrap();
    stamp(&mut f, v2);
    let ret = f.create_node(NodeKind::Return, [ctrl, mem_out, v1_out, v2_out], []);
    stamp(&mut f, ret);

    validate(&f, entry).expect("Return with declared 2 ret-val regs and 2 value inputs must validate");
}
