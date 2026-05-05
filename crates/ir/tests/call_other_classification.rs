//! Outcome-level tests for the IR's CallOther construction helpers.
//! Spec: `docs/superpowers/specs/2026-05-06-callother-precise-abi-design.md`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ir::FunctionBuilder;
use ir::node::{NodeKind, NodeOutputKind, NodeOutputType};

fn make_builder() -> FunctionBuilder {
    let mut b = FunctionBuilder::empty().expect("builder");
    let r = b.create_region().expect("region");
    b.set_entry_region(r).expect("entry");
    b.set_region(r);
    b
}

fn reg_vn(off: u64, size: u32) -> rsleigh::Vn {
    rsleigh::Vn {
        size,
        addr_off: off,
        addr_space: rsleigh::VnSpace::REGISTER,
    }
}

#[test]
fn build_call_other_terminal_emits_ctrl_mem_only() {
    let mut b = make_builder();
    let node = b
        .build_call_other_terminal(7, "invalidInstructionException")
        .expect("terminal ok");
    let kind = b.body().graph.node_kind(node);
    assert!(matches!(kind, NodeKind::CallOther { user_op_id: 7 }), "{kind:?}");
    let n_outs = b.body().graph.node_outputs(node).len();
    assert_eq!(n_outs, 2, "terminal: ctrl + mem only");
    assert_eq!(
        b.body().graph.call_other_name(node),
        Some("invalidInstructionException"),
    );
}

#[test]
fn build_call_other_modeled_with_empty_abi_no_clobbers() {
    let mut b = make_builder();
    let (node, value, clobber_outs) = b
        .build_call_other_modeled(8, "NEON_rev64", &[], None, &[], &[], &[])
        .expect("modeled ok");
    assert!(value.is_none());
    assert!(clobber_outs.is_empty());
    let n_outs = b.body().graph.node_outputs(node).len();
    assert_eq!(n_outs, 2);
    assert_eq!(b.body().graph.call_other_name(node), Some("NEON_rev64"));
}

#[test]
fn build_call_other_modeled_with_value_and_clobbers() {
    let mut b = make_builder();
    let r0 = reg_vn(0, 4);
    let (node, value, clobber_outs) = b
        .build_call_other_modeled(
            9,
            "cpuid",
            &[],
            Some(NodeOutputType::U32),
            &[],
            &[r0],
            &[NodeOutputKind::OutputType(NodeOutputType::U32)],
        )
        .expect("modeled ok");
    assert!(value.is_some());
    assert_eq!(clobber_outs.len(), 1);
    let n_outs = b.body().graph.node_outputs(node).len();
    assert_eq!(n_outs, 4, "ctrl + mem + value + 1 clobber");
    assert_eq!(b.body().graph.call_other_name(node), Some("cpuid"));
}
