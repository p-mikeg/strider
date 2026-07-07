//! The IR sea-of-nodes [`Graph`] — a type alias over the generic
//! [`strider_graph::Graph`] parameterised with the IR payloads
//! ([`crate::node::NodeKind`] / [`crate::node::ValueKind`]) and the IR's
//! dedup policy ([`IrCacheable`]).
//!
//! The structural machinery (node arena, use-lists, compaction, structural
//! walks, `Inputs` / `InputCursor` navigation) lives in `strider-graph`. This
//! module supplies only the strider-specific overlay:
//!
//! - [`IrCacheable`] — the `(NodeKind, inputs, output_kinds)` dedup
//!   policy (`should_cache` / `hash` / `eq`).  It is purely mechanical: it
//!   embeds no domain normalisation.  Integer-constant canonicalisation
//!   (masking + small→wide promotion) happens at construction in
//!   `Function::create_node_attributed`, before a node reaches the cache.
//! - The `Inputs` / `InputCursor` IR-payload aliases.
//!
//! The typed / fallible structural accessors (`node_outputs_exact` /
//! `node_inputs_exact` / `node_input_id_at`) are inherent on the generic
//! [`strider_graph::Graph`]; the function-overlay reads and the control-aware
//! walks live on [`crate::IRViewer`] / [`crate::IRWalker`].

use std::hash::{Hash, Hasher};

use rustc_hash::FxHasher;
use strider_graph::{NodeCacheable, RawStore, ValueId};

use crate::node::{NodeId, NodeKind, ValueKind};

/// The IR's deduplication policy: a stateless ZST supplying the three
/// [`NodeCacheable`] hooks. It owns no state — the generic
/// `strider_graph::Graph` owns the dedup table and per-node hashes.
///
/// Cacheable node kinds (see [`NodeKind::is_cacheable`]) are deduplicated by
/// their `(NodeKind, inputs, output_kinds)` structure; non-cacheable kinds
/// (`Region`, `Phi`, `MemPhi`, `Call`, …) always allocate a fresh node.
pub struct IrCacheable;

impl NodeCacheable<NodeKind, ValueKind> for IrCacheable {
    /// Gates dedup on [`NodeKind::is_cacheable`].
    fn should_cache(kind: &NodeKind) -> bool {
        kind.is_cacheable()
    }

    /// Hashes a `(kind, inputs, output_kinds)` structural key into a `u64`.
    ///
    /// The fields are hashed in declaration order (`kind`, then the input-value
    /// slice, then the output-kind slice). `[T]: Hash` hashes the length
    /// followed by each element, so hashing a borrowed query slice and hashing
    /// a node's re-read `SmallVec` of the same contents agree element-for-
    /// element — which is what lets a query probe land in the same bucket the
    /// node was inserted under.
    ///
    /// Returns a RAW `FxHash` with no sentinel handling: the generic cache
    /// remaps the lone `u64::MAX` value itself.
    fn hash(kind: &NodeKind, inputs: &[ValueId], outputs: &[ValueKind]) -> u64 {
        let mut h = FxHasher::default();
        kind.hash(&mut h);
        inputs.hash(&mut h);
        outputs.hash(&mut h);
        h.finish()
    }

