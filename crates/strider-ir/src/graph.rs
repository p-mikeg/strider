//! The IR [`Graph`], a type alias over the generic [`strider_graph::Graph`]
//! with the IR payloads and dedup policy plugged in. All structural machinery
//! lives in `strider-graph`; this module is only the strider overlay.
//!
//! [`IrCacheable`] is purely mechanical and embeds no domain normalisation.
//! Integer-constant canonicalisation happens at construction in
//! `Function::create_node_attributed`, before a node reaches the cache.

use std::hash::{Hash, Hasher};

use rustc_hash::FxHasher;
use strider_graph::{NodeCacheable, RawStore, ValueId};

use crate::node::{NodeId, NodeKind, ValueKind};

/// A stateless ZST: the generic graph owns the dedup table and per-node
/// hashes. Cacheable kinds dedup on `(NodeKind, inputs, output_kinds)`;
/// non-cacheable ones always allocate fresh.
pub struct IrCacheable;

impl NodeCacheable<NodeKind, ValueKind> for IrCacheable {
    fn should_cache(kind: &NodeKind) -> bool {
        kind.is_cacheable()
    }

    /// `[T]: Hash` hashes length then elements, so a borrowed query slice and
    /// a node's re-read `SmallVec` of the same contents hash alike; that is
    /// what makes a probe land in the bucket the node was inserted under.
    ///
    /// Raw `FxHash` with no sentinel handling: the generic cache remaps the
    /// lone `u64::MAX` value itself.
    fn hash(kind: &NodeKind, inputs: &[ValueId], outputs: &[ValueKind]) -> u64 {
        let mut h = FxHasher::default();
        kind.hash(&mut h);
        inputs.hash(&mut h);
        outputs.hash(&mut h);
        h.finish()
    }

    /// The equality half of the hash-on-demand probe: no owned key payloads
    /// are kept, so identity is recomputed from the live store.
    fn eq(
        store: &RawStore<NodeKind, ValueKind>,
        cand: NodeId,
        kind: &NodeKind,
        inputs: &[ValueId],
        outputs: &[ValueKind],
    ) -> bool {
        store.kind_of(cand) == kind
            && store.input_values(cand).as_slice() == inputs
            && store.output_kinds(cand).as_slice() == outputs
    }
}

pub use strider_graph::NodeIdRemap;

/// Structural verbs are inherited from the generic graph; the
/// function-overlay reads and control-aware walks live on [`crate::IRViewer`]
/// and [`crate::IRWalker`].
pub type Graph = strider_graph::Graph<NodeKind, ValueKind, IrCacheable>;

pub type Inputs<'a> = strider_graph::Inputs<'a, NodeKind, ValueKind, IrCacheable>;

pub type InputCursor<'a> = strider_graph::InputCursor<'a, NodeKind, ValueKind, IrCacheable>;

#[cfg(test)]
mod tests {
    //! White-box tests for the arena, dedup cache, use-list bookkeeping, and
    //! typed accessors.

