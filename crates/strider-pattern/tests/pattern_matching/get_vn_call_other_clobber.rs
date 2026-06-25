//! `build_call_with_cc` records the override CC on the Call and tags each
//! clobber output value with its register via `value_vn`.  Pattern queries
//! against output / clobber slots are no longer expressible (the
//! output-constraint API was deleted), so the remaining test only
//! exercises the side-table directly.

use strider_ir::{
    FunctionBuilder, IRBuilderExt, IRViewer,
    node::{NodeKind, ValueType},
};
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
    let mut b =
        FunctionBuilder::new(vec![rax, rsp], &cc, strider_target::Endianness::Little).unwrap();
    let region = b.create_region().unwrap();
    b.set_entry_region(region).unwrap();
    b.set_region(region);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    // `FunctionBuilder::new` auto-tracks the ret-val regs (rax, rdx, xmm0,
    // xmm1), the arg-passing regs, and the stack pointer.  The
    // "all-preserving" override must mark every one of those callee-saved so
    // none of them surfaces as a clobber output.
    let rdx = regs.name_to_vn("RDX").unwrap();
    let xmm0 = regs.name_to_vn("XMM0").unwrap();
    let xmm1 = regs.name_to_vn("XMM1").unwrap();
    let mut callee_saved = vec![rax, rdx, xmm0, xmm1];
    // RDX is already present (it is both a ret-val and an arg register), so
    // skip it here to avoid a duplicate `try_new` rejects.
    for n in ["RDI", "RSI", "RCX", "R8", "R9"] {
        callee_saved.push(regs.name_to_vn(n).unwrap());
    }
    let override_cc = BuiltCallingConvention::try_new(
        vec![],       // arg_passing_regs
        callee_saved, // callee_saved_regs (every tracked var)
        vec![],       // ret_val_regs
        vec![],       // ret_val_regs_float
        rsp,          // stack_vn
        None,         // stack_args
        0,            // ret_stack_pop
        None,         // link_register_vn
        false,        // preserves_memory
    )
    .unwrap();
    let addr = b.build_int_const(0xdead_u64, ValueType::I64).unwrap();
    let _call_node = b.build_call(addr, Some(&override_cc)).unwrap();
    let ret_vars: Vec<rsleigh::Vn> = b.function().ret_val_regs().to_vec();
    b.build_return(None, &ret_vars).unwrap();
    let function = b.build().unwrap();

    // The single Call has 0 clobber outputs (ctrl + mem only); the override
    // CC is still recorded (it just clobbers nothing).
    let call_id = function
        .graph()
        .all_node_ids()
        .find(|n| matches!(function.node_kind(*n), NodeKind::Call))
        .unwrap();
    assert!(function.call_cc(call_id).is_some());
    assert_eq!(function.node_outputs(call_id).len(), 2);
}
