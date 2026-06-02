//! White-box tests for the graph submodules — arena, dedup cache,
//! use-list bookkeeping, and typed accessors.

use super::*;
use crate::function::Function;
use crate::node::{NodeKind, ValueType};

// ── helpers ───────────────────────────────────────────────────────────────

#[track_caller]
fn check_node_inputs(
    graph: &Graph,
    node_id: NodeId,
    expected: impl IntoIterator<Item = ValueId>,
) {
    let expected: Vec<_> = expected.into_iter().collect();
    let actual: Vec<_> = graph.node_inputs(node_id).into_iter().collect();
    assert_eq!(actual, expected);
}

#[track_caller]
fn check_node_output_kinds(
    graph: &Graph,
    node_id: NodeId,
    expected: impl IntoIterator<Item = ValueKind>,
) {
    let expected: Vec<_> = expected.into_iter().collect();
    let actual: Vec<_> = graph
        .node_outputs(node_id)
        .iter()
        .map(|&output_id| graph.value_kind(output_id))
        .collect();
    assert_eq!(actual, expected);
}

#[track_caller]
fn check_node_output_definitions(
    graph: &Graph,
    node_id: NodeId,
    expected: impl IntoIterator<Item = (NodeId, u32)>,
) {
    let expected: Vec<_> = expected.into_iter().collect();
    let actual: Vec<_> = graph
        .node_outputs(node_id)
        .iter()
        .map(|&output_id| graph.output_definition(output_id))
        .collect();
    assert_eq!(actual, expected);
}

