use super::*;
use crate::node::{NodeKind, ValueKind, ValueType};

/// Sentinel asm-fingerprint base used by [`stamp`] below — distinct from
/// any real machine address.
const SENTINEL: u64 = 0xDEAD_BEEF_0000_0001;

/// Stamp a sentinel asm-fingerprint on `id` so the always-on Layer-C
/// asm-fingerprint check is satisfied for raw `Graph::create_node`-built
/// mock graphs.  Exempt kinds (`Entry`, `InitialMemory`, phis, etc.) can
/// be stamped harmlessly — the check skips them.
fn stamp(function: &mut Function, id: crate::node::NodeId) {
    function.set_asm_fingerprint(id, vec![SENTINEL]);
}

#[test]
fn empty_graph_with_entry_only() {
    let mut function = Function::default();
    let entry = function.graph_mut().create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let _mem = function.graph_mut().create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    assert!(validate(&function, entry).is_ok());
}

#[test]
fn local_typing_wrong_input_kind_on_int_unary_op() {
    use crate::node::ValueType;
    use crate::node::IntUnaryOp;

    let mut function = Function::default();
    let entry = function.graph_mut().create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let _mem = function.graph_mut().create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);

    // IntUnaryOp expects an Typed input, but we feed it a Control output.
    let control_value = function.node_outputs(entry).iter().copied().next().unwrap();
    let _bad = function.graph_mut().create_node(
        NodeKind::IntUnaryOp(IntUnaryOp::Neg),
        [control_value],
        [ValueKind::Typed(ValueType::I64)],
    );

    let errs = validate(&function, entry).unwrap_err();
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
    let mut function = Function::default();
    // Entry should produce Control, we make it produce Memory instead.
    let entry = function.graph_mut().create_node(NodeKind::Entry, [], [ValueKind::Memory]);
    let _mem = function.graph_mut().create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);

    let errs = validate(&function, entry).unwrap_err();
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
    use crate::node::ValueType;
    use crate::node::IntUnaryOp;

    let mut function = Function::default();
    let entry = function.graph_mut().create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let mem = function.graph_mut().create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);

    let c = function.graph_mut().create_node(
        NodeKind::IntConst(3),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    let c_value = function.node_outputs(c).iter().copied().next().unwrap();

    let neg = function.graph_mut().create_node(
        NodeKind::IntUnaryOp(IntUnaryOp::Neg),
        [c_value],
        [ValueKind::Typed(ValueType::I64)],
    );

    // Corrupt the forward link: clear the IntConst output's head-of-use
    // pointer.  The op's input is still recorded, but the producer no
    // longer admits it as a consumer.
    function.graph_mut().test_only_clear_first_use(c_value);

    // use-list consistency is reachability-scoped (matches the local-typing check and
    // check_graph_invariants_phis), so wire `neg` onto the reachable spine via
    // a Return that consumes Control + Memory + the value output.
    let entry_ctrl = function.node_outputs(entry).iter().copied().next().unwrap();
    let mem_value = function.node_outputs(mem).iter().copied().next().unwrap();
    let neg_value = function.node_outputs(neg).iter().copied().next().unwrap();
    let _ret = function.graph_mut().create_node(NodeKind::Return, [entry_ctrl, mem_value, neg_value], []);

    let errs = validate(&function, entry).unwrap_err();
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
    use crate::node::ValueType;
    use crate::node::IntUnaryOp;

    let mut function = Function::default();
    let entry = function.graph_mut().create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let mem = function.graph_mut().create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);

    let a = function.graph_mut().create_node(
        NodeKind::IntConst(1),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    let a_value = function.node_outputs(a).iter().copied().next().unwrap();

    let b = function.graph_mut().create_node(
        NodeKind::IntConst(2),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    let b_value = function.node_outputs(b).iter().copied().next().unwrap();

    let neg = function.graph_mut().create_node(
        NodeKind::IntUnaryOp(IntUnaryOp::Neg),
        [a_value],
        [ValueKind::Typed(ValueType::I64)],
    );

    // Retarget the op's input at idx 0 to `b_value` without updating any
    // use-list.  `a_value`'s use-list still references this input, but the
    // input itself now points at `b_value` — that's a stale entry.
    let use_id = function.graph().node_input_id_at(neg, 0).unwrap();
    function.graph_mut().test_only_retarget_input(use_id, b_value);

    // use-list consistency is reachability-scoped; wire `neg` AND `a` onto the
    // reachable spine.  `a_value` must be reachable so the use-list sweep
    // visits its (now-stale) head; otherwise the forward check on
    // `neg`'s input fires first as InputMissingFromUseList instead of
    // the intended UseListContainsStaleInput.  Threading both through
    // a 2-value Return keeps both producers in the reachable set.
    let entry_ctrl = function.node_outputs(entry).iter().copied().next().unwrap();
    let mem_value = function.node_outputs(mem).iter().copied().next().unwrap();
    let neg_value = function.node_outputs(neg).iter().copied().next().unwrap();
    let _ret = function.graph_mut().create_node(
        NodeKind::Return,
        [entry_ctrl, mem_value, neg_value, a_value],
        [],
    );

    let errs = validate(&function, entry).unwrap_err();
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
    use crate::node::ValueType;
    use crate::node::IntBinaryOp;

    let mut function = Function::default();
    let entry = function.graph_mut().create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let mem = function.graph_mut().create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);

    let a = function.graph_mut().create_node(
        NodeKind::IntConst(11),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    let a_value = function.node_outputs(a).iter().copied().next().unwrap();

    let b = function.graph_mut().create_node(
        NodeKind::IntConst(13),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    let b_value = function.node_outputs(b).iter().copied().next().unwrap();

    // Add(a, b) — a at slot 0, b at slot 1.
    let add = function.graph_mut().create_node(
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        [a_value, b_value],
        [ValueKind::Typed(ValueType::I64)],
    );

    // Corrupt only b's use-list head, leaving a's intact.  Only the
    // slot-1 input should be flagged as missing.
    function.graph_mut().test_only_clear_first_use(b_value);

    // use-list consistency is reachability-scoped; wire `add` onto the reachable
    // spine via Return[Ctrl, Memory, add_value].
    let entry_ctrl = function.node_outputs(entry).iter().copied().next().unwrap();
    let mem_value = function.node_outputs(mem).iter().copied().next().unwrap();
    let add_value = function.node_outputs(add).iter().copied().next().unwrap();
    let _ret = function.graph_mut().create_node(NodeKind::Return, [entry_ctrl, mem_value, add_value], []);

    let errs = validate(&function, entry).unwrap_err();
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
    // (DeadBranchElimination, CfgDetach) detach unreachable
    // subgraphs but leave the zombie nodes in the arena; surfacing
    // their use-list inconsistencies is noise, not real bugs.
    use crate::node::ValueType;
    use crate::node::IntUnaryOp;

    let mut function = Function::default();
    let entry = function.graph_mut().create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let mem = function.graph_mut().create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);

    // Detached / unreachable producer + consumer pair.  Corrupt their
    // use-list link so that, were the use-list check graph-wide, it would fire.
    let c = function.graph_mut().create_node(
        NodeKind::IntConst(7),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    let c_value = function.node_outputs(c).iter().copied().next().unwrap();
    let _zombie_consumer = function.graph_mut().create_node(
        NodeKind::IntUnaryOp(IntUnaryOp::Neg),
        [c_value],
        [ValueKind::Typed(ValueType::I64)],
    );
    function.graph_mut().test_only_clear_first_use(c_value); // Would fire the use-list check graph-wide.

    // Minimal reachable spine — entry + memory + a Return that takes
    // no values.  Neither `c` nor `_zombie_consumer` is reachable.
    let entry_ctrl = function.node_outputs(entry).iter().copied().next().unwrap();
    let mem_value = function.node_outputs(mem).iter().copied().next().unwrap();
    let ret = function.graph_mut().create_node(NodeKind::Return, [entry_ctrl, mem_value], []);
    stamp(&mut function, ret);

    validate(&function, entry).expect("validator must skip unreachable use-list inconsistencies");
}

#[test]
fn graph_invariants_missing_initial_memory() {
    let mut function = Function::default();
    let entry = function.graph_mut().create_node(NodeKind::Entry, [], [ValueKind::Control]);

    let errs = validate(&function, entry).unwrap_err();
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
    let mut function = Function::default();
    let entry1 = function.graph_mut().create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let entry2 = function.graph_mut().create_node(NodeKind::Entry, [], [ValueKind::Control]);
    assert_eq!(entry1, entry2, "Entry must dedup");
    let _mem = function.graph_mut().create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    validate(&function, entry1).expect("graph with single deduped Entry must validate");
}

#[test]
fn graph_invariants_initial_memory_dedupes_on_repeated_create() {
    let mut function = Function::default();
    let entry = function.graph_mut().create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let mem1 = function.graph_mut().create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    let mem2 = function.graph_mut().create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    assert_eq!(mem1, mem2, "InitialMemory must dedup");
    validate(&function, entry).expect("graph with single deduped InitialMemory must validate");
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
    let mut function = Function::default();
    let entry = function.graph_mut().create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let mem = function.graph_mut().create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    let entry_ctrl = function.node_outputs(entry).iter().copied().next().unwrap();
    let mem_value = function.node_outputs(mem).iter().copied().next().unwrap();

    // Region with [Control, Memory] inputs — input[1] is wrong.
    let bad_cs = function.graph_mut().create_node(
        NodeKind::Region,
        [entry_ctrl, mem_value],
        [ValueKind::Control, ValueKind::PhiToken],
    );
    let bad_cs_ctrl = function.node_outputs(bad_cs).iter().copied().next().unwrap();
    let _ret = function.graph_mut().create_node(NodeKind::Return, [bad_cs_ctrl, mem_value], []);

    let errs = validate(&function, entry).unwrap_err();
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
    let mut function = Function::default();
    let entry = function.graph_mut().create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let _mem = function.graph_mut().create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    let entry_value = function.node_outputs(entry).iter().copied().next().unwrap();
    let cs = function.graph_mut().create_node(
        NodeKind::Region,
        [entry_value],
        [ValueKind::Control, ValueKind::PhiToken],
    );
    let cs_control_value = function.node_outputs(cs).iter().copied().next().unwrap(); // index 0 = Control
    let vn = test_vn();
    let phi = function.graph_mut().create_node(
        NodeKind::Phi,
        [cs_control_value],
        [ValueKind::Typed(ValueType::I64)],
    );
    function.set_phi_var_tag(phi, vn);

    let errs = validate(&function, entry).unwrap_err();
    assert!(
        errs.0
            .iter()
            .any(|e| matches!(e, ValidationError::PhiTokenNotFromRegion { .. })),
        "got: {errs:?}"
    );
}

#[test]
fn graph_invariants_phi_value_arity_mismatch() {
    let mut function = Function::default();
    let entry = function.graph_mut().create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let _mem = function.graph_mut().create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    let entry_value = function.node_outputs(entry).iter().copied().next().unwrap();

    let cs = function.graph_mut().create_node(
        NodeKind::Region,
        [entry_value],
        [ValueKind::Control, ValueKind::PhiToken],
    );
    let cs_phi_value = function.node_outputs(cs).iter().copied().nth(1).unwrap();

    let c1 = function.graph_mut().create_node(
        NodeKind::IntConst(1),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    let c2 = function.graph_mut().create_node(
        NodeKind::IntConst(2),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    let c1_value = function.node_outputs(c1).iter().copied().next().unwrap();
    let c2_value = function.node_outputs(c2).iter().copied().next().unwrap();
    let vn = test_vn();
    let phi = function.graph_mut().create_node(
        NodeKind::Phi,
        [cs_phi_value, c1_value, c2_value],
        [ValueKind::Typed(ValueType::I64)],
    );
    function.set_phi_var_tag(phi, vn);

    // V-2: graph_invariants_phis is reachability-scoped, so the phi must be
    // attached to something reachable from the entry.  Wire its value
    // output through a Return that consumes the Region's Control
    // output too — this puts the phi on the cfg-reachable spine.
    let cs_ctrl_value = function.node_outputs(cs).iter().copied().next().unwrap();
    let phi_val_value = function.node_outputs(phi).iter().copied().next().unwrap();
    let ret = function.graph_mut().create_node(NodeKind::Return, [], []);
    function.graph_mut().add_node_input(ret, cs_ctrl_value).unwrap();
    function.graph_mut().add_node_input(ret, phi_val_value).unwrap();

    let errs = validate(&function, entry).unwrap_err();
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
fn graph_invariants_phi_input_type_mismatch() {
    let mut function = Function::default();
    let entry = function.graph_mut().create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let _mem = function.graph_mut().create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    let entry_value = function.node_outputs(entry).iter().copied().next().unwrap();

    let cs = function.graph_mut().create_node(
        NodeKind::Region,
        [entry_value],
        [ValueKind::Control, ValueKind::PhiToken],
    );
    let cs_phi_value = function.node_outputs(cs).iter().copied().nth(1).unwrap();

    // One value input typed I8 but the phi declares output I64 — a type
    // mismatch: a value phi must merge values of a single type.
    let c1 = function.graph_mut().create_node(
        NodeKind::IntConst(1),
        [],
        [ValueKind::Typed(ValueType::I8)],
    );
    let c1_value = function.node_outputs(c1).iter().copied().next().unwrap();
    let phi = function.graph_mut().create_node(
        NodeKind::Phi,
        [cs_phi_value, c1_value],
        [ValueKind::Typed(ValueType::I64)],
    );
    function.set_phi_var_tag(phi, test_vn());

    // Put the phi on the reachable spine (see the arity test above).
    let cs_ctrl_value = function.node_outputs(cs).iter().copied().next().unwrap();
    let phi_val_value = function.node_outputs(phi).iter().copied().next().unwrap();
    let ret = function.graph_mut().create_node(NodeKind::Return, [], []);
    function.graph_mut().add_node_input(ret, cs_ctrl_value).unwrap();
    function.graph_mut().add_node_input(ret, phi_val_value).unwrap();

    let errs = validate(&function, entry).unwrap_err();
    assert!(
        errs.0.iter().any(|e| matches!(
            e,
            ValidationError::PhiInputTypeMismatch {
                input_index: 1,
                output_ty: ValueType::I64,
                input_ty: ValueType::I8,
                ..
            }
        )),
        "got: {errs:?}"
    );
}

#[test]
fn graph_invariants_phis_skips_unreachable_zombie_phi() {
    // V-2 regression: opt passes (DeadBranchElimination, CfgDetach)
    // detach phi inputs and leave the zero-input zombie node in the
    // arena.  The validator must not falsely fire
    // PhiTokenNotFromRegion on these — the phi is no longer on
    // the reachable spine.  Exercise the contract by creating a
    // detached Phi (zero inputs) alongside an otherwise-valid
    // function and asserting validate() succeeds.
    let mut function = Function::default();
    let entry = function.graph_mut().create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let _mem = function.graph_mut().create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    let entry_ctrl = function.node_outputs(entry).iter().copied().next().unwrap();
    // Return needs Ctrl + Memory inputs (per node_signature: [CTRL, MEM]).
    let mem_node = function.graph()
        .nodes
        .keys()
        .find(|n| matches!(function.node_kind(*n), NodeKind::InitialMemory))
        .unwrap();
    let mem_value = function.node_outputs(mem_node).iter().copied().next().unwrap();
    let ret = function.graph_mut().create_node(NodeKind::Return, [entry_ctrl, mem_value], []);
    stamp(&mut function, ret);

    // Detached zombie Phi with NO inputs.
    let vn = test_vn();
    let zombie = function.graph_mut().create_node(
        NodeKind::Phi,
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    function.set_phi_var_tag(zombie, vn);

    validate(&function, entry).expect("validator must skip unreachable zombie phis");
}

#[test]
fn local_typing_wrong_input_count() {
    use crate::node::ValueType;
    use crate::node::IntBinaryOp;

    let mut function = Function::default();
    let entry = function.graph_mut().create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let _mem = function.graph_mut().create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    let entry_ctrl = function.node_outputs(entry).iter().copied().next().unwrap();
    let c = function.graph_mut().create_node(
        NodeKind::IntConst(5),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    let c_value = function.node_outputs(c).iter().copied().next().unwrap();

    // IntBinaryOp expects 2 inputs; give it 1.
    let bad = function.graph_mut().create_node(
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        [c_value],
        [ValueKind::Typed(ValueType::I64)],
    );
    let bad_value = function.node_outputs(bad).iter().copied().next().unwrap();

    // Wire `bad` into the reachable sub-graph so the reachability-scoped
    // the local-typing check actually inspects it.  A Return consuming entry's Control
    // plus `bad`'s value output is the smallest reachable shape.
    let _ret = function.graph_mut().create_node(NodeKind::Return, [entry_ctrl, bad_value], []);

    let errs = validate(&function, entry).unwrap_err();
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
    let mut function = Function::default();
    let entry = function.graph_mut().create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let mem = function.graph_mut().create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    let entry_ctrl = function.node_outputs(entry).iter().copied().next().unwrap();
    let init_mem = function.node_outputs(mem).iter().copied().next().unwrap();

    // Region with one valid Control predecessor (entry).
    let cs = function.graph_mut().create_node(
        NodeKind::Region,
        [entry_ctrl],
        [ValueKind::Control, ValueKind::PhiToken],
    );
    let cs_outputs: Vec<_> = function.node_outputs(cs).to_vec();
    let cs_ctrl = cs_outputs[0];
    let cs_phi_token = cs_outputs[1];

    // MemPhi with: phi_token (correct PHI kind), then a Control output as
    // its variadic predecessor (WRONG — should be Memory).
    let bad_mem_phi = function.graph_mut().create_node(
        NodeKind::MemPhi,
        [cs_phi_token, entry_ctrl],
        [ValueKind::Memory],
    );
    let bad_mem_value = function.node_outputs(bad_mem_phi).iter().copied().next().unwrap();
    let _ = init_mem; // unused but kept to satisfy InitialMemory uniqueness

    // Reach the MemPhi via a Return so the local-typing check walks to it.
    function.graph_mut().create_node(NodeKind::Return, [cs_ctrl, bad_mem_value], []);

    let errs = validate(&function, entry).unwrap_err();
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
    let mut function = Function::default();
    let entry = function.graph_mut().create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let init_mem = function.graph_mut().create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    let entry_ctrl = function.node_outputs(entry).iter().copied().next().unwrap();
    let mem = function.node_outputs(init_mem).iter().copied().next().unwrap();

    let cs = function.graph_mut().create_node(
        NodeKind::Region,
        [entry_ctrl],
        [ValueKind::Control, ValueKind::PhiToken],
    );
    let cs_ctrl = function.node_outputs(cs).iter().copied().next().unwrap();
    let phi_token = function.node_outputs(cs).iter().copied().nth(1).unwrap();

    let bc = function.graph_mut().create_node(
        NodeKind::IntConst(1),
        [],
        [ValueKind::Typed(ValueType::I1)],
    );
    let bc_value = function.node_outputs(bc).iter().copied().next().unwrap();

    // Anonymous Phi taking [phi_token, bool_value] — the Bool flows through IN_PHI.
    let vp = function.graph_mut().create_node(
        NodeKind::Phi,
        [phi_token, bc_value],
        [ValueKind::Typed(ValueType::I1)],
    );
    let vp_value = function.node_outputs(vp).iter().copied().next().unwrap();

    // Use the phi'd value so the validator's reachability walk hits it.
    let ret = function.graph_mut().create_node(NodeKind::Return, [cs_ctrl, mem, vp_value], []);
    stamp(&mut function, bc);
    stamp(&mut function, ret);

    validate(&function, entry).expect("Bool-typed value phi inputs must validate");
}

#[test]
fn graph_invariants_mem_phi_arity_mismatch() {
    let mut function = Function::default();
    let entry = function.graph_mut().create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let init_mem = function.graph_mut().create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    let entry_value = function.node_outputs(entry).iter().copied().next().unwrap();
    let init_mem_value = function.node_outputs(init_mem).iter().copied().next().unwrap();

    let cs = function.graph_mut().create_node(
        NodeKind::Region,
        [entry_value],
        [ValueKind::Control, ValueKind::PhiToken],
    );
    let cs_phi_value = function.node_outputs(cs).iter().copied().nth(1).unwrap();
    let cs_ctrl_value = function.node_outputs(cs).iter().copied().next().unwrap();

    // MemPhi with two memory inputs but the owning Region has one predecessor.
    let mem_phi = function.graph_mut().create_node(
        NodeKind::MemPhi,
        [cs_phi_value, init_mem_value, init_mem_value],
        [ValueKind::Memory],
    );
    let mem_phi_value = function.node_outputs(mem_phi).iter().copied().next().unwrap();
    function.graph_mut().create_node(NodeKind::Return, [cs_ctrl_value, mem_phi_value], []);

    let errs = validate(&function, entry).unwrap_err();
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
    let mut function = Function::default();
    let entry = function.graph_mut().create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let init_mem = function.graph_mut().create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    let entry_value = function.node_outputs(entry).iter().copied().next().unwrap();
    let init_mem_value = function.node_outputs(init_mem).iter().copied().next().unwrap();

    let cs = function.graph_mut().create_node(
        NodeKind::Region,
        [entry_value],
        [ValueKind::Control, ValueKind::PhiToken],
    );
    let cs_phi_value = function.node_outputs(cs).iter().copied().nth(1).unwrap();
    let cs_ctrl_value = function.node_outputs(cs).iter().copied().next().unwrap();

    let c1 = function.graph_mut().create_node(
        NodeKind::IntConst(1),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    let c1_value = function.node_outputs(c1).iter().copied().next().unwrap();

    // Anonymous Phi with two value inputs but the owning Region has one predecessor.
    let vp = function.graph_mut().create_node(
        NodeKind::Phi,
        [cs_phi_value, c1_value, c1_value],
        [ValueKind::Typed(ValueType::I64)],
    );
    let vp_value = function.node_outputs(vp).iter().copied().next().unwrap();
    function.graph_mut().create_node(NodeKind::Return, [cs_ctrl_value, init_mem_value, vp_value], []);

    let errs = validate(&function, entry).unwrap_err();
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
    let mut function = Function::default();
    let entry = function.graph_mut().create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let _mem = function.graph_mut().create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    // IntConst expects exactly one output but we give it two.
    let bad = function.graph_mut().create_node(
        NodeKind::IntConst(0),
        [],
        [
            ValueKind::Typed(ValueType::I64),
            ValueKind::Typed(ValueType::I64),
        ],
    );
    let bad_value0 = function.node_outputs(bad).iter().copied().next().unwrap();
    let entry_ctrl = function.node_outputs(entry).iter().copied().next().unwrap();
    let mem = function.node_outputs(_mem).iter().copied().next().unwrap();
    function.graph_mut().create_node(NodeKind::Return, [entry_ctrl, mem, bad_value0], []);

    let errs = validate(&function, entry).unwrap_err();
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
    let mut function = Function::default();
    let entry = function.graph_mut().create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let init_mem = function.graph_mut().create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    let entry_ctrl = function.node_outputs(entry).iter().copied().next().unwrap();
    let mem = function.node_outputs(init_mem).iter().copied().next().unwrap();
    let cs = function.graph_mut().create_node(
        NodeKind::Region,
        [],
        [ValueKind::Control, ValueKind::PhiToken],
    );
    let cs_ctrl = function.node_outputs(cs).iter().copied().next().unwrap();
    // Return consumes entry's control (reaches Return via cfg_succs of Entry)
    // and cs_ctrl as a "ret value" (reaches Region via Return's backward-data).
    function.graph_mut().create_node(NodeKind::Return, [entry_ctrl, mem, cs_ctrl], []);

    let errs = validate(&function, entry).unwrap_err();
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
    // Zombie Region with zero inputs left behind by RegionCollapse is
    // expected; the validator must not flag it (this happens routinely on
    // real binaries after dead-branch elimination).
    let mut function = Function::default();
    let entry = function.graph_mut().create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let init_mem = function.graph_mut().create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    let entry_ctrl = function.node_outputs(entry).iter().copied().next().unwrap();
    let mem = function.node_outputs(init_mem).iter().copied().next().unwrap();
    // Zombie Region that nothing references — not reachable from entry.
    let _zombie_cs = function.graph_mut().create_node(
        NodeKind::Region,
        [],
        [ValueKind::Control, ValueKind::PhiToken],
    );
    let ret = function.graph_mut().create_node(NodeKind::Return, [entry_ctrl, mem], []);
    stamp(&mut function, ret);

    validate(&function, entry).expect("zombie Region must not trigger validation error");
}

/// IndirectBranch consumes (control, memory, target_value) and produces no
/// outputs; the validator must accept this exact shape.  IndirectBranch is
/// the lifter's placeholder for `RegionTerminator::UnresolvedIndirectBranch`
/// — it's mutated in-place by the indirect-branch resolver into a real
/// `Return` (LinkRegister) or replaced by a `Call+Return` pair (tail call).
#[test]
fn asm_fingerprint_check_off_by_default_accepts_empty_fingerprints() {
    // Opt-in is off → fully-empty fingerprints on a non-exempt node are OK.
    let mut function = Function::default();
    let entry = function.graph_mut().create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let _mem = function.graph_mut().create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    let _const_node = function.graph_mut().create_node(
        NodeKind::IntConst(7),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    // The IntConst is unreachable from entry; default validate ignores it.
    validate(&function, entry).expect("default validate is unaffected");
}

#[test]
fn asm_fingerprint_check_flags_reachable_non_exempt_empty() {
    // Opt-in is on → a reachable IntConst with no fingerprint is an error.
    let mut function = Function::default();
    let entry = function.graph_mut().create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let init_mem = function.graph_mut().create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    let entry_ctrl = function.node_outputs(entry).iter().copied().next().unwrap();
    let mem_value = function.node_outputs(init_mem).iter().copied().next().unwrap();
    let int_const = function.graph_mut().create_node(
        NodeKind::IntConst(7),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    let const_value = function.node_outputs(int_const).iter().copied().next().unwrap();
    // Return takes [ctrl, mem, ...values].
    let _ret = function.graph_mut().create_node(NodeKind::Return, [entry_ctrl, mem_value, const_value], []);
    let errs = validate(&function, entry).unwrap_err();
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
    let mut function = Function::default();
    let entry = function.graph_mut().create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let init_mem = function.graph_mut().create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    let entry_ctrl = function.node_outputs(entry).iter().copied().next().unwrap();
    let mem_value = function.node_outputs(init_mem).iter().copied().next().unwrap();
    let int_const = function.graph_mut().create_node(
        NodeKind::IntConst(7),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    let const_value = function.node_outputs(int_const).iter().copied().next().unwrap();
    let ret = function.graph_mut().create_node(NodeKind::Return, [entry_ctrl, mem_value, const_value], []);
    function.set_asm_fingerprint(int_const, vec![0x1000]);
    function.set_asm_fingerprint(ret, vec![0x1004]);
    validate(&function, entry).expect("populated fingerprints validate");
}

#[test]
fn asm_fingerprint_check_exempts_phis_and_initials() {
    // Build a tiny join: Entry → Region ← (mem? no, just one pred);
    // verify that Region/InitialMemory are exempt from the check.
    let mut function = Function::default();
    let entry = function.graph_mut().create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let init_mem = function.graph_mut().create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    let entry_ctrl = function.node_outputs(entry).iter().copied().next().unwrap();
    let cs = function.graph_mut().create_node(
        NodeKind::Region,
        [entry_ctrl],
        [ValueKind::Control, ValueKind::PhiToken],
    );
    let cs_ctrl = function.node_outputs(cs).iter().copied().next().unwrap();
    let mem_value = function.node_outputs(init_mem).iter().copied().next().unwrap();
    let _ret = function.graph_mut().create_node(NodeKind::Return, [cs_ctrl, mem_value], []);
    let res = validate(&function, entry);
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
    let mut function = Function::default();
    // Reachable spine: Entry → Return.
    let entry = function.graph_mut().create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let init_mem = function.graph_mut().create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    let entry_ctrl = function.node_outputs(entry).iter().copied().next().unwrap();
    let mem_value = function.node_outputs(init_mem).iter().copied().next().unwrap();
    let ret = function.graph_mut().create_node(NodeKind::Return, [entry_ctrl, mem_value], []);
    stamp(&mut function, ret);

    // Detached zombie: a Region whose input is a non-Control output
    // (an IntConst's value output).  This shape can be left behind by a
    // future pass that surgery-edits without scrubbing inputs.  The
    // node IS in the arena but is NOT reachable from `entry`.
    let int_const = function.graph_mut().create_node(
        NodeKind::IntConst(0x1234),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    let bogus_value = function.node_outputs(int_const).iter().copied().next().unwrap();
    let _zombie_cs = function.graph_mut().create_node(
        NodeKind::Region,
        [bogus_value],
        [ValueKind::Control, ValueKind::PhiToken],
    );

    // The unreachable zombie must be skipped by the reachability gate;
    // the validator must not flag a `RegionNonControlPredecessor`
    // error.  (Pre-fix this would have fired.)
    validate(&function, entry).expect(
        "unreachable Region zombies must not produce \
         RegionNonControlPredecessor errors",
    );
}

#[test]
fn indirect_branch_with_control_memory_and_value_validates() {
    let mut function = Function::default();
    let entry = function.graph_mut().create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let init_mem = function.graph_mut().create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    let entry_ctrl = function.node_outputs(entry).iter().copied().next().unwrap();
    let mem = function.node_outputs(init_mem).iter().copied().next().unwrap();
    let target = function.graph_mut().create_node(
        NodeKind::IntConst(0x1234),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    let target_val = function.node_outputs(target).iter().copied().next().unwrap();
    let ib = function.graph_mut().create_node(
        NodeKind::IndirectBranch,
        [entry_ctrl, mem, target_val],
        [],
    );
    stamp(&mut function, target);
    stamp(&mut function, ib);
    validate(&function, entry).expect("IndirectBranch with [ctrl, mem, target] must validate");
}

#[test]
fn graph_invariants_dangling_wide_const_id_detected() {
    use crate::wide_const::WideConstId;
    let mut function = Function::default();
    let entry = function.graph_mut().create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let mem = function.graph_mut().create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    let entry_ctrl = function.node_outputs(entry).iter().copied().next().unwrap();
    let mem_value = function.node_outputs(mem).iter().copied().next().unwrap();
    // Construct an IntConstWide pointing at an id that was never interned.
    let bogus_id = WideConstId::from_u32(99);
    let bogus = function.graph_mut().create_node(
        NodeKind::IntConstWide(bogus_id),
        [],
        [ValueKind::Typed(ValueType::I256)],
    );
    let bogus_value = function.node_outputs(bogus).iter().copied().next().unwrap();
    let _ret = function.graph_mut().create_node(NodeKind::Return, [entry_ctrl, mem_value, bogus_value], []);

    let errs = validate(&function, entry).unwrap_err();
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
    let mut function = Function::default();
    let entry = function.graph_mut().create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let mem = function.graph_mut().create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    let entry_ctrl = function.node_outputs(entry).iter().copied().next().unwrap();
    let mem_value = function.node_outputs(mem).iter().copied().next().unwrap();
    // Intern a I256 storage but assign it to a I512-typed output.
    let id = function.intern_wide_const(WideConstStorage::I256([0; 4]));
    let bad = function.graph_mut().create_node(
        NodeKind::IntConstWide(id),
        [],
        [ValueKind::Typed(ValueType::I512)],
    );
    let bad_value = function.node_outputs(bad).iter().copied().next().unwrap();
    let _ret = function.graph_mut().create_node(NodeKind::Return, [entry_ctrl, mem_value, bad_value], []);

    let errs = validate(&function, entry).unwrap_err();
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

#[test]
fn graph_invariants_wide_const_non_wide_output_type_detected() {
    use crate::wide_const::WideConstStorage;
    let mut function = Function::default();
    let entry = function.graph_mut().create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let mem = function.graph_mut().create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    let entry_ctrl = function.node_outputs(entry).iter().copied().next().unwrap();
    let mem_value = function.node_outputs(mem).iter().copied().next().unwrap();
    let id = function.intern_wide_const(WideConstStorage::I256([0; 4]));
    // IntConstWide declaring a non-wide (I64) output type — invalid: only
    // I256 / I512 are valid wide-const output types.  The validator must
    // report this as a distinct WideConstInvalidOutputType, not a width
    // mismatch with a misleading 0-byte "expected" size.
    let bad = function.graph_mut().create_node(
        NodeKind::IntConstWide(id),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    let bad_value = function.node_outputs(bad).iter().copied().next().unwrap();
    let _ret = function.graph_mut().create_node(NodeKind::Return, [entry_ctrl, mem_value, bad_value], []);

    let errs = validate(&function, entry).unwrap_err();
    assert!(
        errs.0.iter().any(|e| matches!(
            e,
            ValidationError::WideConstInvalidOutputType {
                output_type: ValueType::I64,
                ..
            }
        )),
        "expected WideConstInvalidOutputType for a non-wide output type, got: {errs:?}"
    );
}

// ── CC arity check ───────────────────────────────────────────────────────

/// Build a minimal Function that declares `ret_val_regs = [v1, v2]`.
/// Used by the cc-arity tests below.
fn fn_with_declared_cc() -> (Function, crate::node::NodeId) {
    let mut f = Function::default();
    let entry = f.graph_mut().create_node(NodeKind::Entry, [], [ValueKind::Control]);
    stamp(&mut f, entry);
    f.set_entry(entry);
    let mk_vn = |off: u64| rsleigh::Vn {
        addr_off: off,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 8,
    };
    // `ret_val_regs()` returns `default_cc.ret_val_regs` (chained with the
    // float ret list) verbatim; declare both the CC's ABI ret list and the
    // tracked set those vns live in so the function is internally consistent.
    let ret = vec![mk_vn(0x10), mk_vn(0x18)];
    f.all_vns = ret.clone();
    f.default_cc.ret_val_regs = ret;
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
    let mem = f.graph_mut().create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();
    stamp(&mut f, mem);
    let v1 = f.graph_mut().create_node(
        NodeKind::IntConst(7),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    let [v1_value] = f.node_outputs_exact::<1>(v1).unwrap();
    stamp(&mut f, v1);
    // Return with only ONE ret-val input — dropping v2's slot.
    let ret = f.graph_mut().create_node(NodeKind::Return, [ctrl, mem_value, v1_value], []);
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
fn cc_arity_catches_override_call_with_untagged_clobber_output() {
    // Function with EMPTY CC defaults but an override Call (identified by
    // a recorded `call_cc`).  The override arity invariant is "every output
    // slot past Control/Memory must be a tagged clobber output (carrying a
    // `value_vn`)".  Here the Call has one clobber output slot that was
    // never tagged, so the expected count (2 tagged-clobber-free outputs)
    // drifts from the actual output count (3) and must be flagged.
    let arch = strider_target::SleighArch::x86_64();
    let regs = arch.probe_regs().unwrap();
    let cc = strider_target::CallingConvention::x86_64_systemv()
        .unwrap()
        .build(&regs)
        .unwrap();

    let mut f = Function::default();
    let entry = f.graph_mut().create_node(NodeKind::Entry, [], [ValueKind::Control]);
    stamp(&mut f, entry);
    f.set_entry(entry);
    let [ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
    let mem = f.graph_mut().create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();
    stamp(&mut f, mem);
    let target = f.graph_mut().create_node(
        NodeKind::IntConst(0x1000),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    let [target_value] = f.node_outputs_exact::<1>(target).unwrap();
    stamp(&mut f, target);
    // Call has [Control, Memory, clobber] outputs (3) but the clobber
    // output is never tagged via `set_clobbered_vn`, so the expected count
    // (2 + 0 tagged) drifts from the actual (3).
    let call = f.graph_mut().create_node(
        NodeKind::Call,
        [ctrl, mem_value, target_value],
        [ValueKind::Control, ValueKind::Memory, ValueKind::Typed(ValueType::I64)],
    );
    stamp(&mut f, call);
    f.set_call_cc(call, cc);
    let [call_ctrl, call_mem, _clob] = f.node_outputs_exact::<3>(call).unwrap();
    let ret = f.graph_mut().create_node(NodeKind::Return, [call_ctrl, call_mem], []);
    stamp(&mut f, ret);

    let err = validate(&f, entry).expect_err("expected cc-arity violation for the Call override");
    assert!(
        err.0.iter().any(|e| matches!(
            e,
            ValidationError::NodeOutputCountMismatch { expected: 2, actual: 3, .. }
        )),
        "expected NodeOutputCountMismatch {{ expected: 2, actual: 3 }} for the Call, got: {err:?}"
    );
}

#[test]
fn cc_arity_passes_override_call_with_tagged_clobber_output() {
    // Counterpart: the same override Call but with its clobber output
    // properly tagged.  Expected (2 + 1 tagged) matches actual (3), so the
    // arity check passes.
    let arch = strider_target::SleighArch::x86_64();
    let regs = arch.probe_regs().unwrap();
    let cc = strider_target::CallingConvention::x86_64_systemv()
        .unwrap()
        .build(&regs)
        .unwrap();

    let mut f = Function::default();
    let entry = f.graph_mut().create_node(NodeKind::Entry, [], [ValueKind::Control]);
    stamp(&mut f, entry);
    f.set_entry(entry);
    let [ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
    let mem = f.graph_mut().create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();
    stamp(&mut f, mem);
    let target = f.graph_mut().create_node(
        NodeKind::IntConst(0x1000),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    let [target_value] = f.node_outputs_exact::<1>(target).unwrap();
    stamp(&mut f, target);
    let sp = f.graph_mut().create_node(
        NodeKind::IntConst(0x7fff_0000),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    let [sp_value] = f.node_outputs_exact::<1>(sp).unwrap();
    stamp(&mut f, sp);
    let call = f.graph_mut().create_node(
        NodeKind::Call,
        [ctrl, mem_value, target_value, sp_value],
        [ValueKind::Control, ValueKind::Memory, ValueKind::Typed(ValueType::I64)],
    );
    stamp(&mut f, call);
    f.set_call_cc(call, cc);
    let [call_ctrl, call_mem, clob] = f.node_outputs_exact::<3>(call).unwrap();
    let clob_vn = rsleigh::Vn { addr_off: 0x10, addr_space: rsleigh::VnSpace::REGISTER, size: 8 };
    f.set_clobbered_vn(clob, clob_vn);
    let ret = f.graph_mut().create_node(NodeKind::Return, [call_ctrl, call_mem], []);
    stamp(&mut f, ret);

    validate(&f, entry).expect("override Call with a tagged clobber output must validate");
}

#[test]
fn cc_arity_passes_when_return_matches_declared_ret_val_regs() {
    let (mut f, entry) = fn_with_declared_cc();
    let [ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
    let mem = f.graph_mut().create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();
    stamp(&mut f, mem);
    let v1 = f.graph_mut().create_node(
        NodeKind::IntConst(7),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    let [v1_value] = f.node_outputs_exact::<1>(v1).unwrap();
    stamp(&mut f, v1);
    let v2 = f.graph_mut().create_node(
        NodeKind::IntConst(8),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    let [v2_value] = f.node_outputs_exact::<1>(v2).unwrap();
    stamp(&mut f, v2);
    let ret = f.graph_mut().create_node(NodeKind::Return, [ctrl, mem_value, v1_value, v2_value], []);
    stamp(&mut f, ret);

    validate(&f, entry).expect("Return with declared 2 ret-val regs and 2 value inputs must validate");
}
