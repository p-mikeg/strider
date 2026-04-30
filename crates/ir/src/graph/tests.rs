//! White-box tests for the graph submodules — arena, dedup cache,
//! use-list bookkeeping, and typed accessors.

use super::*;
use crate::node::{NodeKind, NodeOutputType};

// ── helpers ───────────────────────────────────────────────────────────────

#[track_caller]
fn check_node_inputs(
    graph: &Graph,
    node_id: NodeId,
    expected: impl IntoIterator<Item = NodeOutputId>,
) {
    let expected: Vec<_> = expected.into_iter().collect();
    let actual: Vec<_> = graph.node_inputs(node_id).into_iter().collect();
    assert_eq!(actual, expected);
}

#[track_caller]
fn check_node_output_kinds(
    graph: &Graph,
    node_id: NodeId,
    expected: impl IntoIterator<Item = NodeOutputKind>,
) {
    let expected: Vec<_> = expected.into_iter().collect();
    let actual: Vec<_> = graph
        .node_outputs(node_id)
        .into_iter()
        .map(|output_id| graph.output_kind(output_id))
        .collect();
    assert_eq!(actual, expected);
}

#[track_caller]
fn check_node_output_defintions(
    graph: &Graph,
    node_id: NodeId,
    expected: impl IntoIterator<Item = (NodeId, u32)>,
) {
    let expected: Vec<_> = expected.into_iter().collect();
    let actual: Vec<_> = graph
        .node_outputs(node_id)
        .into_iter()
        .map(|output_id| graph.output_definition(output_id))
        .collect();
    assert_eq!(actual, expected);
}

/// Creates a simple constant node (no inputs) and checks that its
/// metadata is stored correctly.
#[test]
fn create_single_node() {
    let mut graph = Graph::new();
    let node_id = graph.create_node(
        NodeKind::IntConst(5),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    assert_eq!(graph.node_kind(node_id), &NodeKind::IntConst(5));
    assert_eq!(graph.nodes.len(), 1);
    check_node_inputs(&graph, node_id, []);
    check_node_output_kinds(
        &graph,
        node_id,
        vec![NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    check_node_output_defintions(&graph, node_id, vec![(node_id, 0)]);
}

/// `kind_of_output` agrees with the two-step `node_kind(get_node_from_output(out))`
/// lookup it replaces — pinned because ~100 callsites depend on the equivalence.
#[test]
fn kind_of_output_matches_two_step_lookup() {
    let mut graph = Graph::new();
    let node_id = graph.create_node(
        NodeKind::IntConst(7),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let [out] = graph.node_outputs_exact::<1>(node_id).unwrap();
    let two_step = graph.node_kind(graph.get_node_from_output(out));
    let one_step = graph.kind_of_output(out);
    assert_eq!(one_step, two_step);
    assert_eq!(one_step, &NodeKind::IntConst(7));
}

/// Cacheable nodes with identical kind and inputs must be deduplicated:
/// the second call must return the same [`NodeId`] as the first and must
/// not grow the node table.
#[test]
fn cacheable_node_is_deduplicated() {
    let mut graph = Graph::new();
    let id_a = graph.create_node(
        NodeKind::IntConst(42),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U32)],
    );
    let id_b = graph.create_node(
        NodeKind::IntConst(42),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U32)],
    );
    assert_eq!(
        id_a, id_b,
        "identical cacheable nodes must alias to the same id"
    );
    assert_eq!(
        graph.nodes.len(),
        1,
        "deduplication must not create a second node"
    );
}

/// Non-cacheable nodes (e.g. `Return`) must always produce fresh ids even
/// when all arguments are identical.
#[test]
fn non_cacheable_node_is_never_deduplicated() {
    let mut graph = Graph::new();
    let id_a = graph.create_node(NodeKind::Return, [], []);
    let id_b = graph.create_node(NodeKind::Return, [], []);
    assert_ne!(
        id_a, id_b,
        "non-cacheable nodes must always produce distinct ids"
    );
}

