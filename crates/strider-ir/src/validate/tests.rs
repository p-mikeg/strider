use super::*;
use crate::node::{NodeId, NodeKind, ValueId, ValueKind, ValueType};

/// Sentinel asm-fingerprint base used by [`stamp`] below — distinct from
/// any real machine address.
const SENTINEL: u64 = 0xDEAD_BEEF_0000_0001;

/// Stamp a sentinel asm-fingerprint on `id` so the always-on Layer-C
/// asm-fingerprint check is satisfied for raw `Graph::create_node`-built
/// mock graphs.  Exempt kinds (`Entry`, `InitialMemory`, phis, etc.) can
/// be stamped harmlessly — the check skips them.
fn stamp(function: &mut Function, id: NodeId) {
    function.extend_asm_fingerprint(id, &[SENTINEL]);
}

/// The shared corruption-test prelude: a fresh [`Function`] with the
/// `Entry` + `InitialMemory` spine every reachable mock graph starts from,
/// plus the four handles the test bodies wire against.
struct Spine {
    f: Function,
    entry: NodeId,
    #[allow(dead_code)]
    mem: NodeId,
    entry_ctrl: ValueId,
    mem_value: ValueId,
}

fn spine() -> Spine {
    use crate::function::{test_function, test_initial_memory};
    let f = test_function();
    let entry = f.entry();
    let mem = test_initial_memory(&f);
    let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
    let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();
    Spine {
        f,
        entry,
        mem,
        entry_ctrl,
        mem_value,
    }
}

/// Create an `IntConst(v)` node typed `ty`, returning
/// `(node, value output)`.
fn int_const(f: &mut Function, v: u64, ty: ValueType) -> (NodeId, ValueId) {
    let id = f.intern_int_const(u128::from(v), ty);
    let n = f
        .graph_mut()
        .create_node(NodeKind::IntConst(id), [], [ValueKind::Typed(ty)]);
    let [value] = f.node_outputs_exact::<1>(n).unwrap();
    (n, value)
}

/// Assert `validate` fails with at least one error matching `pred`.
#[track_caller]
fn assert_validation_err(f: &Function, pred: impl Fn(&ValidationError) -> bool) {
    let errs = validate(f).unwrap_err();
    assert!(
        errs.0.iter().any(pred),
        "no validation error matched the predicate; got: {errs:?}"
    );
}

#[test]
fn empty_graph_with_entry_only() {
    let function = crate::function::test_function();
    assert!(validate(&function).is_ok());
}

#[test]
fn local_typing_wrong_input_kind_on_int_unary_op() {
    use crate::node::IntUnaryOp;

    let mut s = spine();
    // IntUnaryOp expects an Typed input, but we feed it a Control output.
    let _bad = s.f.graph_mut().create_node(
        NodeKind::IntUnaryOp(IntUnaryOp::Neg),
        [s.entry_ctrl],
        [ValueKind::Typed(ValueType::I64)],
    );

    assert_validation_err(&s.f, |e| {
        matches!(
            e,
            ValidationError::NodeInputKindMismatch { input_idx: 0, .. }
        )
    });
}

#[test]
fn local_typing_wrong_output_kind() {
    use crate::node::IntUnaryOp;

    let mut s = spine();
    let (_c, c_value) = int_const(&mut s.f, 3, ValueType::I64);
    // IntUnaryOp must produce a Typed output; make it produce Memory instead.
    let bad = s.f.graph_mut().create_node(
        NodeKind::IntUnaryOp(IntUnaryOp::Neg),
        [c_value],
        [ValueKind::Memory],
    );
    // Wire the bad node onto the reachable spine so the (reachability-scoped)
    // local-typing check visits it.
    let bad_value = s.f.node_outputs(bad).iter().copied().next().unwrap();
    let _ret =
        s.f.graph_mut()
            .create_node(NodeKind::Return, [s.entry_ctrl, s.mem_value, bad_value], []);

    assert_validation_err(&s.f, |e| {
        matches!(
            e,
            ValidationError::NodeOutputKindMismatch { output_idx: 0, .. }
        )
    });
}

#[test]
fn use_list_input_missing_from_use_list() {
    use crate::node::IntUnaryOp;

    let mut s = spine();
    let (_c, c_value) = int_const(&mut s.f, 3, ValueType::I64);
    let neg = s.f.graph_mut().create_node(
        NodeKind::IntUnaryOp(IntUnaryOp::Neg),
        [c_value],
        [ValueKind::Typed(ValueType::I64)],
    );

    // Corrupt the forward link: clear the IntConst output's head-of-use
    // pointer.  The op's input is still recorded, but the producer no
    // longer admits it as a consumer.
    s.f.graph_mut().corrupt_clear_first_use(c_value);

    // use-list consistency is reachability-scoped (matches the local-typing check and
    // check_graph_invariants_phis), so wire `neg` onto the reachable spine via
    // a Return that consumes Control + Memory + the value output.
    let neg_value = s.f.node_outputs(neg).iter().copied().next().unwrap();
    let _ret =
        s.f.graph_mut()
            .create_node(NodeKind::Return, [s.entry_ctrl, s.mem_value, neg_value], []);

    assert_validation_err(&s.f, |e| {
        matches!(
            e,
            ValidationError::InputMissingFromUseList { input_idx: 0, .. }
        )
    });
}

#[test]
fn use_list_stale_input_in_use_list() {
    use crate::node::IntUnaryOp;

    let mut s = spine();
    let (_a, a_value) = int_const(&mut s.f, 1, ValueType::I64);
    let (_b, b_value) = int_const(&mut s.f, 2, ValueType::I64);
    let neg = s.f.graph_mut().create_node(
        NodeKind::IntUnaryOp(IntUnaryOp::Neg),
        [a_value],
        [ValueKind::Typed(ValueType::I64)],
    );

    // Retarget the op's input at idx 0 to `b_value` without updating any
    // use-list.  `a_value`'s use-list still references this input, but the
    // input itself now points at `b_value` — that's a stale entry.
    let use_id = s.f.graph().node_input_id_at(neg, 0).unwrap();
    s.f.graph_mut().corrupt_retarget_input(use_id, b_value);

    // use-list consistency is reachability-scoped; wire `neg` AND `a` onto the
    // reachable spine.  `a_value` must be reachable so the use-list sweep
    // visits its (now-stale) head; otherwise the forward check on
    // `neg`'s input fires first as InputMissingFromUseList instead of
    // the intended UseListContainsStaleInput.  Threading both through
    // a 2-value Return keeps both producers in the reachable set.
    let neg_value = s.f.node_outputs(neg).iter().copied().next().unwrap();
    let _ret = s.f.graph_mut().create_node(
        NodeKind::Return,
        [s.entry_ctrl, s.mem_value, neg_value, a_value],
        [],
    );

    assert_validation_err(&s.f, |e| {
        matches!(e, ValidationError::UseListContainsStaleInput { .. })
    });
}