    /// Re-reads candidate node `cand` from the store and reports whether its
    /// stored `(kind, inputs, output_kinds)` structure equals the query. This
    /// is the equality half of the hash-on-demand probe: no owned key payloads
    /// are kept, so structural identity is recomputed from the live store.
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

// The id translation table is structural — it comes from `strider-graph`.
pub use strider_graph::NodeIdRemap;

/// The IR sea-of-nodes graph.
///
/// A [`strider_graph::Graph`] over the IR node payload ([`NodeKind`]), the IR
/// value payload ([`ValueKind`]), and the IR dedup policy ([`IrCacheable`]).
/// Cacheable node kinds (see [`NodeKind::is_cacheable`]) are deduplicated by
/// `(NodeKind, inputs, output_kinds)`; non-cacheable kinds always allocate a
/// fresh [`NodeId`].
///
/// All structural verbs (`create_node`, `add_node_input`, `update_input`,
/// `replace_all_uses`, the read accessors, the typed `node_outputs_exact` /
/// `node_inputs_exact` / `node_input_id_at`, …) are inherited from the generic
/// graph. The function-overlay reads and control-aware walks live on
/// [`crate::IRViewer`] / [`crate::IRWalker`].
pub type Graph = strider_graph::Graph<NodeKind, ValueKind, IrCacheable>;

/// An iterable view over the input values of a node — the IR-payload
/// instantiation of [`strider_graph::Inputs`].
pub type Inputs<'a> = strider_graph::Inputs<'a, NodeKind, ValueKind, IrCacheable>;

/// A cursor over the use-list of a single value — the IR-payload
/// instantiation of [`strider_graph::InputCursor`].
pub type InputCursor<'a> = strider_graph::InputCursor<'a, NodeKind, ValueKind, IrCacheable>;

#[cfg(test)]
mod tests {
    //! White-box tests for the graph submodules — arena, dedup cache,
    //! use-list bookkeeping, and typed accessors.

    use super::*;
    use crate::IRViewer;
    use crate::function::test_function;
    use crate::node::{NodeId, NodeKind, ValueId, ValueKind, ValueType};
    use cranelift_entity::EntityRef;
    use rustc_hash::FxHashSet;

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

    /// Creates a simple constant node (no inputs) and checks that its
    /// metadata is stored correctly.
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

    /// `kind_of_value` agrees with the two-step `node_kind(producer(out))`
    /// lookup it replaces — pinned because ~100 callsites depend on the equivalence.
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

    /// Cacheable nodes with identical kind and inputs must be deduplicated:
    /// the second call must return the same [`NodeId`] as the first and must
    /// not grow the node table.
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