/// Two adjacent `Call` nodes with identical target and argument outputs
/// must stay distinct — Call is non-cacheable because `CallStackArgCollect`
/// mutates its inputs after construction.
#[test]
fn adjacent_calls_with_same_args_are_distinct() {
    let mut graph = Graph::new();
    let ctrl_a = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let mem_a = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let [ctrl_out] = graph.node_outputs_exact::<1>(ctrl_a).unwrap();
    let [mem_out] = graph.node_outputs_exact::<1>(mem_a).unwrap();
    let target = graph.create_node(
        NodeKind::IntConst(0x1000),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let [target_out] = graph.node_outputs_exact::<1>(target).unwrap();
    let outs = [NodeOutputKind::Control, NodeOutputKind::Memory];
    let call_a = graph.create_node(NodeKind::Call, [ctrl_out, mem_out, target_out], outs);
    let call_b = graph.create_node(NodeKind::Call, [ctrl_out, mem_out, target_out], outs);
    assert_ne!(
        call_a, call_b,
        "Call is non-cacheable so identical-argument calls must be distinct"
    );
}

/// `Graph::call_other_name` round-trip: setting and reading back a name
/// works, and unset nodes return `None`.  This is the side-table parallel
/// to `stack_phi_offsets` for `CallOther` nodes — kept external so the
/// node payload (`user_op_id: u64`) stays `Copy`.
#[test]
fn call_other_name_round_trip() {
    let mut graph = Graph::new();
    // Two CallOther nodes with the same user_op_id.  CallOther is
    // non-cacheable (see `is_cacheable`), so they get distinct ids.
    let outs = [NodeOutputKind::Control, NodeOutputKind::Memory];
    // We need a control + memory input to construct a CallOther; build a
    // throwaway Entry and InitialMemory.
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let [entry_ctrl] = graph.node_outputs_exact::<1>(entry).unwrap();
    let init_mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let [init_mem_out] = graph.node_outputs_exact::<1>(init_mem).unwrap();
    let id_a = graph.create_node(
        NodeKind::CallOther { user_op_id: 62 },
        [entry_ctrl, init_mem_out],
        outs,
    );
    let id_b = graph.create_node(
        NodeKind::CallOther { user_op_id: 62 },
        [entry_ctrl, init_mem_out],
        outs,
    );
    assert_ne!(id_a, id_b, "CallOther is non-cacheable");
    assert_eq!(graph.call_other_name(id_a), None);
    graph.set_call_other_name(id_a, "setISAMode".to_string());
    assert_eq!(graph.call_other_name(id_a), Some("setISAMode"));
    assert_eq!(graph.call_other_name(id_b), None);
    // Replacement
    graph.set_call_other_name(id_a, "OtherName".to_string());
    assert_eq!(graph.call_other_name(id_a), Some("OtherName"));
}

/// `StackStorePhi` is non-cacheable; its offsets live in a side-map and
/// two distinct phis with the same space and inputs must remain distinct.
#[test]
fn stack_store_phi_is_never_deduplicated() {
    let mut graph = Graph::new();
    let space = rsleigh::VnSpace::RAM;
    let id_a = graph.create_node(
        NodeKind::StackStorePhi { space },
        [],
        [NodeOutputKind::Memory],
    );
    let id_b = graph.create_node(
        NodeKind::StackStorePhi { space },
        [],
        [NodeOutputKind::Memory],
    );
    assert_ne!(id_a, id_b);
    graph.set_stack_phi_offsets(id_a, vec![0, -4]);
    assert_eq!(graph.stack_phi_offsets(id_a), &[0, -4]);
    assert_eq!(graph.stack_phi_offsets(id_b), &[] as &[i64]);
}

/// After adding an input to a non-cacheable node the output's use-list
/// must contain exactly that input, and `node_inputs` must reflect it.
#[test]
fn add_node_input_registers_use() {
    let mut graph = Graph::new();
    // Produce a value
    let const_node = graph.create_node(
        NodeKind::IntConst(1),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let [const_out] = graph.node_outputs_exact::<1>(const_node).unwrap();

    // Create a non-cacheable sink
    let ret_node = graph.create_node(NodeKind::Return, [], []);

    graph.add_node_input(ret_node, const_out).unwrap();

    // The input must appear in node_inputs
    check_node_inputs(&graph, ret_node, [const_out]);

    // The output's use-list must contain this input
    let use_count = graph.output_uses(const_out).count();
    assert_eq!(use_count, 1);
}

/// `remove_node_input` must shrink the input list, update subsequent
/// input indices, and unregister the use from the output's use-list.
#[test]
fn remove_node_input_cleans_up_use_list() {
    let mut graph = Graph::new();

    let c0 = graph.create_node(
        NodeKind::IntConst(0),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let [out0] = graph.node_outputs_exact::<1>(c0).unwrap();

    let c1 = graph.create_node(
        NodeKind::IntConst(1),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let [out1] = graph.node_outputs_exact::<1>(c1).unwrap();

    let ret = graph.create_node(NodeKind::Return, [], []);
    graph.add_node_input(ret, out0).unwrap();
    graph.add_node_input(ret, out1).unwrap();

    // Remove the first input (index 0 = out0)
    graph.remove_node_input(ret, 0).unwrap();

    // Only out1 should remain
    check_node_inputs(&graph, ret, [out1]);

    // out0 must no longer be used
    assert_eq!(
        graph.output_uses(out0).count(),
        0,
        "out0 should have no uses after removal"
    );
    // out1 must still be used
    assert_eq!(
        graph.output_uses(out1).count(),
        1,
        "out1 should still have one use"
    );

    // The surviving input must have its index adjusted to 0
    let inputs_slice = graph.nodes[ret].inputs.as_slice(&graph.input_pool);
    assert_eq!(graph.inputs[inputs_slice[0]].input_index, 0);
}

/// `update_input` must move the use from the old output to the new one
/// so that use-lists stay consistent.
#[test]
fn update_input_moves_use_to_new_output() {
    let mut graph = Graph::new();

    let old = graph.create_node(
        NodeKind::IntConst(10),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let [old_out] = graph.node_outputs_exact::<1>(old).unwrap();

    let new = graph.create_node(
        NodeKind::IntConst(20),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let [new_out] = graph.node_outputs_exact::<1>(new).unwrap();

    let ret = graph.create_node(NodeKind::Return, [], []);
    graph.add_node_input(ret, old_out).unwrap();

    // Find the single input id
    let input_id = graph.nodes[ret].inputs.as_slice(&graph.input_pool)[0];

    graph.update_input(input_id, new_out);

    // old_out must have no uses; new_out must have one
    assert_eq!(graph.output_uses(old_out).count(), 0);
    assert_eq!(graph.output_uses(new_out).count(), 1);

    // The node input must now reference new_out
    check_node_inputs(&graph, ret, [new_out]);
}

/// `detach_node_inputs` must clear all inputs from the node and remove
/// them from every output's use-list.
#[test]
fn detach_node_inputs_removes_all_uses() {
    let mut graph = Graph::new();

    let c = graph.create_node(
        NodeKind::IntConst(5),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let [out] = graph.node_outputs_exact::<1>(c).unwrap();

    let ret = graph.create_node(NodeKind::Return, [], []);
    graph.add_node_input(ret, out).unwrap();
    graph.add_node_input(ret, out).unwrap(); // same output used twice

    assert_eq!(graph.output_uses(out).count(), 2);

    graph.detach_node_inputs(ret);

    assert_eq!(
        graph.output_uses(out).count(),
        0,
        "all uses must be removed after detach"
    );
    assert_eq!(
        graph.node_inputs(ret).len(),
        0,
        "node must have no inputs after detach"
    );
}

/// After `detach_node_inputs` on a cacheable node, a subsequent
/// `create_node` call with the same `(kind, inputs, output_kinds)` must
/// produce a fresh, fully-connected node — not return the detached
/// zombie whose input list is empty.
///
/// Regression: before the dedup-cache was cleaned on detach, optimizer
/// passes that created identical Adds after `RedundantPhis` had detached
/// the original unreachable Add would alias to the zombie, and any
/// follow-up pass calling `node_inputs_exact::<2>` would fail with
/// `WrongInputCount(..., 2, 0)`.
#[test]
fn detach_evicts_cacheable_node_from_dedup_cache() {
    use crate::ops::IntBinaryOp;
    let mut graph = Graph::new();
    let lhs = graph.create_node(
        NodeKind::IntConst(7),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U32)],
    );
    let rhs = graph.create_node(
        NodeKind::IntConst(9),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U32)],
    );
    let [lhs_out] = graph.node_outputs_exact::<1>(lhs).unwrap();
    let [rhs_out] = graph.node_outputs_exact::<1>(rhs).unwrap();

    let ty = NodeOutputKind::OutputType(NodeOutputType::U32);
    let add_a = graph.create_node(
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        [lhs_out, rhs_out],
        [ty],
    );

    graph.detach_node_inputs(add_a);
    assert_eq!(graph.node_inputs(add_a).len(), 0);

    let add_b = graph.create_node(
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        [lhs_out, rhs_out],
        [ty],
    );

    assert_ne!(
        add_a, add_b,
        "detach must evict the zombie from the dedup cache so a re-created \
         identical node is fresh"
    );
    assert_eq!(
        graph.node_inputs(add_b).len(),
        2,
        "the re-created node must be fully connected"
    );
}

/// An output consumed by a single node must be reported by
/// `output_has_one_usage` as `true`; consuming it a second time must
/// flip it to `false`.
#[test]
fn output_has_one_usage_tracks_consumer_count() {
    let mut graph = Graph::new();

    let c = graph.create_node(
        NodeKind::IntConst(99),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U32)],
    );
    let [out] = graph.node_outputs_exact::<1>(c).unwrap();

    assert!(!graph.output_has_one_usage(out), "zero uses is not one");

    let ret1 = graph.create_node(NodeKind::Return, [], []);
    graph.add_node_input(ret1, out).unwrap();
    assert!(
        graph.output_has_one_usage(out),
        "one use should return true"
    );

    let ret2 = graph.create_node(NodeKind::Return, [], []);
    graph.add_node_input(ret2, out).unwrap();
    assert!(
        !graph.output_has_one_usage(out),
        "two uses should return false"
    );
}

/// `get_node_from_output` must return the node that created the output.
#[test]
fn get_node_from_output_returns_source_node() {
    let mut graph = Graph::new();
    let node = graph.create_node(
        NodeKind::IntConst(7),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U8)],
    );
    let [out] = graph.node_outputs_exact::<1>(node).unwrap();
    assert_eq!(graph.get_node_from_output(out), node);
}

