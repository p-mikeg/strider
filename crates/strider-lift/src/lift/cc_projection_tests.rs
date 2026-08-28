use rsleigh::{Vn, VnSpace};

use super::handler_tests::with_test_lifter_cc;

fn reg(off: u64, size: u32) -> Vn {
    Vn {
        size,
        addr_off: off,
        addr_space: VnSpace::REGISTER,
    }
}

/// Struct-literal construction skips ABI-disjointness validation, fine for a
/// synthetic fixture.
fn make_cc(
    ret_val_regs: Vec<Vn>,
    callee_saved_regs: Vec<Vn>,
    stack_vn: Vn,
) -> strider_target::BuiltCallingConvention {
    strider_target::BuiltCallingConvention {
        arg_passing_regs: Vec::new(),
        callee_saved_regs,
        ret_val_regs,
        ret_val_regs_float: Vec::new(),
        stack_vn,
        stack_args: None,
        ret_stack_pop: 0,
        link_register_vn: None,
        preserves_memory: false,
        preserves_all_registers: false,
        no_return: false,
        ..Default::default()
    }
}

/// A sub-register ret reg (`eax`) must resolve to its container (`rax`) in
/// BOTH the ret-val list and the clobber exclusion.
#[test]
fn sub_register_ret_reg_routes_to_container() {
    let rax = reg(0x0, 8);
    let eax = reg(0x0, 4); // sub-register of rax
    let sp = reg(0x7000, 8);
    let all_vns = vec![rax, sp];
    let cc = make_cc(vec![eax], Vec::new(), sp);
    with_test_lifter_cc(cc.clone(), all_vns, |d, _rid| {
        assert_eq!(
            d.call_ret_vals_for(&cc),
            vec![rax],
            "eax ret reg must resolve to its tracked rax container"
        );
        assert!(
            !d.call_clobbered_for(&cc).contains(&rax),
            "rax is the ret container and must not appear in the clobber list"
        );
    });
}

#[test]
fn ret_and_clobber_split_full_width() {
    let rax = reg(0x00, 8);
    let rcx = reg(0x08, 8);
    let rbx = reg(0x10, 8);
    let sp = reg(0x18, 8);
    let all_vns = vec![rax, rcx, rbx, sp];
    let cc = make_cc(vec![rax], vec![rbx], sp);
    with_test_lifter_cc(cc.clone(), all_vns, |d, _rid| {
        assert_eq!(d.call_ret_vals_for(&cc), vec![rax], "rax is the ret reg");
        let clobbered = d.call_clobbered_for(&cc);
        assert!(!clobbered.contains(&rax), "rax is ret, not a clobber");
        assert!(clobbered.contains(&rcx), "rcx is caller-saved, a clobber");
        assert!(!clobbered.contains(&rbx), "rbx is callee-saved, excluded");
        assert!(!clobbered.contains(&sp), "sp is the stack_vn, excluded");
        assert_eq!(clobbered, vec![rcx], "rcx is the sole clobber");
    });
}

/// Fewer ret regs and more callee-saved must shrink the combined set.
#[test]
fn override_cc_yields_smaller_clobber_set() {
    let r0 = reg(0x10, 8);
    let r1 = reg(0x20, 8);
    let r2 = reg(0x30, 8);
    let sp = reg(0x40, 8);
    let all_vns = vec![r0, r1, r2, sp];

    // cc_A: ret=[r0], callee-saved=[r2], so clobber=[r1].
    let cc_a = make_cc(vec![r0], vec![r2], sp);
    // cc_B: ret=[], callee-saved=[r1, r2], so clobber=[r0].
    let cc_b = make_cc(Vec::new(), vec![r1, r2], sp);

    with_test_lifter_cc(cc_a.clone(), all_vns, |d, _rid| {
        assert_eq!(d.call_ret_vals_for(&cc_a), vec![r0]);
        assert_eq!(d.call_clobbered_for(&cc_a), vec![r1]);

        assert_eq!(d.call_ret_vals_for(&cc_b), Vec::<Vn>::new());
        assert_eq!(d.call_clobbered_for(&cc_b), vec![r0]);

        let combined_a = d.call_ret_vals_for(&cc_a).len() + d.call_clobbered_for(&cc_a).len();
        let combined_b = d.call_ret_vals_for(&cc_b).len() + d.call_clobbered_for(&cc_b).len();
        assert!(
            combined_b < combined_a,
            "override cc_B combined ret+clobber ({combined_b}) must be smaller than cc_A ({combined_a})"
        );
    });
}

