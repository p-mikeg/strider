//! Verification for the two composite mutations that route through
//! [`RewriteCtx`] (rather than `Function`): [`RewriteCtx::replace_value`]
//! and [`RewriteCtx::remove_region_predecessors`].
//!
//! These mirror the assertions of the strider-ir `Function`-level tests
//! that previously lived in `graph/tests.rs`, but exercise the methods
//! at their new home on `RewriteCtx`.  Both build a *built* `Function`
//! (entry set) so `RewriteCtx::try_for_built` succeeds.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

use strider_ir::node::{NodeKind, NodeOutputType};
use strider_ir::{FunctionBuilder, IntBinaryOp};
use strider_ir_test_utils::{reg_vn, RegisterSet};
use strider_pattern::RewriteCtx;

// ── replace_value ────────────────────────────────────────────────────

/// `replace_value` absorbs the old producer's asm-fingerprint into the
/// new producer (superset union) and redirects every use of `old` to
/// `new`.  Mirrors the deleted strider-ir
/// `replace_value_absorbs_fingerprint_and_redirects_uses`.
#[test]
fn replace_value_absorbs_fingerprint_and_redirects_uses() {
    let mut b: FunctionBuilder = RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn_single_region");

    // old: IntConst(10) stamped with fingerprint 0xAA.
    b.set_lift_addr(Some(0xAA));
    let old_out = b.build_int_const(10u64, NodeOutputType::I64).unwrap();
    // new: IntConst(20) stamped with fingerprint 0xBB.
    b.set_lift_addr(Some(0xBB));
    let new_out = b.build_int_const(20u64, NodeOutputType::I64).unwrap();
    // sink: Add(old, old) — two uses of old_out.
    let sink = b
        .build_int_binary_operation(old_out, old_out, IntBinaryOp::Add, NodeOutputType::I64)
        .unwrap();
    b.build_return(Some(sink), &[]).unwrap();
    b.set_lift_addr(None);
    let mut function = b.build().unwrap();

    let new_node = function.node_for_output(new_out);
    let sink_node = function.node_for_output(sink);

    let mut ctx = RewriteCtx::try_for_built(&mut function).unwrap();
    let changed = ctx.replace_value(old_out, new_out).unwrap();
    assert!(changed, "a live use existed → changed");

    // new_node absorbs old_node's fingerprint (superset) while keeping
    // its own.
    let fp = function.asm_fingerprint(new_node);
    assert!(fp.contains(&0xAA), "absorbed old's fingerprint 0xAA: {fp:?}");
    assert!(fp.contains(&0xBB), "kept new's own fingerprint 0xBB: {fp:?}");

    // sink now refers to new_out for all inputs.
    let sink_inputs: Vec<_> = function.node_inputs(sink_node).into_iter().collect();
    assert_eq!(
        sink_inputs,
        vec![new_out, new_out],
        "sink inputs must now point at new_out"
    );

    // old_out has no remaining uses.
    assert_eq!(
        function.output_uses(old_out).count(),
        0,
        "old_out must have no remaining uses"
    );
}

/// With no uses to redirect, `replace_value` returns `false` but STILL
/// absorbs the old producer's fingerprint into the new one.  Mirrors the
/// deleted `replace_value_no_uses_returns_false`.
#[test]
fn replace_value_no_uses_returns_false() {
    let mut b: FunctionBuilder = RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn_single_region");

    // old has fingerprint 0xAA but is wired to nothing.
    b.set_lift_addr(Some(0xAA));
    let old_out = b.build_int_const(1u64, NodeOutputType::I64).unwrap();
    // new (the Return value) has fingerprint 0xBB.
    b.set_lift_addr(Some(0xBB));
    let new_out = b.build_int_const(2u64, NodeOutputType::I64).unwrap();
    // Only `new_out` is used (by the Return); `old_out` is unused.
    b.build_return(Some(new_out), &[]).unwrap();
    b.set_lift_addr(None);
    let mut function = b.build().unwrap();

    let new_node = function.node_for_output(new_out);

    let mut ctx = RewriteCtx::try_for_built(&mut function).unwrap();
    let changed = ctx.replace_value(old_out, new_out).unwrap();
    assert!(!changed, "no uses of old → changed must be false");

    // Fingerprint is still absorbed even with no uses redirected.
    let fp = function.asm_fingerprint(new_node);
    assert!(
        fp.contains(&0xAA),
        "fingerprint absorbed even when no uses redirected: {fp:?}"
    );
    assert!(fp.contains(&0xBB), "kept new's own fingerprint 0xBB: {fp:?}");
}