/// A node with two outputs must expose both with correct kinds and
/// definitions.
#[test]
fn node_with_multiple_outputs() {
    let mut graph = Graph::new();
    let node = graph.create_node(
        NodeKind::If,
        [],
        [NodeOutputKind::Control, NodeOutputKind::Control],
    );
    let [true_ctrl, false_ctrl] = graph.node_outputs_exact::<2>(node).unwrap();
    assert_eq!(graph.output_kind(true_ctrl), NodeOutputKind::Control);
    assert_eq!(graph.output_kind(false_ctrl), NodeOutputKind::Control);
    assert_eq!(graph.output_definition(true_ctrl), (node, 0));
    assert_eq!(graph.output_definition(false_ctrl), (node, 1));
}

/// `output_uses` must yield one `(node_id, input_index)` tuple per
/// consumer, with the correct node id and position within that node's
/// input list.  Three independent consumers all at input-index 0 must
/// all appear exactly once.
#[test]
fn output_uses_reports_all_consumers_with_correct_indices() {
    let mut graph = Graph::new();
    let src = graph.create_node(
        NodeKind::IntConst(7),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U32)],
    );
    let [out] = graph.node_outputs_exact::<1>(src).unwrap();

    let ret0 = graph.create_node(NodeKind::Return, [], []);
    graph.add_node_input(ret0, out).unwrap();
    let ret1 = graph.create_node(NodeKind::Return, [], []);
    graph.add_node_input(ret1, out).unwrap();
    let ret2 = graph.create_node(NodeKind::Return, [], []);
    graph.add_node_input(ret2, out).unwrap();

    let uses: Vec<(NodeId, u32)> = graph.output_uses(out).collect();
    assert_eq!(uses.len(), 3, "all three consumers must appear");

    for expected_node in [ret0, ret1, ret2] {
        assert!(
            uses.iter().any(|(n, _)| *n == expected_node),
            "consumer {expected_node:?} missing from output_uses"
        );
    }
    // Each of the three nodes has exactly one input, so input_index is 0.
    for (_, idx) in &uses {
        assert_eq!(*idx, 0, "each single-input node's input_index must be 0");
    }
}

