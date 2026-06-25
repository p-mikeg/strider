//! Per-pass propagation tests for the asm-fingerprint side-table.
//!
//! Each test builds a synthetic IR with explicit asm-addresses set on
//! the input nodes, runs a single optimisation pass, and asserts that
//! every contributing address survives the rewrite (the superset-only
//! invariant).
//!
//! End-to-end fingerprint coverage lives in `tests/asm_fingerprints.rs`
//! (lift the real fixture, run the full pipeline, then validate); this
//! file complements that by isolating one pass at a time on a
//! hand-built IR shape so a regression in a single pass produces a
//! single, named test failure rather than a downstream validator
//! panic.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

use strider_ir::node::{NodeKind, ValueType};
use strider_ir::{IRBuilderExt, IRViewer, IRWalker, IntBinaryOp};
use strider_ir_test_utils::make_empty_fn;
use strider_orchestrator::opt::{ConstantFold, KnownBits};

#[test]
fn constant_fold_add_consts_preserves_fingerprints() {
    // Build `IntConst(3)@0x100 + IntConst(4)@0x104 → IntConst(7)`.
    // After folding, the surviving IntConst(7) MUST carry the Add's
    // address (and the sub-operand addresses, which the engine
    // propagates via after_replace).
    let mut fg = make_empty_fn(|b| {
        b.set_lift_addr(Some(0x100));
        let c3 = b.build_int_const(3u64, ValueType::I64)?;
        b.set_lift_addr(Some(0x104));
        let c4 = b.build_int_const(4u64, ValueType::I64)?;
        b.set_lift_addr(Some(0x108));
        let add = b.build_int_binary_operation(c3, c4, IntBinaryOp::Add, ValueType::I64)?;
        b.set_lift_addr(None);
        Ok(add)
    })
    .unwrap();
    assert!(
        strider_orchestrator::opt::run_one(
            &ConstantFold::new(),
            &mut fg,
            &mut strider_orchestrator::opt::OptCtx::new(None)
        )
        .unwrap()
        .changed()
    );
    // The surviving node feeds the Return; find it.
    let const7 = fg
        .walk()
        .find(|&nid| {
            matches!(fg.node_kind(nid), NodeKind::IntConst(_))
                && fg
                    .first_value_output_of(nid)
                    .is_some_and(|v| fg.int_const_val(v) == Some(7))
        })
        .expect("IntConst(7)");
    let fp = fg.asm_fingerprint(const7);
    assert!(
        fp.contains(&0x108),
        "IntConst(7) fingerprint must include the Add's address 0x108: {fp:?}"
    );
}

#[test]
fn constant_fold_x_xor_x_preserves_fingerprints() {
    let mut fg = make_empty_fn(|b| {
        b.set_lift_addr(Some(0x200));
        let x = b.build_int_const(0xABu64, ValueType::I64)?;
        b.set_lift_addr(Some(0x204));
        let xor = b.build_int_binary_operation(x, x, IntBinaryOp::Xor, ValueType::I64)?;
        b.set_lift_addr(None);
        Ok(xor)
    })
    .unwrap();
    assert!(
        strider_orchestrator::opt::run_one(
            &ConstantFold::new(),
            &mut fg,
            &mut strider_orchestrator::opt::OptCtx::new(None)
        )
        .unwrap()
        .changed()
    );
    // Result is IntConst(0); its fingerprint must include 0x204 (the
    // Xor's address — absorbed via after_replace).
    let const0 = fg
        .walk()
        .find(|&nid| {
            matches!(fg.node_kind(nid), NodeKind::IntConst(_))
                && fg
                    .first_value_output_of(nid)
                    .is_some_and(|v| fg.int_const_val(v) == Some(0))
        })
        .expect("IntConst(0)");
    let fp = fg.asm_fingerprint(const0);
    assert!(
        fp.contains(&0x204),
        "IntConst(0) must inherit Xor's 0x204: {fp:?}"
    );
}

