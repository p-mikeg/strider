use super::*;
use crate::node::{NodeId, NodeKind, ValueId, ValueKind, ValueType};

/// Distinct from any real machine address.
const SENTINEL: u64 = 0xDEAD_BEEF_0000_0001;

/// Satisfies the always-on asm-fingerprint check.
fn stamp(function: &mut Function, id: NodeId) {
    function
        .side_tables_mut()
        .extend_asm_fingerprint(id, &[SENTINEL]);
}

/// A fresh [`Function`] with an `Entry` + `InitialMemory` spine.
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

/// Returns `(node, value output)`.
fn int_const(f: &mut Function, v: u64, ty: ValueType) -> (NodeId, ValueId) {
    let id = f.intern_int_const(u128::from(v), ty);
    let n = f
        .graph_mut()
        .create_node(NodeKind::IntConst(id), [], [ValueKind::Typed(ty)]);
    let [value] = f.node_outputs_exact::<1>(n).unwrap();
    (n, value)
}

#[track_caller]
fn assert_validation_err(f: &Function, pred: impl Fn(&ValidationError) -> bool) {
    let errs = validate(f).unwrap_err();
    assert!(
        errs.0.iter().any(pred),
        "no validation error matched the predicate; got: {errs:?}"
    );
}

#[test]
fn local_typing_wrong_input_kind_on_int_unary_op() {
    use crate::node::IntUnaryOp;

    let mut s = spine();
    // Feed a Control output where a Typed input belongs.
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
    // IntUnaryOp must produce Typed; make it produce Memory.
    let bad = s.f.graph_mut().create_node(
        NodeKind::IntUnaryOp(IntUnaryOp::Neg),
        [c_value],
        [ValueKind::Memory],
    );
    // Onto the reachable spine, since the checks are reachability-scoped.
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

// Entry and InitialMemory are cacheable, so no legal construction path can
// mint a duplicate.  The two tests below pin that dedup instead.

#[test]
fn graph_invariants_entry_dedupes_on_repeated_create() {
    let mut s = spine();
    let entry2 =
        s.f.graph_mut()
            .create_node(NodeKind::Entry, [], [ValueKind::Control]);
    assert_eq!(s.entry, entry2, "Entry must dedup");
    let u =
        s.f.graph_mut()
            .create_node(NodeKind::Unreachable, [s.entry_ctrl], []);
    stamp(&mut s.f, u);
    validate(&s.f).expect("graph with single deduped Entry must validate");
}

#[test]
fn graph_invariants_initial_memory_dedupes_on_repeated_create() {
    let mut s = spine();
    let mem2 =
        s.f.graph_mut()
            .create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    assert_eq!(s.mem, mem2, "InitialMemory must dedup");
    let u =
        s.f.graph_mut()
            .create_node(NodeKind::Unreachable, [s.entry_ctrl], []);
    stamp(&mut s.f, u);
    validate(&s.f).expect("graph with single deduped InitialMemory must validate");
}

#[test]
fn graph_invariants_region_bad_predecessor() {
    // Region with inputs [entry Control, InitialMemory Memory]: input[1] is
    // the wrong kind.  The Return keeps the Region reachable.
    let mut s = spine();

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
            ValidationError::NodeInputKindMismatch {
                node,
                input_idx: 1,
                ..
            } if *node == bad_cs
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

/// An `initial_var_index` entry pointing at a reachable node whose payload was
/// rewritten away from `InitialVar(vn)`.
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
    s.f.set_all_vns(vec![vn, other_vn]);
    let iv = s.f.graph_mut().create_node(
        NodeKind::InitialVar(crate::node::InitialVnId::from_index(0)),
        [],
        [ValueKind::Typed(ValueType::I32)],
    );
    stamp(&mut s.f, iv);
    let iv_value = s.f.node_outputs(iv)[0];
    let vn_id = s.f.vn_id_of(&vn).expect("vn is tracked");
    s.f.side_tables_mut().initial_var_index.insert(vn_id, iv);
    let ret =
        s.f.graph_mut()
            .create_node(NodeKind::Return, [s.entry_ctrl, s.mem_value, iv_value], []);
    stamp(&mut s.f, ret);
    validate(&s.f).expect("a well-formed initial_var_index entry validates");

    // Rewrite the payload in place to varnode index 1, staling the entry.
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

/// A `value_vn` tag whose reachable producer is not a Phi / Call / CallOther.
#[test]
fn validate_flags_stale_value_vn_entry() {
    let mut s = spine();
    let vn = test_vn();
    let (k, kv) = int_const(&mut s.f, 7, ValueType::I32);
    stamp(&mut s.f, k);
    s.f.set_all_vns(vec![vn]); // only a tracked vn can be tagged
    s.f.set_vn_for_value(kv, vn);
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
    s.f.set_vn_for_value(phi_value, vn);

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
    s.f.set_vn_for_value(phi_value, vn);

    // Put the phi on the reachable spine.
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

    // Value input typed I8 under an I64 phi output.
    let (_c1, c1_value) = int_const(&mut s.f, 1, ValueType::I8);
    let phi = s.f.graph_mut().create_node(
        NodeKind::Phi,
        [cs_phi_value, c1_value],
        [ValueKind::Typed(ValueType::I64)],
    );
    let phi_value = s.f.node_outputs(phi)[0];
    s.f.set_vn_for_value(phi_value, test_vn());

    // Put the phi on the reachable spine.
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
    let mut s = spine();
    let ret =
        s.f.graph_mut()
            .create_node(NodeKind::Return, [s.entry_ctrl, s.mem_value], []);
    stamp(&mut s.f, ret);

    let vn = test_vn();
    let zombie =
        s.f.graph_mut()
            .create_node(NodeKind::Phi, [], [ValueKind::Typed(ValueType::I64)]);
    let zombie_value = s.f.node_outputs(zombie)[0];
    s.f.set_vn_for_value(zombie_value, vn);

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

/// Variadic input tails are kind-checked, not just the fixed head prefix.
#[test]
fn local_typing_mem_phi_variadic_tail_must_be_memory() {
    let mut s = spine();

    let cs = s.f.graph_mut().create_node(
        NodeKind::Region,
        [s.entry_ctrl],
        [ValueKind::Control, ValueKind::PhiToken],
    );
    let cs_outputs: Vec<_> = s.f.node_outputs(cs).to_vec();
    let cs_ctrl = cs_outputs[0];
    let cs_phi_token = cs_outputs[1];

    // Correct phi token, then a Control output where Memory belongs.
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
    let mut s = spine();

    let cs = s.f.graph_mut().create_node(
        NodeKind::Region,
        [s.entry_ctrl],
        [ValueKind::Control, ValueKind::PhiToken],
    );
    let cs_ctrl = s.f.node_outputs(cs).iter().copied().next().unwrap();
    let phi_token = s.f.node_outputs(cs).iter().copied().nth(1).unwrap();

    let (bc, bc_value) = int_const(&mut s.f, 1, ValueType::I1);

    let vp = s.f.graph_mut().create_node(
        NodeKind::Phi,
        [phi_token, bc_value],
        [ValueKind::Typed(ValueType::I1)],
    );
    let vp_value = s.f.node_outputs(vp).iter().copied().next().unwrap();

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

    // Two memory inputs under a one-predecessor Region.
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

    // Two value inputs under a one-predecessor Region.
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
    // IntConst with two outputs instead of one.
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
    // The Return consumes Entry's control (so the walk reaches the Return) and
    // the Region's control (so it reaches the Region walking back).
    let mut s = spine();
    let cs = s.f.graph_mut().create_node(
        NodeKind::Region,
        [],
        [ValueKind::Control, ValueKind::PhiToken],
    );
    let cs_ctrl = s.f.node_outputs(cs).iter().copied().next().unwrap();
    s.f.graph_mut()
        .create_node(NodeKind::Return, [s.entry_ctrl, s.mem_value, cs_ctrl], []);

    assert_validation_err(
        &s.f,
        |e| matches!(e, ValidationError::EmptyRegionPredecessors { region } if *region == cs),
    );
}

#[test]
fn graph_invariants_tolerates_unreachable_zero_predecessor_region() {
    let mut s = spine();
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

#[test]
fn asm_fingerprint_check_off_by_default_accepts_empty_fingerprints() {
    let mut s = spine();
    // Unstamped, but unreachable from entry.
    let _const_node = int_const(&mut s.f, 7, ValueType::I64);
    let u =
        s.f.graph_mut()
            .create_node(NodeKind::Unreachable, [s.entry_ctrl], []);
    stamp(&mut s.f, u);
    validate(&s.f).expect("default validate is unaffected");
}

#[test]
fn asm_fingerprint_check_flags_reachable_non_exempt_empty() {
    let mut s = spine();
    let (_c, const_value) = int_const(&mut s.f, 7, ValueType::I64);
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
    s.f.side_tables_mut()
        .extend_asm_fingerprint(int_const_node, &[0x1000]);
    s.f.side_tables_mut().extend_asm_fingerprint(ret, &[0x1004]);
    validate(&s.f).expect("populated fingerprints validate");
}

#[test]
fn asm_fingerprint_check_exempts_phis_and_initials() {
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

/// An unreachable `Region` zombie carrying stale non-Control inputs must not
/// be flagged.
#[test]
fn unreachable_region_with_non_control_input_does_not_fire() {
    let mut s = spine();
    let ret =
        s.f.graph_mut()
            .create_node(NodeKind::Return, [s.entry_ctrl, s.mem_value], []);
    stamp(&mut s.f, ret);

    // In the arena, but unreachable from entry.
    let (_int_const, bogus_value) = int_const(&mut s.f, 0x1234, ValueType::I64);
    let _zombie_cs = s.f.graph_mut().create_node(
        NodeKind::Region,
        [bogus_value],
        [ValueKind::Control, ValueKind::PhiToken],
    );

    validate(&s.f).expect(
        "unreachable Region zombies must not produce \
         NodeInputKindMismatch errors",
    );
}

#[test]
fn control_output_consumed_twice_is_flagged() {
    let mut s = spine();
    // Entry's single Control output feeds two Return terminators.
    let r1 =
        s.f.graph_mut()
            .create_node(NodeKind::Return, [s.entry_ctrl, s.mem_value], []);
    stamp(&mut s.f, r1);
    let r2 =
        s.f.graph_mut()
            .create_node(NodeKind::Return, [s.entry_ctrl, s.mem_value], []);
    stamp(&mut s.f, r2);

    assert_validation_err(&s.f, |e| {
        matches!(
            e,
            ValidationError::ReusedControlOutput { node, .. } if *node == s.entry
        )
    });
}

#[test]
fn unused_control_output_is_flagged() {
    let mut s = spine();
    // Reachable via entry's Control, but its own Control output goes nowhere.
    let region = s.f.graph_mut().create_node(
        NodeKind::Region,
        [s.entry_ctrl],
        [ValueKind::Control, ValueKind::PhiToken],
    );
    stamp(&mut s.f, region);

    assert_validation_err(&s.f, |e| {
        matches!(
            e,
            ValidationError::UnusedControlOutput { node, .. } if *node == region
        )
    });
}

#[test]
fn entry_into_unreachable_validates() {
    let mut s = spine();
    let unreachable =
        s.f.graph_mut()
            .create_node(NodeKind::Unreachable, [s.entry_ctrl], []);
    stamp(&mut s.f, unreachable);
    validate(&s.f).expect("Entry -> Unreachable is a valid terminated graph");
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
    use crate::node::const_value::ConstId;
    use cranelift_entity::EntityRef;
    let mut s = spine();
    // IntConst pointing at an id that was never interned.
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
    use crate::node::const_value::ConstValue;
    let mut s = spine();
    // A 4-limb wide value under an I64 output: bits set above the width.
    let id =
        s.f.const_interner
            .intern(ConstValue::Wide(vec![0, 0, 0, 1].into_boxed_slice()));
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
    use crate::node::const_value::ConstValue;
    let mut s = spine();
    // An unmasked `Bits` value overflowing its declared I8 width.
    let id = s.f.const_interner.intern(ConstValue::Bits(0x1FF));
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

/// Returns `(store, mem_out)`.
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

#[test]
fn memory_chain_dead_control_store_not_flagged() {
    let mut s = spine();
    // The Return takes InitialMemory directly, leaving the Store unreachable.
    let ret =
        s.f.graph_mut()
            .create_node(NodeKind::Return, [s.entry_ctrl, s.mem_value], []);
    stamp(&mut s.f, ret);

    let (addr_n, addr) = int_const(&mut s.f, 0x3000, ValueType::I64);
    stamp(&mut s.f, addr_n);
    let (data_n, data) = int_const(&mut s.f, 0x7, ValueType::I64);
    stamp(&mut s.f, data_n);
    let (_dead_store, _dead_mem) = store(&mut s.f, s.mem_value, addr, data);

    validate(&s.f).expect("a Store in dead control must not be flagged");
}

/// A reachable `Store` whose memory output has no consumer is flagged.
///
/// The broken state has to be built directly: the `Return` keeps its backing
/// input edge (so the walk still reaches the store) while the store's forward
/// use-list head is cleared (so the output reports zero uses).
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

/// A memory-preserving `Call` legitimately leaves its Memory output
/// unconsumed.
#[test]
fn memory_chain_preserving_call_unconsumed_memory_output_not_flagged() {
    let mut s = spine();
    let (target_n, target) = int_const(&mut s.f, 0x1000, ValueType::I64);
    stamp(&mut s.f, target_n);
    let (sp_n, sp) = int_const(&mut s.f, 0x7fff_0000, ValueType::I64);
    stamp(&mut s.f, sp_n);

    // The Memory output is deliberately left unconsumed.
    let call = s.f.graph_mut().create_node(
        NodeKind::Call,
        [s.entry_ctrl, s.mem_value, target, sp],
        [ValueKind::Control, ValueKind::Memory],
    );
    stamp(&mut s.f, call);
    let [call_ctrl, _call_mem] = s.f.node_outputs_exact::<2>(call).unwrap();

    // The Return takes the Call's control but the pre-call memory edge.
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

    // I64 down to I32.
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

    // I32 up to I64.
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
