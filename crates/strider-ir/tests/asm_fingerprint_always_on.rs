#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::todo
)]

//! Layer-C asm-fingerprint check must fire on default `validate()`.
//!
//! Every optimization pass that forgets `extend_asm_fingerprint_from`
//! would otherwise produce silently invalid output.

use strider_ir::node::{NodeKind, NodeOutputKind, NodeOutputType};
use strider_ir::{Function, IntBinaryOp};

#[test]
fn default_validate_flags_missing_asm_fingerprint() {
    let mut g = Function::new();
    // Entry + InitialMemory are required by graph-invariants uniqueness checks.
    let entry = g.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let mem = g.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let mem_out = g.node_outputs(mem).iter().copied().next().unwrap();

    // Two constants and an Add — these are NOT structural / exempt kinds,
    // so they MUST carry a non-empty asm fingerprint to pass the graph-invariants check.
    let a = g.create_node(
        NodeKind::IntConst(1),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let b = g.create_node(
        NodeKind::IntConst(2),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let a_out = g.node_outputs(a).iter().copied().next().unwrap();
    let b_out = g.node_outputs(b).iter().copied().next().unwrap();
    let add = g.create_node(
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        [a_out, b_out],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let add_out = g.node_outputs(add).iter().copied().next().unwrap();

    // Wire reachability: Entry → Region → Return(Add).
    let entry_out = g.node_outputs(entry).iter().copied().next().unwrap();
    let cs = g.create_node(
        NodeKind::Region,
        [entry_out],
        [NodeOutputKind::Control, NodeOutputKind::PhiToken],
    );
    let cs_ctrl = g.node_outputs(cs).iter().copied().next().unwrap();
    let _ret = g.create_node(NodeKind::Return, [cs_ctrl, mem_out, add_out], []);

    let result = strider_ir::validate::validate(&g, entry);
    assert!(
        result.is_err(),
        "default validate must catch missing fingerprint",
    );
    let err = result.unwrap_err();
    let msg = format!("{:?}", err);
    assert!(
        msg.contains("MissingAsmFingerprint")
            || msg.contains("asm_fingerprint")
            || msg.contains("fingerprint"),
        "error must mention fingerprint: {msg}",
    );
}
