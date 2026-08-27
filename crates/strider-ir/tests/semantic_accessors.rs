//! The semantic-slot accessors on [`IRViewer`] must pick the same operand
//! slots as positional `node_inputs_exact::<N>(n)[k]` indexing.

use strider_ir::node::{NodeKind, ValueType};
use strider_ir::{IRBuilderExt, IRViewer, IRWalker};
use strider_ir_test_utils::SENTINEL_LIFT_ADDR;

#[test]
fn store_and_load_address_data_accessors() {
    let mut b = strider_ir_test_utils::empty_builder().unwrap();
    let entry = b.create_region_all().unwrap();
    b.set_entry_region_all(entry).unwrap();
    b.set_region(entry);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

    let store_addr_val = b.build_int_const(0x1000u64, ValueType::I64).unwrap();
    let store_data_val = b.build_int_const(0x42u64, ValueType::I32).unwrap();
    b.build_store(store_addr_val, store_data_val, rsleigh::VnSpace::RAM)
        .unwrap();

    let load_addr_val = b.build_int_const(0x2000u64, ValueType::I64).unwrap();
    let loaded = b
        .build_load(load_addr_val, rsleigh::VnSpace::RAM, ValueType::I32)
        .unwrap();
    b.build_return(Some(loaded), &[]).unwrap();
    b.set_lift_addr(None);
    let f = b.build().unwrap();

    let store = f
        .walk()
        .find(|&n| matches!(f.node_kind(n), NodeKind::Store(_)))
        .expect("a Store node");
    assert_eq!(f.store_addr(store), store_addr_val, "store_addr is slot 1");
    assert_eq!(f.store_data(store), store_data_val, "store_data is slot 2");

    let load = f.producer(loaded);
    assert_eq!(f.load_addr(load), load_addr_val, "load_addr is slot 1");
}

#[test]
fn if_cond_accessor() {
    let mut b = strider_ir_test_utils::empty_builder().unwrap();
    let entry = b.create_region_all().unwrap();
    let true_region = b.create_region_all().unwrap();
    let false_region = b.create_region_all().unwrap();
    b.set_entry_region_all(entry).unwrap();
    b.set_region(entry);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let cond = b.build_boolean_const(true);
    b.build_if(cond, true_region, false_region).unwrap();

    b.set_region(true_region);
    let tv = b.build_int_const(1u64, ValueType::I64).unwrap();
    b.build_return(Some(tv), &[]).unwrap();
    b.set_region(false_region);
    let fv = b.build_int_const(2u64, ValueType::I64).unwrap();
    b.build_return(Some(fv), &[]).unwrap();
    b.set_lift_addr(None);
    let f = b.build().unwrap();

    let if_node = f
        .walk()
        .find(|&n| matches!(f.node_kind(n), NodeKind::If))
        .expect("an If node");
    assert_eq!(f.if_cond(if_node), cond, "if_cond is slot 1");
}

#[test]
fn indirect_branch_target_accessor() {
    let mut b = strider_ir_test_utils::empty_builder().unwrap();
    let entry = b.create_region_all().unwrap();
    b.set_entry_region_all(entry).unwrap();
    b.set_region(entry);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let target = b.build_int_const(0xDEAD_0000u64, ValueType::I64).unwrap();
    let branch = b.build_indirect_branch(target).unwrap();
    b.set_lift_addr(None);
    let f = b.build().unwrap();

    assert_eq!(
        f.indirect_branch_target(branch),
        target,
        "indirect_branch_target is slot 2",
    );
}

/// `IndirectBranch` takes 3 or 4 inputs, so no fixed-arity check pins the kind: without a
/// kind guard these read a `Call`'s SP anchor and a `Store`'s data operand.
mod wrong_kind {
    use super::*;

    fn call_node() -> (strider_ir::Function, strider_ir::node::NodeId) {
        let mut b = strider_ir_test_utils::RegisterSet::new()
            .stack_vn(strider_ir_test_utils::stack_vn_x86_64())
            .tracked(strider_ir_test_utils::stack_vn_x86_64())
            .build_fn_single_region()
            .unwrap();
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let target = b.build_int_const(0xF00Du64, ValueType::I64).unwrap();
        let (call, _) = b.build_call(target, &[], &[], 0).unwrap();
        b.build_return(None, &[]).unwrap();
        b.set_lift_addr(None);
        (b.build().unwrap(), call)
    }

    #[test]
    #[should_panic(expected = "indirect_branch_isa_mode on a non-IndirectBranch")]
    fn isa_mode_rejects_a_call() {
        let (f, call) = call_node();
        let _ = f.indirect_branch_isa_mode(call);
    }

    #[test]
    #[should_panic(expected = "indirect_branch_target on a non-IndirectBranch")]
    fn target_rejects_a_store() {
        let mut b = strider_ir_test_utils::empty_builder().unwrap();
        let entry = b.create_region_all().unwrap();
        b.set_entry_region_all(entry).unwrap();
        b.set_region(entry);
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let addr = b.build_int_const(0x1000u64, ValueType::I64).unwrap();
        let data = b.build_int_const(0x42u64, ValueType::I32).unwrap();
        b.build_store(addr, data, rsleigh::VnSpace::RAM).unwrap();
        b.build_return(None, &[]).unwrap();
        b.set_lift_addr(None);
        let f = b.build().unwrap();
        let store = f
            .walk()
            .find(|&n| matches!(f.node_kind(n), NodeKind::Store(_)))
            .expect("a Store node");
        let _ = f.indirect_branch_target(store);
    }
}
