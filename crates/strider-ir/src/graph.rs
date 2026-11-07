use std::hash::BuildHasher;

use rustc_hash::FxBuildHasher;
use strider_graph::{NodeCacheable, RawStore, ValueId};

use crate::node::{NodeId, NodeKind, ValueKind};

/// The IR dedup policy: cacheable kinds dedup on
/// `(NodeKind, inputs, output_kinds)`, non-cacheable ones always allocate
/// fresh.
#[derive(Clone)]
pub struct IrCacheable;

impl NodeCacheable<NodeKind, ValueKind> for IrCacheable {
    fn should_cache(kind: &NodeKind) -> bool {
        kind.is_cacheable()
    }

    fn hash(kind: &NodeKind, inputs: &[ValueId], outputs: &[ValueKind]) -> u64 {
        FxBuildHasher.hash_one((kind, inputs, outputs))
    }

    /// Recomputes `cand`'s identity from the live store.
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

pub type Graph = strider_graph::Graph<NodeKind, ValueKind, IrCacheable>;

// The per-node and per-value arena records.
const _: () = assert!(Graph::NODE_RECORD_SIZE == 24);
const _: () = assert!(Graph::VALUE_RECORD_SIZE == 16);

pub type Inputs<'a> = strider_graph::Inputs<'a, NodeKind, ValueKind, IrCacheable>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IRViewer;
    use crate::function::{test_function, test_function_tracking};
    use crate::node::{NodeId, NodeKind, ValueId, ValueKind, ValueType};
    use cranelift_entity::EntityRef;
    use rustc_hash::{FxHashSet, FxHasher};
    use std::hash::{Hash, Hasher};

