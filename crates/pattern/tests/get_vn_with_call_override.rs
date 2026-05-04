//! `Match::get_vn` consults per-Call clobber-list override before
//! falling back to `BuiltFunctionGraph::call_clobbered`.

#![allow(clippy::unwrap_used)]

use ir::BuiltFunctionGraph;
use ir::FunctionBuilder;
use ir::Graph;
use ir::node::{NodeKind, NodeOutputKind, NodeOutputType};
use pattern::{Binding, Bindings, Capture};
use target::{BuiltCallingConvention, CallingConvention, SleighArch};

#[test]
fn build_call_with_cc_override_records_empty_clobber_list() {
    let arch = SleighArch::x86_64();
    let regs = arch.probe_regs().unwrap();
    let rax = regs.name_to_vn("RAX").unwrap();
    let rsp = regs.name_to_vn("RSP").unwrap();

    let cc = CallingConvention::x86_64_systemv_abi().build(&regs).unwrap();
    let mut b = FunctionBuilder::new(vec![rax, rsp], &cc).unwrap();
    let region = b.create_region().unwrap();
    b.set_entry_region(region).unwrap();
    b.set_region(region);

    // Override: every tracked variable is callee-saved → 0 clobbers.
    // (Builder auto-adds rdx, xmm0, xmm1 to tracked through ret_val_regs.)
    let rdx = regs.name_to_vn("RDX").unwrap();
    let xmm0 = regs.name_to_vn("XMM0").unwrap();
    let xmm1 = regs.name_to_vn("XMM1").unwrap();
    let override_cc = BuiltCallingConvention {
        arg_passing_regs: vec![],
        callee_saved_regs: vec![rax, rdx, xmm0, xmm1],
        ret_val_regs: vec![],
        ret_val_regs_float: vec![],
        stack_ptr_vn: rsp,
        stack_arg_offsets: vec![],
        ret_stack_pop: 0,
        link_register_vn: None,
        syscall_number_vn: None,
    };
    let addr = b.build_int_const(0xdead_u64, NodeOutputType::U64).unwrap();
    let _call_node = b.build_call_with_cc(addr, Some(&override_cc)).unwrap();
    let ret_regs: Vec<rsleigh::Vn> = b.ret_val_vars().to_vec();
    b.build_return(None, &ret_regs).unwrap();
    let bfg = b.build().unwrap();

    // The single Call has 0 clobber outputs; side-table records empty list.
    let call_id = bfg
        .graph
        .all_node_ids()
        .find(|n| matches!(bfg.graph.node_kind(*n), NodeKind::Call))
        .unwrap();
    assert_eq!(bfg.graph.call_clobbered_override(call_id), Some(&[][..]));
    assert_eq!(bfg.graph.node_outputs(call_id).len(), 2);
}

#[test]
fn get_vn_indexes_override_list_for_overridden_call() {
    // Synthetic graph with a single Call node carrying a per-Call
    // override list of `[rax]`.  `get_vn` on slot 2 (the first
    // clobber slot) must return rax even though the function-default
    // `call_clobbered` is empty.
    let arch = SleighArch::x86_64();
    let regs = arch.probe_regs().unwrap();
    let rax = regs.name_to_vn("RAX").unwrap();

    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let entry_ctrl = graph.node_outputs_exact::<1>(entry).unwrap()[0];
    let entry_mem = graph.node_outputs_exact::<1>(mem).unwrap()[0];
    let target_const = graph.create_node(
        NodeKind::IntConst(0xdead),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let target_out = graph.node_outputs_exact::<1>(target_const).unwrap()[0];
    let call = graph.create_node(
        NodeKind::Call,
        [entry_ctrl, entry_mem, target_out],
        [
            NodeOutputKind::Control,
            NodeOutputKind::Memory,
            NodeOutputKind::OutputType(NodeOutputType::U64),
        ],
    );
    graph.set_call_clobbered_override(call, vec![rax]);
    let bfg = BuiltFunctionGraph::from_graph_and_entry(graph, entry);

    let c = Capture::new();
    let slot2 = bfg.graph.node_outputs(call).into_iter().nth(2).unwrap();
    let mut bindings = Bindings::default();
    bindings.bind_capture(c, Binding::new(call, Some(slot2)));
    let m = pattern::Match::new_for_test(call, bindings);
    assert_eq!(m.get_vn(c, &bfg), Some(rax));
}