/// When a node has multiple inputs from the same output, `output_uses`
/// must report all of them with their correct positional indices.
#[test]
fn output_uses_same_output_multiple_times_reports_each_position() {
    let mut graph = Graph::new();
    let src = graph.create_node(
        NodeKind::IntConst(3),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let [out] = graph.node_outputs_exact::<1>(src).unwrap();

    // Same output at positions 0 and 1 of the same sink node.
    let sink = graph.create_node(NodeKind::Return, [], []);
    graph.add_node_input(sink, out).unwrap(); // input_index 0
    graph.add_node_input(sink, out).unwrap(); // input_index 1

    let uses: Vec<(NodeId, u32)> = graph.output_uses(out).collect();
    assert_eq!(uses.len(), 2);

    let mut indices: Vec<u32> = uses.iter().map(|(_, i)| *i).collect();
    indices.sort_unstable();
    assert_eq!(indices, vec![0, 1], "both positional indices must appear");
}

/// `output_use_cursor` iterates the same set as `output_uses`.
/// `replace_current_with` must redirect the first use to a new output
/// and advance past it so the remaining use is untouched.
#[test]
fn output_use_cursor_replace_redirects_first_use() {
    let mut graph = Graph::new();

    let old_src = graph.create_node(
        NodeKind::IntConst(1),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let [old_out] = graph.node_outputs_exact::<1>(old_src).unwrap();

    let new_src = graph.create_node(
        NodeKind::IntConst(2),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let [new_out] = graph.node_outputs_exact::<1>(new_src).unwrap();

    // Two consumers of old_out.
    let ret0 = graph.create_node(NodeKind::Return, [], []);
    graph.add_node_input(ret0, old_out).unwrap();
    let ret1 = graph.create_node(NodeKind::Return, [], []);
    graph.add_node_input(ret1, old_out).unwrap();

    assert_eq!(graph.output_uses(old_out).count(), 2);
    assert_eq!(graph.output_uses(new_out).count(), 0);

    // Redirect the first consumer to new_out.
    {
        let mut cursor = graph.output_use_cursor(old_out);
        cursor.replace_current_with(new_out).unwrap();
    }

    // After one replacement: old_out has one use, new_out has one use.
    assert_eq!(
        graph.output_uses(old_out).count(),
        1,
        "one use must remain on old_out"
    );
    assert_eq!(
        graph.output_uses(new_out).count(),
        1,
        "one use must move to new_out"
    );
}

/// `output_use_cursor` with `replace_current_with` applied to every
/// element must leave the original output with no uses and transfer all
/// uses to the replacement.
#[test]
fn output_use_cursor_replace_all_drains_source() {
    let mut graph = Graph::new();

    let old_src = graph.create_node(
        NodeKind::IntConst(10),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U32)],
    );
    let [old_out] = graph.node_outputs_exact::<1>(old_src).unwrap();

    let new_src = graph.create_node(
        NodeKind::IntConst(20),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U32)],
    );
    let [new_out] = graph.node_outputs_exact::<1>(new_src).unwrap();

    // Three consumers.
    for _ in 0..3 {
        let r = graph.create_node(NodeKind::Return, [], []);
        graph.add_node_input(r, old_out).unwrap();
    }
    assert_eq!(graph.output_uses(old_out).count(), 3);

    // Replace all uses in a single cursor pass.
    let mut cursor = graph.output_use_cursor(old_out);
    while cursor.current().is_some() {
        cursor.replace_current_with(new_out).unwrap();
    }

    assert_eq!(
        graph.output_uses(old_out).count(),
        0,
        "all uses must be drained from old_out"
    );
    assert_eq!(
        graph.output_uses(new_out).count(),
        3,
        "all uses must land on new_out"
    );
}