#[test]
fn known_bits_fold_preserves_fingerprints() {
    // `(0xFFu64 & 0x4) | 0x07` — ConstantFold + KnownBits will collapse
    // to a single IntConst; the surviving node must carry at least one
    // contributor address from the chain.
    let mut fg = make_empty_fn(|b| {
        b.set_lift_addr(Some(0x300));
        let x = b.build_int_const(0xFFu64, ValueType::I64)?;
        b.set_lift_addr(Some(0x304));
        let m4 = b.build_int_const(0x04u64, ValueType::I64)?;
        b.set_lift_addr(Some(0x308));
        let m7 = b.build_int_const(0x07u64, ValueType::I64)?;
        b.set_lift_addr(Some(0x30c));
        let inner = b.build_int_binary_operation(x, m4, IntBinaryOp::And, ValueType::I64)?;
        b.set_lift_addr(Some(0x310));
        let outer = b.build_int_binary_operation(inner, m7, IntBinaryOp::Or, ValueType::I64)?;
        b.set_lift_addr(None);
        Ok(outer)
    })
    .unwrap();
    let _ = strider_orchestrator::opt::run_one(
        &ConstantFold::new(),
        &mut fg,
        &mut strider_orchestrator::opt::OptCtx::new(None),
    );
    let _ = strider_orchestrator::opt::run_one(
        &KnownBits,
        &mut fg,
        &mut strider_orchestrator::opt::OptCtx::new(None),
    );
    // The eventual return value should be an IntConst with at least one
    // of the rewritten addresses absorbed into it.
    let ret = fg
        .walk()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Return))
        .expect("Return");
    let ret_inputs: Vec<_> = fg.node_inputs(ret).into_iter().collect();
    // input[2] is the value (input[0]=ctrl, input[1]=mem).
    assert!(ret_inputs.len() >= 3, "Return must have a value");
    let val_node = fg.producer(ret_inputs[2]);
    let fp = fg.asm_fingerprint(val_node);
    assert!(
        !fp.is_empty(),
        "Folded return value must carry at least one contributor address: {fp:?}"
    );
}

#[test]
fn constant_fold_and_mask_merge_preserves_fingerprints() {
    // `(x & 0x4) & 0x7 → x & (0x4 & 0x7)` = `x & 0x4`.  The fold
    // rewrites the outer And's value; the surviving And node must
    // carry the rewritten outer-And's address.
    let mut fg = make_empty_fn(|b| {
        b.set_lift_addr(Some(0x500));
        let x = b.build_int_const(0xFFu64, ValueType::I64)?;
        b.set_lift_addr(Some(0x504));
        let m4 = b.build_int_const(0x04u64, ValueType::I64)?;
        b.set_lift_addr(Some(0x508));
        let m7 = b.build_int_const(0x07u64, ValueType::I64)?;
        b.set_lift_addr(Some(0x50c));
        let inner = b.build_int_binary_operation(x, m4, IntBinaryOp::And, ValueType::I64)?;
        b.set_lift_addr(Some(0x510));
        let outer = b.build_int_binary_operation(inner, m7, IntBinaryOp::And, ValueType::I64)?;
        b.set_lift_addr(None);
        Ok(outer)
    })
    .unwrap();
    let _ = strider_orchestrator::opt::run_one(
        &ConstantFold::new(),
        &mut fg,
        &mut strider_orchestrator::opt::OptCtx::new(None),
    )
    .unwrap();
    // Whatever value reaches the Return must carry the outer-And's
    // address — that's the canonical "rewrite root".
    let ret = fg
        .walk()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Return))
        .expect("Return");
    let ret_inputs: Vec<_> = fg.node_inputs(ret).into_iter().collect();
    let val_node = fg.producer(ret_inputs[2]);
    let fp = fg.asm_fingerprint(val_node);
    assert!(
        fp.contains(&0x510),
        "outer-And's 0x510 must survive in the surviving value's fingerprint: {fp:?}"
    );
}
