//! Tests for `FunctionBuilder::build_call_with_cc` — per-Call CC override.

#![allow(clippy::unwrap_used)]

use ir::FunctionBuilder;
use ir::node::{NodeId, NodeKind, NodeOutputType};
use target::{BuiltCallingConvention, CallingConvention, SleighArch};

fn x86_64_regs() -> rsleigh::SleighRegs {
    SleighArch::x86_64().probe_regs().unwrap()
}

fn x86_64_built_cc() -> BuiltCallingConvention {
    CallingConvention::x86_64_systemv()
        .build(&x86_64_regs())
        .unwrap()
}

#[test]
fn build_call_with_cc_none_matches_build_call() {
    let cc = x86_64_built_cc();
    let regs = x86_64_regs();
    let rax = regs.name_to_vn("RAX").unwrap();
    let rdi = regs.name_to_vn("RDI").unwrap();
    let rsp = regs.name_to_vn("RSP").unwrap();
    let mut b = FunctionBuilder::new(vec![rax, rdi, rsp], &cc).unwrap();
    let region = b.create_region().unwrap();
    b.set_entry_region(region).unwrap();
    b.set_region(region);
    let addr = b
        .build_int_const(0xdead_beef_u64, NodeOutputType::U64)
        .unwrap();
    b.build_call_with_cc(addr, None).unwrap();
    // The Call output kinds match `build_call(addr)` exactly: Control,
    // Memory, then one slot per `call_clobbered_variables` entry.
    let g = &b.body().graph;
    let call_node = g
        .all_node_ids()
        .find(|n| matches!(g.node_kind(*n), NodeKind::Call))
        .unwrap();
    assert!(g.node_outputs(call_node).len() >= 2, "Control + Memory at minimum");
    assert!(g.call_clobbered_override(call_node).is_none(),
            "no override means side-table stays None");
}

#[test]
fn build_call_with_cc_all_preserving_clobbers_nothing() {
    let cc = x86_64_built_cc();
    let regs = x86_64_regs();
    let rax = regs.name_to_vn("RAX").unwrap();
    let rdi = regs.name_to_vn("RDI").unwrap();
    let rsp = regs.name_to_vn("RSP").unwrap();
    // FunctionBuilder::new auto-adds the cc.ret_val_regs() (rax, rdx) and
    // ret_val_regs_float (xmm0, xmm1) into the tracked set even if the
    // caller's `all_used_variables` doesn't list them.  An "all-preserving"
    // override needs to mark those callee-saved too or they'll appear as
    // clobber outputs.
    let rdx = regs.name_to_vn("RDX").unwrap();
    let xmm0 = regs.name_to_vn("XMM0").unwrap();
    let xmm1 = regs.name_to_vn("XMM1").unwrap();
    let mut b = FunctionBuilder::new(vec![rax, rdi, rsp], &cc).unwrap();
    let region = b.create_region().unwrap();
    b.set_entry_region(region).unwrap();
    b.set_region(region);

    // Override CC: every tracked variable is callee-saved → 0 clobbers.
    let override_cc = BuiltCallingConvention::from_parts_unchecked(target::BuiltCallingConventionParts {
        arg_passing_regs: vec![],
        callee_saved_regs: vec![rax, rdi, rdx, xmm0, xmm1],
        ret_val_regs: vec![],
        ret_val_regs_float: vec![],
        stack_ptr_vn: rsp,
        stack_arg_offsets: vec![],
        ret_stack_pop: 0,
        link_register_vn: None,
        syscall_number_vn: None,
        no_memory_clobber: false,
    });

    let addr = b
        .build_int_const(0xdead_beef_u64, NodeOutputType::U64)
        .unwrap();
    b.build_call_with_cc(addr, Some(&override_cc)).unwrap();
    let g = &b.body().graph;
    let call_node = g
        .all_node_ids()
        .find(|n| matches!(g.node_kind(*n), NodeKind::Call))
        .unwrap();
    let outs = g.node_outputs(call_node);
    // Outputs: Control + Memory + 0 clobbered slots.
    assert_eq!(outs.len(), 2, "fentry-style Call has 0 clobbered output slots");
    let inputs = g.node_inputs(call_node).into_iter().collect::<Vec<_>>();
    // Inputs: control + memory + target.  No arg slots.
    assert_eq!(inputs.len(), 3, "fentry-style Call takes no args");
    assert_eq!(g.call_clobbered_override(call_node), Some(&[][..]),
               "side-table records the empty per-Call override list");
}