/// Removing the middle input of a three-input node must: leave the
/// two survivors in order, re-number their indices contiguously from 0,
/// and remove the deleted input from its output's use-list.
#[test]
fn remove_node_input_from_middle_reindexes_remaining() {
    let mut graph = Graph::new();

    let out0 = {
        let n = graph.create_node(
            NodeKind::IntConst(10),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        graph.node_outputs_exact::<1>(n).unwrap()[0]
    };
    let out1 = {
        let n = graph.create_node(
            NodeKind::IntConst(20),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        graph.node_outputs_exact::<1>(n).unwrap()[0]
    };
    let out2 = {
        let n = graph.create_node(
            NodeKind::IntConst(30),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        graph.node_outputs_exact::<1>(n).unwrap()[0]
    };

    let sink = graph.create_node(NodeKind::Return, [], []);
    graph.add_node_input(sink, out0).unwrap(); // index 0
    graph.add_node_input(sink, out1).unwrap(); // index 1
    graph.add_node_input(sink, out2).unwrap(); // index 2

    graph.remove_node_input(sink, 1).unwrap(); // remove middle

    check_node_inputs(&graph, sink, [out0, out2]);
    assert_eq!(graph.output_uses(out1).count(), 0, "out1 must be removed");
    assert_eq!(graph.output_uses(out0).count(), 1);
    assert_eq!(graph.output_uses(out2).count(), 1);

    let inputs_slice = graph.nodes[sink].inputs.as_slice(&graph.input_pool);
    assert_eq!(
        graph.inputs[inputs_slice[0]].input_index, 0,
        "surviving input 0 must have index 0"
    );
    assert_eq!(
        graph.inputs[inputs_slice[1]].input_index, 1,
        "surviving input 1 must have index 1"
    );
}

/// Removing the last input must not disturb the preceding inputs.
#[test]
fn remove_node_input_from_end_leaves_others_intact() {
    let mut graph = Graph::new();

    let out0 = {
        let n = graph.create_node(
            NodeKind::IntConst(1),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        graph.node_outputs_exact::<1>(n).unwrap()[0]
    };
    let out1 = {
        let n = graph.create_node(
            NodeKind::IntConst(2),
            [],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        graph.node_outputs_exact::<1>(n).unwrap()[0]
    };

    let sink = graph.create_node(NodeKind::Return, [], []);
    graph.add_node_input(sink, out0).unwrap();
    graph.add_node_input(sink, out1).unwrap();

    graph.remove_node_input(sink, 1).unwrap(); // remove last

    check_node_inputs(&graph, sink, [out0]);
    assert_eq!(graph.output_uses(out1).count(), 0);
    assert_eq!(graph.output_uses(out0).count(), 1);

    let inputs_slice = graph.nodes[sink].inputs.as_slice(&graph.input_pool);
    assert_eq!(graph.inputs[inputs_slice[0]].input_index, 0);
}

/// `update_input` on an input belonging to a cacheable node must evict the
/// stale dedup-cache entry. Otherwise a later `create_node` with the
/// original `(kind, inputs, outputs)` triple returns the now-modified
/// node, which has different inputs — silent miscompilation by the
/// optimizer (which calls `update_input` via `replace_all_uses`).
#[test]
fn update_input_on_cacheable_evicts_stale_cache_entry() {
    use crate::ops::IntBinaryOp;
    let mut graph = Graph::new();

    let a = graph.create_node(
        NodeKind::IntConst(1),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U32)],
    );
    let b = graph.create_node(
        NodeKind::IntConst(2),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U32)],
    );
    let c = graph.create_node(
        NodeKind::IntConst(3),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U32)],
    );
    let [a_out] = graph.node_outputs_exact::<1>(a).unwrap();
    let [b_out] = graph.node_outputs_exact::<1>(b).unwrap();
    let [c_out] = graph.node_outputs_exact::<1>(c).unwrap();
    let ty = NodeOutputKind::OutputType(NodeOutputType::U32);

    // Cache key inserted: (Add, [a, b], [ty]) → add_ab.
    let add_ab = graph.create_node(
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        [a_out, b_out],
        [ty],
    );

    // Redirect input[0] from a → c. Node now actually has inputs [c, b],
    // but the cache (if not maintained) still maps [a, b] → add_ab.
    let in0 = graph.node_input_id_at(add_ab, 0).unwrap();
    graph.update_input(in0, c_out);

    // Re-create with the ORIGINAL key. Must NOT return add_ab — its
    // current inputs are [c, b], not [a, b].
    let fresh = graph.create_node(
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        [a_out, b_out],
        [ty],
    );
    assert_ne!(
        add_ab, fresh,
        "the stale cache entry must be evicted — re-creating the original \
         (kind, inputs, outputs) triple after update_input has redirected \
         one of those inputs must produce a fresh NodeId"
    );
}

