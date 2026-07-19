//! `build_call_with_cc` records the override CC on the Call and tags each
//! clobber output with its register via `value_vn`. Output-slot pattern
//! queries went away with the output-constraint API, so this exercises the
//! side-table directly.

use strider_ir::node::{NodeKind, ValueType};
use strider_ir::{FunctionBuilder, IRBuilderExt, IRViewer};
use strider_ir_test_utils::SENTINEL_LIFT_ADDR;
use strider_target::{
    BuiltCallingConvention, BuiltCallingConventionParts, CallingConvention, SleighArch,
};

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
    // FunctionBuilder::new auto-tracks the ret-val regs, the arg-passing regs
    // and the stack pointer, so the override has to mark all of them
    // callee-saved or one leaks out as a clobber.
    let rdx = regs.name_to_vn("RDX").unwrap();
    let xmm0 = regs.name_to_vn("XMM0").unwrap();
    let xmm1 = regs.name_to_vn("XMM1").unwrap();
    let mut callee_saved = vec![rax, rdx, xmm0, xmm1];
    // RDX is both a ret-val and an arg register; try_new rejects duplicates.
    for n in ["RDI", "RSI", "RCX", "R8", "R9"] {
        callee_saved.push(regs.name_to_vn(n).unwrap());
    }
    let override_cc = BuiltCallingConvention::try_new(BuiltCallingConventionParts {
        arg_passing_regs: vec![],
        callee_saved_regs: callee_saved,
        ret_val_regs: vec![],
        ret_val_regs_float: vec![],
        stack_vn: rsp,
        stack_args: None,
        ret_stack_pop: 0,
        link_register_vn: None,
        preserves_memory: false,
    })
    .unwrap();
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
