//! Tests for the conservative-clobber `build_call_other` default and
//! the new `BuiltFunctionGraph::call_other_clobbered` field.

#![allow(clippy::unwrap_used)]

use ir::FunctionBuilder;
use ir::node::NodeOutputType;
use target::{CallingConvention, SleighArch};

fn x86_64_strider_setup() -> (rsleigh::SleighRegs, target::BuiltCallingConvention) {
    let arch = SleighArch::x86_64();
    let regs = arch.probe_regs().unwrap();
    let cc = CallingConvention::x86_64_systemv_abi().build(&regs).unwrap();
    (regs, cc)
}

#[test]
fn build_call_other_no_value_emits_clobber_per_tracked_var() {
    let (regs, cc) = x86_64_strider_setup();
    let rax = regs.name_to_vn("RAX").unwrap();
    let rbx = regs.name_to_vn("RBX").unwrap();
    let rsp = regs.name_to_vn("RSP").unwrap();
    let mut b = FunctionBuilder::new(vec![rax, rbx, rsp], &cc).unwrap();
    let region = b.create_region().unwrap();
    b.set_entry_region(region).unwrap();
    b.set_region(region);

    let (call_other_id, value_out) = b.build_call_other(7, &[], None).unwrap();
    assert!(value_out.is_none());

    let g = &b.body().graph;
    let outs = g.node_outputs(call_other_id);
    // Outputs: Control + Memory + per-tracked-non-SP slots.
    // Tracked = rax, rbx, rsp + (auto-added) rdx, xmm0, xmm1.
    // SP is excluded → 5 clobber slots + Control + Memory = 7.
    let clobber_count = outs.len().saturating_sub(2);
    assert!(clobber_count >= 2, "at least the explicit rax + rbx clobbers");
    // SP must NOT have a clobber slot.
    // Count is 5 in this setup (rax, rbx, rdx, xmm0, xmm1).
    assert_eq!(outs.len(), 7, "Control + Memory + 5 tracked-non-SP slots");
}

#[test]
fn build_call_other_with_value_keeps_value_in_slot_2_clobber_starts_at_3() {
    let (regs, cc) = x86_64_strider_setup();
    let rax = regs.name_to_vn("RAX").unwrap();
    let rbx = regs.name_to_vn("RBX").unwrap();
    let rsp = regs.name_to_vn("RSP").unwrap();
    let mut b = FunctionBuilder::new(vec![rax, rbx, rsp], &cc).unwrap();
    let region = b.create_region().unwrap();
    b.set_entry_region(region).unwrap();
    b.set_region(region);

    let (call_other_id, value_out) = b
        .build_call_other(7, &[], Some(NodeOutputType::U32))
        .unwrap();
    assert!(value_out.is_some());

    let g = &b.body().graph;
    let outs: Vec<_> = g.node_outputs(call_other_id).into_iter().collect();
    // Outputs: Control + Memory + value + 5 clobber.
    assert_eq!(outs.len(), 8);
    // Slot 2 is the value output (matches the explicit Some(NodeOutputType::U32)).
    let slot2_kind = g.output_kind(outs[2]);
    assert!(slot2_kind.is_value());
}

#[test]
fn build_call_other_rebinds_tracked_variables() {
    let (regs, cc) = x86_64_strider_setup();
    let rax = regs.name_to_vn("RAX").unwrap();
    let rsp = regs.name_to_vn("RSP").unwrap();
    let mut b = FunctionBuilder::new(vec![rax, rsp], &cc).unwrap();
    let region = b.create_region().unwrap();
    b.set_entry_region(region).unwrap();
    b.set_region(region);

    // Snapshot rax's pre-CallOther producer.
    let pre_rax_value = b.read_variable(&rax).unwrap();

    let (call_other_id, _) = b.build_call_other(7, &[], None).unwrap();

    // Post-CallOther rax is bound to the CallOther's clobber slot.
    let post_rax_value = b.read_variable(&rax).unwrap();
    assert_ne!(pre_rax_value, post_rax_value, "rax must be rebound after CallOther");
    let (post_node, _) = b.body().graph.output_definition(post_rax_value);
    assert_eq!(
        post_node, call_other_id,
        "post-CallOther rax must come from the CallOther's clobber slot"
    );
}

#[test]
fn built_function_graph_call_other_clobbered_excludes_stack_pointer() {
    let (regs, cc) = x86_64_strider_setup();
    let rax = regs.name_to_vn("RAX").unwrap();
    let rbx = regs.name_to_vn("RBX").unwrap();
    let rsp = regs.name_to_vn("RSP").unwrap();
    let mut b = FunctionBuilder::new(vec![rax, rbx, rsp], &cc).unwrap();
    let region = b.create_region().unwrap();
    b.set_entry_region(region).unwrap();
    b.set_region(region);
    let ret_regs: Vec<rsleigh::Vn> = b.ret_val_vars().to_vec();
    b.build_return(None, &ret_regs).unwrap();

    let bfg = b.build().unwrap();
    let coc: &[rsleigh::Vn] = &bfg.call_other_clobbered;
    assert!(coc.contains(&rax), "rax must be in call_other_clobbered");
    assert!(coc.contains(&rbx), "rbx must be in call_other_clobbered");
    assert!(!coc.contains(&rsp), "RSP must NOT be in call_other_clobbered");
}