    /// Repeated `create_node` calls with the same cacheable-kind key must
    /// return the same `NodeId` and grow the arena exactly once.  This pins
    /// the behavioural contract of the hash-on-demand dedup cache: a cache
    /// *hit* re-reads the candidate from the store for equality and must
    /// allocate no duplicate node.  Bulk-shape variant of
    /// `cacheable_node_is_deduplicated` to guard against accidental
    /// disagreement between the query hash (`hash_key`) and the per-node
    /// cached hash an entry was inserted under.
    #[test]
    fn cacheable_node_dedup_is_stable_across_many_calls() {
        let mut function = test_function();
        let first = function.graph_mut().create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new((0xdead_beef_u64) as usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let arena_after_first = function.graph().all_node_ids().count();
        for _ in 0..1000 {
            let id = function.graph_mut().create_node(
                NodeKind::IntConst(crate::node::const_value::ConstId::new((0xdead_beef_u64) as usize)),
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

    /// Two cacheable nodes with identical kind + inputs but different
    /// `output_kinds` (e.g. `IntConst(0): I32` vs `IntConst(0): I64`)
    /// must NOT dedup.  Pins that the dedup key includes `output_kinds`;
    /// a regression that hashed only `(kind, inputs)` would alias values
    /// of different widths and produce type-incorrect outputs at
    /// consumers.
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

    /// Two `IntConst` nodes that are semantically equal under their declared
    /// integer output type — one built from a value already masked to the width,
    /// the other from a value with extra high bits above the width — must dedup
    /// to the SAME `NodeId`.  Masking now lives at the interning choke-point
    /// (`Function::intern_int_const`, reached via `build_int_const`): equal
    /// masked values share one `ConstId`, so the two `IntConst(id)` nodes are
    /// structurally equal and dedup.
    #[test]
    fn int_const_payload_is_normalised_to_output_type_width() {
        use crate::{IRBuilderExt, IRViewer};
        let mut function = test_function();
        // -4 as I8: value with bits above bit 7 vs the 8-bit-masked form.
        // 0x1FC = 0b1_1111_1100 — only low 8 bits (0xFC) matter for I8.
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

    /// Non-cacheable nodes (e.g. `Return`) must always produce fresh ids even
    /// when all arguments are identical.
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

    /// Two structurally identical `Region` nodes must get distinct ids —
    /// Region is non-cacheable (a join's identity is positional, not
    /// structural).
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

    /// Two structurally identical `Phi` nodes must get distinct ids — Phi is
    /// non-cacheable (two same-shaped phis over one region are still distinct
    /// merge points).
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

    /// `Entry` is now cacheable — repeated `create_node` calls with the same
    /// signature must return the same `NodeId` (only one Entry per function).
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

    /// `InitialMemory` is now cacheable — repeated `create_node` calls must
    /// return the same `NodeId`.
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

    /// `InitialVar` is cacheable — the `InitialVnId` is part of the node kind, so
    /// two calls with the **same** id dedup and two calls with **different** ids
    /// produce distinct nodes.
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

    /// Two adjacent `Call` nodes with identical target and argument outputs
    /// must stay distinct — Call is non-cacheable because `CallStackArgCollect`
    /// mutates its inputs after construction.
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

    /// `Graph::call_other_name` round-trip: setting and reading back a name
    /// works, and unset nodes return `None`.
    #[test]
    fn call_other_name_round_trip() {
        let mut function = test_function();
        // Two CallOther nodes with the same user_op_id.  CallOther is
        // non-cacheable (see `is_cacheable`), so they get distinct ids.
        let outs = [ValueKind::Control, ValueKind::Memory];
        // We need a control + memory input to construct a CallOther; build a
        // throwaway Entry and InitialMemory.
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
        assert_eq!(function.side_tables().call_other_name(id_a), Some("setISAMode"));
        assert_eq!(function.side_tables().call_other_name(id_b), None);
        // Replacement
        function
            .side_tables_mut()
            .set_call_other_name(id_a, "OtherName");
        assert_eq!(function.side_tables().call_other_name(id_a), Some("OtherName"));
    }

    /// After adding an input to a non-cacheable node the output's use-list
    /// must contain exactly that input, and `node_inputs` must reflect it.
    #[test]
    fn add_node_input_registers_use() {
        let mut function = test_function();
        // Produce a value
        let const_node = function.graph_mut().create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(1_usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [const_value] = function.node_outputs_exact::<1>(const_node).unwrap();

        // Create a non-cacheable sink
        let ret_node = function.graph_mut().create_node(NodeKind::Return, [], []);

        function.graph_mut().add_node_input(ret_node, const_value);

        // The input must appear in node_inputs
        check_node_inputs(function.graph(), ret_node, [const_value]);

        // The output's use-list must contain this input
        let use_count = function.graph().value_uses(const_value).count();
        assert_eq!(use_count, 1);
    }

    /// `remove_node_input` must shrink the input list, update subsequent
    /// input indices, and unregister the use from the output's use-list.
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

        // Remove the first input (index 0 = out0)
        assert!(
            function.graph_mut().remove_node_input(ret, 0),
            "removal must succeed"
        );

        // Only out1 should remain
        check_node_inputs(function.graph(), ret, [out1]);

        // out0 must no longer be used
        assert_eq!(
            function.graph().value_uses(out0).count(),
            0,
            "out0 should have no uses after removal"
        );
        // out1 must still be used
        assert_eq!(
            function.graph().value_uses(out1).count(),
            1,
            "out1 should still have one use"
        );

        // The surviving input must have its index adjusted to 0
        let (consumer, idx) = function
            .graph()
            .value_uses(out1)
            .next()
            .expect("out1 must still be used");
        assert_eq!(consumer, ret);
        assert_eq!(idx, 0);
    }

    /// `update_input` must move the use from the old output to the new one
    /// so that use-lists stay consistent.
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

        // Find the single input id
        let use_id = function.graph().node_input_id_at(ret, 0).unwrap();

        function.graph_mut().update_input(use_id, new_value);

        // old_value must have no uses; new_value must have one
        assert_eq!(function.graph().value_uses(old_value).count(), 0);
        assert_eq!(function.graph().value_uses(new_value).count(), 1);

        // The node input must now reference new_value
        check_node_inputs(function.graph(), ret, [new_value]);
    }

    /// `detach_node_inputs` must clear all inputs from the node and remove
    /// them from every output's use-list.
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

    /// An output consumed by a single node must be reported by
    /// `value_has_one_use` as `true`; consuming it a second time must
    /// flip it to `false`.
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

    /// `producer` must return the node that created the output.
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

    /// A node with two outputs must expose both with correct kinds and
    /// definitions.
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

    /// `value_uses` must yield one `(node_id, input_index)` tuple per
    /// consumer, with the correct node id and position within that node's
    /// input list.  Three independent consumers all at input-index 0 must
    /// all appear exactly once.
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
        // Each of the three nodes has exactly one input, so input_index is 0.
        for (_, idx) in &uses {
            assert_eq!(*idx, 0, "each single-input node's input_index must be 0");
        }
    }

    /// When a node has multiple inputs from the same output, `value_uses`
    /// must report all of them with their correct positional indices.
    #[test]
    fn output_uses_same_output_multiple_times_reports_each_position() {
        let mut function = test_function();
        let src = function.graph_mut().create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(3_usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [value] = function.node_outputs_exact::<1>(src).unwrap();

        // Same output at positions 0 and 1 of the same sink node.
        let sink = function.graph_mut().create_node(NodeKind::Return, [], []);
        function.graph_mut().add_node_input(sink, value); // input_index 0
        function.graph_mut().add_node_input(sink, value); // input_index 1

        let uses: Vec<(NodeId, u32)> = function.graph().value_uses(value).collect();
        assert_eq!(uses.len(), 2);

        let mut indices: Vec<u32> = uses.iter().map(|(_, i)| *i).collect();
        indices.sort_unstable();
        assert_eq!(indices, vec![0, 1], "both positional indices must appear");
    }

    /// `value_use_cursor` iterates the same set as `value_uses`.
    /// `replace_current_with` must redirect the first use to a new output
    /// and advance past it so the remaining use is untouched.
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

        // Two consumers of old_value.
        let ret0 = function.graph_mut().create_node(NodeKind::Return, [], []);
        function.graph_mut().add_node_input(ret0, old_value);
        let ret1 = function.graph_mut().create_node(NodeKind::Return, [], []);
        function.graph_mut().add_node_input(ret1, old_value);

        assert_eq!(function.graph().value_uses(old_value).count(), 2);
        assert_eq!(function.graph().value_uses(new_value).count(), 0);

        // Redirect the first consumer to new_value.
        {
            let mut cursor = function.graph_mut().value_use_cursor(old_value);
            cursor.replace_current_with(new_value);
        }

        // After one replacement: old_value has one use, new_value has one use.
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

    /// `value_use_cursor` with `replace_current_with` applied to every
    /// element must leave the original output with no uses and transfer all
    /// uses to the replacement.
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

        // Three consumers.
        for _ in 0..3 {
            let r = function.graph_mut().create_node(NodeKind::Return, [], []);
            function.graph_mut().add_node_input(r, old_value);
        }
        assert_eq!(function.graph().value_uses(old_value).count(), 3);

        // Replace all uses in a single cursor pass.
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

    /// Removing the middle input of a three-input node must: leave the
    /// two survivors in order, re-number their indices contiguously from 0,
    /// and remove the deleted input from its output's use-list.
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

        // Surviving inputs must be reindexed contiguously (0, 1).
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

    /// Removing the last input must not disturb the preceding inputs.
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

    /// `update_input` on an input belonging to a cacheable node must evict the
    /// stale dedup-cache entry. Otherwise a later `create_node` with the
    /// original `(kind, inputs, outputs)` triple returns the now-modified
    /// node, which has different inputs — silent miscompilation by the
    /// optimizer (which calls `update_input` via `replace_all_uses`).
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

        // Cache key inserted: (Add, [a, b], [ty]) → add_ab.
        let add_ab = function.graph_mut().create_node(
            NodeKind::IntBinaryOp(IntBinaryOp::Add),
            [a_value, b_value],
            [ty],
        );

        // Redirect input[0] from a → c. Node now actually has inputs [c, b],
        // but the cache (if not maintained) still maps [a, b] → add_ab.
        let in0 = function.graph().node_input_id_at(add_ab, 0).unwrap();
        function.graph_mut().update_input(in0, c_value);

        // Re-create with the ORIGINAL key. Must NOT return add_ab — its
        // current inputs are [c, b], not [a, b].
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

    /// `update_input` where the new output equals the old output must leave
    /// the use count unchanged and keep the node input pointing at the same
    /// output.
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

    /// After `detach_node_inputs`, re-adding the same inputs must restore
    /// the use-list count to its original value.
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

        // Re-add; use count must be restored.
        function.graph_mut().add_node_input(sink, value);
        function.graph_mut().add_node_input(sink, value);
        assert_eq!(
            function.graph().value_uses(value).count(),
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

    /// `node_outputs_exact` must return `Err(WrongOutputCount)` when asked
    /// for a count that does not match the actual number of outputs.
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

    /// `node_inputs_exact` must return `Err(WrongInputCount)` when asked for
    /// a count that does not match the actual number of inputs.
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
        // Two consumers of cval to give the use-list real ordering.  Use
        // `Truncate` and `Neg` since `IntUnaryOp` has only the one variant
        // since `BitNot` was removed in favour of `Xor(_, all_ones)`.
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
        function.graph_mut().update_input(b_in0, cval); // self-redirect — should be a no-op

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
        // Out-of-bounds removal is an infallible no-op that reports `false`.
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

    // ── asm-fingerprint side-table tests ──────────────────────────────────────

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
        function.side_tables_mut().extend_asm_fingerprint(n, &[0x1000, 0x1004, 0x1008]);
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
        function.side_tables_mut().extend_asm_fingerprint(n, &[0x1004, 0x1000, 0x1004]);
        assert_eq!(
            function.side_tables().asm_fingerprint(n),
            FxHashSet::from_iter([0x1000, 0x1004])
        );
        // Extending with one new + two duplicates yields a deduplicated set.
        function.side_tables_mut().extend_asm_fingerprint(n, &[0x1008, 0x1000, 0x1004]);
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
        function.side_tables_mut().extend_asm_fingerprint(a, &[0x1000, 0x1004]);
        function.side_tables_mut().extend_asm_fingerprint(b, &[0x1004, 0x100C]);
        function.side_tables_mut().extend_asm_fingerprint_from(a, b);
        assert_eq!(
            function.side_tables().asm_fingerprint(a),
            FxHashSet::from_iter([0x1000, 0x1004, 0x100C])
        );
        // Source unaffected.
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
        function.side_tables_mut().extend_asm_fingerprint(n, &[0x1000, 0x1004, 0x1008]);
        // Extending with a strict subset must NOT remove any existing entries.
        function.side_tables_mut().extend_asm_fingerprint(n, &[0x1004]);
        assert_eq!(
            function.side_tables().asm_fingerprint(n),
            FxHashSet::from_iter([0x1000, 0x1004, 0x1008])
        );
        // Extending with the empty slice is a no-op.
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
        function.side_tables_mut().extend_asm_fingerprint(n, &[0x1000, 0x1004]);
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
        // No recorded descriptor: get_cc falls back to the (trivial) default CC,
        // which has no stack args.
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
        // The override differs from the trivial default, and get_cc returns it,
        // so its stack_args derive from the override.
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
        // The clobber output value is slot 2.
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
        // Two `create_node` calls for IntConst(7) hit the dedup cache — they
        // return the same NodeId.  Production code calls
        // `extend_asm_fingerprint(id, &[addr])` at every create_node site, so
        // both contributors end up unioned into the single side-table entry.
        let mut function = test_function();
        let a = function.graph_mut().create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(7_usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        function.side_tables_mut().extend_asm_fingerprint(a, &[0x2000]);
        let b = function.graph_mut().create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(7_usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        assert_eq!(a, b, "cacheable nodes should dedup");
        function.side_tables_mut().extend_asm_fingerprint(b, &[0x3000]);
        assert_eq!(
            function.side_tables().asm_fingerprint(a),
            FxHashSet::from_iter([0x2000, 0x3000])
        );
    }
}