/// the use-list forward check must still flag missing-from-use-list cases
/// at non-zero input slots (covers the O(E) refactor — the existing
/// `use_list_input_missing_from_use_list` only covers slot 0).
#[test]
fn use_list_forward_check_catches_missing_at_non_zero_slot() {
    use crate::node::IntBinaryOp;

    let mut s = spine();
    let (_a, a_value) = int_const(&mut s.f, 11, ValueType::I64);
    let (_b, b_value) = int_const(&mut s.f, 13, ValueType::I64);

    // Add(a, b) — a at slot 0, b at slot 1.
    let add = s.f.graph_mut().create_node(
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        [a_value, b_value],
        [ValueKind::Typed(ValueType::I64)],
    );

    // Corrupt only b's use-list head, leaving a's intact.  Only the
    // slot-1 input should be flagged as missing.
    s.f.graph_mut().corrupt_clear_first_use(b_value);

    // use-list consistency is reachability-scoped; wire `add` onto the reachable
    // spine via Return[Ctrl, Memory, add_value].
    let add_value = s.f.node_outputs(add).iter().copied().next().unwrap();
    let _ret =
        s.f.graph_mut()
            .create_node(NodeKind::Return, [s.entry_ctrl, s.mem_value, add_value], []);

    let errs = validate(&s.f).unwrap_err();
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
    use crate::node::IntUnaryOp;

    let mut s = spine();
    // Detached / unreachable producer + consumer pair.  Corrupt their
    // use-list link so that, were the use-list check graph-wide, it would fire.
    let (_c, c_value) = int_const(&mut s.f, 7, ValueType::I64);
    let _zombie_consumer = s.f.graph_mut().create_node(
        NodeKind::IntUnaryOp(IntUnaryOp::Neg),
        [c_value],
        [ValueKind::Typed(ValueType::I64)],
    );
    s.f.graph_mut().corrupt_clear_first_use(c_value); // Would fire the use-list check graph-wide.

    // Minimal reachable spine — entry + memory + a Return that takes
    // no values.  Neither `c` nor `_zombie_consumer` is reachable.
    let ret =
        s.f.graph_mut()
            .create_node(NodeKind::Return, [s.entry_ctrl, s.mem_value], []);
    stamp(&mut s.f, ret);

    validate(&s.f).expect("validator must skip unreachable use-list inconsistencies");
}

// `MissingInitialMemoryNode` was verified via a test that built an Entry-only
// graph.  `Function::new` now always builds the Entry + InitialMemory spine, so
// an InitialMemory-less function is unconstructable; the validator check
// remains as defence-in-depth.

// `MultipleEntryNodes` / `MultipleInitialMemoryNodes` were verified via tests
// that called `create_node` twice and expected the validator to flag the
// duplicate.  Once Entry and InitialMemory became cacheable, dedup makes the
// "duplicate" construction structurally impossible from any code path that
// goes through `create_node`.  The validator checks themselves remain as
// defence-in-depth against future graph-construction bugs (e.g. compact()
// ordering issues that resurrect a stale node).

#[test]
fn graph_invariants_entry_dedupes_on_repeated_create() {
    let mut s = spine();
    let entry2 =
        s.f.graph_mut()
            .create_node(NodeKind::Entry, [], [ValueKind::Control]);
    assert_eq!(s.entry, entry2, "Entry must dedup");
    validate(&s.f).expect("graph with single deduped Entry must validate");
}

#[test]
fn graph_invariants_initial_memory_dedupes_on_repeated_create() {
    let mut s = spine();
    let mem2 =
        s.f.graph_mut()
            .create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    assert_eq!(s.mem, mem2, "InitialMemory must dedup");
    validate(&s.f).expect("graph with single deduped InitialMemory must validate");
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
    let mut s = spine();

    // Region with [Control, Memory] inputs — input[1] is wrong.
    let bad_cs = s.f.graph_mut().create_node(
        NodeKind::Region,
        [s.entry_ctrl, s.mem_value],
        [ValueKind::Control, ValueKind::PhiToken],
    );
    let bad_cs_ctrl = s.f.node_outputs(bad_cs).iter().copied().next().unwrap();
    let _ret =
        s.f.graph_mut()
            .create_node(NodeKind::Return, [bad_cs_ctrl, s.mem_value], []);

    assert_validation_err(&s.f, |e| {
        matches!(
            e,
            ValidationError::RegionNonControlPredecessor { input_idx: 1, .. }
        )
    });
}

fn test_vn() -> rsleigh::Vn {
    rsleigh::Vn {
        addr_off: 0x20,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 4,
    }
}

/// IR-5: the validator flags an `initial_var_index` entry that points at a
/// REACHABLE node whose payload was rewritten away from `InitialVar(vn)`.  The
/// NodeId survives (so `compact` keeps the entry), but the index no longer
/// describes the live graph.
#[test]
fn validate_flags_stale_initial_var_index_entry() {
    let mut s = spine();
    let vn = test_vn();
    let other_vn = rsleigh::Vn {
        addr_off: 0x40,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 4,
    };
    // Two tracked varnodes so `InitialVnId` 0/1 resolve to vn/other_vn.
    s.f.all_vns = vec![vn, other_vn];
    // A reachable InitialVar(vn): its value output is returned so the walk
    // keeps it in the reachable set.
    let iv = s.f.graph_mut().create_node(
        NodeKind::InitialVar(crate::node::InitialVnId::from_index(0)),
        [],
        [ValueKind::Typed(ValueType::I32)],
    );
    stamp(&mut s.f, iv);
    let iv_value = s.f.node_outputs(iv)[0];
    s.f.side_tables.initial_var_index.insert(vn, iv);
    let ret =
        s.f.graph_mut()
            .create_node(NodeKind::Return, [s.entry_ctrl, s.mem_value, iv_value], []);
    stamp(&mut s.f, ret);
    // Valid so far.
    validate(&s.f).expect("a well-formed initial_var_index entry validates");

    // Rewrite the node's payload IN PLACE (NodeId survives) to a DIFFERENT
    // InitialVar varnode (index 1 → other_vn), so the index entry for `vn` is
    // now stale.
    *s.f.graph_mut().node_kind_mut(iv) =
        NodeKind::InitialVar(crate::node::InitialVnId::from_index(1));

    assert_validation_err(&s.f, |e| {
        matches!(
            e,
            ValidationError::StaleInitialVarIndex { node, vn: indexed_vn, .. }
                if *node == iv && *indexed_vn == vn
        )
    });
}

