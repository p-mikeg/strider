//! `build_call_other_modeled` emits a CallOther whose clobber slots
//! correspond exactly to the ABI's implicit_writes (no conservative
//! all-vars default).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ir::FunctionBuilder;
use ir::node::{NodeKind, NodeOutputType};

fn make_builder() -> FunctionBuilder {
    let mut b = FunctionBuilder::empty().expect("builder");
    let r = b.create_region().expect("region");
    b.set_entry_region(r).expect("entry");
    b.set_region(r);
    b
}

#[test]
fn modeled_with_no_implicit_writes_emits_no_clobber_slots() {
    let mut b = make_builder();
    let (node, value, clobber_outs) = b
        .build_call_other_modeled(7, "NEON_rev64", &[], None, &[], &[])
        .expect("modeled ok");
    let kind = b.body().graph.node_kind(node);
    assert!(matches!(kind, NodeKind::CallOther { user_op_id: 7 }), "{kind:?}");
    assert!(value.is_none(), "no output_ty -> no value slot");
    assert!(clobber_outs.is_empty(), "no implicit_writes -> no clobber slots");
    let n_outs = b.body().graph.node_outputs(node).len();
    assert_eq!(n_outs, 2, "ctrl + mem only");
}

fn reg_vn(off: u64, size: u32) -> rsleigh::Vn {
    rsleigh::Vn {
        size,
        addr_off: off,
        addr_space: rsleigh::VnSpace::REGISTER,
    }
}

#[test]
fn modeled_with_value_emits_value_then_clobbers_in_order() {
    // For implicit_writes, we need tracked variables.  Use new_raw to
    // declare two registers as tracked, then set up the region.
    let r0 = reg_vn(0, 4);
    let r1 = reg_vn(4, 4);
    let mut b = FunctionBuilder::new_raw(vec![r0, r1], &[], &[], &[], None, 0)
        .expect("builder");
    let region = b.create_region().expect("region");
    b.set_entry_region(region).expect("entry");
    b.set_region(region);

    let (node, value, clobber_outs) = b
        .build_call_other_modeled(
            8,
            "cpuid",
            &[],
            Some(NodeOutputType::U32),
            &[],
            &[r0, r1],
        )
        .expect("modeled ok");
    assert!(value.is_some(), "output_ty -> value slot");
    assert_eq!(clobber_outs.len(), 2, "two implicit_writes -> two clobber slots");
    let n_outs = b.body().graph.node_outputs(node).len();
    assert_eq!(n_outs, 5, "ctrl + mem + value + 2 clobbers");
    assert_eq!(b.body().graph.call_other_name(node), Some("cpuid"));
}

#[test]
fn modeled_does_not_advance_memory_token() {
    // Caller (strider) is responsible for advancing memory based on
    // memory_edge.  build_call_other_modeled should NOT advance it.
    //
    // Verify by emitting two consecutive build_call_other_modeled calls
    // and checking they share the same mem_in slot.  Since mem_in is
    // taken from the active region, if either call advanced memory the
    // second call's input would differ.
    let mut b = make_builder();
    let (node1, _, _) = b
        .build_call_other_modeled(9, "NEON_rev64", &[], None, &[], &[])
        .expect("ok");
    let (node2, _, _) = b
        .build_call_other_modeled(10, "NEON_rev64", &[], None, &[], &[])
        .expect("ok");
    // Both nodes consume the same mem input (slot 1) — the region's
    // memory token never advanced.
    let g = &b.body().graph;
    let n1_inputs: Vec<_> = g.node_inputs(node1).into_iter().collect();
    let n2_inputs: Vec<_> = g.node_inputs(node2).into_iter().collect();
    assert_eq!(
        n1_inputs[1], n2_inputs[1],
        "build_call_other_modeled must not advance the memory token"
    );
}