    use super::*;
    use crate::IRViewer;
    use crate::function::test_function;
    use crate::node::{NodeId, NodeKind, ValueId, ValueKind, ValueType};
    use cranelift_entity::EntityRef;
    use rustc_hash::FxHashSet;

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
            .map(|&value_id| graph.value_kind(value_id))
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
            .map(|&value_id| graph.value_definition(value_id))
            .collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn create_single_node() {
        let mut function = test_function();
        let node_id = function.graph_mut().create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(5_usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        assert_eq!(
            function.node_kind(node_id),
            &NodeKind::IntConst(crate::node::const_value::ConstId::new(5_usize))
        );
        assert_eq!(function.graph().all_node_ids().count(), 3);
        check_node_inputs(function.graph(), node_id, []);
        check_node_output_kinds(
            function.graph(),
            node_id,
            vec![ValueKind::Typed(ValueType::I64)],
        );
        check_node_output_definitions(function.graph(), node_id, vec![(node_id, 0)]);
    }

    /// `kind_of_value` must agree with the two-step
    /// `node_kind(producer(out))` lookup it replaces; many call sites assume
    /// the equivalence.
    #[test]
    fn kind_of_output_matches_two_step_lookup() {
        let mut function = test_function();
        let node_id = function.graph_mut().create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(7_usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [value] = function.node_outputs_exact::<1>(node_id).unwrap();
        let two_step = function.node_kind(function.producer(value));
        let one_step = function.kind_of_value(value);
        assert_eq!(one_step, two_step);
        assert_eq!(
            one_step,
            &NodeKind::IntConst(crate::node::const_value::ConstId::new(7_usize))
        );
    }

    #[test]
    fn cacheable_node_is_deduplicated() {
        let mut function = test_function();
        let id_a = function.graph_mut().create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(42_usize)),
            [],
            [ValueKind::Typed(ValueType::I32)],
        );
        let id_b = function.graph_mut().create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(42_usize)),
            [],
            [ValueKind::Typed(ValueType::I32)],
        );
        assert_eq!(
            id_a, id_b,
            "identical cacheable nodes must alias to the same id"
        );
        assert_eq!(
            function.graph().all_node_ids().count(),
            3,
            "deduplication must not create a second node"
        );
    }

    /// Bulk variant of `cacheable_node_is_deduplicated`, guarding against the
    /// query hash disagreeing with the per-node cached hash an entry was
    /// inserted under.
    #[test]
    fn cacheable_node_dedup_is_stable_across_many_calls() {
        let mut function = test_function();
        let first = function.graph_mut().create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(
                (0xdead_beef_u64) as usize,
            )),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let arena_after_first = function.graph().all_node_ids().count();
        for _ in 0..1000 {
            let id = function.graph_mut().create_node(
                NodeKind::IntConst(crate::node::const_value::ConstId::new(
                    (0xdead_beef_u64) as usize,
                )),
                [],
                [ValueKind::Typed(ValueType::I64)],
            );
            assert_eq!(id, first, "cache hit must return the original id");
        }
        assert_eq!(
            function.graph().all_node_ids().count(),
            arena_after_first,
            "no new nodes should be allocated on repeated cache hits",
        );
    }

    /// `output_kinds` is part of the dedup key: hashing only `(kind, inputs)`
    /// would alias values of different widths and hand consumers a
    /// type-incorrect output.
    #[test]
    fn cacheable_int_const_with_different_type_does_not_dedup() {
        let mut function = test_function();
        let a = function.graph_mut().create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(0_usize)),
            [],
            [ValueKind::Typed(ValueType::I32)],
        );
        let b = function.graph_mut().create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(0_usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        assert_ne!(
            a, b,
            "IntConst(0):I32 must NOT alias IntConst(0):I64 — output_kinds is \
         part of the dedup key"
        );
    }

    /// Masking happens at the interning choke-point, so a value with bits
    /// above the declared width and its masked form share one `ConstId` and
    /// the two `IntConst` nodes dedup.
    #[test]
    fn int_const_payload_is_normalised_to_output_type_width() {
        use crate::{IRBuilderExt, IRViewer};
        let mut function = test_function();
        // -4 at I8: only the low 8 bits of 0x1FC matter.
        let wide = function.build_int_const(0x1FCu128, ValueType::I8).unwrap();
        let masked = function.build_int_const(0xFCu128, ValueType::I8).unwrap();
        assert_eq!(
            function.producer(wide),
            function.producer(masked),
            "semantically-equal IntConst values must normalise to the output \
         type width and dedup to the same node"
        );
        assert_eq!(
            function.int_const_u128(wide),
            Some(0xFC),
            "stored value must be masked to the I8 width"
        );
        assert_eq!(
            function.graph().all_node_ids().count(),
            3,
            "normalised IntConst constants must not allocate a second node"
        );
    }

    #[test]
    fn non_cacheable_node_is_never_deduplicated() {
        let mut function = test_function();
        let id_a = function.graph_mut().create_node(NodeKind::Return, [], []);
        let id_b = function.graph_mut().create_node(NodeKind::Return, [], []);
        assert_ne!(
            id_a, id_b,
            "non-cacheable nodes must always produce distinct ids"
        );
    }

    /// Region is non-cacheable: a join's identity is positional, not
    /// structural.
    #[test]
    fn region_nodes_never_dedup() {
        let mut function = test_function();
        let entry = function
            .graph_mut()
            .create_node(NodeKind::Entry, [], [ValueKind::Control]);
        let [entry_ctrl] = function.node_outputs_exact::<1>(entry).unwrap();
        let outs = [ValueKind::Control, ValueKind::PhiToken];
        let r1 = function
            .graph_mut()
            .create_node(NodeKind::Region, [entry_ctrl], outs);
        let r2 = function
            .graph_mut()
            .create_node(NodeKind::Region, [entry_ctrl], outs);
        assert_ne!(
            r1, r2,
            "identical Regions must stay distinct (non-cacheable)"
        );
    }

    /// Phi is non-cacheable: two same-shaped phis over one region are still
    /// distinct merge points.
    #[test]
    fn phi_nodes_never_dedup() {
        let mut function = test_function();
        let entry = function
            .graph_mut()
            .create_node(NodeKind::Entry, [], [ValueKind::Control]);
        let [entry_ctrl] = function.node_outputs_exact::<1>(entry).unwrap();
        let region = function.graph_mut().create_node(
            NodeKind::Region,
            [entry_ctrl],
            [ValueKind::Control, ValueKind::PhiToken],
        );
        let [_region_ctrl, phi_token] = function.node_outputs_exact::<2>(region).unwrap();
        let c = function.graph_mut().create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(7_usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [c_value] = function.node_outputs_exact::<1>(c).unwrap();
        let ty = ValueKind::Typed(ValueType::I64);
        let p1 = function
            .graph_mut()
            .create_node(NodeKind::Phi, [phi_token, c_value], [ty]);
        let p2 = function
            .graph_mut()
            .create_node(NodeKind::Phi, [phi_token, c_value], [ty]);
        assert_ne!(p1, p2, "identical Phis must stay distinct (non-cacheable)");
    }

    /// Entry is cacheable: a function has only one.
    #[test]
    fn entry_node_kind_dedupes_on_repeated_create() {
        let mut function = test_function();
        let e1 = function
            .graph_mut()
            .create_node(NodeKind::Entry, [], [ValueKind::Control]);
        let e2 = function
            .graph_mut()
            .create_node(NodeKind::Entry, [], [ValueKind::Control]);
        assert_eq!(e1, e2, "Entry must dedupe — only one per function");
    }

    #[test]
    fn initial_memory_dedupes_on_repeated_create() {
        let mut function = test_function();
        let m1 = function
            .graph_mut()
            .create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
        let m2 = function
            .graph_mut()
            .create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
        assert_eq!(m1, m2, "InitialMemory must dedupe — only one per function");
    }

    /// The `InitialVnId` is part of the node kind, so same-id calls dedup and
    /// different-id calls do not.
    #[test]
    fn initial_var_dedupes_per_vn() {
        use crate::node::InitialVnId;
        let mut function = test_function();
        let id_a = InitialVnId::from_index(0);
        let id_b = InitialVnId::from_index(1);
        let value_kind = ValueKind::Typed(ValueType::I64);
        let v1 = function
            .graph_mut()
            .create_node(NodeKind::InitialVar(id_a), [], [value_kind]);
        let v2 = function
            .graph_mut()
            .create_node(NodeKind::InitialVar(id_a), [], [value_kind]);
        assert_eq!(v1, v2, "InitialVar with the same id must dedupe");

        let v3 = function
            .graph_mut()
            .create_node(NodeKind::InitialVar(id_b), [], [value_kind]);
        assert_ne!(v1, v3, "InitialVar with a different id must NOT dedupe");
    }

    /// Call is non-cacheable because `CallStackArgCollect` mutates its inputs
    /// after construction.
    #[test]
    fn adjacent_calls_with_same_args_are_distinct() {
        let mut function = test_function();
        let ctrl_a = function
            .graph_mut()
            .create_node(NodeKind::Entry, [], [ValueKind::Control]);
        let mem_a =
            function
                .graph_mut()
                .create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
        let [ctrl_value] = function.node_outputs_exact::<1>(ctrl_a).unwrap();
        let [mem_value] = function.node_outputs_exact::<1>(mem_a).unwrap();
        let target = function.graph_mut().create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(0x1000_usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [target_value] = function.node_outputs_exact::<1>(target).unwrap();
        let outs = [ValueKind::Control, ValueKind::Memory];
        let call_a = function.graph_mut().create_node(
            NodeKind::Call,
            [ctrl_value, mem_value, target_value],
            outs,
        );
        let call_b = function.graph_mut().create_node(
            NodeKind::Call,
            [ctrl_value, mem_value, target_value],
            outs,
        );
        assert_ne!(
            call_a, call_b,
            "Call is non-cacheable so identical-argument calls must be distinct"
        );
    }

    #[test]
    fn call_other_name_round_trip() {
        let mut function = test_function();
        // CallOther is non-cacheable, so the two nodes below stay distinct
        // despite sharing a user_op_id.
        let outs = [ValueKind::Control, ValueKind::Memory];
        let entry = function
            .graph_mut()
            .create_node(NodeKind::Entry, [], [ValueKind::Control]);
        let [entry_ctrl] = function.node_outputs_exact::<1>(entry).unwrap();
        let init_mem =
            function
                .graph_mut()
                .create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
        let [init_mem_value] = function.node_outputs_exact::<1>(init_mem).unwrap();
        let id_a = function.graph_mut().create_node(
            NodeKind::CallOther { user_op_id: 62 },
            [entry_ctrl, init_mem_value],
            outs,
        );
        let id_b = function.graph_mut().create_node(
            NodeKind::CallOther { user_op_id: 62 },
            [entry_ctrl, init_mem_value],
            outs,
        );
        assert_ne!(id_a, id_b, "CallOther is non-cacheable");
        assert_eq!(function.side_tables().call_other_name(id_a), None);
        function
            .side_tables_mut()
            .set_call_other_name(id_a, "setISAMode");
        assert_eq!(
            function.side_tables().call_other_name(id_a),
            Some("setISAMode")
        );
        assert_eq!(function.side_tables().call_other_name(id_b), None);
        function
            .side_tables_mut()
            .set_call_other_name(id_a, "OtherName");
        assert_eq!(
            function.side_tables().call_other_name(id_a),
            Some("OtherName")
        );
    }

    #[test]
    fn add_node_input_registers_use() {
        let mut function = test_function();
        let const_node = function.graph_mut().create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(1_usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [const_value] = function.node_outputs_exact::<1>(const_node).unwrap();

        // A non-cacheable sink.
        let ret_node = function.graph_mut().create_node(NodeKind::Return, [], []);

        function.graph_mut().add_node_input(ret_node, const_value);

        check_node_inputs(function.graph(), ret_node, [const_value]);

        let use_count = function.graph().value_uses(const_value).count();
        assert_eq!(use_count, 1);
    }

    /// Removal must also renumber the surviving inputs and unregister the use.
    #[test]
    fn remove_node_input_cleans_up_use_list() {
        let mut function = test_function();

        let c0 = function.graph_mut().create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(0_usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [out0] = function.node_outputs_exact::<1>(c0).unwrap();

        let c1 = function.graph_mut().create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(1_usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [out1] = function.node_outputs_exact::<1>(c1).unwrap();

        let ret = function.graph_mut().create_node(NodeKind::Return, [], []);
        function.graph_mut().add_node_input(ret, out0);
        function.graph_mut().add_node_input(ret, out1);

        assert!(
            function.graph_mut().remove_node_input(ret, 0),
            "removal must succeed"
        );

        check_node_inputs(function.graph(), ret, [out1]);

        assert_eq!(
            function.graph().value_uses(out0).count(),
            0,
            "out0 should have no uses after removal"
        );
        assert_eq!(
            function.graph().value_uses(out1).count(),
            1,
            "out1 should still have one use"
        );

        let (consumer, idx) = function
            .graph()
            .value_uses(out1)
            .next()
            .expect("out1 must still be used");
        assert_eq!(consumer, ret);
        assert_eq!(idx, 0);
    }

    #[test]
    fn update_input_moves_use_to_new_output() {
        let mut function = test_function();

        let old = function.graph_mut().create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(10_usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [old_value] = function.node_outputs_exact::<1>(old).unwrap();

        let new = function.graph_mut().create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(20_usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [new_value] = function.node_outputs_exact::<1>(new).unwrap();

        let ret = function.graph_mut().create_node(NodeKind::Return, [], []);
        function.graph_mut().add_node_input(ret, old_value);

        let use_id = function.graph().node_input_id_at(ret, 0).unwrap();

        function.graph_mut().update_input(use_id, new_value);

        assert_eq!(function.graph().value_uses(old_value).count(), 0);
        assert_eq!(function.graph().value_uses(new_value).count(), 1);

        check_node_inputs(function.graph(), ret, [new_value]);
    }

    #[test]
    fn detach_node_inputs_removes_all_uses() {
        let mut function = test_function();

        let c = function.graph_mut().create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(5_usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [value] = function.node_outputs_exact::<1>(c).unwrap();

        let ret = function.graph_mut().create_node(NodeKind::Return, [], []);
        function.graph_mut().add_node_input(ret, value);
        function.graph_mut().add_node_input(ret, value); // same output used twice

        assert_eq!(function.graph().value_uses(value).count(), 2);

        function.graph_mut().detach_node_inputs(ret);

        assert_eq!(
            function.graph().value_uses(value).count(),
            0,
            "all uses must be removed after detach"
        );
        assert_eq!(
            function.node_inputs(ret).len(),
            0,
            "node must have no inputs after detach"
        );
    }

    /// Detaching a cacheable node must also evict it from the dedup cache, or
    /// a later `create_node` with the same key returns the detached zombie
    /// whose input list is now empty and the next `node_inputs_exact` fails.
    #[test]
    fn detach_evicts_cacheable_node_from_dedup_cache() {
        use crate::node::IntBinaryOp;
        let mut function = test_function();
        let lhs = function.graph_mut().create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(7_usize)),
            [],
            [ValueKind::Typed(ValueType::I32)],
        );
        let rhs = function.graph_mut().create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(9_usize)),
            [],
            [ValueKind::Typed(ValueType::I32)],
        );
        let [lhs_value] = function.node_outputs_exact::<1>(lhs).unwrap();
        let [rhs_value] = function.node_outputs_exact::<1>(rhs).unwrap();

        let ty = ValueKind::Typed(ValueType::I32);
        let add_a = function.graph_mut().create_node(
            NodeKind::IntBinaryOp(IntBinaryOp::Add),
            [lhs_value, rhs_value],
            [ty],
        );

        function.graph_mut().detach_node_inputs(add_a);
        assert_eq!(function.node_inputs(add_a).len(), 0);

        let add_b = function.graph_mut().create_node(
            NodeKind::IntBinaryOp(IntBinaryOp::Add),
            [lhs_value, rhs_value],
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

    #[test]
    fn output_has_one_usage_tracks_consumer_count() {
        let mut function = test_function();

        let c = function.graph_mut().create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(99_usize)),
            [],
            [ValueKind::Typed(ValueType::I32)],
        );
        let [value] = function.node_outputs_exact::<1>(c).unwrap();

        assert!(
            !function.graph().value_has_one_use(value),
            "zero uses is not one"
        );

        let ret1 = function.graph_mut().create_node(NodeKind::Return, [], []);
        function.graph_mut().add_node_input(ret1, value);
        assert!(
            function.graph().value_has_one_use(value),
            "one use should return true"
        );

        let ret2 = function.graph_mut().create_node(NodeKind::Return, [], []);
        function.graph_mut().add_node_input(ret2, value);
        assert!(
            !function.graph().value_has_one_use(value),
            "two uses should return false"
        );
    }

    #[test]
    fn node_for_output_returns_source_node() {
        let mut function = test_function();
        let node = function.graph_mut().create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(7_usize)),
            [],
            [ValueKind::Typed(ValueType::I8)],
        );
        let [value] = function.node_outputs_exact::<1>(node).unwrap();
        assert_eq!(function.producer(value), node);
    }

    #[test]
    fn node_with_multiple_outputs() {
        let mut function = test_function();
        let node = function.graph_mut().create_node(
            NodeKind::If,
            [],
            [ValueKind::Control, ValueKind::Control],
        );
        let [true_ctrl, false_ctrl] = function.node_outputs_exact::<2>(node).unwrap();
        assert_eq!(function.value_kind(true_ctrl), ValueKind::Control);
        assert_eq!(function.value_kind(false_ctrl), ValueKind::Control);
        assert_eq!(function.value_definition(true_ctrl), (node, 0));
        assert_eq!(function.value_definition(false_ctrl), (node, 1));
    }

    /// One `(node_id, input_index)` per consumer, each appearing exactly once.
    #[test]
    fn output_uses_reports_all_consumers_with_correct_indices() {
        let mut function = test_function();
        let src = function.graph_mut().create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(7_usize)),
            [],
            [ValueKind::Typed(ValueType::I32)],
        );
        let [value] = function.node_outputs_exact::<1>(src).unwrap();

        let ret0 = function.graph_mut().create_node(NodeKind::Return, [], []);
        function.graph_mut().add_node_input(ret0, value);
        let ret1 = function.graph_mut().create_node(NodeKind::Return, [], []);
        function.graph_mut().add_node_input(ret1, value);
        let ret2 = function.graph_mut().create_node(NodeKind::Return, [], []);
        function.graph_mut().add_node_input(ret2, value);

        let uses: Vec<(NodeId, u32)> = function.graph().value_uses(value).collect();
        assert_eq!(uses.len(), 3, "all three consumers must appear");

        for expected_node in [ret0, ret1, ret2] {
            assert!(
                uses.iter().any(|(n, _)| *n == expected_node),
                "consumer {expected_node:?} missing from value_uses"
            );
        }
        for (_, idx) in &uses {
            assert_eq!(*idx, 0, "each single-input node's input_index must be 0");
        }
    }

    /// A node consuming one output twice must show up as two uses at their
    /// own indices.
    #[test]
    fn output_uses_same_output_multiple_times_reports_each_position() {
        let mut function = test_function();
        let src = function.graph_mut().create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(3_usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [value] = function.node_outputs_exact::<1>(src).unwrap();

        let sink = function.graph_mut().create_node(NodeKind::Return, [], []);
        function.graph_mut().add_node_input(sink, value); // input_index 0
        function.graph_mut().add_node_input(sink, value); // input_index 1

        let uses: Vec<(NodeId, u32)> = function.graph().value_uses(value).collect();
        assert_eq!(uses.len(), 2);

        let mut indices: Vec<u32> = uses.iter().map(|(_, i)| *i).collect();
        indices.sort_unstable();
        assert_eq!(indices, vec![0, 1], "both positional indices must appear");
    }

    /// `replace_current_with` must redirect the current use and advance past
    /// it, leaving the remaining use untouched.
    #[test]
    fn output_use_cursor_replace_redirects_first_use() {
        let mut function = test_function();

        let old_src = function.graph_mut().create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(1_usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [old_value] = function.node_outputs_exact::<1>(old_src).unwrap();

        let new_src = function.graph_mut().create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(2_usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [new_value] = function.node_outputs_exact::<1>(new_src).unwrap();

        let ret0 = function.graph_mut().create_node(NodeKind::Return, [], []);
        function.graph_mut().add_node_input(ret0, old_value);
        let ret1 = function.graph_mut().create_node(NodeKind::Return, [], []);
        function.graph_mut().add_node_input(ret1, old_value);

        assert_eq!(function.graph().value_uses(old_value).count(), 2);
        assert_eq!(function.graph().value_uses(new_value).count(), 0);

        {
            let mut cursor = function.graph_mut().value_use_cursor(old_value);
            cursor.replace_current_with(new_value);
        }

        assert_eq!(
            function.graph().value_uses(old_value).count(),
            1,
            "one use must remain on old_value"
        );
        assert_eq!(
            function.graph().value_uses(new_value).count(),
            1,
            "one use must move to new_value"
        );
    }

    #[test]
    fn output_use_cursor_replace_all_drains_source() {
        let mut function = test_function();

        let old_src = function.graph_mut().create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(10_usize)),
            [],
            [ValueKind::Typed(ValueType::I32)],
        );
        let [old_value] = function.node_outputs_exact::<1>(old_src).unwrap();

        let new_src = function.graph_mut().create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(20_usize)),
            [],
            [ValueKind::Typed(ValueType::I32)],
        );
        let [new_value] = function.node_outputs_exact::<1>(new_src).unwrap();

        for _ in 0..3 {
            let r = function.graph_mut().create_node(NodeKind::Return, [], []);
            function.graph_mut().add_node_input(r, old_value);
        }
        assert_eq!(function.graph().value_uses(old_value).count(), 3);

        let mut cursor = function.graph_mut().value_use_cursor(old_value);
        while cursor.current().is_some() {
            cursor.replace_current_with(new_value);
        }

        assert_eq!(
            function.graph().value_uses(old_value).count(),
            0,
            "all uses must be drained from old_value"
        );
        assert_eq!(
            function.graph().value_uses(new_value).count(),
            3,
            "all uses must land on new_value"
        );
    }

    /// Survivors keep their order and get contiguous indices from 0.
    #[test]
    fn remove_node_input_from_middle_reindexes_remaining() {
        let mut function = test_function();

        let out0 = {
            let n = function.graph_mut().create_node(
                NodeKind::IntConst(crate::node::const_value::ConstId::new(10_usize)),
                [],
                [ValueKind::Typed(ValueType::I64)],
            );
            function.node_outputs_exact::<1>(n).unwrap()[0]
        };
        let out1 = {
            let n = function.graph_mut().create_node(
                NodeKind::IntConst(crate::node::const_value::ConstId::new(20_usize)),
                [],
                [ValueKind::Typed(ValueType::I64)],
            );
            function.node_outputs_exact::<1>(n).unwrap()[0]
        };
        let out2 = {
            let n = function.graph_mut().create_node(
                NodeKind::IntConst(crate::node::const_value::ConstId::new(30_usize)),
                [],
                [ValueKind::Typed(ValueType::I64)],
            );
            function.node_outputs_exact::<1>(n).unwrap()[0]
        };

        let sink = function.graph_mut().create_node(NodeKind::Return, [], []);
        function.graph_mut().add_node_input(sink, out0); // index 0
        function.graph_mut().add_node_input(sink, out1); // index 1
        function.graph_mut().add_node_input(sink, out2); // index 2

        assert!(
            function.graph_mut().remove_node_input(sink, 1),
            "removal must succeed"
        ); // remove middle

        check_node_inputs(function.graph(), sink, [out0, out2]);
        assert_eq!(
            function.graph().value_uses(out1).count(),
            0,
            "out1 must be removed"
        );
        assert_eq!(function.graph().value_uses(out0).count(), 1);
        assert_eq!(function.graph().value_uses(out2).count(), 1);

        assert_eq!(
            function.graph().value_uses(out0).next().map(|(_, i)| i),
            Some(0),
            "surviving input 0 must have index 0"
        );
        assert_eq!(
            function.graph().value_uses(out2).next().map(|(_, i)| i),
            Some(1),
            "surviving input 1 must have index 1"
        );
    }

    #[test]
    fn remove_node_input_from_end_leaves_others_intact() {
        let mut function = test_function();

        let out0 = {
            let n = function.graph_mut().create_node(
                NodeKind::IntConst(crate::node::const_value::ConstId::new(1_usize)),
                [],
                [ValueKind::Typed(ValueType::I64)],
            );
            function.node_outputs_exact::<1>(n).unwrap()[0]
        };
        let out1 = {
            let n = function.graph_mut().create_node(
                NodeKind::IntConst(crate::node::const_value::ConstId::new(2_usize)),
                [],
                [ValueKind::Typed(ValueType::I64)],
            );
            function.node_outputs_exact::<1>(n).unwrap()[0]
        };

        let sink = function.graph_mut().create_node(NodeKind::Return, [], []);
        function.graph_mut().add_node_input(sink, out0);
        function.graph_mut().add_node_input(sink, out1);

        assert!(
            function.graph_mut().remove_node_input(sink, 1),
            "removal must succeed"
        ); // remove last

        check_node_inputs(function.graph(), sink, [out0]);
        assert_eq!(function.graph().value_uses(out1).count(), 0);
        assert_eq!(function.graph().value_uses(out0).count(), 1);

        assert_eq!(
            function.graph().value_uses(out0).next().map(|(_, i)| i),
            Some(0),
            "surviving input must keep index 0"
        );
    }

    /// Rewriting an input must evict the stale dedup-cache entry, or a later
    /// `create_node` with the original key returns the now-modified node and
    /// the optimizer silently miscompiles through `replace_all_uses`.
    #[test]
    fn update_input_on_cacheable_evicts_stale_cache_entry() {
        use crate::node::IntBinaryOp;
        let mut function = test_function();

        let a = function.graph_mut().create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(1_usize)),
            [],
            [ValueKind::Typed(ValueType::I32)],
        );
        let b = function.graph_mut().create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(2_usize)),
            [],
            [ValueKind::Typed(ValueType::I32)],
        );
        let c = function.graph_mut().create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(3_usize)),
            [],
            [ValueKind::Typed(ValueType::I32)],
        );
        let [a_value] = function.node_outputs_exact::<1>(a).unwrap();
        let [b_value] = function.node_outputs_exact::<1>(b).unwrap();
        let [c_value] = function.node_outputs_exact::<1>(c).unwrap();
        let ty = ValueKind::Typed(ValueType::I32);

        let add_ab = function.graph_mut().create_node(
            NodeKind::IntBinaryOp(IntBinaryOp::Add),
            [a_value, b_value],
            [ty],
        );

        // Redirect input[0] to c. The node now holds [c, b], while an
        // unmaintained cache would still map [a, b] to it.
        let in0 = function.graph().node_input_id_at(add_ab, 0).unwrap();
        function.graph_mut().update_input(in0, c_value);

        // Re-creating with the original key must not return add_ab, whose
        // inputs are now [c, b].
        let fresh = function.graph_mut().create_node(
            NodeKind::IntBinaryOp(IntBinaryOp::Add),
            [a_value, b_value],
            [ty],
        );
        assert_ne!(
            add_ab, fresh,
            "the stale cache entry must be evicted — re-creating the original \
         (kind, inputs, outputs) triple after update_input has redirected \
         one of those inputs must produce a fresh NodeId"
        );
    }

    /// A self-directed `update_input` must be a no-op.
    #[test]
    fn update_input_to_same_output_is_idempotent() {
        let mut function = test_function();

        let src = function.graph_mut().create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(99_usize)),
            [],
            [ValueKind::Typed(ValueType::I32)],
        );
        let [value] = function.node_outputs_exact::<1>(src).unwrap();

        let sink = function.graph_mut().create_node(NodeKind::Return, [], []);
        function.graph_mut().add_node_input(sink, value);

        let use_id = function.graph().node_input_id_at(sink, 0).unwrap();
        function.graph_mut().update_input(use_id, value);

        assert_eq!(
            function.graph().value_uses(value).count(),
            1,
            "self-update must not change use count"
        );
        check_node_inputs(function.graph(), sink, [value]);
    }

    #[test]
    fn detach_then_readd_restores_use_count() {
        let mut function = test_function();

        let src = function.graph_mut().create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(42_usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [value] = function.node_outputs_exact::<1>(src).unwrap();

        let sink = function.graph_mut().create_node(NodeKind::Return, [], []);
        function.graph_mut().add_node_input(sink, value);
        function.graph_mut().add_node_input(sink, value);
        assert_eq!(function.graph().value_uses(value).count(), 2);

        function.graph_mut().detach_node_inputs(sink);
        assert_eq!(
            function.graph().value_uses(value).count(),
            0,
            "uses cleared after detach"
        );
        assert_eq!(function.node_inputs(sink).len(), 0);

        function.graph_mut().add_node_input(sink, value);
        function.graph_mut().add_node_input(sink, value);
        assert_eq!(
            function.graph().value_uses(value).count(),
            2,
            "re-adding inputs must restore use count"
        );
        assert_eq!(function.node_inputs(sink).len(), 2);
    }

    /// The use linked-list must stay consistent across distinct consumers.
    #[test]
    fn two_independent_consumers_both_in_use_list() {
        let mut function = test_function();

        let src = function.graph_mut().create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(1_usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [value] = function.node_outputs_exact::<1>(src).unwrap();

        let b = function.graph_mut().create_node(NodeKind::Return, [], []);
        function.graph_mut().add_node_input(b, value);
        let c = function.graph_mut().create_node(NodeKind::Return, [], []);
        function.graph_mut().add_node_input(c, value);

        let uses: Vec<_> = function.graph().value_uses(value).collect();
        assert_eq!(uses.len(), 2);
        let nodes: Vec<_> = uses.iter().map(|(n, _)| *n).collect();
        assert!(nodes.contains(&b), "b must appear in use-list");
        assert!(nodes.contains(&c), "c must appear in use-list");
    }

    #[test]
    fn node_outputs_exact_errors_on_wrong_count() {
        let mut function = test_function();
        let node = function.graph_mut().create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(0_usize)),
            [],
            [ValueKind::Typed(ValueType::I8)],
        );
        let err = function.node_outputs_exact::<2>(node).unwrap_err();
        assert!(
            err.to_string().contains("does not have exactly 2 outputs"),
            "got: {err}"
        );
    }

    #[test]
    fn node_inputs_exact_errors_on_wrong_count() {
        let mut function = test_function();
        let src = function.graph_mut().create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(0_usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [value] = function.node_outputs_exact::<1>(src).unwrap();

        let sink = function.graph_mut().create_node(NodeKind::Return, [], []);
        function.graph_mut().add_node_input(sink, value); // exactly 1 input

        let err = function.graph().node_inputs_exact::<2>(sink).unwrap_err();
        assert!(
            err.to_string().contains("does not have exactly 2 inputs"),
            "got: {err}"
        );
    }

    #[test]
    fn update_input_self_redirect_preserves_use_list_order() {
        use crate::node::IntUnaryOp;
        let mut function = test_function();
        let c = function.graph_mut().create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(0_usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let cval = function.node_outputs(c).iter().copied().next().unwrap();
        // Two consumers of cval, to give the use-list real ordering.
        // `Truncate` and `Neg` because `Neg` is `IntUnaryOp`'s only variant.
        let _a = function.graph_mut().create_node(
            NodeKind::Truncate,
            [cval],
            [ValueKind::Typed(ValueType::I32)],
        );
        let b = function.graph_mut().create_node(
            NodeKind::IntUnaryOp(IntUnaryOp::Neg),
            [cval],
            [ValueKind::Typed(ValueType::I64)],
        );

        let head_before = function.graph().value_first_use_id(cval);

        let b_in0 = function.graph().node_input_id_at(b, 0).unwrap();
        function.graph_mut().update_input(b_in0, cval); // self-redirect, a no-op

        assert_eq!(
            head_before,
            function.graph().value_first_use_id(cval),
            "self-redirect must not re-order the use-list"
        );
    }

    #[test]
    fn remove_node_input_returns_false_on_out_of_bounds() {
        let mut function = test_function();
        let cs = function.graph_mut().create_node(
            NodeKind::Region,
            [],
            [ValueKind::Control, ValueKind::PhiToken],
        );
        assert!(
            !function.graph_mut().remove_node_input(cs, 7),
            "out-of-bounds remove must report no-op via false"
        );
    }

    #[test]
    fn node_input_id_at_returns_error_on_out_of_bounds() {
        let mut function = test_function();
        let n = function
            .graph_mut()
            .create_node(NodeKind::Entry, [], [ValueKind::Control]);
        let err = function
            .graph()
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

    #[test]
    fn asm_fingerprint_unset_returns_empty() {
        let mut function = test_function();
        let n = function
            .graph_mut()
            .create_node(NodeKind::Entry, [], [ValueKind::Control]);
        assert!(function.side_tables().asm_fingerprint(n).is_empty());
    }

    #[test]
    fn asm_fingerprint_extend_then_get() {
        let mut function = test_function();
        let n = function
            .graph_mut()
            .create_node(NodeKind::Entry, [], [ValueKind::Control]);
        function
            .side_tables_mut()
            .extend_asm_fingerprint(n, &[0x1000, 0x1004, 0x1008]);
        assert_eq!(
            function.side_tables().asm_fingerprint(n),
            FxHashSet::from_iter([0x1000, 0x1004, 0x1008])
        );
    }

    #[test]
    fn asm_fingerprint_extend_dedupes() {
        let mut function = test_function();
        let n = function
            .graph_mut()
            .create_node(NodeKind::Entry, [], [ValueKind::Control]);
        function
            .side_tables_mut()
            .extend_asm_fingerprint(n, &[0x1004, 0x1000, 0x1004]);
        assert_eq!(
            function.side_tables().asm_fingerprint(n),
            FxHashSet::from_iter([0x1000, 0x1004])
        );
        function
            .side_tables_mut()
            .extend_asm_fingerprint(n, &[0x1008, 0x1000, 0x1004]);
        assert_eq!(
            function.side_tables().asm_fingerprint(n),
            FxHashSet::from_iter([0x1000, 0x1004, 0x1008])
        );
    }

    #[test]
    fn asm_fingerprint_extend_from_unions_two_nodes() {
        let mut function = test_function();
        let a = function
            .graph_mut()
            .create_node(NodeKind::Entry, [], [ValueKind::Control]);
        let b = function
            .graph_mut()
            .create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
        function
            .side_tables_mut()
            .extend_asm_fingerprint(a, &[0x1000, 0x1004]);
        function
            .side_tables_mut()
            .extend_asm_fingerprint(b, &[0x1004, 0x100C]);
        function.side_tables_mut().extend_asm_fingerprint_from(a, b);
        assert_eq!(
            function.side_tables().asm_fingerprint(a),
            FxHashSet::from_iter([0x1000, 0x1004, 0x100C])
        );
        assert_eq!(
            function.side_tables().asm_fingerprint(b),
            FxHashSet::from_iter([0x1004, 0x100C])
        );
    }

    #[test]
    fn asm_fingerprint_extend_never_shrinks() {
        let mut function = test_function();
        let n = function
            .graph_mut()
            .create_node(NodeKind::Entry, [], [ValueKind::Control]);
        function
            .side_tables_mut()
            .extend_asm_fingerprint(n, &[0x1000, 0x1004, 0x1008]);
        // A strict subset must not remove existing entries.
        function
            .side_tables_mut()
            .extend_asm_fingerprint(n, &[0x1004]);
        assert_eq!(
            function.side_tables().asm_fingerprint(n),
            FxHashSet::from_iter([0x1000, 0x1004, 0x1008])
        );
        function.side_tables_mut().extend_asm_fingerprint(n, &[]);
        assert_eq!(
            function.side_tables().asm_fingerprint(n),
            FxHashSet::from_iter([0x1000, 0x1004, 0x1008])
        );
    }

    #[test]
    fn asm_fingerprint_extend_from_self_is_noop() {
        let mut function = test_function();
        let n = function
            .graph_mut()
            .create_node(NodeKind::Entry, [], [ValueKind::Control]);
        function
            .side_tables_mut()
            .extend_asm_fingerprint(n, &[0x1000, 0x1004]);
        function.side_tables_mut().extend_asm_fingerprint_from(n, n);
        assert_eq!(
            function.side_tables().asm_fingerprint(n),
            FxHashSet::from_iter([0x1000, 0x1004])
        );
    }

    #[test]
    fn get_cc_default_falls_back_to_function_cc() {
        let mut function = test_function();
        let nid = function.graph_mut().create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(0_usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        // With no recorded descriptor, get_cc falls back to the trivial
        // default CC, which has no stack args.
        assert_eq!(function.get_cc(nid), function.default_cc());
        assert!(function.get_cc(nid).stack_args.is_none());
    }

    #[test]
    fn get_cc_override_round_trips_and_derives_stack_args() {
        let arch = strider_target::SleighArch::x86_64();
        let regs = arch.probe_regs().unwrap();
        let cc = strider_target::CallingConvention::x86_64_systemv()
            .build(&regs)
            .unwrap();

        let mut function = test_function();
        let nid = function.graph_mut().create_node(
            NodeKind::Call,
            [],
            [ValueKind::Control, ValueKind::Memory],
        );
        function.side_tables_mut().set_call_cc(nid, cc.clone());
        // The override differs from the default, so the stack args derive
        // from it.
        assert_ne!(function.get_cc(nid), function.default_cc());
        assert_eq!(function.get_cc(nid).stack_args, cc.stack_args,);
    }

    #[test]
    fn value_vn_clobber_tag_round_trips() {
        let mut function = test_function();
        let nid = function.graph_mut().create_node(
            NodeKind::Call,
            [],
            [
                ValueKind::Control,
                ValueKind::Memory,
                ValueKind::Typed(ValueType::I64),
            ],
        );
        let clobber_value = function.node_outputs(nid)[2];
        let vn = rsleigh::Vn {
            size: 8,
            addr_off: 0x10,
            addr_space: rsleigh::VnSpace::REGISTER,
        };
        function.set_all_vns(vec![vn]); // only a tracked vn can be tagged
        assert!(function.get_vn_for_value(clobber_value).is_none());
        function.set_vn_for_value(clobber_value, vn);
        assert_eq!(function.get_vn_for_value(clobber_value), Some(vn));
    }

    #[test]
    fn asm_fingerprint_dedup_cache_hit_unions_via_extend() {
        // Both IntConst(7) creations hit the dedup cache and land on one
        // NodeId. Production stamps a fingerprint at every create_node site,
        // so both contributors union into that single side-table entry.
        let mut function = test_function();
        let a = function.graph_mut().create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(7_usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        function
            .side_tables_mut()
            .extend_asm_fingerprint(a, &[0x2000]);
        let b = function.graph_mut().create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(7_usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        assert_eq!(a, b, "cacheable nodes should dedup");
        function
            .side_tables_mut()
            .extend_asm_fingerprint(b, &[0x3000]);
        assert_eq!(
            function.side_tables().asm_fingerprint(a),
            FxHashSet::from_iter([0x2000, 0x3000])
        );
    }
}