/// Creates a simple constant node (no inputs) and checks that its
/// metadata is stored correctly.
#[test]
fn create_single_node() {
    let mut function = Function::new();
    let node_id = function.create_node(
        NodeKind::IntConst(5),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    assert_eq!(function.node_kind(node_id), &NodeKind::IntConst(5));
    assert_eq!(function.nodes.len(), 1);
    check_node_inputs(&function, node_id, []);
    check_node_output_kinds(
        &function,
        node_id,
        vec![ValueKind::Typed(ValueType::I64)],
    );
    check_node_output_definitions(&function, node_id, vec![(node_id, 0)]);
}

/// `kind_of_value` agrees with the two-step `node_kind(producer(out))`
/// lookup it replaces — pinned because ~100 callsites depend on the equivalence.
#[test]
fn kind_of_output_matches_two_step_lookup() {
    let mut function = Function::new();
    let node_id = function.create_node(
        NodeKind::IntConst(7),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    let [out] = function.node_outputs_exact::<1>(node_id).unwrap();
    let two_step = function.node_kind(function.producer(out));
    let one_step = function.kind_of_value(out);
    assert_eq!(one_step, two_step);
    assert_eq!(one_step, &NodeKind::IntConst(7));
}

/// Cacheable nodes with identical kind and inputs must be deduplicated:
/// the second call must return the same [`NodeId`] as the first and must
/// not grow the node table.
#[test]
fn cacheable_node_is_deduplicated() {
    let mut function = Function::new();
    let id_a = function.create_node(
        NodeKind::IntConst(42),
        [],
        [ValueKind::Typed(ValueType::I32)],
    );
    let id_b = function.create_node(
        NodeKind::IntConst(42),
        [],
        [ValueKind::Typed(ValueType::I32)],
    );
    assert_eq!(
        id_a, id_b,
        "identical cacheable nodes must alias to the same id"
    );
    assert_eq!(
        function.nodes.len(),
        1,
        "deduplication must not create a second node"
    );
}

/// Repeated `create_node` calls with the same cacheable-kind key must
/// return the same `NodeId` and grow the arena exactly once.  This pins
/// the behavioural contract of the borrowed-key dedup-cache lookup
/// (`raw_entry_mut().from_hash(…)`): a cache *hit* must allocate
/// neither the owned key nor a duplicate node.  Bulk-shape variant of
/// `cacheable_node_is_deduplicated` to guard against accidental hash
/// mismatches between the borrowed `(&Node, &[…], &[…])` probe shape
/// and the owned `(Node, Vec<…>, Vec<…>)` insert shape.
#[test]
fn cacheable_node_dedup_is_stable_across_many_calls() {
    let mut function = Function::new();
    let first = function.create_node(
        NodeKind::IntConst(0xdead_beefu128),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    let arena_after_first = function.nodes.len();
    for _ in 0..1000 {
        let id = function.create_node(
            NodeKind::IntConst(0xdead_beefu128),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        assert_eq!(id, first, "cache hit must return the original id");
    }
    assert_eq!(
        function.nodes.len(),
        arena_after_first,
        "no new nodes should be allocated on repeated cache hits",
    );
}

/// Two cacheable nodes with identical kind + inputs but different
/// `output_kinds` (e.g. `IntConst(0): I32` vs `IntConst(0): I64`)
/// must NOT dedup.  Pins that the dedup key includes `output_kinds`;
/// a regression that hashed only `(kind, inputs)` would alias values
/// of different widths and produce type-incorrect outputs at
/// consumers.
#[test]
fn cacheable_int_const_with_different_type_does_not_dedup() {
    let mut function = Function::new();
    let a = function.create_node(
        NodeKind::IntConst(0),
        [],
        [ValueKind::Typed(ValueType::I32)],
    );
    let b = function.create_node(
        NodeKind::IntConst(0),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    assert_ne!(
        a, b,
        "IntConst(0):I32 must NOT alias IntConst(0):I64 — output_kinds is \
         part of the dedup key"
    );
}

/// Two `IntConst` nodes that are semantically equal under their declared
/// integer output type — one carrying a payload already masked to the
/// width, the other carrying extra high bits above the width — must dedup
/// to the SAME `NodeId`.  `create_node` normalises every `IntConst`
/// payload to its integer output type's bit width before computing the
/// dedup-cache key, so an un-masked constant (e.g. produced by a
/// big-endian sub-register read or an un-masking rewrite closure) and the
/// masked constant for the same value share one node.
#[test]
fn int_const_payload_is_normalised_to_output_type_width() {
    let mut function = Function::new();
    // -4 as I64: extra leading ones above bit 63 vs the 64-bit-masked form.
    let wide = function.create_node(
        NodeKind::IntConst(0xFFFF_FFFF_FFFF_FFFF_FFFF_FFFF_FFFF_FFFC),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    let masked = function.create_node(
        NodeKind::IntConst(0xFFFF_FFFF_FFFF_FFFC),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    assert_eq!(
        wide, masked,
        "semantically-equal IntConst values must normalise to the output \
         type width and dedup to the same node"
    );
    assert_eq!(
        function.node_kind(wide),
        &NodeKind::IntConst(0xFFFF_FFFF_FFFF_FFFC),
        "stored payload must be masked to the I64 width"
    );
    assert_eq!(
        function.nodes.len(),
        1,
        "normalised IntConst constants must not allocate a second node"
    );
}

/// Non-cacheable nodes (e.g. `Return`) must always produce fresh ids even
/// when all arguments are identical.
#[test]
fn non_cacheable_node_is_never_deduplicated() {
    let mut function = Function::new();
    let id_a = function.create_node(NodeKind::Return, [], []);
    let id_b = function.create_node(NodeKind::Return, [], []);
    assert_ne!(
        id_a, id_b,
        "non-cacheable nodes must always produce distinct ids"
    );
}

/// `Entry` is now cacheable — repeated `create_node` calls with the same
/// signature must return the same `NodeId` (only one Entry per function).
#[test]
fn entry_node_kind_dedupes_on_repeated_create() {
    let mut function = Function::new();
    let e1 = function.create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let e2 = function.create_node(NodeKind::Entry, [], [ValueKind::Control]);
    assert_eq!(e1, e2, "Entry must dedupe — only one per function");
}

/// `InitialMemory` is now cacheable — repeated `create_node` calls must
/// return the same `NodeId`.
#[test]
fn initial_memory_dedupes_on_repeated_create() {
    let mut function = Function::new();
    let m1 = function.create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    let m2 = function.create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    assert_eq!(m1, m2, "InitialMemory must dedupe — only one per function");
}

/// `InitialVar` is now cacheable — the `Vn` is part of the node kind, so
/// two calls with the **same** `Vn` dedup and two calls with **different**
/// `Vn`s produce distinct nodes.
#[test]
fn initial_var_dedupes_per_vn() {
    let mut function = Function::new();
    let vn_a = rsleigh::Vn {
        addr_off: 0,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 8,
    };
    let vn_b = rsleigh::Vn {
        addr_off: 8,
        addr_space: rsleigh::VnSpace::REGISTER,
        size: 8,
    };
    let value_kind = ValueKind::Typed(ValueType::I64);
    let v1 = function.create_node(NodeKind::InitialVar(vn_a), [], [value_kind]);
    let v2 = function.create_node(NodeKind::InitialVar(vn_a), [], [value_kind]);
    assert_eq!(v1, v2, "InitialVar with the same Vn must dedupe");

    let v3 = function.create_node(NodeKind::InitialVar(vn_b), [], [value_kind]);
    assert_ne!(v1, v3, "InitialVar with a different Vn must NOT dedupe");
}

/// Two adjacent `Call` nodes with identical target and argument outputs
/// must stay distinct — Call is non-cacheable because `CallStackArgCollect`
/// mutates its inputs after construction.
#[test]
fn adjacent_calls_with_same_args_are_distinct() {
    let mut function = Function::new();
    let ctrl_a = function.create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let mem_a = function.create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    let [ctrl_out] = function.node_outputs_exact::<1>(ctrl_a).unwrap();
    let [mem_value] = function.node_outputs_exact::<1>(mem_a).unwrap();
    let target = function.create_node(
        NodeKind::IntConst(0x1000),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    let [target_out] = function.node_outputs_exact::<1>(target).unwrap();
    let outs = [ValueKind::Control, ValueKind::Memory];
    let call_a = function.create_node(NodeKind::Call, [ctrl_out, mem_value, target_out], outs);
    let call_b = function.create_node(NodeKind::Call, [ctrl_out, mem_value, target_out], outs);
    assert_ne!(
        call_a, call_b,
        "Call is non-cacheable so identical-argument calls must be distinct"
    );
}

/// `Graph::call_other_name` round-trip: setting and reading back a name
/// works, and unset nodes return `None`.
#[test]
fn call_other_name_round_trip() {
    let mut function = Function::new();
    // Two CallOther nodes with the same user_op_id.  CallOther is
    // non-cacheable (see `is_cacheable`), so they get distinct ids.
    let outs = [ValueKind::Control, ValueKind::Memory];
    // We need a control + memory input to construct a CallOther; build a
    // throwaway Entry and InitialMemory.
    let entry = function.create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let [entry_ctrl] = function.node_outputs_exact::<1>(entry).unwrap();
    let init_mem = function.create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    let [init_mem_out] = function.node_outputs_exact::<1>(init_mem).unwrap();
    let id_a = function.create_node(
        NodeKind::CallOther { user_op_id: 62 },
        [entry_ctrl, init_mem_out],
        outs,
    );
    let id_b = function.create_node(
        NodeKind::CallOther { user_op_id: 62 },
        [entry_ctrl, init_mem_out],
        outs,
    );
    assert_ne!(id_a, id_b, "CallOther is non-cacheable");
    assert_eq!(function.call_other_name(id_a), None);
    function.set_call_other_name(id_a, "setISAMode".to_string());
    assert_eq!(function.call_other_name(id_a), Some("setISAMode"));
    assert_eq!(function.call_other_name(id_b), None);
    // Replacement
    function.set_call_other_name(id_a, "OtherName".to_string());
    assert_eq!(function.call_other_name(id_a), Some("OtherName"));
}

/// After adding an input to a non-cacheable node the output's use-list
/// must contain exactly that input, and `node_inputs` must reflect it.
#[test]
fn add_node_input_registers_use() {
    let mut function = Function::new();
    // Produce a value
    let const_node = function.create_node(
        NodeKind::IntConst(1),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    let [const_out] = function.node_outputs_exact::<1>(const_node).unwrap();

    // Create a non-cacheable sink
    let ret_node = function.create_node(NodeKind::Return, [], []);

    function.add_node_input(ret_node, const_out).unwrap();

    // The input must appear in node_inputs
    check_node_inputs(&function, ret_node, [const_out]);

    // The output's use-list must contain this input
    let use_count = function.value_uses(const_out).count();
    assert_eq!(use_count, 1);
}

/// `remove_node_input` must shrink the input list, update subsequent
/// input indices, and unregister the use from the output's use-list.
#[test]
fn remove_node_input_cleans_up_use_list() {
    let mut function = Function::new();

    let c0 = function.create_node(
        NodeKind::IntConst(0),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    let [out0] = function.node_outputs_exact::<1>(c0).unwrap();

    let c1 = function.create_node(
        NodeKind::IntConst(1),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    let [out1] = function.node_outputs_exact::<1>(c1).unwrap();

    let ret = function.create_node(NodeKind::Return, [], []);
    function.add_node_input(ret, out0).unwrap();
    function.add_node_input(ret, out1).unwrap();

    // Remove the first input (index 0 = out0)
    function.remove_node_input(ret, 0).unwrap();

    // Only out1 should remain
    check_node_inputs(&function, ret, [out1]);

    // out0 must no longer be used
    assert_eq!(
        function.value_uses(out0).count(),
        0,
        "out0 should have no uses after removal"
    );
    // out1 must still be used
    assert_eq!(
        function.value_uses(out1).count(),
        1,
        "out1 should still have one use"
    );

    // The surviving input must have its index adjusted to 0
    let inputs_slice = function.nodes[ret].inputs.as_slice(&function.input_pool);
    assert_eq!(function.inputs[inputs_slice[0]].input_index, 0);
}

/// `update_input` must move the use from the old output to the new one
/// so that use-lists stay consistent.
#[test]
fn update_input_moves_use_to_new_output() {
    let mut function = Function::new();

    let old = function.create_node(
        NodeKind::IntConst(10),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    let [old_out] = function.node_outputs_exact::<1>(old).unwrap();

    let new = function.create_node(
        NodeKind::IntConst(20),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    let [new_out] = function.node_outputs_exact::<1>(new).unwrap();

    let ret = function.create_node(NodeKind::Return, [], []);
    function.add_node_input(ret, old_out).unwrap();

    // Find the single input id
    let input_id = function.nodes[ret].inputs.as_slice(&function.input_pool)[0];

    function.update_input(input_id, new_out);

    // old_out must have no uses; new_out must have one
    assert_eq!(function.value_uses(old_out).count(), 0);
    assert_eq!(function.value_uses(new_out).count(), 1);

    // The node input must now reference new_out
    check_node_inputs(&function, ret, [new_out]);
}

/// `detach_node_inputs` must clear all inputs from the node and remove
/// them from every output's use-list.
#[test]
fn detach_node_inputs_removes_all_uses() {
    let mut function = Function::new();

    let c = function.create_node(
        NodeKind::IntConst(5),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    let [out] = function.node_outputs_exact::<1>(c).unwrap();

    let ret = function.create_node(NodeKind::Return, [], []);
    function.add_node_input(ret, out).unwrap();
    function.add_node_input(ret, out).unwrap(); // same output used twice

    assert_eq!(function.value_uses(out).count(), 2);

    function.detach_node_inputs(ret);

    assert_eq!(
        function.value_uses(out).count(),
        0,
        "all uses must be removed after detach"
    );
    assert_eq!(
        function.node_inputs(ret).len(),
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
/// passes that created identical Adds after `PhiCollapse` had detached
/// the original unreachable Add would alias to the zombie, and any
/// follow-up pass calling `node_inputs_exact::<2>` would fail with
/// `WrongInputCount(..., 2, 0)`.
#[test]
fn detach_evicts_cacheable_node_from_dedup_cache() {
    use crate::ops::IntBinaryOp;
    let mut function = Function::new();
    let lhs = function.create_node(
        NodeKind::IntConst(7),
        [],
        [ValueKind::Typed(ValueType::I32)],
    );
    let rhs = function.create_node(
        NodeKind::IntConst(9),
        [],
        [ValueKind::Typed(ValueType::I32)],
    );
    let [lhs_out] = function.node_outputs_exact::<1>(lhs).unwrap();
    let [rhs_out] = function.node_outputs_exact::<1>(rhs).unwrap();

    let ty = ValueKind::Typed(ValueType::I32);
    let add_a = function.create_node(
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        [lhs_out, rhs_out],
        [ty],
    );

    function.detach_node_inputs(add_a);
    assert_eq!(function.node_inputs(add_a).len(), 0);

    let add_b = function.create_node(
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
        function.node_inputs(add_b).len(),
        2,
        "the re-created node must be fully connected"
    );
}

/// An output consumed by a single node must be reported by
/// `output_has_one_usage` as `true`; consuming it a second time must
/// flip it to `false`.
#[test]
fn output_has_one_usage_tracks_consumer_count() {
    let mut function = Function::new();

    let c = function.create_node(
        NodeKind::IntConst(99),
        [],
        [ValueKind::Typed(ValueType::I32)],
    );
    let [out] = function.node_outputs_exact::<1>(c).unwrap();

    assert!(!function.output_has_one_usage(out), "zero uses is not one");

    let ret1 = function.create_node(NodeKind::Return, [], []);
    function.add_node_input(ret1, out).unwrap();
    assert!(
        function.output_has_one_usage(out),
        "one use should return true"
    );

    let ret2 = function.create_node(NodeKind::Return, [], []);
    function.add_node_input(ret2, out).unwrap();
    assert!(
        !function.output_has_one_usage(out),
        "two uses should return false"
    );
}

/// `producer` must return the node that created the output.
#[test]
fn node_for_output_returns_source_node() {
    let mut function = Function::new();
    let node = function.create_node(
        NodeKind::IntConst(7),
        [],
        [ValueKind::Typed(ValueType::I8)],
    );
    let [out] = function.node_outputs_exact::<1>(node).unwrap();
    assert_eq!(function.producer(out), node);
}

/// A node with two outputs must expose both with correct kinds and
/// definitions.
#[test]
fn node_with_multiple_outputs() {
    let mut function = Function::new();
    let node = function.create_node(
        NodeKind::If,
        [],
        [ValueKind::Control, ValueKind::Control],
    );
    let [true_ctrl, false_ctrl] = function.node_outputs_exact::<2>(node).unwrap();
    assert_eq!(function.value_kind(true_ctrl), ValueKind::Control);
    assert_eq!(function.value_kind(false_ctrl), ValueKind::Control);
    assert_eq!(function.output_definition(true_ctrl), (node, 0));
    assert_eq!(function.output_definition(false_ctrl), (node, 1));
}

/// `value_uses` must yield one `(node_id, input_index)` tuple per
/// consumer, with the correct node id and position within that node's
/// input list.  Three independent consumers all at input-index 0 must
/// all appear exactly once.
#[test]
fn output_uses_reports_all_consumers_with_correct_indices() {
    let mut function = Function::new();
    let src = function.create_node(
        NodeKind::IntConst(7),
        [],
        [ValueKind::Typed(ValueType::I32)],
    );
    let [out] = function.node_outputs_exact::<1>(src).unwrap();

    let ret0 = function.create_node(NodeKind::Return, [], []);
    function.add_node_input(ret0, out).unwrap();
    let ret1 = function.create_node(NodeKind::Return, [], []);
    function.add_node_input(ret1, out).unwrap();
    let ret2 = function.create_node(NodeKind::Return, [], []);
    function.add_node_input(ret2, out).unwrap();

    let uses: Vec<(NodeId, u32)> = function.value_uses(out).collect();
    assert_eq!(uses.len(), 3, "all three consumers must appear");

    for expected_node in [ret0, ret1, ret2] {
        assert!(
            uses.iter().any(|(n, _)| *n == expected_node),
            "consumer {expected_node:?} missing from value_uses"
        );
    }
    // Each of the three nodes has exactly one input, so input_index is 0.
    for (_, idx) in &uses {
        assert_eq!(*idx, 0, "each single-input node's input_index must be 0");
    }
}

/// When a node has multiple inputs from the same output, `value_uses`
/// must report all of them with their correct positional indices.
#[test]
fn output_uses_same_output_multiple_times_reports_each_position() {
    let mut function = Function::new();
    let src = function.create_node(
        NodeKind::IntConst(3),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    let [out] = function.node_outputs_exact::<1>(src).unwrap();

    // Same output at positions 0 and 1 of the same sink node.
    let sink = function.create_node(NodeKind::Return, [], []);
    function.add_node_input(sink, out).unwrap(); // input_index 0
    function.add_node_input(sink, out).unwrap(); // input_index 1

    let uses: Vec<(NodeId, u32)> = function.value_uses(out).collect();
    assert_eq!(uses.len(), 2);

    let mut indices: Vec<u32> = uses.iter().map(|(_, i)| *i).collect();
    indices.sort_unstable();
    assert_eq!(indices, vec![0, 1], "both positional indices must appear");
}

/// `output_use_cursor` iterates the same set as `value_uses`.
/// `replace_current_with` must redirect the first use to a new output
/// and advance past it so the remaining use is untouched.
#[test]
fn output_use_cursor_replace_redirects_first_use() {
    let mut function = Function::new();

    let old_src = function.create_node(
        NodeKind::IntConst(1),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    let [old_out] = function.node_outputs_exact::<1>(old_src).unwrap();

    let new_src = function.create_node(
        NodeKind::IntConst(2),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    let [new_out] = function.node_outputs_exact::<1>(new_src).unwrap();

    // Two consumers of old_out.
    let ret0 = function.create_node(NodeKind::Return, [], []);
    function.add_node_input(ret0, old_out).unwrap();
    let ret1 = function.create_node(NodeKind::Return, [], []);
    function.add_node_input(ret1, old_out).unwrap();

    assert_eq!(function.value_uses(old_out).count(), 2);
    assert_eq!(function.value_uses(new_out).count(), 0);

    // Redirect the first consumer to new_out.
    {
        let mut cursor = function.output_use_cursor(old_out);
        cursor.replace_current_with(new_out).unwrap();
    }

    // After one replacement: old_out has one use, new_out has one use.
    assert_eq!(
        function.value_uses(old_out).count(),
        1,
        "one use must remain on old_out"
    );
    assert_eq!(
        function.value_uses(new_out).count(),
        1,
        "one use must move to new_out"
    );
}

/// `output_use_cursor` with `replace_current_with` applied to every
/// element must leave the original output with no uses and transfer all
/// uses to the replacement.
#[test]
fn output_use_cursor_replace_all_drains_source() {
    let mut function = Function::new();

    let old_src = function.create_node(
        NodeKind::IntConst(10),
        [],
        [ValueKind::Typed(ValueType::I32)],
    );
    let [old_out] = function.node_outputs_exact::<1>(old_src).unwrap();

    let new_src = function.create_node(
        NodeKind::IntConst(20),
        [],
        [ValueKind::Typed(ValueType::I32)],
    );
    let [new_out] = function.node_outputs_exact::<1>(new_src).unwrap();

    // Three consumers.
    for _ in 0..3 {
        let r = function.create_node(NodeKind::Return, [], []);
        function.add_node_input(r, old_out).unwrap();
    }
    assert_eq!(function.value_uses(old_out).count(), 3);

    // Replace all uses in a single cursor pass.
    let mut cursor = function.output_use_cursor(old_out);
    while cursor.current().is_some() {
        cursor.replace_current_with(new_out).unwrap();
    }

    assert_eq!(
        function.value_uses(old_out).count(),
        0,
        "all uses must be drained from old_out"
    );
    assert_eq!(
        function.value_uses(new_out).count(),
        3,
        "all uses must land on new_out"
    );
}

/// Removing the middle input of a three-input node must: leave the
/// two survivors in order, re-number their indices contiguously from 0,
/// and remove the deleted input from its output's use-list.
#[test]
fn remove_node_input_from_middle_reindexes_remaining() {
    let mut function = Function::new();

    let out0 = {
        let n = function.create_node(
            NodeKind::IntConst(10),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        function.node_outputs_exact::<1>(n).unwrap()[0]
    };
    let out1 = {
        let n = function.create_node(
            NodeKind::IntConst(20),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        function.node_outputs_exact::<1>(n).unwrap()[0]
    };
    let out2 = {
        let n = function.create_node(
            NodeKind::IntConst(30),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        function.node_outputs_exact::<1>(n).unwrap()[0]
    };

    let sink = function.create_node(NodeKind::Return, [], []);
    function.add_node_input(sink, out0).unwrap(); // index 0
    function.add_node_input(sink, out1).unwrap(); // index 1
    function.add_node_input(sink, out2).unwrap(); // index 2

    function.remove_node_input(sink, 1).unwrap(); // remove middle

    check_node_inputs(&function, sink, [out0, out2]);
    assert_eq!(function.value_uses(out1).count(), 0, "out1 must be removed");
    assert_eq!(function.value_uses(out0).count(), 1);
    assert_eq!(function.value_uses(out2).count(), 1);

    let inputs_slice = function.nodes[sink].inputs.as_slice(&function.input_pool);
    assert_eq!(
        function.inputs[inputs_slice[0]].input_index, 0,
        "surviving input 0 must have index 0"
    );
    assert_eq!(
        function.inputs[inputs_slice[1]].input_index, 1,
        "surviving input 1 must have index 1"
    );
}

/// Removing the last input must not disturb the preceding inputs.
#[test]
fn remove_node_input_from_end_leaves_others_intact() {
    let mut function = Function::new();

    let out0 = {
        let n = function.create_node(
            NodeKind::IntConst(1),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        function.node_outputs_exact::<1>(n).unwrap()[0]
    };
    let out1 = {
        let n = function.create_node(
            NodeKind::IntConst(2),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        function.node_outputs_exact::<1>(n).unwrap()[0]
    };

    let sink = function.create_node(NodeKind::Return, [], []);
    function.add_node_input(sink, out0).unwrap();
    function.add_node_input(sink, out1).unwrap();

    function.remove_node_input(sink, 1).unwrap(); // remove last

    check_node_inputs(&function, sink, [out0]);
    assert_eq!(function.value_uses(out1).count(), 0);
    assert_eq!(function.value_uses(out0).count(), 1);

    let inputs_slice = function.nodes[sink].inputs.as_slice(&function.input_pool);
    assert_eq!(function.inputs[inputs_slice[0]].input_index, 0);
}

/// `update_input` on an input belonging to a cacheable node must evict the
/// stale dedup-cache entry. Otherwise a later `create_node` with the
/// original `(kind, inputs, outputs)` triple returns the now-modified
/// node, which has different inputs — silent miscompilation by the
/// optimizer (which calls `update_input` via `replace_all_uses`).
#[test]
fn update_input_on_cacheable_evicts_stale_cache_entry() {
    use crate::ops::IntBinaryOp;
    let mut function = Function::new();

    let a = function.create_node(
        NodeKind::IntConst(1),
        [],
        [ValueKind::Typed(ValueType::I32)],
    );
    let b = function.create_node(
        NodeKind::IntConst(2),
        [],
        [ValueKind::Typed(ValueType::I32)],
    );
    let c = function.create_node(
        NodeKind::IntConst(3),
        [],
        [ValueKind::Typed(ValueType::I32)],
    );
    let [a_out] = function.node_outputs_exact::<1>(a).unwrap();
    let [b_out] = function.node_outputs_exact::<1>(b).unwrap();
    let [c_out] = function.node_outputs_exact::<1>(c).unwrap();
    let ty = ValueKind::Typed(ValueType::I32);

    // Cache key inserted: (Add, [a, b], [ty]) → add_ab.
    let add_ab = function.create_node(
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        [a_out, b_out],
        [ty],
    );

    // Redirect input[0] from a → c. Node now actually has inputs [c, b],
    // but the cache (if not maintained) still maps [a, b] → add_ab.
    let in0 = function.node_input_id_at(add_ab, 0).unwrap();
    function.update_input(in0, c_out);

    // Re-create with the ORIGINAL key. Must NOT return add_ab — its
    // current inputs are [c, b], not [a, b].
    let fresh = function.create_node(
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
    let mut function = Function::new();

    let src = function.create_node(
        NodeKind::IntConst(99),
        [],
        [ValueKind::Typed(ValueType::I32)],
    );
    let [out] = function.node_outputs_exact::<1>(src).unwrap();

    let sink = function.create_node(NodeKind::Return, [], []);
    function.add_node_input(sink, out).unwrap();

    let input_id = function.nodes[sink].inputs.as_slice(&function.input_pool)[0];
    function.update_input(input_id, out);

    assert_eq!(
        function.value_uses(out).count(),
        1,
        "self-update must not change use count"
    );
    check_node_inputs(&function, sink, [out]);
}

/// After `detach_node_inputs`, re-adding the same inputs must restore
/// the use-list count to its original value.
#[test]
fn detach_then_readd_restores_use_count() {
    let mut function = Function::new();

    let src = function.create_node(
        NodeKind::IntConst(42),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    let [out] = function.node_outputs_exact::<1>(src).unwrap();

    let sink = function.create_node(NodeKind::Return, [], []);
    function.add_node_input(sink, out).unwrap();
    function.add_node_input(sink, out).unwrap();
    assert_eq!(function.value_uses(out).count(), 2);

    function.detach_node_inputs(sink);
    assert_eq!(
        function.value_uses(out).count(),
        0,
        "uses cleared after detach"
    );
    assert_eq!(function.node_inputs(sink).len(), 0);

    // Re-add; use count must be restored.
    function.add_node_input(sink, out).unwrap();
    function.add_node_input(sink, out).unwrap();
    assert_eq!(
        function.value_uses(out).count(),
        2,
        "re-adding inputs must restore use count"
    );
    assert_eq!(function.node_inputs(sink).len(), 2);
}

/// Two independent sinks each consuming the same output must all appear
/// in the use-list.  This verifies the linked-list stays consistent when
/// multiple distinct nodes reference the same output.
#[test]
fn two_independent_consumers_both_in_use_list() {
    let mut function = Function::new();

    let src = function.create_node(
        NodeKind::IntConst(1),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    let [out] = function.node_outputs_exact::<1>(src).unwrap();

    let b = function.create_node(NodeKind::Return, [], []);
    function.add_node_input(b, out).unwrap();
    let c = function.create_node(NodeKind::Return, [], []);
    function.add_node_input(c, out).unwrap();

    let uses: Vec<_> = function.value_uses(out).collect();
    assert_eq!(uses.len(), 2);
    let nodes: Vec<_> = uses.iter().map(|(n, _)| *n).collect();
    assert!(nodes.contains(&b), "b must appear in use-list");
    assert!(nodes.contains(&c), "c must appear in use-list");
}

/// `node_outputs_exact` must return `Err(WrongOutputCount)` when asked
/// for a count that does not match the actual number of outputs.
#[test]
fn node_outputs_exact_errors_on_wrong_count() {
    let mut function = Function::new();
    let node = function.create_node(
        NodeKind::IntConst(0),
        [],
        [ValueKind::Typed(ValueType::I8)],
    );
    let err = function.node_outputs_exact::<2>(node).unwrap_err();
    assert!(
        err.to_string().contains("does not have exactly 2 outputs"),
        "got: {err}"
    );
}

/// `node_inputs_exact` must return `Err(WrongInputCount)` when asked for
/// a count that does not match the actual number of inputs.
#[test]
fn node_inputs_exact_errors_on_wrong_count() {
    let mut function = Function::new();
    let src = function.create_node(
        NodeKind::IntConst(0),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    let [out] = function.node_outputs_exact::<1>(src).unwrap();

    let sink = function.create_node(NodeKind::Return, [], []);
    function.add_node_input(sink, out).unwrap(); // exactly 1 input

    let err = function.node_inputs_exact::<2>(sink).unwrap_err();
    assert!(
        err.to_string().contains("does not have exactly 2 inputs"),
        "got: {err}"
    );
}

#[test]
fn update_input_self_redirect_preserves_use_list_order() {
    use crate::ops::IntUnaryOp;
    let mut function = Function::new();
    let c = function.create_node(
        NodeKind::IntConst(0),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    let cval = function.node_outputs(c).iter().copied().next().unwrap();
    // Two consumers of cval to give the use-list real ordering.  Use
    // `Truncate` and `Neg` since `IntUnaryOp` has only the one variant
    // since `BitNot` was removed in favour of `Xor(_, all_ones)`.
    let _a = function.create_node(
        NodeKind::Truncate,
        [cval],
        [ValueKind::Typed(ValueType::I32)],
    );
    let b = function.create_node(
        NodeKind::IntUnaryOp(IntUnaryOp::Neg),
        [cval],
        [ValueKind::Typed(ValueType::I64)],
    );

    let head_before = function.output_first_use_id(cval);

    let b_in0 = function.node_input_id_at(b, 0).unwrap();
    function.update_input(b_in0, cval); // self-redirect — should be a no-op

    assert_eq!(
        head_before,
        function.output_first_use_id(cval),
        "self-redirect must not re-order the use-list"
    );
}

#[test]
fn remove_node_input_returns_error_on_out_of_bounds() {
    let mut function = Function::new();
    let cs = function.create_node(
        NodeKind::Region,
        [],
        [ValueKind::Control, ValueKind::PhiToken],
    );
    let err = function.remove_node_input(cs, 7).expect_err("oob expected");
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
    let mut function = Function::new();
    let c = function.create_node(
        NodeKind::IntConst(0),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    let err = function
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
    let mut function = Function::new();
    let n = function.create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let err = function
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

// ── asm-fingerprint side-table tests ──────────────────────────────────────

#[test]
fn asm_fingerprint_unset_returns_empty_slice() {
    let mut function = Function::new();
    let n = function.create_node(NodeKind::Entry, [], [ValueKind::Control]);
    assert_eq!(function.asm_fingerprint(n), &[] as &[u64]);
}

#[test]
fn asm_fingerprint_set_then_get() {
    let mut function = Function::new();
    let n = function.create_node(NodeKind::Entry, [], [ValueKind::Control]);
    function.set_asm_fingerprint(n, vec![0x1000, 0x1004, 0x1008]);
    assert_eq!(function.asm_fingerprint(n), &[0x1000, 0x1004, 0x1008]);
}

#[test]
fn asm_fingerprint_extend_sorts_and_dedupes() {
    let mut function = Function::new();
    let n = function.create_node(NodeKind::Entry, [], [ValueKind::Control]);
    function.extend_asm_fingerprint(n, &[0x1004, 0x1000, 0x1004]);
    assert_eq!(function.asm_fingerprint(n), &[0x1000, 0x1004]);
    // Extending with one new + two duplicates yields a sorted, deduplicated set.
    function.extend_asm_fingerprint(n, &[0x1008, 0x1000, 0x1004]);
    assert_eq!(function.asm_fingerprint(n), &[0x1000, 0x1004, 0x1008]);
}

#[test]
fn asm_fingerprint_extend_from_unions_two_nodes() {
    let mut function = Function::new();
    let a = function.create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let b = function.create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    function.set_asm_fingerprint(a, vec![0x1000, 0x1004]);
    function.set_asm_fingerprint(b, vec![0x1004, 0x100C]);
    function.extend_asm_fingerprint_from(a, b);
    assert_eq!(function.asm_fingerprint(a), &[0x1000, 0x1004, 0x100C]);
    // Source unaffected.
    assert_eq!(function.asm_fingerprint(b), &[0x1004, 0x100C]);
}

#[test]
fn asm_fingerprint_extend_never_shrinks() {
    let mut function = Function::new();
    let n = function.create_node(NodeKind::Entry, [], [ValueKind::Control]);
    function.set_asm_fingerprint(n, vec![0x1000, 0x1004, 0x1008]);
    // Extending with a strict subset must NOT remove any existing entries.
    function.extend_asm_fingerprint(n, &[0x1004]);
    assert_eq!(function.asm_fingerprint(n), &[0x1000, 0x1004, 0x1008]);
    // Extending with the empty slice is a no-op.
    function.extend_asm_fingerprint(n, &[]);
    assert_eq!(function.asm_fingerprint(n), &[0x1000, 0x1004, 0x1008]);
}

#[test]
fn asm_fingerprint_extend_from_self_is_noop() {
    let mut function = Function::new();
    let n = function.create_node(NodeKind::Entry, [], [ValueKind::Control]);
    function.set_asm_fingerprint(n, vec![0x1000, 0x1004]);
    function.extend_asm_fingerprint_from(n, n);
    assert_eq!(function.asm_fingerprint(n), &[0x1000, 0x1004]);
}

#[test]
fn call_clobbered_override_default_is_none() {
    let mut function = Function::new();
    let nid = function.create_node(
        NodeKind::IntConst(0),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    assert!(function.call_clobbered_override(nid).is_none());
}

#[test]
fn call_clobbered_override_set_then_get_round_trips() {
    let mut function = Function::new();
    let nid = function.create_node(
        NodeKind::IntConst(0),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    let vns: Vec<rsleigh::Vn> = vec![];
    function.set_call_clobbered_override(nid, vns.clone());
    assert_eq!(function.call_clobbered_override(nid), Some(vns.as_slice()));
}

#[test]
fn asm_fingerprint_dedup_cache_hit_unions_via_extend() {
    // Two `create_node` calls for IntConst(7) hit the dedup cache — they
    // return the same NodeId.  Production code calls
    // `extend_asm_fingerprint(id, &[addr])` at every create_node site, so
    // both contributors end up unioned into the single side-table entry.
    let mut function = Function::new();
    let a = function.create_node(
        NodeKind::IntConst(7),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    function.extend_asm_fingerprint(a, &[0x2000]);
    let b = function.create_node(
        NodeKind::IntConst(7),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    assert_eq!(a, b, "cacheable nodes should dedup");
    function.extend_asm_fingerprint(b, &[0x3000]);
    assert_eq!(function.asm_fingerprint(a), &[0x2000, 0x3000]);
}