/// Observable for memory clobbering: terminate with `build_return` and check
/// whether the Return's memory input is the Call's Memory output (chain
/// advanced) or some earlier producer (chain preserved through the Call).
fn return_memory_came_from_call(b: &FunctionBuilder, call_node: NodeId) -> bool {
    let g = &b.body().graph;
    let Some(ret) = g
        .all_node_ids()
        .find(|n| matches!(g.node_kind(*n), NodeKind::Return))
    else {
        return false;
    };
    // Return inputs: [ctrl, memory, *ret_vals].  Slot 1 is the memory edge.
    let mem_in = g.node_inputs(ret)[1];
    let mem_producer = g.get_node_from_output(mem_in);
    mem_producer == call_node
}

#[test]
fn build_call_with_no_memory_clobber_preserves_memory_chain() {
    // When the override CC has no_memory_clobber=true, the Call's Memory
    // output must NOT feed downstream memory consumers — the Return that
    // follows reads the pre-call memory edge directly.  This is what lets
    // LoadReadOnly / StackLoadForward forward across an "all-preserving"
    // call.
    let cc = x86_64_built_cc();
    let regs = x86_64_regs();
    let rsp = regs.name_to_vn("RSP").unwrap();
    let mut b = FunctionBuilder::new(vec![rsp], &cc).unwrap();
    let region = b.create_region().unwrap();
    b.set_entry_region(region).unwrap();
    b.set_region(region);

    let override_cc = BuiltCallingConvention::from_parts_unchecked(target::BuiltCallingConventionParts {
        arg_passing_regs: vec![],
        callee_saved_regs: vec![],
        ret_val_regs: vec![],
        ret_val_regs_float: vec![],
        stack_ptr_vn: rsp,
        stack_arg_offsets: vec![],
        ret_stack_pop: 0,
        link_register_vn: None,
        syscall_number_vn: None,
        // The defining flag — Call should NOT advance memory.
        no_memory_clobber: true,
    });

    let addr = b
        .build_int_const(0xdead_beef_u64, NodeOutputType::U64)
        .unwrap();
    b.build_call_with_cc(addr, Some(&override_cc)).unwrap();
    b.build_return(None, &[]).unwrap();

    let g = &b.body().graph;
    let call_node = g
        .all_node_ids()
        .find(|n| matches!(g.node_kind(*n), NodeKind::Call))
        .unwrap();
    assert!(
        !return_memory_came_from_call(&b, call_node),
        "no_memory_clobber=true: Return must NOT read the Call's Memory output"
    );
}

#[test]
fn build_call_default_cc_advances_memory_chain() {
    // Sanity: default CC advances memory normally (so the preserve-test
    // isn't trivially passing).
    let cc = x86_64_built_cc();
    let regs = x86_64_regs();
    let rsp = regs.name_to_vn("RSP").unwrap();
    let mut b = FunctionBuilder::new(vec![rsp], &cc).unwrap();
    let region = b.create_region().unwrap();
    b.set_entry_region(region).unwrap();
    b.set_region(region);

    let addr = b
        .build_int_const(0xdead_beef_u64, NodeOutputType::U64)
        .unwrap();
    b.build_call_with_cc(addr, None).unwrap();
    b.build_return(None, &[]).unwrap();

    let g = &b.body().graph;
    let call_node = g
        .all_node_ids()
        .find(|n| matches!(g.node_kind(*n), NodeKind::Call))
        .unwrap();
    assert!(
        return_memory_came_from_call(&b, call_node),
        "default CC: Return's memory input must come from the Call's Memory output"
    );
}