/// `update_input` where the new output equals the old output must leave
/// the use count unchanged and keep the node input pointing at the same
/// output.
#[test]
fn update_input_to_same_output_is_idempotent() {
    let mut graph = Graph::new();

    let src = graph.create_node(
        NodeKind::IntConst(99),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U32)],
    );
    let [out] = graph.node_outputs_exact::<1>(src).unwrap();

    let sink = graph.create_node(NodeKind::Return, [], []);
    graph.add_node_input(sink, out).unwrap();

    let input_id = graph.nodes[sink].inputs.as_slice(&graph.input_pool)[0];
    graph.update_input(input_id, out);

    assert_eq!(
        graph.output_uses(out).count(),
        1,
        "self-update must not change use count"
    );
    check_node_inputs(&graph, sink, [out]);
}

/// After `detach_node_inputs`, re-adding the same inputs must restore
/// the use-list count to its original value.
#[test]
fn detach_then_readd_restores_use_count() {
    let mut graph = Graph::new();

    let src = graph.create_node(
        NodeKind::IntConst(42),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let [out] = graph.node_outputs_exact::<1>(src).unwrap();

    let sink = graph.create_node(NodeKind::Return, [], []);
    graph.add_node_input(sink, out).unwrap();
    graph.add_node_input(sink, out).unwrap();
    assert_eq!(graph.output_uses(out).count(), 2);

    graph.detach_node_inputs(sink);
    assert_eq!(
        graph.output_uses(out).count(),
        0,
        "uses cleared after detach"
    );
    assert_eq!(graph.node_inputs(sink).len(), 0);

    // Re-add; use count must be restored.
    graph.add_node_input(sink, out).unwrap();
    graph.add_node_input(sink, out).unwrap();
    assert_eq!(
        graph.output_uses(out).count(),
        2,
        "re-adding inputs must restore use count"
    );
    assert_eq!(graph.node_inputs(sink).len(), 2);
}

/// Two independent sinks each consuming the same output must all appear
/// in the use-list.  This verifies the linked-list stays consistent when
/// multiple distinct nodes reference the same output.
#[test]
fn two_independent_consumers_both_in_use_list() {
    let mut graph = Graph::new();

    let src = graph.create_node(
        NodeKind::IntConst(1),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let [out] = graph.node_outputs_exact::<1>(src).unwrap();

    let b = graph.create_node(NodeKind::Return, [], []);
    graph.add_node_input(b, out).unwrap();
    let c = graph.create_node(NodeKind::Return, [], []);
    graph.add_node_input(c, out).unwrap();

    let uses: Vec<_> = graph.output_uses(out).collect();
    assert_eq!(uses.len(), 2);
    let nodes: Vec<_> = uses.iter().map(|(n, _)| *n).collect();
    assert!(nodes.contains(&b), "b must appear in use-list");
    assert!(nodes.contains(&c), "c must appear in use-list");
}

/// `node_outputs_exact` must return `Err(WrongOutputCount)` when asked
/// for a count that does not match the actual number of outputs.
#[test]
fn node_outputs_exact_errors_on_wrong_count() {
    let mut graph = Graph::new();
    let node = graph.create_node(
        NodeKind::IntConst(0),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U8)],
    );
    let err = graph.node_outputs_exact::<2>(node).unwrap_err();
    assert!(
        err.to_string().contains("does not have exactly 2 outputs"),
        "got: {err}"
    );
}