    /// The policy hash is the field-by-field `FxHasher` stream of the key.
    #[test]
    fn ir_cacheable_hash_is_the_fx_stream_of_its_key() {
        let int = ValueKind::Typed(ValueType::int(32));
        let keys: [(NodeKind, Vec<ValueId>, Vec<ValueKind>); 3] = [
            (NodeKind::Entry, vec![], vec![ValueKind::Control]),
            (
                NodeKind::IntConst(crate::node::IntConstId::new(7_usize)),
                vec![],
                vec![int],
            ),
            (
                NodeKind::IntBinaryOp(crate::node::IntBinaryOp::Add),
                vec![ValueId::new(3), ValueId::new(1)],
                vec![int],
            ),
        ];
        for (kind, inputs, outputs) in &keys {
            let mut h = FxHasher::default();
            kind.hash(&mut h);
            inputs.as_slice().hash(&mut h);
            outputs.as_slice().hash(&mut h);
            assert_eq!(IrCacheable::hash(kind, inputs, outputs), h.finish());
        }
    }

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
            NodeKind::IntConst(crate::node::IntConstId::new(5_usize)),
            [],
            [ValueKind::Typed(ValueType::int(64))],
        );
        assert_eq!(
            function.node_kind(node_id),
            &NodeKind::IntConst(crate::node::IntConstId::new(5_usize))
        );
        assert_eq!(function.graph().all_node_ids().count(), 3);
        check_node_inputs(function.graph(), node_id, []);
        check_node_output_kinds(
            function.graph(),
            node_id,
            vec![ValueKind::Typed(ValueType::int(64))],
        );
        check_node_output_definitions(function.graph(), node_id, vec![(node_id, 0)]);
    }

    /// `kind_of_value` must agree with the two-step
    /// `node_kind(producer(out))` lookup.
    #[test]
    fn kind_of_output_matches_two_step_lookup() {
        let mut function = test_function();
        let node_id = function.graph_mut().create_node(
            NodeKind::IntConst(crate::node::IntConstId::new(7_usize)),
            [],
            [ValueKind::Typed(ValueType::int(64))],
        );
        let [value] = function.node_outputs_exact::<1>(node_id).unwrap();
        let two_step = function.node_kind(function.producer(value));
        let one_step = function.kind_of_value(value);
        assert_eq!(one_step, two_step);
        assert_eq!(
            one_step,
            &NodeKind::IntConst(crate::node::IntConstId::new(7_usize))
        );
    }

    /// The gate for the borrowed-key dedup probe, whose hash must agree with
    /// the owned-key insert or every repeat allocates a fresh node.
    #[test]
    fn cacheable_node_dedup_is_stable_across_many_calls() {
        let mut function = test_function();
        let first = function.graph_mut().create_node(
            NodeKind::IntConst(crate::node::IntConstId::new((0xdead_beef_u64) as usize)),
            [],
            [ValueKind::Typed(ValueType::int(64))],
        );
        let arena_after_first = function.graph().all_node_ids().count();
        for _ in 0..1000 {
            let id = function.graph_mut().create_node(
                NodeKind::IntConst(crate::node::IntConstId::new((0xdead_beef_u64) as usize)),
                [],
                [ValueKind::Typed(ValueType::int(64))],
            );
            assert_eq!(id, first, "cache hit must return the original id");
        }
        assert_eq!(
            function.graph().all_node_ids().count(),
            arena_after_first,
            "no new nodes should be allocated on repeated cache hits",
        );
    }

    /// A value with bits above the declared width and its masked form must
    /// dedup to one node.
    #[test]
    fn int_const_payload_is_normalised_to_output_type_width() {
        use crate::{IRBuilderExt, IRViewer};
        let mut function = test_function();
        // -4 at I8: only the low 8 bits of 0x1FC matter.
        let wide = function
            .build_int_const(0x1FCu128, ValueType::int(8))
            .unwrap();
        let masked = function
            .build_int_const(0xFCu128, ValueType::int(8))
            .unwrap();
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
            NodeKind::IntConst(crate::node::IntConstId::new(7_usize)),
            [],
            [ValueKind::Typed(ValueType::int(64))],
        );
        let [c_value] = function.node_outputs_exact::<1>(c).unwrap();
        let ty = ValueKind::Typed(ValueType::int(64));
        let p1 = function
            .graph_mut()
            .create_node(NodeKind::Phi, [phi_token, c_value], [ty]);
        let p2 = function
            .graph_mut()
            .create_node(NodeKind::Phi, [phi_token, c_value], [ty]);
        assert_ne!(p1, p2, "identical Phis must stay distinct (non-cacheable)");
    }

    /// The `InitialVnId` is part of the node kind, so same-id calls dedup and
    /// different-id calls do not.
    #[test]
    fn initial_var_dedupes_per_vn() {
        use crate::node::InitialVnId;
        let mut function = test_function();
        let id_a = InitialVnId::from_index(0);
        let id_b = InitialVnId::from_index(1);
        let value_kind = ValueKind::Typed(ValueType::int(64));
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

    /// Call is non-cacheable: its inputs are mutated after construction.
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
            NodeKind::IntConst(crate::node::IntConstId::new(0x1000_usize)),
            [],
            [ValueKind::Typed(ValueType::int(64))],
        );
        let [target_value] = function.node_outputs_exact::<1>(target).unwrap();
        let outs = [ValueKind::Control, ValueKind::Memory];
        let call_a = function.graph_mut().create_node(
            NodeKind::Call { cc: None },
            [ctrl_value, mem_value, target_value],
            outs,
        );
        let call_b = function.graph_mut().create_node(
            NodeKind::Call { cc: None },
            [ctrl_value, mem_value, target_value],
            outs,
        );
        assert_ne!(
            call_a, call_b,
            "Call is non-cacheable so identical-argument calls must be distinct"
        );
    }

    /// The name belongs to the user-op, so every node of that op carries it.
    #[test]
    fn call_other_name_round_trip() {
        let mut function = test_function();
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
        let call_other = |function: &mut crate::Function, user_op_id| {
            function.graph_mut().create_node(
                NodeKind::CallOther { user_op_id },
                [entry_ctrl, init_mem_value],
                outs,
            )
        };
        let id_a = call_other(&mut function, 62);
        let id_b = call_other(&mut function, 62);
        let id_c = call_other(&mut function, 7);
        assert_ne!(id_a, id_b, "CallOther is non-cacheable");
        assert_eq!(function.call_other_name(id_a), None);
        function.set_call_other_name(62, "setISAMode");
        assert_eq!(function.call_other_name(id_a), Some("setISAMode"));
        assert_eq!(function.call_other_name(id_b), Some("setISAMode"));
        assert_eq!(function.call_other_name(id_c), None);
        function.set_call_other_name(62, "OtherName");
        assert_eq!(function.call_other_name(id_a), Some("OtherName"));
        assert_eq!(function.call_other_name(entry), None, "not a CallOther");
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
            NodeKind::IntConst(crate::node::IntConstId::new(0_usize)),
            [],
            [ValueKind::Typed(ValueType::int(64))],
        );
        assert_eq!(function.get_cc(nid), function.default_cc());
        assert!(function.get_cc(nid).stack_args.is_none());
    }

    #[test]
    fn get_cc_override_round_trips_and_derives_stack_args() {
        let arch = strider_target::SleighArch::x86_64();
        let regs = arch.probe_regs();
        let cc = strider_target::CallingConvention::x86_64_systemv()
            .build(&regs)
            .unwrap();

        let mut function = test_function();
        let nid = function.graph_mut().create_node(
            NodeKind::Call { cc: None },
            [],
            [ValueKind::Control, ValueKind::Memory],
        );
        function.set_call_cc(nid, cc.clone());
        assert_ne!(function.get_cc(nid), function.default_cc());
        assert_eq!(function.get_cc(nid).stack_args, cc.stack_args,);
    }

    #[test]
    fn value_vn_clobber_tag_round_trips() {
        let vn = rsleigh::Vn {
            size: 8,
            addr_off: 0x10,
            addr_space: rsleigh::VnSpace::REGISTER,
        };
        let mut function = test_function_tracking(vec![vn]);
        let nid = function.graph_mut().create_node(
            NodeKind::Call { cc: None },
            [],
            [
                ValueKind::Control,
                ValueKind::Memory,
                ValueKind::Typed(ValueType::int(64)),
            ],
        );
        let clobber_value = function.node_outputs(nid)[2];
        assert!(function.get_vn_for_value(clobber_value).is_none());
        function.set_vn_for_value(clobber_value, vn);
        assert_eq!(function.get_vn_for_value(clobber_value), Some(vn));
    }

    #[test]
    fn asm_fingerprint_dedup_cache_hit_unions_via_extend() {
        // Both IntConst(7) creations land on one NodeId, so both contributors
        // union into that single side-table entry.
        let mut function = test_function();
        let a = function.graph_mut().create_node(
            NodeKind::IntConst(crate::node::IntConstId::new(7_usize)),
            [],
            [ValueKind::Typed(ValueType::int(64))],
        );
        function
            .side_tables_mut()
            .extend_asm_fingerprint(a, &[0x2000]);
        let b = function.graph_mut().create_node(
            NodeKind::IntConst(crate::node::IntConstId::new(7_usize)),
            [],
            [ValueKind::Typed(ValueType::int(64))],
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
