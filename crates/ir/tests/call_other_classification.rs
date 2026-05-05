//! Outcome-level tests for [`ir::FunctionBuilder::build_call_other`]'s
//! classification dispatch.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ir::error::UnknownUserOpError;
use ir::node::{NodeKind, NodeOutputType};
use ir::{CallOtherOutcome, FunctionBuilder};
use target::{CallingConvention, SleighArch};

fn make_x86_64_builder() -> FunctionBuilder {
    let arch = SleighArch::x86_64();
    let regs = arch.probe_regs().unwrap();
    let cc = CallingConvention::x86_64_systemv_abi().build(&regs).unwrap();
    let rax = regs.name_to_vn("RAX").unwrap();
    let rbx = regs.name_to_vn("RBX").unwrap();
    let rsp = regs.name_to_vn("RSP").unwrap();
    let mut b = FunctionBuilder::new(vec![rax, rbx, rsp], &cc).unwrap();
    let region = b.create_region().unwrap();
    b.set_entry_region(region).unwrap();
    b.set_region(region);
    b
}

#[test]
fn build_call_other_noop_skips_node() {
    let mut b = make_x86_64_builder();
    let before = b.body().graph.all_node_ids().count();
    let outcome = b
        .build_call_other("setISAMode", 7, &[], None)
        .expect("classify ok");
    assert!(matches!(outcome, CallOtherOutcome::NoOp), "got {outcome:?}");
    let after = b.body().graph.all_node_ids().count();
    assert_eq!(before, after, "no node added for NoOp");
}

#[test]
fn build_call_other_noreturn_emits_terminal_node() {
    let mut b = make_x86_64_builder();
    let before = b.body().graph.all_node_ids().count();
    let outcome = b
        .build_call_other("invalidInstructionException", 8, &[], None)
        .expect("classify ok");
    let CallOtherOutcome::NoReturn = outcome else {
        panic!("expected NoReturn, got {outcome:?}")
    };
    let after = b.body().graph.all_node_ids().count();
    assert!(after > before, "a node was added");
    // The new node has user_op_id = 8, kind CallOther.
    let new_node = b
        .body()
        .graph
        .all_node_ids()
        .find(|n| matches!(b.body().graph.node_kind(*n), NodeKind::CallOther { user_op_id: 8 }))
        .expect("CallOther{8} node");
    let n_outputs = b.body().graph.node_outputs(new_node).len();
    assert_eq!(
        n_outputs, 2,
        "terminal CallOther has only ctrl + mem outputs"
    );
    assert_eq!(
        b.body().graph.call_other_name(new_node),
        Some("invalidInstructionException"),
    );
}

#[test]
fn build_call_other_opaque_builds_with_clobbers_and_name() {
    let mut b = make_x86_64_builder();
    let outcome = b
        .build_call_other("cpuid", 9, &[], Some(NodeOutputType::U32))
        .expect("classify ok");
    let CallOtherOutcome::Built { node, value } = outcome else {
        panic!("expected Built, got {outcome:?}")
    };
    assert!(value.is_some(), "value output present");
    let kind = b.body().graph.node_kind(node);
    assert!(
        matches!(kind, NodeKind::CallOther { user_op_id: 9 }),
        "got {kind:?}"
    );
    let n_outputs = b.body().graph.node_outputs(node).len();
    assert!(
        n_outputs > 2,
        "opaque has clobber outputs after [ctrl, mem, value]"
    );
    assert_eq!(b.body().graph.call_other_name(node), Some("cpuid"));
}

#[test]
fn build_call_other_unknown_name_errors() {
    let mut b = make_x86_64_builder();
    let err = b
        .build_call_other("nonexistent_op_xyz_qqq", 0, &[], None)
        .unwrap_err();
    let downcast = err.downcast_ref::<UnknownUserOpError>();
    assert!(downcast.is_some(), "expected UnknownUserOpError, got: {err}");
    assert_eq!(downcast.unwrap().name, "nonexistent_op_xyz_qqq");
}