/// Both `Call` output groups are drawn from the tracked set, so every entry
/// is a REGISTER / UNIQUE container `write_variable` reaches whole.
#[test]
fn call_outputs_are_tracked_containers() {
    let rax = reg(0x0, 8);
    let rcx = reg(0x8, 8);
    let sp = reg(0x18, 8);
    let all_vns = vec![rax, rcx, sp];
    let cc = make_cc(vec![rax], Vec::new(), sp);
    with_test_lifter_cc(cc.clone(), all_vns.clone(), |d, _rid| {
        let outputs = [d.call_ret_vals_for(&cc), d.call_clobbered_for(&cc)].concat();
        assert!(
            !outputs.is_empty(),
            "the fixture has a ret val and a clobber"
        );
        for vn in outputs {
            assert!(
                matches!(vn.addr_space, VnSpace::REGISTER | VnSpace::UNIQUE),
                "{vn:?} is neither REGISTER nor UNIQUE"
            );
            assert!(
                d.builder.function().all_vns().contains(&vn),
                "{vn:?} is not a tracked container"
            );
        }
    });
}

/// AAPCS-VFP `d0` and `d1` collapse into the `q0` container the moment the
/// function names any NEON register, but they are still float parameters 0
/// and 1 and must carry distinct incoming values.
#[test]
fn aliased_float_arg_regs_keep_their_abi_positions() {
    let q0 = reg(0x300, 16);
    let (d0, d1, d2, d3) = (reg(0x300, 8), reg(0x308, 8), reg(0x310, 8), reg(0x318, 8));
    let sp = reg(0x7000, 4);
    let mut cc = make_cc(Vec::new(), Vec::new(), sp);
    cc.arg_passing_regs_float = vec![d0, d1, d2, d3];
    with_test_lifter_cc(cc, vec![q0, d2, sp], |d, _rid| {
        d.record_register_arg_carriers().unwrap();
        let st = d.builder.function().side_tables();
        let (p0, p1, p2) = (
            st.float_arg_index_to_values(0),
            st.float_arg_index_to_values(1),
            st.float_arg_index_to_values(2),
        );
        assert_eq!(p0.len(), 1, "d0 is float param 0");
        assert_eq!(p1.len(), 1, "d1 is float param 1");
        assert_ne!(
            p0[0], p1[0],
            "d0 and d1 share the q0 container but are distinct float params"
        );
        assert_eq!(p2.len(), 1, "d2 is float param 2, not float param 1");
        assert_eq!(
            st.float_arg_index_to_values(3).len(),
            1,
            "d3 is seeded like every other ABI float register, so it is param 3"
        );
    });
}

/// A float argument register the function never names still carries its
/// parameter: `arg_passing_regs_float` is seeded into the tracked set the way
/// the integer list already was, so no position is dropped and none shifts.
#[test]
fn every_float_arg_position_has_a_carrier() {
    let (d0, d1) = (reg(0x300, 8), reg(0x308, 8));
    let sp = reg(0x7000, 4);
    let mut cc = make_cc(Vec::new(), Vec::new(), sp);
    cc.arg_passing_regs_float = vec![d0, d1];
    with_test_lifter_cc(cc, vec![d1, sp], |d, _rid| {
        d.record_register_arg_carriers().unwrap();
        let st = d.builder.function().side_tables();
        assert_eq!(
            st.float_arg_index_to_values(0).len(),
            1,
            "d0 is float param 0 even though no instruction names it"
        );
        assert_eq!(
            st.float_arg_index_to_values(1).len(),
            1,
            "d1 stays float param 1 rather than shifting down to 0",
        );
    });
}