/// IR-5: a `value_vn` tag on a value whose REACHABLE producer is not a
/// Phi / Call / CallOther is flagged (the tag's documented populations are
/// exactly those three).
#[test]
fn validate_flags_stale_value_vn_entry() {
    let mut s = spine();
    let vn = test_vn();
    // Tag an IntConst output with a value_vn — IntConst is NOT a valid tag
    // population, so the producer-kind check must flag it.
    let (k, kv) = int_const(&mut s.f, 7, ValueType::I32);
    stamp(&mut s.f, k);
    s.f.side_tables.value_vn.insert(kv, vn);
    // Make it reachable via the Return.
    let ret =
        s.f.graph_mut()
            .create_node(NodeKind::Return, [s.entry_ctrl, s.mem_value, kv], []);
    stamp(&mut s.f, ret);

    assert_validation_err(&s.f, |e| {
        matches!(
            e,
            ValidationError::StaleValueVn { value, .. } if *value == kv
        )
    });
}

#[test]
fn graph_invariants_phi_token_from_wrong_node() {
    let mut s = spine();
    let cs = s.f.graph_mut().create_node(
        NodeKind::Region,
        [s.entry_ctrl],
        [ValueKind::Control, ValueKind::PhiToken],
    );
    let cs_control_value = s.f.node_outputs(cs).iter().copied().next().unwrap(); // index 0 = Control
    let vn = test_vn();
    let phi = s.f.graph_mut().create_node(
        NodeKind::Phi,
        [cs_control_value],
        [ValueKind::Typed(ValueType::I64)],
    );
    let phi_value = s.f.node_outputs(phi)[0];
    s.f.side_tables.value_vn.insert(phi_value, vn);

    assert_validation_err(&s.f, |e| {
        matches!(e, ValidationError::PhiTokenNotFromRegion { .. })
    });
}

#[test]
fn graph_invariants_phi_value_arity_mismatch() {
    let mut s = spine();
    let cs = s.f.graph_mut().create_node(
        NodeKind::Region,
        [s.entry_ctrl],
        [ValueKind::Control, ValueKind::PhiToken],
    );
    let cs_phi_value = s.f.node_outputs(cs).iter().copied().nth(1).unwrap();

    let (_c1, c1_value) = int_const(&mut s.f, 1, ValueType::I64);
    let (_c2, c2_value) = int_const(&mut s.f, 2, ValueType::I64);
    let vn = test_vn();
    let phi = s.f.graph_mut().create_node(
        NodeKind::Phi,
        [cs_phi_value, c1_value, c2_value],
        [ValueKind::Typed(ValueType::I64)],
    );
    let phi_value = s.f.node_outputs(phi)[0];
    s.f.side_tables.value_vn.insert(phi_value, vn);

    // V-2: graph_invariants_phis is reachability-scoped, so the phi must be
    // attached to something reachable from the entry.  Wire its value
    // output through a Return that consumes the Region's Control
    // output too — this puts the phi on the cfg-reachable spine.
    let cs_ctrl_value = s.f.node_outputs(cs).iter().copied().next().unwrap();
    let phi_val_value = s.f.node_outputs(phi).iter().copied().next().unwrap();
    let ret = s.f.graph_mut().create_node(NodeKind::Return, [], []);
    s.f.graph_mut().add_node_input(ret, cs_ctrl_value);
    s.f.graph_mut().add_node_input(ret, phi_val_value);

    assert_validation_err(&s.f, |e| {
        matches!(
            e,
            ValidationError::PhiValueArityMismatch {
                expected_predecessors: 1,
                actual_values: 2,
                ..
            }
        )
    });
}

#[test]
fn graph_invariants_phi_input_type_mismatch() {
    let mut s = spine();
    let cs = s.f.graph_mut().create_node(
        NodeKind::Region,
        [s.entry_ctrl],
        [ValueKind::Control, ValueKind::PhiToken],
    );
    let cs_phi_value = s.f.node_outputs(cs).iter().copied().nth(1).unwrap();

    // One value input typed I8 but the phi declares output I64 — a type
    // mismatch: a value phi must merge values of a single type.
    let (_c1, c1_value) = int_const(&mut s.f, 1, ValueType::I8);
    let phi = s.f.graph_mut().create_node(
        NodeKind::Phi,
        [cs_phi_value, c1_value],
        [ValueKind::Typed(ValueType::I64)],
    );
    let phi_value = s.f.node_outputs(phi)[0];
    s.f.side_tables.value_vn.insert(phi_value, test_vn());

    // Put the phi on the reachable spine (see the arity test above).
    let cs_ctrl_value = s.f.node_outputs(cs).iter().copied().next().unwrap();
    let phi_val_value = s.f.node_outputs(phi).iter().copied().next().unwrap();
    let ret = s.f.graph_mut().create_node(NodeKind::Return, [], []);
    s.f.graph_mut().add_node_input(ret, cs_ctrl_value);
    s.f.graph_mut().add_node_input(ret, phi_val_value);

    assert_validation_err(&s.f, |e| {
        matches!(
            e,
            ValidationError::PhiInputTypeMismatch {
                input_index: 1,
                output_ty: ValueType::I64,
                input_ty: ValueType::I8,
                ..
            }
        )
    });
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
    let mut s = spine();
    // Return needs Ctrl + Memory inputs (per node_signature: [CTRL, MEM]).
    let ret =
        s.f.graph_mut()
            .create_node(NodeKind::Return, [s.entry_ctrl, s.mem_value], []);
    stamp(&mut s.f, ret);

    // Detached zombie Phi with NO inputs.
    let vn = test_vn();
    let zombie =
        s.f.graph_mut()
            .create_node(NodeKind::Phi, [], [ValueKind::Typed(ValueType::I64)]);
    let zombie_value = s.f.node_outputs(zombie)[0];
    s.f.side_tables.value_vn.insert(zombie_value, vn);

    validate(&s.f).expect("validator must skip unreachable zombie phis");
}

#[test]
fn local_typing_wrong_input_count() {
    use crate::node::IntBinaryOp;

    let mut s = spine();
    let (_c, c_value) = int_const(&mut s.f, 5, ValueType::I64);

    // IntBinaryOp expects 2 inputs; give it 1.
    let bad = s.f.graph_mut().create_node(
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        [c_value],
        [ValueKind::Typed(ValueType::I64)],
    );
    let bad_value = s.f.node_outputs(bad).iter().copied().next().unwrap();

    // Wire `bad` into the reachable sub-graph so the reachability-scoped
    // the local-typing check actually inspects it.  A Return consuming entry's Control
    // plus `bad`'s value output is the smallest reachable shape.
    let _ret =
        s.f.graph_mut()
            .create_node(NodeKind::Return, [s.entry_ctrl, bad_value], []);

    assert_validation_err(&s.f, |e| {
        matches!(
            e,
            ValidationError::NodeInputCountMismatch {
                expected: 2,
                actual: 1,
                ..
            }
        )
    });
}

