use strider_ir::node::{NodeKind, ValueType};
use strider_ir::{FunctionBuilder, IRBuilderExt, IRViewer};
use strider_ir_test_utils::SENTINEL_LIFT_ADDR;
use strider_target::{BuiltCallingConvention, CallingConvention, SleighArch};

#[test]
fn build_call_with_cc_override_records_empty_clobber_list() {
    // Every tracked variable is callee-saved in the override CC, so the
    // per-call clobber set is empty: no clobber output slots.
    let arch = SleighArch::x86_64();
    let regs = arch.probe_regs().unwrap();
    let rax = regs.name_to_vn("RAX").unwrap();
    let rsp = regs.name_to_vn("RSP").unwrap();

    let cc = CallingConvention::x86_64_systemv().build(&regs).unwrap();
    let mut b =
        FunctionBuilder::new(vec![rax, rsp], cc, strider_target::Endianness::Little).unwrap();
    let region = b.create_region_all().unwrap();
    b.set_entry_region_all(region).unwrap();
    b.set_region(region);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    // FunctionBuilder::new auto-tracks the ret-val regs, both arg-passing lists
    // and the stack pointer, so the override has to mark all of them
    // callee-saved or one leaks out as a clobber.
    let rdx = regs.name_to_vn("RDX").unwrap();
    // ST0/ST1 carry the X87-class (`long double`) and COMPLEX_X87 returns.
    let st0 = regs.name_to_vn("ST0").unwrap();
    let st1 = regs.name_to_vn("ST1").unwrap();
    let mut callee_saved = vec![rax, rdx, st0, st1];
    // RDX is both a ret-val and an arg register; validate rejects duplicates.
    for n in ["RDI", "RSI", "RCX", "R8", "R9"] {
        callee_saved.push(regs.name_to_vn(n).unwrap());
    }
    // The float argument registers are seeded like the integer ones.
    for n in [
        "XMM0", "XMM1", "XMM2", "XMM3", "XMM4", "XMM5", "XMM6", "XMM7",
    ] {
        callee_saved.push(regs.name_to_vn(n).unwrap());
    }
    let override_cc = BuiltCallingConvention {
        arg_passing_regs: vec![],
        callee_saved_regs: callee_saved,
        ret_val_regs: vec![],
        ret_val_regs_float: vec![],
        stack_vn: rsp,
        stack_args: None,
        ret_stack_pop: 0,
        link_register_vn: None,
        preserves_memory: false,
        preserves_all_registers: false,
        no_return: false,
        ..Default::default()
    };
    override_cc.validate().unwrap();
    let addr = b.build_int_const(0xdead_u64, ValueType::I64).unwrap();
    let _call_node = b.build_call_cc(addr, Some(&override_cc)).unwrap();
    b.build_function_return().unwrap();
    let function = b.build().unwrap();

    // Call has ctrl + mem outputs only, but the override CC is still recorded.
    let call_id = function
        .graph()
        .all_node_ids()
        .find(|n| matches!(function.node_kind(*n), NodeKind::Call))
        .unwrap();
    assert_eq!(function.get_cc(call_id), &override_cc);
    assert_eq!(function.node_outputs(call_id).len(), 2);
}