// ── remove_region_predecessors ───────────────────────────────────────

/// A 2-predecessor `Region` with a value `Phi` over it: removing
/// predecessor 0 strips the first control slot from the Region AND the
/// matching value slot (phi index 1) from the Phi, leaving 1 control
/// input on the Region and `[token, surviving_value]` on the Phi.
/// Mirrors the deleted `remove_region_predecessors_strips_ctrl_and_phi_slot`.
#[test]
fn remove_region_predecessors_strips_ctrl_and_phi_slot() {
    // Build `if (true) { var = 1 } else { var = 2 }; return var;` so the
    // `join` Region has two control predecessors and a 2-value VarPhi.
    let var = reg_vn(0x1000, 8);
    let mut b = RegisterSet::new().tracked(var).arg(var).build_fn().unwrap();
    let entry = b.create_region().unwrap();
    let true_r = b.create_region().unwrap();
    let false_r = b.create_region().unwrap();
    let join = b.create_region().unwrap();
    b.set_entry_region(entry).unwrap();

    b.set_region(entry);
    let cond = b.build_boolean_const(true);
    b.build_if(cond, true_r, false_r).unwrap();

    b.set_region(true_r);
    let v_t = b.build_int_const(1u64, NodeOutputType::I64).unwrap();
    b.write_variable(&var, v_t).unwrap();
    b.build_branch(join).unwrap();

    b.set_region(false_r);
    let v_f = b.build_int_const(2u64, NodeOutputType::I64).unwrap();
    b.write_variable(&var, v_f).unwrap();
    b.build_branch(join).unwrap();

    b.set_region(join);
    let merged = b.read_variable(&var).unwrap();
    b.build_return(Some(merged), &[]).unwrap();
    b.set_lift_addr(None);
    let mut function = b.build().unwrap();

    // Locate the 2-value VarPhi at the join (inputs `[token, val0, val1]`)
    // and the Region it joins.  Filtering on input count = 3 skips any
    // single-predecessor VarPhi the builder may have produced for an
    // intermediate region.
    let phi = function
        .all_node_ids()
        .find(|&n| {
            matches!(function.node_kind(n), NodeKind::Phi)
                && function.phi_var_tag(n) == Some(var)
                && function.node_inputs(n).len() == 3
        })
        .expect("2-value VarPhi at the join must exist");
    let phi_token = function.node_inputs(phi)[0];
    let region = function.node_for_output(phi_token);
    assert!(
        matches!(function.node_kind(region), NodeKind::Region),
        "phi token producer must be the join Region"
    );

    // Sanity: two control predecessors, phi inputs [token, val0, val1].
    assert_eq!(
        function.node_inputs(region).len(),
        2,
        "join region starts with 2 control predecessors"
    );
    let pre_phi_inputs: Vec<_> = function.node_inputs(phi).into_iter().collect();
    assert_eq!(pre_phi_inputs.len(), 3, "phi: [token, val0, val1]");
    // Capture pred-1's value (phi index 2) before removal.
    let pred1_val = pre_phi_inputs[2];

    // Act: remove predecessor 0 via the RewriteCtx.
    let mut ctx = RewriteCtx::try_for_built(&mut function).unwrap();
    ctx.remove_region_predecessors(region, &[0])
        .expect("remove_region_predecessors must succeed");

    // Region drops to 1 control input.
    assert_eq!(
        function.node_inputs(region).len(),
        1,
        "region drops to 1 ctrl input"
    );

    // Phi must have exactly 2 inputs: [token, surviving value].
    let phi_inputs: Vec<_> = function.node_inputs(phi).into_iter().collect();
    assert_eq!(phi_inputs.len(), 2, "phi: [token, surviving value]");
    assert_eq!(phi_inputs[1], pred1_val, "surviving slot is pred 1's value");
}