/// Regression: the local-typing check must check variadic input tails, not just the fixed
/// head prefix. A `MemPhi` whose per-predecessor inputs are not Memory
/// (e.g. a Control token leaks through) used to slip past validation
/// because the variadic-tail kind check was elided.
#[test]
fn local_typing_mem_phi_variadic_tail_must_be_memory() {
    let mut s = spine();

    // Region with one valid Control predecessor (entry).
    let cs = s.f.graph_mut().create_node(
        NodeKind::Region,
        [s.entry_ctrl],
        [ValueKind::Control, ValueKind::PhiToken],
    );
    let cs_outputs: Vec<_> = s.f.node_outputs(cs).to_vec();
    let cs_ctrl = cs_outputs[0];
    let cs_phi_token = cs_outputs[1];

    // MemPhi with: phi_token (correct PHI kind), then a Control output as
    // its variadic predecessor (WRONG — should be Memory).
    let bad_mem_phi = s.f.graph_mut().create_node(
        NodeKind::MemPhi,
        [cs_phi_token, s.entry_ctrl],
        [ValueKind::Memory],
    );
    let bad_mem_value =
        s.f.node_outputs(bad_mem_phi)
            .iter()
            .copied()
            .next()
            .unwrap();

    // Reach the MemPhi via a Return so the local-typing check walks to it.
    s.f.graph_mut()
        .create_node(NodeKind::Return, [cs_ctrl, bad_mem_value], []);

    assert_validation_err(&s.f, |e| {
        matches!(
            e,
            ValidationError::NodeInputKindMismatch { input_idx: 1, .. }
        )
    });
}

#[test]
fn local_typing_accepts_bool_value_phi_inputs() {
    // Phi value inputs (the IN_PHI variadic tail) must accept
    // Bool-typed values: real binaries phi-merge x86 flag registers
    // (CF/ZF/SF), which the IR models as Bool. Same rationale as ARG/RET/CALL_OUT.
    let mut s = spine();

    let cs = s.f.graph_mut().create_node(
        NodeKind::Region,
        [s.entry_ctrl],
        [ValueKind::Control, ValueKind::PhiToken],
    );
    let cs_ctrl = s.f.node_outputs(cs).iter().copied().next().unwrap();
    let phi_token = s.f.node_outputs(cs).iter().copied().nth(1).unwrap();

    let (bc, bc_value) = int_const(&mut s.f, 1, ValueType::I1);

    // Anonymous Phi taking [phi_token, bool_value] — the Bool flows through IN_PHI.
    let vp = s.f.graph_mut().create_node(
        NodeKind::Phi,
        [phi_token, bc_value],
        [ValueKind::Typed(ValueType::I1)],
    );
    let vp_value = s.f.node_outputs(vp).iter().copied().next().unwrap();

    // Use the phi'd value so the validator's reachability walk hits it.
    let ret =
        s.f.graph_mut()
            .create_node(NodeKind::Return, [cs_ctrl, s.mem_value, vp_value], []);
    stamp(&mut s.f, bc);
    stamp(&mut s.f, ret);

    validate(&s.f).expect("Bool-typed value phi inputs must validate");
}

#[test]
fn graph_invariants_mem_phi_arity_mismatch() {
    let mut s = spine();
    let cs = s.f.graph_mut().create_node(
        NodeKind::Region,
        [s.entry_ctrl],
        [ValueKind::Control, ValueKind::PhiToken],
    );
    let cs_phi_value = s.f.node_outputs(cs).iter().copied().nth(1).unwrap();
    let cs_ctrl_value = s.f.node_outputs(cs).iter().copied().next().unwrap();

    // MemPhi with two memory inputs but the owning Region has one predecessor.
    let mem_phi = s.f.graph_mut().create_node(
        NodeKind::MemPhi,
        [cs_phi_value, s.mem_value, s.mem_value],
        [ValueKind::Memory],
    );
    let mem_phi_value = s.f.node_outputs(mem_phi).iter().copied().next().unwrap();
    s.f.graph_mut()
        .create_node(NodeKind::Return, [cs_ctrl_value, mem_phi_value], []);

    assert_validation_err(&s.f, |e| {
        matches!(
            e,
            ValidationError::PhiValueArityMismatch {
                expected_predecessors: 1,
                actual_values: 2,
                ..
            }
        )
    });
}

#[test]
fn graph_invariants_value_phi_arity_mismatch() {
    let mut s = spine();
    let cs = s.f.graph_mut().create_node(
        NodeKind::Region,
        [s.entry_ctrl],
        [ValueKind::Control, ValueKind::PhiToken],
    );
    let cs_phi_value = s.f.node_outputs(cs).iter().copied().nth(1).unwrap();
    let cs_ctrl_value = s.f.node_outputs(cs).iter().copied().next().unwrap();

    let (_c1, c1_value) = int_const(&mut s.f, 1, ValueType::I64);

    // Anonymous Phi with two value inputs but the owning Region has one predecessor.
    let vp = s.f.graph_mut().create_node(
        NodeKind::Phi,
        [cs_phi_value, c1_value, c1_value],
        [ValueKind::Typed(ValueType::I64)],
    );
    let vp_value = s.f.node_outputs(vp).iter().copied().next().unwrap();
    s.f.graph_mut()
        .create_node(NodeKind::Return, [cs_ctrl_value, s.mem_value, vp_value], []);

    assert_validation_err(&s.f, |e| {
        matches!(
            e,
            ValidationError::PhiValueArityMismatch {
                expected_predecessors: 1,
                actual_values: 2,
                ..
            }
        )
    });
}

