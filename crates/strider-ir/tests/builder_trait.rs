#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Not a unit test in `src/builder/`: `strider_ir_test_utils` is a dev-dep
//! returning `strider-ir`'s own types, and a unit-test block would see a
//! second compilation of the crate. Integration tests link the same
//! `strider-ir` downstream crates do.

use strider_ir::node::{NodeKind, ValueKind, ValueType};
use strider_ir::{IRBuilder, IRViewer};
use strider_ir_test_utils::empty_builder;

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