/// `node_inputs_exact` must return `Err(WrongInputCount)` when asked for
/// a count that does not match the actual number of inputs.
#[test]
fn node_inputs_exact_errors_on_wrong_count() {
    let mut graph = Graph::new();
    let src = graph.create_node(
        NodeKind::IntConst(0),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let [out] = graph.node_outputs_exact::<1>(src).unwrap();

    let sink = graph.create_node(NodeKind::Return, [], []);
    graph.add_node_input(sink, out).unwrap(); // exactly 1 input

    let err = graph.node_inputs_exact::<2>(sink).unwrap_err();
    assert!(
        err.to_string().contains("does not have exactly 2 inputs"),
        "got: {err}"
    );
}

#[test]
fn update_input_self_redirect_preserves_use_list_order() {
    use crate::ops::IntUnaryOp;
    let mut graph = Graph::new();
    let c = graph.create_node(
        NodeKind::IntConst(0),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let cval = graph.node_outputs(c).into_iter().next().unwrap();
    // Two consumers of cval to give the use-list real ordering.
    let _a = graph.create_node(
        NodeKind::IntUnaryOp(IntUnaryOp::Neg),
        [cval],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let b = graph.create_node(
        NodeKind::IntUnaryOp(IntUnaryOp::Not),
        [cval],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );

    let head_before = graph.output_first_use_id(cval);

    let b_in0 = graph.node_input_id_at(b, 0).unwrap();
    graph.update_input(b_in0, cval); // self-redirect — should be a no-op

    assert_eq!(
        head_before,
        graph.output_first_use_id(cval),
        "self-redirect must not re-order the use-list"
    );
}

#[test]
fn remove_node_input_returns_error_on_out_of_bounds() {
    let mut graph = Graph::new();
    let cs = graph.create_node(
        NodeKind::ControlState,
        [],
        [NodeOutputKind::Control, NodeOutputKind::PhiToken],
    );
    let err = graph.remove_node_input(cs, 7).expect_err("oob expected");
    let msg = err.to_string();
    assert!(
        msg.contains("input index 7 out of bounds")
            && msg.contains(&format!("{cs:?}"))
            && msg.contains("len=0"),
        "wrong error: {err:?}"
    );
}

#[test]
fn remove_node_input_on_cacheable_uses_dedicated_error() {
    let mut graph = Graph::new();
    let c = graph.create_node(
        NodeKind::IntConst(0),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let err = graph
        .remove_node_input(c, 0)
        .expect_err("cacheable expected");
    let msg = err.to_string();
    assert!(
        msg.contains("attempted to remove input from cacheable node")
            && msg.contains(&format!("{c:?}")),
        "wrong error: {err:?}"
    );
}

#[test]
fn node_input_id_at_returns_error_on_out_of_bounds() {
    let mut graph = Graph::new();
    let n = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let err = graph
        .node_input_id_at(n, 0)
        .expect_err("Entry has no inputs");
    let msg = err.to_string();
    assert!(
        msg.contains("input index 0 out of bounds")
            && msg.contains(&format!("{n:?}"))
            && msg.contains("len=0"),
        "wrong error: {err:?}"
    );
}

