//! `Match::get_vn` returns the right varnode for a CallOther's
//! clobber output slot.  Both the function-default
//! (`BuiltFunctionGraph::call_other_clobbered`) and the per-CallOther
//! override (`Graph::call_clobbered_overrides`) are exercised.

#![allow(clippy::unwrap_used)]

use strider_ir::BuiltFunctionGraph;
use strider_ir::Graph;
use strider_ir::node::{NodeKind, NodeOutputKind, NodeOutputType};
use pattern::{Binding, Bindings, Capture};
use target::SleighArch;

#[test]
fn get_vn_for_callother_clobber_slot_uses_function_default() {
    let arch = SleighArch::x86_64();
    let regs = arch.probe_regs().unwrap();
    let rax = regs.name_to_vn("RAX").unwrap();

    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let entry_ctrl = graph.node_outputs_exact::<1>(entry).unwrap()[0];
    let entry_mem = graph.node_outputs_exact::<1>(mem).unwrap()[0];
    let callother = graph.create_node(
        NodeKind::CallOther { user_op_id: 7 },
        [entry_ctrl, entry_mem],
        [
            NodeOutputKind::Control,
            NodeOutputKind::Memory,
            NodeOutputKind::OutputType(NodeOutputType::U64),
        ],
    );
    let mut bfg = BuiltFunctionGraph::from_graph_and_entry_for_rewrite(graph, entry);
    bfg.set_call_other_clobbered_for_test(Box::new([rax]));

    let c = Capture::new();
    let slot2 = bfg.graph.node_outputs(callother).into_iter().nth(2).unwrap();
    let mut bindings = Bindings::default();
    bindings.bind_capture_for_test(c, Binding::new(callother, Some(slot2)));
    let m = pattern::Match::new_for_test(callother, bindings);
    assert_eq!(m.get_vn(c, &bfg), Some(rax));
}

#[test]
fn get_vn_for_callother_with_value_output_skips_value_slot() {
    // CallOther with a value output at slot 2 and a clobber output at
    // slot 3.  call_other_clobbered = [rax]; total outputs = 4 =
    // 3 + 1 ⇒ value-bearing form, clobber_start = 3.
    let arch = SleighArch::x86_64();
    let regs = arch.probe_regs().unwrap();
    let rax = regs.name_to_vn("RAX").unwrap();

    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let entry_ctrl = graph.node_outputs_exact::<1>(entry).unwrap()[0];
    let entry_mem = graph.node_outputs_exact::<1>(mem).unwrap()[0];
    let callother = graph.create_node(
        NodeKind::CallOther { user_op_id: 7 },
        [entry_ctrl, entry_mem],
        [
            NodeOutputKind::Control,
            NodeOutputKind::Memory,
            NodeOutputKind::OutputType(NodeOutputType::U32),
            NodeOutputKind::OutputType(NodeOutputType::U64),
        ],
    );
    let mut bfg = BuiltFunctionGraph::from_graph_and_entry_for_rewrite(graph, entry);
    bfg.set_call_other_clobbered_for_test(Box::new([rax]));

    let c = Capture::new();
    let slot3 = bfg.graph.node_outputs(callother).into_iter().nth(3).unwrap();
    let mut bindings = Bindings::default();
    bindings.bind_capture_for_test(c, Binding::new(callother, Some(slot3)));
    let m = pattern::Match::new_for_test(callother, bindings);
    assert_eq!(m.get_vn(c, &bfg), Some(rax));

    // Slot 2 (the value output) returns None (no varnode mapping).
    let c2 = Capture::new();
    let slot2 = bfg.graph.node_outputs(callother).into_iter().nth(2).unwrap();
    let mut bindings2 = Bindings::default();
    bindings2.bind_capture_for_test(c2, Binding::new(callother, Some(slot2)));
    let m2 = pattern::Match::new_for_test(callother, bindings2);
    assert_eq!(m2.get_vn(c2, &bfg), None);
}

#[test]
fn get_vn_for_callother_clobber_slot_uses_override_when_set() {
    // CallOther with per-CallOther clobber override.  Override list =
    // [rbx]; function-default = [rax].  Slot 2 binding must return rbx.
    let arch = SleighArch::x86_64();
    let regs = arch.probe_regs().unwrap();
    let rax = regs.name_to_vn("RAX").unwrap();
    let rbx = regs.name_to_vn("RBX").unwrap();

    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let entry_ctrl = graph.node_outputs_exact::<1>(entry).unwrap()[0];
    let entry_mem = graph.node_outputs_exact::<1>(mem).unwrap()[0];
    let callother = graph.create_node(
        NodeKind::CallOther { user_op_id: 7 },
        [entry_ctrl, entry_mem],
        [
            NodeOutputKind::Control,
            NodeOutputKind::Memory,
            NodeOutputKind::OutputType(NodeOutputType::U64),
        ],
    );
    graph.set_call_clobbered_override(callother, vec![rbx]);
    let mut bfg = BuiltFunctionGraph::from_graph_and_entry_for_rewrite(graph, entry);
    bfg.set_call_other_clobbered_for_test(Box::new([rax]));

    let c = Capture::new();
    let slot2 = bfg.graph.node_outputs(callother).into_iter().nth(2).unwrap();
    let mut bindings = Bindings::default();
    bindings.bind_capture_for_test(c, Binding::new(callother, Some(slot2)));
    let m = pattern::Match::new_for_test(callother, bindings);
    assert_eq!(m.get_vn(c, &bfg), Some(rbx),
               "per-CallOther override must shadow function-default");
}