/// The whole shape end to end: a NEON instruction pulls `q0` into the tracked
/// set, and `d0`/`d1` must still be float parameters 0 and 1 both as carriers
/// and as `Call` arguments.
///
/// `vadd.i32 q0,q0,q0 ; vmov.f64 d0,d2 ; vmov.f64 d1,d3 ; bl g ; bx lr`.
#[test]
fn arm_neon_float_args_keep_their_abi_positions_end_to_end() {
    use strider_ir::IRViewer;
    use strider_ir::node::NodeKind;

    let bytes = vec![
        0x40, 0x08, 0x20, 0xf2, // vadd.i32 q0,q0,q0
        0x42, 0x0b, 0xb0, 0xee, // vmov.f64 d0,d2
        0x43, 0x1b, 0xb0, 0xee, // vmov.f64 d1,d3
        0x00, 0x00, 0x00, 0xeb, // bl g
        0x1e, 0xff, 0x2f, 0xe1, // bx lr
        0x1e, 0xff, 0x2f, 0xe1, // g: bx lr
    ];
    let f = super::handler_tests::lift_bytes(
        strider_target::SleighArch::arm(),
        strider_target::CallingConvention::arm_aapcs(),
        bytes,
    )
    .expect("the ARM NEON fixture must lift");

    let q0 = reg(0x300, 16);
    assert!(
        f.all_vns().contains(&q0),
        "the NEON instruction must make q0 the tracked container, got {:?}",
        f.all_vns(),
    );

    let carriers: Vec<Vec<strider_ir::node::ValueId>> = (0..4)
        .map(|j| f.side_tables().float_arg_index_to_values(j).to_vec())
        .collect();
    for (j, c) in carriers.iter().enumerate() {
        assert_eq!(c.len(), 1, "float param {j} must have exactly one carrier");
    }
    let flat: Vec<strider_ir::node::ValueId> = carriers.iter().flatten().copied().collect();
    assert_eq!(
        distinct_count(&flat),
        4,
        "d0..d3 are four distinct float parameters"
    );

    // The four float arguments follow the four integer ones in the `Call`.
    let call = f
        .graph()
        .all_node_ids()
        .find(|n| matches!(f.node_kind(*n), NodeKind::Call))
        .expect("the fixture calls g");
    let inputs: Vec<strider_ir::node::ValueId> = f.node_inputs(call).into_iter().collect();
    // [Control, Memory, target, SP, r0..r3, d0..d3].
    let float_args = &inputs[inputs.len() - 4..];
    for (j, v) in float_args.iter().enumerate() {
        assert_eq!(
            f.value_type(*v).unwrap(),
            strider_ir::ValueType::I64,
            "float argument {j} is one 64-bit d register, not a fused q container",
        );
    }
    assert_eq!(
        distinct_count(float_args),
        4,
        "no two float arguments are the same value"
    );
}

fn distinct_count(values: &[strider_ir::node::ValueId]) -> usize {
    let mut seen: Vec<strider_ir::node::ValueId> = Vec::new();
    for v in values {
        if !seen.contains(v) {
            seen.push(*v);
        }
    }
    seen.len()
}

/// A float argument's index is its ABI POSITION on both sides: the `Call`
/// slot at `arg_passing_regs.len() + j` and the callee-side carrier for float
/// parameter `j` name the same register.
///
/// `vldr d5,[r0] ; vldr d7,[r0,#8] ; bl ; bx lr` names d5 and d7 but not d4 or
/// d6. Every ABI float register is seeded regardless, so d5 and d7 are
/// arguments 5 and 7 rather than being dropped with the positions below them.
#[test]
fn float_call_args_keep_their_abi_positions() {
    use strider_ir::IRViewer;
    use strider_ir::node::NodeKind;

    let bytes = vec![
        0x00, 0x5b, 0x90, 0xed, // vldr d5,[r0]
        0x02, 0x7b, 0x90, 0xed, // vldr d7,[r0,#8]
        0x02, 0x00, 0x00, 0xeb, // bl 0x1018, past the buffer: only the Call
        //                         node's arguments are under test
        0x1e, 0xff, 0x2f, 0xe1, // bx lr
        0x1e, 0xff, 0x2f, 0xe1, // never reached
    ];
    let f = super::handler_tests::lift_bytes(
        strider_target::SleighArch::arm(),
        strider_target::CallingConvention::arm_aapcs(),
        bytes,
    )
    .expect("the ARM VFP fixture must lift");

    let cc = f.default_cc();
    let slots = cc.float_arg_slots(f.all_vns(), |v| {
        vn_container::largest_container_in(f.all_vns(), v)
    });
    assert!(
        slots.iter().all(Option::is_some),
        "every ABI float register is seeded, so no position gaps, got {slots:?}"
    );

    let call = f
        .graph()
        .all_node_ids()
        .find(|n| matches!(f.node_kind(*n), NodeKind::Call))
        .expect("the fixture calls g");
    let inputs: Vec<strider_ir::node::ValueId> = f.node_inputs(call).into_iter().collect();
    // [Control, Memory, target, SP] then the integer arguments.
    let float_args = &inputs[4 + cc.arg_passing_regs.len()..];

    assert_eq!(
        float_args.len(),
        slots.len(),
        "one Call argument per ABI float position; got {} for {} slots",
        float_args.len(),
        slots.len(),
    );
    // The fixture loads into d5 and d7, so those two carry the loaded value
    // rather than their entry value; every other position is untouched.
    for (j, arg) in float_args.iter().enumerate() {
        let reg = slots[j].expect("every slot is tracked");
        let initial = f.initial_var_value(&reg);
        if j == 5 || j == 7 {
            assert_ne!(
                initial,
                Some(*arg),
                "float argument {j} must carry the value the fixture loaded into {reg:?}"
            );
        } else {
            assert_eq!(
                initial,
                Some(*arg),
                "float argument {j} must carry ABI float register {reg:?}"
            );
        }
    }
}
