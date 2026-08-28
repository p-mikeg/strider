#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo
)]

//! The asm-fingerprint check must fire on a plain `validate()`.

use strider_ir::node::{NodeKind, ValueKind, ValueType};
use strider_ir::{Function, IRViewer, IntBinaryOp};

#[test]
fn default_validate_flags_missing_asm_fingerprint() {
    let mut function = Function::new(
        strider_target::BuiltCallingConvention::default(),
        strider_target::Endianness::Little,
        Vec::new(),
    );
    let entry = function.entry();
    // Dedups against the skeleton `Function::new` already built.
    let mem = function
        .graph_mut()
        .create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    let mem_value = function.node_outputs(mem).iter().copied().next().unwrap();

    // Consts and Add are not fingerprint-exempt, and are left unstamped.
    let a_id = function.intern_int_const(1, ValueType::I64);
    let a = function.graph_mut().create_node(
        NodeKind::IntConst(a_id),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    let b_id = function.intern_int_const(2, ValueType::I64);
    let b = function.graph_mut().create_node(
        NodeKind::IntConst(b_id),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    let a_value = function.node_outputs(a).iter().copied().next().unwrap();
    let b_value = function.node_outputs(b).iter().copied().next().unwrap();
    let add = function.graph_mut().create_node(
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        [a_value, b_value],
        [ValueKind::Typed(ValueType::I64)],
    );
    let add_value = function.node_outputs(add).iter().copied().next().unwrap();

    // Entry -> Region -> Return(Add), so the nodes are reachable.
    let entry_value = function.node_outputs(entry).iter().copied().next().unwrap();
    let cs = function.graph_mut().create_node(
        NodeKind::Region,
        [entry_value],
        [ValueKind::Control, ValueKind::PhiToken],
    );
    let cs_ctrl = function.node_outputs(cs).iter().copied().next().unwrap();
    let _ret =
        function
            .graph_mut()
            .create_node(NodeKind::Return, [cs_ctrl, mem_value, add_value], []);

    let result = strider_ir::validate::validate(&function);
    assert!(
        result.is_err(),
        "default validate must catch missing fingerprint",
    );
    let err = result.unwrap_err();
    let msg = format!("{err:?}");
    assert!(
        msg.contains("MissingAsmFingerprint")
            || msg.contains("asm_fingerprint")
            || msg.contains("fingerprint"),
        "error must mention fingerprint: {msg}",
    );
}
