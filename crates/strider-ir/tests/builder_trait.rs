#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Integration tests for the `IRBuilder` creation trait.
//!
//! Lives here (not in `src/builder/build_trait.rs`) because
//! `strider_ir_test_utils` is a dev-dependency; using it from within
//! `strider-ir`'s own unit-test blocks would cause a double-compile
//! mismatch.  Integration tests get the same compilation of `strider-ir`
//! that downstream crates use, so there is no mismatch.

use strider_ir::node::{NodeKind, ValueKind, ValueType};
use strider_ir::{IRBuilder, IRViewer};
use strider_ir_test_utils::empty_builder;

/// `FunctionBuilder`'s `IRBuilder` impl stamps the active `lift_addr` into
/// the resulting node's asm fingerprint, delegating to the inherent
/// `create_node` attribution policy.
#[test]
fn lift_builder_trait_stamps_lift_addr() {
    let mut b = empty_builder().unwrap();
    b.set_lift_addr(Some(0x4000));
    let id = b.function_mut().intern_int_const(3, ValueType::I64);
    let n = <strider_ir::FunctionBuilder as IRBuilder>::create_node(
        &mut b,
        NodeKind::IntConst(id),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    let fp = IRViewer::function(&b).side_tables().asm_fingerprint(n);
    assert!(
        fp.contains(&0x4000),
        "expected fingerprint to contain 0x4000, got {fp:?}"
    );
}
