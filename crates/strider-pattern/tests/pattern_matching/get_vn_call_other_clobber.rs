//! `build_call_with_cc` records the per-call clobber-list override on
//! the side-table.  Pattern queries against output / clobber slots are
//! no longer expressible (the output-constraint API was deleted), so
//! the remaining test only exercises the side-table directly.

use strider_ir::FunctionBuilder;
use strider_ir::node::{NodeKind, ValueType};
use strider_ir_test_utils::SENTINEL_LIFT_ADDR;
use strider_target::{BuiltCallingConvention, CallingConvention, SleighArch};

// ── Call: build_call_with_cc records per-call override ───────────────────────

#[test]
fn build_call_with_cc_override_records_empty_clobber_list() {
    // Every tracked variable is callee-saved in the override CC →
    // per-call clobber = ∅ → 0 clobber output slots and an empty
    // override list on the side-table.
    let arch = SleighArch::x86_64();
    let regs = arch.probe_regs().unwrap();
    let rax = regs.name_to_vn("RAX").unwrap();
    let rsp = regs.name_to_vn("RSP").unwrap();

    let cc = CallingConvention::x86_64_systemv()
        .unwrap()
        .build(&regs)
        .unwrap();
    let mut b = FunctionBuilder::new(vec![rax, rsp], &cc).unwrap();
    let region = b.create_region().unwrap();
    b.set_entry_region(region).unwrap();
    b.set_region(region);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    // `FunctionBuilder::new` auto-tracks ret-val regs (rax, rdx, xmm0, xmm1).
    let rdx = regs.name_to_vn("RDX").unwrap();
    let xmm0 = regs.name_to_vn("XMM0").unwrap();
    let xmm1 = regs.name_to_vn("XMM1").unwrap();
    let override_cc = BuiltCallingConvention::try_new(
        vec![],                          // arg_passing_regs
        vec![rax, rdx, xmm0, xmm1],      // callee_saved_regs (every tracked var)
        vec![],                          // ret_val_regs
        vec![],                          // ret_val_regs_float
        rsp,                             // stack_vn
        vec![],                          // stack_arg_offsets
        0,                               // ret_stack_pop
        None,                            // link_register_vn
        false,                           // preserves_memory
    )
    .unwrap();
    let addr = b
        .build_int_const(0xdead_u64, ValueType::I64)
        .unwrap();
    let _call_node = b.build_call_with_cc(addr, Some(&override_cc)).unwrap();
    let ret_vars: Vec<rsleigh::Vn> = b.ret_val_vars().to_vec();
    b.build_return(None, &ret_vars).unwrap();
    let function = b.build().unwrap();

    // The single Call has 0 clobber outputs (ctrl + mem only) and the
    // side-table records an empty override list.
    let call_id = function
        .all_node_ids()
        .find(|n| matches!(function.node_kind(*n), NodeKind::Call))
        .unwrap();
    assert_eq!(function.call_clobbered_override(call_id), Some(&[][..]));
    assert_eq!(function.node_outputs(call_id).len(), 2);
}
