use super::*;
use crate::node::{IntBinaryOp, NodeId, NodeKind, ValueId, ValueKind, ValueType};

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

#[track_caller]
fn assert_no_validation_err(f: &Function, pred: impl Fn(&ValidationError) -> bool) {
    if let Err(errs) = validate(f) {
        assert!(
            !errs.0.iter().any(pred),
            "an excluded validation error was reported; got: {errs:?}"
        );
    }
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
// mint a duplicate. The two tests below pin that dedup instead.

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
    // the wrong kind. The Return keeps the Region reachable.
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

/// The `IndirectBranch` ISA-mode input is ONE optional trailing slot, not a
/// repeating tail: the arity bound is four, so a fifth input is rejected.
#[test]
fn local_typing_indirect_branch_rejects_a_second_isa_mode_input() {
    let mut s = spine();
    let (_t, target) = int_const(&mut s.f, 0x1000, ValueType::I64);
    let (_m, mode) = int_const(&mut s.f, 1, ValueType::I64);

    let bad = s.f.graph_mut().create_node(
        NodeKind::IndirectBranch,
        [s.entry_ctrl, s.mem_value, target, mode, mode],
        [],
    );
    stamp(&mut s.f, bad);

    assert_validation_err(&s.f, |e| {
        matches!(e, ValidationError::NodeInputCountMismatch { actual: 5, .. })
    });
}

/// The companion bound: three or four inputs are both admissible.
#[test]
fn local_typing_indirect_branch_accepts_optional_isa_mode_input() {
    for with_mode in [false, true] {
        let mut s = spine();
        let (_t, target) = int_const(&mut s.f, 0x1000, ValueType::I64);
        let (_m, mode) = int_const(&mut s.f, 1, ValueType::I64);
        let mut inputs = vec![s.entry_ctrl, s.mem_value, target];
        if with_mode {
            inputs.push(mode);
        }
        let branch =
            s.f.graph_mut()
                .create_node(NodeKind::IndirectBranch, inputs, []);
        stamp(&mut s.f, branch);
        assert_no_validation_err(&s.f, |e| {
            matches!(e, ValidationError::NodeInputCountMismatch { .. })
        });
    }
}

/// A genuinely repeating tail keeps its unbounded semantics.
#[test]
fn local_typing_call_accepts_arbitrarily_many_argument_inputs() {
    let mut s = spine();
    let (_t, target) = int_const(&mut s.f, 0x1000, ValueType::I64);
    let (_sp, sp) = int_const(&mut s.f, 0x7000, ValueType::I64);
    let mut inputs = vec![s.entry_ctrl, s.mem_value, target, sp];
    inputs.extend(std::iter::repeat_n(sp, 9));
    let call = s.f.graph_mut().create_node(
        NodeKind::Call,
        inputs,
        [ValueKind::Control, ValueKind::Memory],
    );
    stamp(&mut s.f, call);
    assert_no_validation_err(&s.f, |e| {
        matches!(e, ValidationError::NodeInputCountMismatch { .. })
    });
}

/// Variadic input tails are kind-checked, not only the fixed head prefix.
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

/// A width that ends inside a limb keeps that limb's low bits. Comparing whole
/// limbs against the width rejects `0xFF << 64` at `I72`, which fits exactly.
#[test]
fn graph_invariants_wide_const_filling_a_partial_top_limb_is_accepted() {
    use crate::node::const_value::ConstValue;
    let mut s = spine();
    let id =
        s.f.const_interner
            .intern(ConstValue::Wide(vec![0, 0xFF].into_boxed_slice()));
    let ok = s.f.graph_mut().create_node(
        NodeKind::IntConst(id),
        [],
        [ValueKind::Typed(ValueType::I72)],
    );
    let ok_value = s.f.node_outputs(ok).iter().copied().next().unwrap();
    let _ret =
        s.f.graph_mut()
            .create_node(NodeKind::Return, [s.entry_ctrl, s.mem_value, ok_value], []);

    let errs = crate::validate::validate(&s.f);
    assert!(
        !format!("{errs:?}").contains("ConstWidthMismatch"),
        "0xFF << 64 spans exactly 72 bits: {errs:?}"
    );
}

/// One bit above the declared width, in the same partial top limb.
#[test]
fn graph_invariants_wide_const_one_bit_over_a_partial_width_detected() {
    use crate::node::const_value::ConstValue;
    let mut s = spine();
    let id =
        s.f.const_interner
            .intern(ConstValue::Wide(vec![0, 0x1FF].into_boxed_slice()));
    let bad = s.f.graph_mut().create_node(
        NodeKind::IntConst(id),
        [],
        [ValueKind::Typed(ValueType::I72)],
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

    validate(&s.f).expect("wired Store->Return memory chain must validate");
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

    // The Memory output is left unconsumed.
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

/// Builds `kind(IntConst:in_ty) -> out_ty` into a returned spine and asserts
/// the validator rejects it as a width-direction violation.
fn assert_width_direction_rejected(
    kind: NodeKind,
    in_ty: ValueType,
    out_ty: ValueType,
    in_width: usize,
    out_width: usize,
) {
    let mut s = spine();
    let (c, c_value) = int_const(&mut s.f, 5, in_ty);
    stamp(&mut s.f, c);

    let bad =
        s.f.graph_mut()
            .create_node(kind, [c_value], [ValueKind::Typed(out_ty)]);
    stamp(&mut s.f, bad);
    let [bad_value] = s.f.node_outputs_exact::<1>(bad).unwrap();
    s.f.graph_mut()
        .create_node(NodeKind::Return, [s.entry_ctrl, s.mem_value, bad_value], []);

    assert_validation_err(&s.f, |e| {
        matches!(
            e,
            ValidationError::ExtendTruncateWidthDirection { in_width: i, out_width: o, .. }
                if *i == in_width && *o == out_width
        )
    });
}

#[test]
fn graph_invariants_extend_must_strictly_widen() {
    assert_width_direction_rejected(
        NodeKind::Extend(crate::node::ExtendOp::ZeroExtend),
        ValueType::I64,
        ValueType::I32,
        64,
        32,
    );
}

#[test]
fn graph_invariants_truncate_must_strictly_narrow() {
    assert_width_direction_rejected(NodeKind::Truncate, ValueType::I32, ValueType::I64, 32, 64);
}

#[test]
fn graph_invariants_equal_width_extend_is_rejected() {
    assert_width_direction_rejected(
        NodeKind::Extend(crate::node::ExtendOp::SignExtend),
        ValueType::I32,
        ValueType::I32,
        32,
        32,
    );
}

/// `Entry -> Region` whose control output is its own second predecessor. Both
/// use-count checks pass: the region's control output has exactly one consumer.
#[test]
fn graph_invariants_exit_free_control_cycle_is_rejected() {
    let mut s = spine();
    let region = s.f.graph_mut().create_node(
        NodeKind::Region,
        [s.entry_ctrl],
        [ValueKind::Control, ValueKind::PhiToken],
    );
    let region_ctrl = s.f.node_outputs(region)[0];
    s.f.graph_mut().add_node_input(region, region_ctrl);

    assert_no_validation_err(&s.f, |e| {
        matches!(
            e,
            ValidationError::UnusedControlOutput { .. }
                | ValidationError::ReusedControlOutput { .. }
        )
    });
    assert_validation_err(&s.f, |e| {
        matches!(e, ValidationError::NoTerminatorReachable { .. })
    });
}

/// The same cycle with an `If` arm escaping into a `Return` is well formed.
#[test]
fn graph_invariants_control_cycle_with_an_exit_validates() {
    let mut s = spine();
    let (cond_node, cond) = int_const(&mut s.f, 1, ValueType::I1);
    stamp(&mut s.f, cond_node);

    let region = s.f.graph_mut().create_node(
        NodeKind::Region,
        [s.entry_ctrl],
        [ValueKind::Control, ValueKind::PhiToken],
    );
    let region_ctrl = s.f.node_outputs(region)[0];
    let branch = s.f.graph_mut().create_node(
        NodeKind::If,
        [region_ctrl, cond],
        [ValueKind::Control, ValueKind::Control],
    );
    stamp(&mut s.f, branch);
    let [back, exit] = s.f.node_outputs_exact::<2>(branch).unwrap();
    s.f.graph_mut().add_node_input(region, back);
    let ret =
        s.f.graph_mut()
            .create_node(NodeKind::Return, [exit, s.mem_value], []);
    stamp(&mut s.f, ret);

    validate(&s.f).expect("a control cycle with a Return exit is well formed");
}

/// An exit-free cycle terminated by an `Unreachable` sink hanging off an `If`.
#[test]
fn graph_invariants_control_cycle_with_unreachable_sink_validates() {
    let mut s = spine();
    let (cond_node, cond) = int_const(&mut s.f, 1, ValueType::I1);
    stamp(&mut s.f, cond_node);

    let region = s.f.graph_mut().create_node(
        NodeKind::Region,
        [s.entry_ctrl],
        [ValueKind::Control, ValueKind::PhiToken],
    );
    let region_ctrl = s.f.node_outputs(region)[0];
    let branch = s.f.graph_mut().create_node(
        NodeKind::If,
        [region_ctrl, cond],
        [ValueKind::Control, ValueKind::Control],
    );
    stamp(&mut s.f, branch);
    let [back, sink_ctrl] = s.f.node_outputs_exact::<2>(branch).unwrap();
    s.f.graph_mut().add_node_input(region, back);
    let sink =
        s.f.graph_mut()
            .create_node(NodeKind::Unreachable, [sink_ctrl], []);
    stamp(&mut s.f, sink);

    validate(&s.f).expect("an Unreachable sink terminates the cycle");
}

/// Control alone cannot anchor a memory chain, so the sink that terminates an
/// exit-free cycle takes the chain as its optional second input.
#[test]
fn unreachable_with_memory_input_keeps_the_store_live() {
    let mut s = spine();
    let (addr_n, addr) = int_const(&mut s.f, 0x2000, ValueType::I64);
    stamp(&mut s.f, addr_n);
    let (data_n, data) = int_const(&mut s.f, 0x42, ValueType::I64);
    stamp(&mut s.f, data_n);
    let (st, st_mem) = store(&mut s.f, s.mem_value, addr, data);
    let sink =
        s.f.graph_mut()
            .create_node(NodeKind::Unreachable, [s.entry_ctrl, st_mem], []);
    stamp(&mut s.f, sink);

    validate(&s.f).expect("Unreachable accepts the memory chain as an optional input");
    use crate::IRWalker as _;
    assert!(
        s.f.walk().any(|node| node == st),
        "the memory input must keep the Store on the compaction walk"
    );
}

#[test]
fn graph_invariants_float_const_bits_above_declared_width_detected() {
    let mut s = spine();
    // An F32 pattern carrying garbage in the unused upper half.
    let bad = s.f.graph_mut().create_node(
        NodeKind::FloatConst(1.0f32.to_bits() as u64 | 0x1_0000_0000),
        [],
        [ValueKind::Typed(ValueType::F32)],
    );
    let bad_value = s.f.node_outputs(bad).iter().copied().next().unwrap();
    let _ret =
        s.f.graph_mut()
            .create_node(NodeKind::Return, [s.entry_ctrl, s.mem_value, bad_value], []);

    assert_validation_err(&s.f, |e| {
        matches!(e, ValidationError::FloatConstWidthMismatch { .. })
    });
}

/// `mask_float_bits` is the identity at 64 bits and wider, so the bits-above-
/// width rule cannot see an F80 / F128 `FloatConst`: the payload is a `u64` and
/// the declared width is what has to be rejected.
#[test]
fn graph_invariants_float_const_wider_than_its_payload_detected() {
    for ty in [ValueType::F80, ValueType::F128] {
        let mut s = spine();
        let bad =
            s.f.graph_mut()
                .create_node(NodeKind::FloatConst(0xBEEF), [], [ValueKind::Typed(ty)]);
        let bad_value = s.f.node_outputs(bad).iter().copied().next().unwrap();
        let _ret = s.f.graph_mut().create_node(
            NodeKind::Return,
            [s.entry_ctrl, s.mem_value, bad_value],
            [],
        );

        assert_validation_err(&s.f, |e| {
            matches!(e, ValidationError::FloatConstUnrepresentableType { .. })
        });
    }
}

/// A non-phi node reachable from its own output. The walk terminates on it, so
/// nothing else notices, and reverse post-order then yields the node before its
/// own producer.
#[test]
fn self_referential_data_edge_flagged() {
    let mut s = spine();
    let (a_n, a) = int_const(&mut s.f, 1, ValueType::I64);
    stamp(&mut s.f, a_n);
    let (b_n, b) = int_const(&mut s.f, 2, ValueType::I64);
    stamp(&mut s.f, b_n);
    let add = s.f.graph_mut().create_node(
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        [a, b],
        [ValueKind::Typed(ValueType::I64)],
    );
    stamp(&mut s.f, add);
    let [sum] = s.f.node_outputs_exact::<1>(add).unwrap();
    let ret =
        s.f.graph_mut()
            .create_node(NodeKind::Return, [s.entry_ctrl, s.mem_value, sum], []);
    stamp(&mut s.f, ret);

    let slot = s.f.graph().node_input_id_at(add, 1).unwrap();
    s.f.graph_mut().update_input(slot, sum);

    assert_validation_err(&s.f, |e| matches!(e, ValidationError::DataCycle { .. }));
}

/// A loop-carried `Phi` closes a data cycle legitimately.
#[test]
fn phi_carried_data_cycle_not_flagged() {
    let mut s = spine();
    let (one_n, one) = int_const(&mut s.f, 1, ValueType::I64);
    stamp(&mut s.f, one_n);
    let token =
        s.f.graph_mut()
            .create_node(NodeKind::Region, [s.entry_ctrl], [ValueKind::Control]);
    stamp(&mut s.f, token);
    let [region_ctrl] = s.f.node_outputs_exact::<1>(token).unwrap();
    let phi = s.f.graph_mut().create_node(
        NodeKind::Phi,
        [region_ctrl, one],
        [ValueKind::Typed(ValueType::I64)],
    );
    stamp(&mut s.f, phi);
    let [phi_val] = s.f.node_outputs_exact::<1>(phi).unwrap();
    let add = s.f.graph_mut().create_node(
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        [phi_val, one],
        [ValueKind::Typed(ValueType::I64)],
    );
    stamp(&mut s.f, add);
    let [sum] = s.f.node_outputs_exact::<1>(add).unwrap();
    s.f.graph_mut().add_node_input(phi, sum);
    let ret =
        s.f.graph_mut()
            .create_node(NodeKind::Return, [region_ctrl, s.mem_value, sum], []);
    stamp(&mut s.f, ret);

    assert_no_validation_err(&s.f, |e| matches!(e, ValidationError::DataCycle { .. }));
}

/// A diamond `Entry -> If -> {then, else} -> merge` with a one-input phi in
/// each arm. Returns `(function, [then_val, else_val], merge_token, merge_ctrl)`.
fn diamond_with_arm_phis() -> (Function, [ValueId; 2], ValueId, ValueId) {
    let mut s = spine();
    let (cond_node, cond) = int_const(&mut s.f, 1, ValueType::I1);
    stamp(&mut s.f, cond_node);
    let (seed_node, seed) = int_const(&mut s.f, 7, ValueType::I64);
    stamp(&mut s.f, seed_node);
    let branch = s.f.graph_mut().create_node(
        NodeKind::If,
        [s.entry_ctrl, cond],
        [ValueKind::Control, ValueKind::Control],
    );
    stamp(&mut s.f, branch);
    let [then_edge, else_edge] = s.f.node_outputs_exact::<2>(branch).unwrap();
    let mut arm = |edge: ValueId| {
        let region = s.f.graph_mut().create_node(
            NodeKind::Region,
            [edge],
            [ValueKind::Control, ValueKind::PhiToken],
        );
        let [ctrl, token] = s.f.node_outputs_exact::<2>(region).unwrap();
        let phi = s.f.graph_mut().create_node(
            NodeKind::Phi,
            [token, seed],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [value] = s.f.node_outputs_exact::<1>(phi).unwrap();
        (ctrl, value)
    };
    let (then_ctrl, then_val) = arm(then_edge);
    let (else_ctrl, else_val) = arm(else_edge);
    let merge = s.f.graph_mut().create_node(
        NodeKind::Region,
        [then_ctrl, else_ctrl],
        [ValueKind::Control, ValueKind::PhiToken],
    );
    let [merge_ctrl, merge_token] = s.f.node_outputs_exact::<2>(merge).unwrap();
    (s.f, [then_val, else_val], merge_token, merge_ctrl)
}

fn close_with_return(f: &mut Function, ctrl: ValueId, values: &[ValueId]) {
    let mem = crate::function::test_initial_memory(f);
    let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();
    let inputs: Vec<ValueId> = [ctrl, mem_value]
        .into_iter()
        .chain(values.iter().copied())
        .collect();
    let ret = f.graph_mut().create_node(NodeKind::Return, inputs, []);
    stamp(f, ret);
}

#[test]
fn phi_input_from_the_other_arm_is_not_available() {
    let (mut f, [then_val, _], token, ctrl) = diamond_with_arm_phis();
    let merge_phi = f.graph_mut().create_node(
        NodeKind::Phi,
        [token, then_val, then_val],
        [ValueKind::Typed(ValueType::I64)],
    );
    let [merged] = f.node_outputs_exact::<1>(merge_phi).unwrap();
    close_with_return(&mut f, ctrl, &[merged]);

    assert_validation_err(&f, |e| {
        matches!(
            e,
            ValidationError::InputNotAvailable { node, input_idx: 2, .. } if *node == merge_phi
        )
    });
    assert_no_validation_err(&f, |e| {
        matches!(e, ValidationError::InputNotAvailable { input_idx: 1, .. })
    });
}

#[test]
fn phi_inputs_from_their_own_arms_are_available() {
    let (mut f, [then_val, else_val], token, ctrl) = diamond_with_arm_phis();
    let merge_phi = f.graph_mut().create_node(
        NodeKind::Phi,
        [token, then_val, else_val],
        [ValueKind::Typed(ValueType::I64)],
    );
    let [merged] = f.node_outputs_exact::<1>(merge_phi).unwrap();
    close_with_return(&mut f, ctrl, &[merged]);

    assert_no_validation_err(&f, |e| {
        matches!(e, ValidationError::InputNotAvailable { .. })
    });
}

/// An arm's value used past the merge, directly and through arithmetic.
#[test]
fn arm_value_used_after_the_merge_is_not_available() {
    let (mut f, [then_val, else_val], _, ctrl) = diamond_with_arm_phis();
    let add = f.graph_mut().create_node(
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        [then_val, else_val],
        [ValueKind::Typed(ValueType::I64)],
    );
    stamp(&mut f, add);
    let [sum] = f.node_outputs_exact::<1>(add).unwrap();
    close_with_return(&mut f, ctrl, &[then_val, sum]);

    assert_validation_err(
        &f,
        |e| matches!(e, ValidationError::InputNotAvailable { node, .. } if *node == add),
    );
    assert_validation_err(&f, |e| {
        matches!(e, ValidationError::InputNotAvailable { input_idx: 2, .. })
    });
}

/// A Region no control edge reaches, and a phi value it owns.
fn unreachable_region_value(f: &mut Function, seed: ValueId) -> (ValueId, ValueId) {
    let region = f.graph_mut().create_node(
        NodeKind::Region,
        [],
        [ValueKind::Control, ValueKind::PhiToken],
    );
    let [ctrl, token] = f.node_outputs_exact::<2>(region).unwrap();
    let phi = f.graph_mut().create_node(
        NodeKind::Phi,
        [token, seed],
        [ValueKind::Typed(ValueType::I64)],
    );
    let [value] = f.node_outputs_exact::<1>(phi).unwrap();
    (ctrl, value)
}

#[test]
fn a_value_from_unreachable_code_is_not_available_to_a_live_use() {
    let mut s = spine();
    let (seed_node, seed) = int_const(&mut s.f, 7, ValueType::I64);
    stamp(&mut s.f, seed_node);
    let (_, dead) = unreachable_region_value(&mut s.f, seed);
    let add = s.f.graph_mut().create_node(
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        [dead, seed],
        [ValueKind::Typed(ValueType::I64)],
    );
    stamp(&mut s.f, add);
    let [sum] = s.f.node_outputs_exact::<1>(add).unwrap();
    close_with_return(&mut s.f, s.entry_ctrl, &[sum]);

    assert_validation_err(
        &s.f,
        |e| matches!(e, ValidationError::InputNotAvailable { node, input_idx: 0, .. } if *node == add),
    );
}

#[test]
fn a_phi_input_from_unreachable_code_on_a_live_edge_is_not_available() {
    let (mut f, [then_val, _], token, ctrl) = diamond_with_arm_phis();
    let (seed_node, seed) = int_const(&mut f, 3, ValueType::I64);
    stamp(&mut f, seed_node);
    let (_, dead) = unreachable_region_value(&mut f, seed);
    let merge_phi = f.graph_mut().create_node(
        NodeKind::Phi,
        [token, then_val, dead],
        [ValueKind::Typed(ValueType::I64)],
    );
    let [merged] = f.node_outputs_exact::<1>(merge_phi).unwrap();
    close_with_return(&mut f, ctrl, &[merged]);

    assert_validation_err(
        &f,
        |e| matches!(e, ValidationError::InputNotAvailable { node, input_idx: 2, .. } if *node == merge_phi),
    );
}

/// A phi input on an edge from unreachable code can never be selected.
#[test]
fn a_dead_edge_into_a_phi_is_not_judged() {
    let mut s = spine();
    let (seed_node, seed) = int_const(&mut s.f, 7, ValueType::I64);
    stamp(&mut s.f, seed_node);
    let live = s.f.graph_mut().create_node(
        NodeKind::Region,
        [s.entry_ctrl],
        [ValueKind::Control, ValueKind::PhiToken],
    );
    let [live_ctrl, _] = s.f.node_outputs_exact::<2>(live).unwrap();
    let (dead_ctrl, dead) = unreachable_region_value(&mut s.f, seed);
    let merge = s.f.graph_mut().create_node(
        NodeKind::Region,
        [live_ctrl, dead_ctrl],
        [ValueKind::Control, ValueKind::PhiToken],
    );
    let [merge_ctrl, merge_token] = s.f.node_outputs_exact::<2>(merge).unwrap();
    let merge_phi = s.f.graph_mut().create_node(
        NodeKind::Phi,
        [merge_token, seed, dead],
        [ValueKind::Typed(ValueType::I64)],
    );
    let [merged] = s.f.node_outputs_exact::<1>(merge_phi).unwrap();
    close_with_return(&mut s.f, merge_ctrl, &[merged]);

    assert_no_validation_err(&s.f, |e| {
        matches!(e, ValidationError::InputNotAvailable { .. })
    });
}

/// A floating value used only on an unreachable edge into a phi is never
/// computed on a live path, so nothing about it is judged.
#[test]
fn a_floating_value_on_a_dead_edge_into_a_phi_is_not_judged() {
    let mut s = spine();
    let (seed_node, seed) = int_const(&mut s.f, 7, ValueType::I64);
    stamp(&mut s.f, seed_node);
    let live = s.f.graph_mut().create_node(
        NodeKind::Region,
        [s.entry_ctrl],
        [ValueKind::Control, ValueKind::PhiToken],
    );
    let [live_ctrl, _] = s.f.node_outputs_exact::<2>(live).unwrap();
    let (dead_ctrl, dead) = unreachable_region_value(&mut s.f, seed);
    let add = s.f.graph_mut().create_node(
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        [dead, seed],
        [ValueKind::Typed(ValueType::I64)],
    );
    stamp(&mut s.f, add);
    let [sum] = s.f.node_outputs_exact::<1>(add).unwrap();
    let merge = s.f.graph_mut().create_node(
        NodeKind::Region,
        [live_ctrl, dead_ctrl],
        [ValueKind::Control, ValueKind::PhiToken],
    );
    let [merge_ctrl, merge_token] = s.f.node_outputs_exact::<2>(merge).unwrap();
    let merge_phi = s.f.graph_mut().create_node(
        NodeKind::Phi,
        [merge_token, seed, sum],
        [ValueKind::Typed(ValueType::I64)],
    );
    let [merged] = s.f.node_outputs_exact::<1>(merge_phi).unwrap();
    close_with_return(&mut s.f, merge_ctrl, &[merged]);

    assert_no_validation_err(&s.f, |e| {
        matches!(e, ValidationError::InputNotAvailable { .. })
    });
}
