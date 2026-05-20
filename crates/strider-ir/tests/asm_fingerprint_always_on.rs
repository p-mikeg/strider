//! Layer-C asm-fingerprint check must fire on default `validate()` — not
//! require `validate_with_options(check_asm_fingerprints: true)`.
//!
//! Phase 1 Task 1.4 / Generalization Audit finding G3: the always-on Layer-C
//! check is the highest-correctness change in the audit. Every optimization
//! pass that forgets `extend_asm_fingerprint_from` would otherwise produce
//! silently invalid output.

use strider_ir::node::{NodeKind, NodeOutputKind, NodeOutputType};
use strider_ir::{Graph, IntBinaryOp};

#[test]
fn default_validate_flags_missing_asm_fingerprint() {
    let mut g = Graph::new();
    // Entry + InitialMemory are required by graph-invariants uniqueness checks.
    let entry = g.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let mem = g.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let mem_out = g.node_outputs(mem).into_iter().next().unwrap();

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
    let a_out = g.node_outputs(a).into_iter().next().unwrap();
    let b_out = g.node_outputs(b).into_iter().next().unwrap();
    let add = g.create_node(
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        [a_out, b_out],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let add_out = g.node_outputs(add).into_iter().next().unwrap();

    // Wire reachability: Entry → ControlState → Return(Add).
    let entry_out = g.node_outputs(entry).into_iter().next().unwrap();
    let cs = g.create_node(
        NodeKind::ControlState,
        [entry_out],
        [NodeOutputKind::Control, NodeOutputKind::PhiToken],
    );
    let cs_ctrl = g.node_outputs(cs).into_iter().next().unwrap();
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