#[test]
fn local_typing_rejects_wrong_output_count() {
    let mut s = spine();
    // IntConst expects exactly one output but we give it two.
    let id = s.f.intern_int_const(0, ValueType::I64);
    let bad = s.f.graph_mut().create_node(
        NodeKind::IntConst(id),
        [],
        [
            ValueKind::Typed(ValueType::I64),
            ValueKind::Typed(ValueType::I64),
        ],
    );
    let bad_value0 = s.f.node_outputs(bad).iter().copied().next().unwrap();
    s.f.graph_mut().create_node(
        NodeKind::Return,
        [s.entry_ctrl, s.mem_value, bad_value0],
        [],
    );

    assert_validation_err(
        &s.f,
        |e| matches!(e, ValidationError::NodeOutputCountMismatch { node, expected: 1, actual: 2 } if *node == bad),
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
    let mut s = spine();
    let cs = s.f.graph_mut().create_node(
        NodeKind::Region,
        [],
        [ValueKind::Control, ValueKind::PhiToken],
    );
    let cs_ctrl = s.f.node_outputs(cs).iter().copied().next().unwrap();
    // Return consumes entry's control (reaches Return via cfg_succs of Entry)
    // and cs_ctrl as a "ret value" (reaches Region via Return's backward-data).
    s.f.graph_mut()
        .create_node(NodeKind::Return, [s.entry_ctrl, s.mem_value, cs_ctrl], []);

    assert_validation_err(
        &s.f,
        |e| matches!(e, ValidationError::EmptyRegionPredecessors { region } if *region == cs),
    );
}

#[test]
fn graph_invariants_tolerates_unreachable_zero_predecessor_region() {
    // Zombie Region with zero inputs left behind by RegionCollapse is
    // expected; the validator must not flag it (this happens routinely on
    // real binaries after dead-branch elimination).
    let mut s = spine();
    // Zombie Region that nothing references — not reachable from entry.
    let _zombie_cs = s.f.graph_mut().create_node(
        NodeKind::Region,
        [],
        [ValueKind::Control, ValueKind::PhiToken],
    );
    let ret =
        s.f.graph_mut()
            .create_node(NodeKind::Return, [s.entry_ctrl, s.mem_value], []);
    stamp(&mut s.f, ret);

    validate(&s.f).expect("zombie Region must not trigger validation error");
}

/// IndirectBranch consumes (control, memory, target_value) and produces no
/// outputs; the validator must accept this exact shape.  IndirectBranch is
/// the lifter's placeholder for `RegionTerminator::UnresolvedIndirectBranch`
/// — it's mutated in-place by the indirect-branch resolver into a real
/// `Return` (LinkRegister) or replaced by a `Call+Return` pair (tail call).
#[test]
fn asm_fingerprint_check_off_by_default_accepts_empty_fingerprints() {
    // Opt-in is off → fully-empty fingerprints on a non-exempt node are OK.
    let mut s = spine();
    let _const_node = int_const(&mut s.f, 7, ValueType::I64);
    // The IntConst is unreachable from entry; default validate ignores it.
    validate(&s.f).expect("default validate is unaffected");
}

#[test]
fn asm_fingerprint_check_flags_reachable_non_exempt_empty() {
    // Opt-in is on → a reachable IntConst with no fingerprint is an error.
    let mut s = spine();
    let (_c, const_value) = int_const(&mut s.f, 7, ValueType::I64);
    // Return takes [ctrl, mem, ...values].
    let _ret = s.f.graph_mut().create_node(
        NodeKind::Return,
        [s.entry_ctrl, s.mem_value, const_value],
        [],
    );
    assert_validation_err(&s.f, |e| {
        matches!(
            e,
            ValidationError::MissingAsmFingerprint {
                kind: NodeKind::IntConst(_),
                ..
            }
        )
    });
    assert_validation_err(&s.f, |e| {
        matches!(
            e,
            ValidationError::MissingAsmFingerprint {
                kind: NodeKind::Return,
                ..
            }
        )
    });
}

#[test]
fn asm_fingerprint_check_accepts_when_fingerprint_present() {
    let mut s = spine();
    let (int_const_node, const_value) = int_const(&mut s.f, 7, ValueType::I64);
    let ret = s.f.graph_mut().create_node(
        NodeKind::Return,
        [s.entry_ctrl, s.mem_value, const_value],
        [],
    );
    s.f.extend_asm_fingerprint(int_const_node, &[0x1000]);
    s.f.extend_asm_fingerprint(ret, &[0x1004]);
    validate(&s.f).expect("populated fingerprints validate");
}

#[test]
fn asm_fingerprint_check_exempts_phis_and_initials() {
    // Build a tiny join: Entry → Region ← (mem? no, just one pred);
    // verify that Region/InitialMemory are exempt from the check.
    let mut s = spine();
    let cs = s.f.graph_mut().create_node(
        NodeKind::Region,
        [s.entry_ctrl],
        [ValueKind::Control, ValueKind::PhiToken],
    );
    let cs_ctrl = s.f.node_outputs(cs).iter().copied().next().unwrap();
    let _ret =
        s.f.graph_mut()
            .create_node(NodeKind::Return, [cs_ctrl, s.mem_value], []);
    let res = validate(&s.f);
    // The Return is reachable and non-exempt — it must be flagged.  But
    // Region / Entry / InitialMemory must NOT be flagged.
    let errs = res.unwrap_err();
    for e in &errs.0 {
        if let ValidationError::MissingAsmFingerprint { kind, .. } = e {
            assert!(
                !matches!(
                    kind,
                    NodeKind::Entry | NodeKind::InitialMemory | NodeKind::Region
                ),
                "exempt kind {kind:?} was flagged"
            );
        }
    }
    // Sanity: at least one MissingAsmFingerprint for the Return.
    assert!(
        errs.0.iter().any(|e| matches!(
            e,
            ValidationError::MissingAsmFingerprint {
                kind: NodeKind::Return,
                ..
            }
        )),
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
    let mut s = spine();
    // Reachable spine: Entry → Return.
    let ret =
        s.f.graph_mut()
            .create_node(NodeKind::Return, [s.entry_ctrl, s.mem_value], []);
    stamp(&mut s.f, ret);

    // Detached zombie: a Region whose input is a non-Control output
    // (an IntConst's value output).  This shape can be left behind by a
    // future pass that surgery-edits without scrubbing inputs.  The
    // node IS in the arena but is NOT reachable from `entry`.
    let (_int_const, bogus_value) = int_const(&mut s.f, 0x1234, ValueType::I64);
    let _zombie_cs = s.f.graph_mut().create_node(
        NodeKind::Region,
        [bogus_value],
        [ValueKind::Control, ValueKind::PhiToken],
    );

    // The unreachable zombie must be skipped by the reachability gate;
    // the validator must not flag a `RegionNonControlPredecessor`
    // error.  (Pre-fix this would have fired.)
    validate(&s.f).expect(
        "unreachable Region zombies must not produce \
         RegionNonControlPredecessor errors",
    );
}

#[test]
fn indirect_branch_with_control_memory_and_value_validates() {
    let mut s = spine();
    let (target, target_val) = int_const(&mut s.f, 0x1234, ValueType::I64);
    let ib = s.f.graph_mut().create_node(
        NodeKind::IndirectBranch,
        [s.entry_ctrl, s.mem_value, target_val],
        [],
    );
    stamp(&mut s.f, target);
    stamp(&mut s.f, ib);
    validate(&s.f).expect("IndirectBranch with [ctrl, mem, target] must validate");
}

#[test]
fn graph_invariants_dangling_const_id_detected() {
    use crate::const_value::ConstId;
    use cranelift_entity::EntityRef;
    let mut s = spine();
    // Construct an IntConst pointing at an id that was never interned.
    let bogus_id = ConstId::new(99);
    let bogus = s.f.graph_mut().create_node(
        NodeKind::IntConst(bogus_id),
        [],
        [ValueKind::Typed(ValueType::I256)],
    );
    let bogus_value = s.f.node_outputs(bogus).iter().copied().next().unwrap();
    let _ret = s.f.graph_mut().create_node(
        NodeKind::Return,
        [s.entry_ctrl, s.mem_value, bogus_value],
        [],
    );

    assert_validation_err(&s.f, |e| {
        matches!(e, ValidationError::DanglingConstId { .. })
    });
}

#[test]
fn graph_invariants_wide_const_width_mismatch_detected() {
    use crate::const_value::ConstValue;
    let mut s = spine();
    // Intern a genuinely-wide (> u128, 4 limbs) value but assign it to a
    // narrower (I64) output — bits set above the declared width.
    let id =
        s.f.intern_const(ConstValue::Wide(vec![0, 0, 0, 1].into_boxed_slice()));
    let bad = s.f.graph_mut().create_node(
        NodeKind::IntConst(id),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    let bad_value = s.f.node_outputs(bad).iter().copied().next().unwrap();
    let _ret =
        s.f.graph_mut()
            .create_node(NodeKind::Return, [s.entry_ctrl, s.mem_value, bad_value], []);

    assert_validation_err(&s.f, |e| {
        matches!(e, ValidationError::ConstWidthMismatch { .. })
    });
}

#[test]
fn graph_invariants_const_bits_above_declared_width_detected() {
    use crate::const_value::ConstValue;
    let mut s = spine();
    // A `Bits` value with bits set above the declared I8 width (un-masked) is
    // non-canonical: the validator flags it as a width mismatch.
    let id = s.f.intern_const(ConstValue::Bits(0x1FF));
    let bad = s.f.graph_mut().create_node(
        NodeKind::IntConst(id),
        [],
        [ValueKind::Typed(ValueType::I8)],
    );
    let bad_value = s.f.node_outputs(bad).iter().copied().next().unwrap();
    let _ret =
        s.f.graph_mut()
            .create_node(NodeKind::Return, [s.entry_ctrl, s.mem_value, bad_value], []);

    assert_validation_err(&s.f, |e| {
        matches!(e, ValidationError::ConstWidthMismatch { .. })
    });
}

// ── CC arity check ───────────────────────────────────────────────────────

/// Build a minimal Function that declares `ret_val_regs = [v1, v2]`.
/// Used by the cc-arity tests below.
fn fn_with_declared_cc() -> (Function, NodeId) {
    let mut f = crate::function::test_function();
    let entry = f.entry();
    stamp(&mut f, entry);
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
    let mem = f
        .graph_mut()
        .create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();
    stamp(&mut f, mem);
    let (v1, v1_value) = int_const(&mut f, 7, ValueType::I64);
    stamp(&mut f, v1);
    // Return with only ONE ret-val input — dropping v2's slot.
    let ret = f
        .graph_mut()
        .create_node(NodeKind::Return, [ctrl, mem_value, v1_value], []);
    stamp(&mut f, ret);

    assert_validation_err(&f, |e| {
        matches!(
            e,
            ValidationError::NodeInputCountMismatch {
                expected: 4,
                actual: 3,
                ..
            }
        )
    });
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

    let mut f = crate::function::test_function();
    let entry = f.entry();
    stamp(&mut f, entry);
    let [ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
    let mem = f
        .graph_mut()
        .create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();
    stamp(&mut f, mem);
    let (target, target_value) = int_const(&mut f, 0x1000, ValueType::I64);
    stamp(&mut f, target);
    // Call has [Control, Memory, clobber] outputs (3) but the clobber
    // output is never tagged via `set_vn_for_value`, so the expected count
    // (2 + 0 tagged) drifts from the actual (3).
    let call = f.graph_mut().create_node(
        NodeKind::Call,
        [ctrl, mem_value, target_value],
        [
            ValueKind::Control,
            ValueKind::Memory,
            ValueKind::Typed(ValueType::I64),
        ],
    );
    stamp(&mut f, call);
    f.set_call_cc(call, cc);
    let [call_ctrl, call_mem, _clob] = f.node_outputs_exact::<3>(call).unwrap();
    let ret = f
        .graph_mut()
        .create_node(NodeKind::Return, [call_ctrl, call_mem], []);
    stamp(&mut f, ret);

    assert_validation_err(&f, |e| {
        matches!(
            e,
            ValidationError::NodeOutputCountMismatch {
                expected: 2,
                actual: 3,
                ..
            }
        )
    });
}

#[test]
fn cc_arity_passes_override_call_with_tagged_clobber_output() {
    // Counterpart: an override Call whose single clobber output matches the
    // CC's clobber list.  The CC clobbers exactly one tracked register, so the
    // CC-derived expected count (2 + 0 ret + 1 clobber = 3) matches the actual
    // (3) and the arity check passes.
    let clob0 = rsleigh::Vn {
        addr_off: 0x10,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 8,
    };
    let (mut f, _entry, ctrl, mem_value, cc) = fn_with_override_clobber_cc(&[clob0]);
    let (target, target_value) = int_const(&mut f, 0x1000, ValueType::I64);
    stamp(&mut f, target);
    let (sp, sp_value) = int_const(&mut f, 0x7fff_0000, ValueType::I64);
    stamp(&mut f, sp);
    let call = f.graph_mut().create_node(
        NodeKind::Call,
        [ctrl, mem_value, target_value, sp_value],
        [
            ValueKind::Control,
            ValueKind::Memory,
            ValueKind::Typed(ValueType::I64),
        ],
    );
    stamp(&mut f, call);
    f.set_call_cc(call, cc);
    let [call_ctrl, call_mem, clob] = f.node_outputs_exact::<3>(call).unwrap();
    f.side_tables.value_vn.insert(clob, clob0);
    let ret = f
        .graph_mut()
        .create_node(NodeKind::Return, [call_ctrl, call_mem], []);
    stamp(&mut f, ret);

    validate(&f).expect("override Call with a tagged clobber output must validate");
}

/// Build a `Function` tracking `clobbers` (REGISTER vns), with an override
/// CC that clobbers all of them (no callee-saved / ret-val / stack
/// overlap). `call_clobbered_for(cc)` projected onto this tracked set
/// returns `clobbers`, giving the validator an independent expected count.
fn fn_with_override_clobber_cc(
    clobbers: &[rsleigh::Vn],
) -> (
    Function,
    NodeId,
    ValueId,
    ValueId,
    strider_target::BuiltCallingConvention,
) {
    let mut f = crate::function::test_function();
    let entry = f.entry();
    stamp(&mut f, entry);
    let [ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
    let mem = f
        .graph_mut()
        .create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();
    stamp(&mut f, mem);
    // Track the clobber regs so call_clobbered_for(cc) (which filters on the
    // tracked all_vns) returns them. A distinct stack vn keeps SP out of the
    // clobber set.
    let sp = rsleigh::Vn {
        addr_off: 0x7000,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 8,
    };
    f.all_vns = clobbers.to_vec();
    let cc = strider_target::BuiltCallingConvention::try_new(
        vec![], // arg_passing_regs
        vec![], // callee_saved_regs (none → every tracked reg clobbers)
        vec![], // ret_val_regs
        vec![], // ret_val_regs_float
        sp,     // stack_vn
        None,   // stack_args
        0,      // ret_stack_pop
        None,   // link_register_vn
        false,  // preserves_memory
    )
    .unwrap();
    (f, entry, ctrl, mem_value, cc)
}

/// The override-Call arity check must cross-check against the CC-derived
/// clobber/ret-val counts — NOT against the Call's own `value_vn` tags.
/// Dropping a clobber output slot together with its tag leaves the
/// tag-derived expected count equal to the (now too-small) actual count, so
/// a tag-only check passes a wrong-arity override Call silently. Deriving
/// expected from `call_clobbered_for(cc)` catches it.
#[test]
fn cc_arity_catches_override_call_dropping_a_clobber_output() {
    let clob0 = rsleigh::Vn {
        addr_off: 0x10,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 8,
    };
    let clob1 = rsleigh::Vn {
        addr_off: 0x18,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 8,
    };
    let (mut f, _entry, ctrl, mem_value, cc) = fn_with_override_clobber_cc(&[clob0, clob1]);
    let (target, target_value) = int_const(&mut f, 0x1000, ValueType::I64);
    stamp(&mut f, target);
    let (sp, sp_value) = int_const(&mut f, 0x7fff_0000, ValueType::I64);
    stamp(&mut f, sp);
    // A WRONG-ARITY override Call: the CC clobbers two regs (so a correct
    // Call has 4 outputs: Control, Memory, clob0, clob1), but this Call has
    // only ONE clobber output — and it IS tagged. A tag-derived expected
    // count (2 + 1 tag = 3) would match the actual (3) and pass silently.
    let call = f.graph_mut().create_node(
        NodeKind::Call,
        [ctrl, mem_value, target_value, sp_value],
        [
            ValueKind::Control,
            ValueKind::Memory,
            ValueKind::Typed(ValueType::I64),
        ],
    );
    stamp(&mut f, call);
    f.set_call_cc(call, cc);
    let [call_ctrl, call_mem, clob] = f.node_outputs_exact::<3>(call).unwrap();
    f.side_tables.value_vn.insert(clob, clob0);
    let ret = f
        .graph_mut()
        .create_node(NodeKind::Return, [call_ctrl, call_mem], []);
    stamp(&mut f, ret);

    assert_validation_err(&f, |e| {
        matches!(
            e,
            ValidationError::NodeOutputCountMismatch {
                expected: 4,
                actual: 3,
                ..
            }
        )
    });
}

#[test]
fn cc_arity_passes_when_return_matches_declared_ret_val_regs() {
    let (mut f, entry) = fn_with_declared_cc();
    let [ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
    let mem = f
        .graph_mut()
        .create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();
    stamp(&mut f, mem);
    let (v1, v1_value) = int_const(&mut f, 7, ValueType::I64);
    stamp(&mut f, v1);
    let (v2, v2_value) = int_const(&mut f, 8, ValueType::I64);
    stamp(&mut f, v2);
    let ret =
        f.graph_mut()
            .create_node(NodeKind::Return, [ctrl, mem_value, v1_value, v2_value], []);
    stamp(&mut f, ret);

    validate(&f).expect("Return with declared 2 ret-val regs and 2 value inputs must validate");
}

// ── memory-chain anchoring ──────────────────────────────────────────────────

/// Build a `Store(RAM)` node consuming `mem_in`, returning `(store, mem_out)`.
fn store(f: &mut Function, mem_in: ValueId, addr: ValueId, data: ValueId) -> (NodeId, ValueId) {
    let n = f.graph_mut().create_node(
        NodeKind::Store(rsleigh::VnSpace::RAM),
        [mem_in, addr, data],
        [ValueKind::Memory],
    );
    stamp(f, n);
    let [mem_out] = f.node_outputs_exact::<1>(n).unwrap();
    (n, mem_out)
}

/// Positive: a normal `Store → Return` memory chain validates clean — every
/// reachable memory output (InitialMemory, Store) is consumed by a reachable
/// node back to the Return terminator.
#[test]
fn memory_chain_wired_store_to_return_validates() {
    let mut s = spine();
    let (addr_n, addr) = int_const(&mut s.f, 0x2000, ValueType::I64);
    stamp(&mut s.f, addr_n);
    let (data_n, data) = int_const(&mut s.f, 0x42, ValueType::I64);
    stamp(&mut s.f, data_n);
    let (_st, st_mem) = store(&mut s.f, s.mem_value, addr, data);
    let ret =
        s.f.graph_mut()
            .create_node(NodeKind::Return, [s.entry_ctrl, st_mem], []);
    stamp(&mut s.f, ret);

    validate(&s.f).expect("wired Store→Return memory chain must validate");
}

/// A `Store` in DEAD control (unreachable from entry) must NOT be flagged:
/// it is itself removable, so its unconsumed memory output is not a broken
/// live chain.  Confirms the check is scoped to the reachable set.
#[test]
fn memory_chain_dead_control_store_not_flagged() {
    let mut s = spine();
    // Live spine: Return consumes Entry's control + InitialMemory directly,
    // so neither the dead Store nor its dangling memory output is reachable.
    let ret =
        s.f.graph_mut()
            .create_node(NodeKind::Return, [s.entry_ctrl, s.mem_value], []);
    stamp(&mut s.f, ret);

    // Dead Store: consumes InitialMemory's output but nothing consumes the
    // Store's memory output, and the Store is not reachable from entry.
    let (addr_n, addr) = int_const(&mut s.f, 0x3000, ValueType::I64);
    stamp(&mut s.f, addr_n);
    let (data_n, data) = int_const(&mut s.f, 0x7, ValueType::I64);
    stamp(&mut s.f, data_n);
    let (_dead_store, _dead_mem) = store(&mut s.f, s.mem_value, addr, data);

    validate(&s.f).expect("a Store in dead control must not be flagged");
}

/// RED: a reachable `Store` whose memory output has no consumer is flagged.
///
/// A `Store` outputs only `[MEM]`, so the entry walk reaches it solely
/// through a consumer of that output — meaning a well-formed reachable store
/// is always anchored, and the check fires only once an edit breaks that.  We
/// reproduce the broken state directly: the `Return` still references the
/// store's memory output as an input (so the walk reaches the store), but the
/// store's forward use-list head is cleared (so the output reports zero uses),
/// the exact shape a memory-output `replace_value` that forgot to keep the
/// store anchored would leave behind.
#[test]
fn memory_chain_orphaned_store_flagged() {
    let mut s = spine();
    let (addr_n, addr) = int_const(&mut s.f, 0x2000, ValueType::I64);
    stamp(&mut s.f, addr_n);
    let (data_n, data) = int_const(&mut s.f, 0x42, ValueType::I64);
    stamp(&mut s.f, data_n);
    let (_st, st_mem) = store(&mut s.f, s.mem_value, addr, data);
    let ret =
        s.f.graph_mut()
            .create_node(NodeKind::Return, [s.entry_ctrl, st_mem], []);
    stamp(&mut s.f, ret);

    // Orphan the store: sever the forward use-list link from its memory
    // output without touching the Return's backing input edge, so the store
    // stays reachable but its memory output has zero uses.
    s.f.graph_mut().corrupt_clear_first_use(st_mem);

    assert_validation_err(&s.f, |e| {
        matches!(
            e,
            ValidationError::OrphanedMemoryOutput {
                kind: NodeKind::Store(_),
                ..
            }
        )
    });
}

/// Regression for the deliberate scope narrowing: a memory-PRESERVING
/// `Call` legitimately leaves its Memory output unconsumed (the builder emits
/// the output unconditionally but advances the region's memory through it only
/// when the call clobbers memory — "you don't have to use it").  The check is
/// `Store`-scoped precisely so this canonical shape is NOT flagged.
#[test]
fn memory_chain_preserving_call_unconsumed_memory_output_not_flagged() {
    let mut s = spine();
    let (target_n, target) = int_const(&mut s.f, 0x1000, ValueType::I64);
    stamp(&mut s.f, target_n);
    let (sp_n, sp) = int_const(&mut s.f, 0x7fff_0000, ValueType::I64);
    stamp(&mut s.f, sp_n);

    // Function-default Call (empty CC → cc-arity check skipped): outputs
    // [Control, Memory].  Reachable via its Control output, but its Memory
    // output is intentionally unconsumed — memory is threaded around it.
    let call = s.f.graph_mut().create_node(
        NodeKind::Call,
        [s.entry_ctrl, s.mem_value, target, sp],
        [ValueKind::Control, ValueKind::Memory],
    );
    stamp(&mut s.f, call);
    let [call_ctrl, _call_mem] = s.f.node_outputs_exact::<2>(call).unwrap();

    // Return takes the Call's control but the pre-call memory edge.
    let ret =
        s.f.graph_mut()
            .create_node(NodeKind::Return, [call_ctrl, s.mem_value], []);
    stamp(&mut s.f, ret);

    validate(&s.f)
        .expect("a memory-preserving Call's unconsumed memory output must not be flagged");
}

#[test]
fn graph_invariants_extend_must_strictly_widen() {
    use crate::node::ExtendOp;

    let mut s = spine();
    let (c, c_value) = int_const(&mut s.f, 5, ValueType::I64);
    stamp(&mut s.f, c);

    // Extend from I64 *down* to I32 — degenerate. `Extend` is direction-typed
    // (it fills new high bits); a non-widening Extend is a redundant spelling
    // of `Truncate` and must be rejected.
    let bad = s.f.graph_mut().create_node(
        NodeKind::Extend(ExtendOp::ZeroExtend),
        [c_value],
        [ValueKind::Typed(ValueType::I32)],
    );
    stamp(&mut s.f, bad);
    let [bad_value] = s.f.node_outputs_exact::<1>(bad).unwrap();
    s.f.graph_mut()
        .create_node(NodeKind::Return, [s.entry_ctrl, s.mem_value, bad_value], []);

    assert_validation_err(&s.f, |e| {
        matches!(
            e,
            ValidationError::ExtendTruncateWidthDirection {
                in_width: 64,
                out_width: 32,
                ..
            }
        )
    });
}

#[test]
fn graph_invariants_truncate_must_strictly_narrow() {
    let mut s = spine();
    let (c, c_value) = int_const(&mut s.f, 5, ValueType::I32);
    stamp(&mut s.f, c);

    // Truncate from I32 *up* to I64 — degenerate. `Truncate` drops high bits;
    // a non-narrowing Truncate is a redundant spelling of `Extend`.
    let bad = s.f.graph_mut().create_node(
        NodeKind::Truncate,
        [c_value],
        [ValueKind::Typed(ValueType::I64)],
    );
    stamp(&mut s.f, bad);
    let [bad_value] = s.f.node_outputs_exact::<1>(bad).unwrap();
    s.f.graph_mut()
        .create_node(NodeKind::Return, [s.entry_ctrl, s.mem_value, bad_value], []);

    assert_validation_err(&s.f, |e| {
        matches!(
            e,
            ValidationError::ExtendTruncateWidthDirection {
                in_width: 32,
                out_width: 64,
                ..
            }
        )
    });
}

#[test]
fn graph_invariants_equal_width_extend_is_rejected() {
    use crate::node::ExtendOp;

    let mut s = spine();
    let (c, c_value) = int_const(&mut s.f, 5, ValueType::I32);
    stamp(&mut s.f, c);

    // Same-width Extend is a no-op miswiring — the builder emits the value
    // unchanged rather than an Extend node, so any equal-width Extend is bad.
    let bad = s.f.graph_mut().create_node(
        NodeKind::Extend(ExtendOp::SignExtend),
        [c_value],
        [ValueKind::Typed(ValueType::I32)],
    );
    stamp(&mut s.f, bad);
    let [bad_value] = s.f.node_outputs_exact::<1>(bad).unwrap();
    s.f.graph_mut()
        .create_node(NodeKind::Return, [s.entry_ctrl, s.mem_value, bad_value], []);

    assert_validation_err(&s.f, |e| {
        matches!(
            e,
            ValidationError::ExtendTruncateWidthDirection {
                in_width: 32,
                out_width: 32,
                ..
            }
        )
    });
}
