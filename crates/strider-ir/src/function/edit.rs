//! [`EditFunction`]: mutation verbs that keep the cached live/roots state current.

use entity_utils::{DenseEntitySet, Worklist};

use cranelift_entity::SecondaryMap;

use crate::DominatorTree;
use crate::builder::IRBuilder;
use crate::node::{NodeId, NodeKind, UseId, ValueId, ValueKind};
use crate::walk::{FnSuccs, PostOrder, def_use_succs, live_def_use_succs};
use crate::{Function, Graph, IRViewer};

bitflags::bitflags! {
    /// `OUTPUT_KILLED`: an output lost its last use, so the node may be dead.
    /// `NEEDS_RECANON`: its inputs changed, so it may be a structural twin.
    #[derive(Clone, Copy, Default)]
    pub(crate) struct NodeFlags: u8 {
        const OUTPUT_KILLED = 0b01;
        const NEEDS_RECANON = 0b10;
    }
}

/// An [`EditFunction`]'s live/roots bookkeeping.
pub(crate) struct FunctionState {
    /// Entry-reachable and not culled.
    live_nodes: DenseEntitySet<NodeId>,
    /// Input-less source nodes, iterated in ascending-`NodeId` order.
    roots: DenseEntitySet<NodeId>,
    /// Nodes whose liveness may have just dropped; drained by `clean`.
    queue: Worklist<NodeId>,
    flags: SecondaryMap<NodeId, NodeFlags>,
}

impl FunctionState {
    /// Seeds `live_nodes` and `roots` from the entry-reachable walk; culls nothing.
    pub(crate) fn populate(function: &Function) -> Self {
        let entry = function.entry();
        let info = crate::walk::GraphWalkInfo::compute_full(function.graph(), entry);
        let roots: DenseEntitySet<NodeId> = info.roots.into_iter().collect();
        Self {
            live_nodes: info.live_nodes,
            roots,
            queue: Worklist::new(),
            flags: SecondaryMap::new(),
        }
    }
}

/// A [`Function`] plus the live/roots bookkeeping the curated edit verbs keep
/// current. Edits that might orphan a producer enqueue it; `clean` drains the
/// queue. A side-effecting node dies only by `cull_dead` or `kill_node`.
pub struct EditFunction<'g> {
    pub(crate) function: &'g mut Function,
    state: FunctionState,
    generation: u64,
    /// Nodes created, resurrected or re-walked into the live set.
    live_entries: u64,
    /// The live post-order and the [`Self::edits`] reading it was walked at.
    order_memo: std::cell::RefCell<Option<(u64, Vec<NodeId>)>>,
    /// Edits that added or removed a use of a `Control` value, plus every
    /// [`Self::function_mut`] borrow.
    control_edits: u64,
    /// [`Self::control_dominator_tree`] and the `control_edits` reading it
    /// was built at.
    dom_memo: std::cell::RefCell<Option<(u64, DominatorTree<NodeId>)>>,
    /// Dominator trees built.
    #[cfg(any(test, feature = "test-util"))]
    dom_builds: std::cell::Cell<u64>,
    /// Def-use post-order walks performed.
    #[cfg(test)]
    postorder_walks: std::cell::Cell<u64>,
    /// Maybe-dead queue pops.
    #[cfg(test)]
    queue_pops: u64,
    /// Use edges [`Self::update_input`] and [`Self::replace_value`] redirected.
    #[cfg(any(test, feature = "test-util"))]
    uses_moved: u64,
}

impl<'g> EditFunction<'g> {
    /// Seeds the live set from an entry walk; culls nothing (see [`Self::cull_dead`]).
    pub fn new(function: &'g mut Function) -> Self {
        let state = FunctionState::populate(function);
        Self {
            function,
            state,
            generation: 0,
            live_entries: 0,
            order_memo: std::cell::RefCell::new(None),
            control_edits: 0,
            dom_memo: std::cell::RefCell::new(None),
            #[cfg(any(test, feature = "test-util"))]
            dom_builds: std::cell::Cell::new(0),
            #[cfg(test)]
            postorder_walks: std::cell::Cell::new(0),
            #[cfg(test)]
            queue_pops: 0,
            #[cfg(any(test, feature = "test-util"))]
            uses_moved: 0,
        }
    }

    /// Edits that moved or removed a use, appended an input, or killed a node;
    /// creating a node and [`Self::function_mut`] writes do not count.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Use edges [`Self::update_input`] and [`Self::replace_value`] redirected.
    #[cfg(any(test, feature = "test-util"))]
    pub fn uses_moved(&self) -> u64 {
        self.uses_moved
    }

    /// [`Self::generation`] plus every node entering the live set, so two equal
    /// readings bracket no change to the live graph, unless
    /// [`Self::function_mut`] wrote without a [`Self::resync_live_set`].
    pub fn edits(&self) -> u64 {
        self.generation + self.live_entries
    }

    /// [`crate::control_dominator_tree`], built once per control-graph edit
    /// count.
    pub fn control_dominator_tree(&self) -> std::cell::Ref<'_, DominatorTree<NodeId>> {
        let at = self.control_edits;
        if self
            .dom_memo
            .borrow()
            .as_ref()
            .is_none_or(|&(stamp, _)| stamp != at)
        {
            #[cfg(any(test, feature = "test-util"))]
            self.dom_builds.set(self.dom_builds.get() + 1);
            let tree = crate::control_dominator_tree(self.function);
            *self.dom_memo.borrow_mut() = Some((at, tree));
        }
        std::cell::Ref::map(self.dom_memo.borrow(), |memo| {
            &memo.as_ref().expect("filled above").1
        })
    }

    /// Dominator trees [`Self::control_dominator_tree`] has built.
    #[cfg(any(test, feature = "test-util"))]
    pub fn control_dominator_builds(&self) -> u64 {
        self.dom_builds.get()
    }

    /// Counts a use of `value` added or removed, if it is a `Control` edge.
    fn note_edge(&mut self, value: ValueId) {
        self.control_edits += u64::from(self.function.value_kind(value).is_control());
    }

    /// Kills every non-live node reachable def->use from a live root, so dead
    /// consumers of live producers are reached. A dead node whose whole input
    /// cone is dead stays until [`Function::compact`]. Idempotent.
    pub fn cull_dead(&mut self) {
        let graph = self.function.graph();
        let succs = FnSuccs::new(|n| def_use_succs(graph, n));
        let order: Vec<NodeId> = PostOrder::new(succs, self.state.roots.iter()).collect();
        for node in order {
            if !self.state.live_nodes.contains(node) {
                self.kill_node(node);
            }
        }
    }

    /// Recomputes live/roots from an entry walk, culls the rest and clears the
    /// queue. O(graph). Incremental bookkeeping tracks data orphaning only, so
    /// call this after a control edit detaches a subgraph.
    pub fn resync_live_set(&mut self) {
        self.live_entries += 1;
        self.state = FunctionState::populate(self.function);
        self.cull_dead();
    }

    pub fn function(&self) -> &Function {
        self.function
    }

    /// Bypasses the live/roots bookkeeping: after a structural edit, call
    /// [`Self::resync_live_set`] before reading an order. Rewriting a cacheable
    /// node's kind needs [`Graph::rebuild_cache`]. Counts as a control edit.
    pub fn function_mut(&mut self) -> &mut Function {
        self.control_edits += 1;
        self.function
    }

    /// Post-order over the live def->use graph from its roots: a node follows its
    /// consumers unless the use closes a cycle. Roots are seeded in ascending
    /// `NodeId` order, so the RPO starts at the lowest-id root. Walked once per
    /// [`Self::edits`] reading.
    #[cfg(test)]
    pub fn live_postorder(&self) -> Vec<NodeId> {
        self.with_live_postorder(<[NodeId]>::to_vec)
    }

    fn with_live_postorder<R>(&self, f: impl FnOnce(&[NodeId]) -> R) -> R {
        let at = self.edits();
        let mut memo = self.order_memo.borrow_mut();
        if memo.as_ref().is_none_or(|&(stamp, _)| stamp != at) {
            #[cfg(test)]
            self.postorder_walks.set(self.postorder_walks.get() + 1);
            let (graph, live) = (self.function.graph(), &self.state.live_nodes);
            let succs = FnSuccs::new(|n| live_def_use_succs(graph, live, n));
            let order: Vec<NodeId> = PostOrder::new(succs, self.state.roots.iter()).collect();
            *memo = Some((at, order));
        }
        f(&memo.as_ref().expect("filled above").1)
    }

    /// Reverse post-order over the live def->use graph from its roots: a node
    /// precedes its consumers unless the use closes a cycle. Starts at the
    /// lowest-id root.
    pub fn live_reverse_postorder(&self) -> Vec<NodeId> {
        self.with_live_postorder(|o| o.iter().rev().copied().collect())
    }

    /// [`Self::live_reverse_postorder`] filtered by kind.
    pub fn live_reverse_postorder_filter<'a>(
        &'a self,
        pred: impl Fn(&NodeKind) -> bool + 'a,
    ) -> impl Iterator<Item = NodeId> + 'a {
        let kept: Vec<NodeId> = self.with_live_postorder(|o| {
            o.iter()
                .rev()
                .copied()
                .filter(|&n| pred(self.function.node_kind(n)))
                .collect()
        });
        kept.into_iter()
    }

    pub fn graph_ref(&self) -> &Graph {
        self.function.graph()
    }

    pub fn entry(&self) -> NodeId {
        self.function.entry()
    }

    pub fn is_live(&self, node: NodeId) -> bool {
        self.state.live_nodes.contains(node)
    }

    #[cfg(any(test, feature = "test-util"))]
    pub fn is_root(&self, node: NodeId) -> bool {
        self.state.roots.contains(node)
    }

    #[cfg(any(test, feature = "test-util"))]
    pub fn live_snapshot(&self) -> DenseEntitySet<NodeId> {
        self.state.live_nodes.clone()
    }

    #[cfg(any(test, feature = "test-util"))]
    pub fn roots_snapshot(&self) -> DenseEntitySet<NodeId> {
        self.state.roots.clone()
    }

    /// Call before rewiring: enqueues the producer if this is the value's last use.
    fn will_detach_value(&mut self, value: ValueId) {
        // Only the use being detached.
        if self.value_uses(value).nth(1).is_none() {
            let def = self.function.producer(value);
            self.enqueue_killed_def_node(def);
        }
    }

    /// `value` is about to gain a use: marks its producer's input cone live
    /// (input-less ones as roots). Control-flow producers are never
    /// resurrected.
    fn will_attach_value(&mut self, value: ValueId) {
        let producer = self.function.producer(value);
        if self.state.live_nodes.contains(producer) {
            return;
        }
        // `live_nodes` is the seen set.
        let mut worklist = std::collections::VecDeque::from([producer]);
        while let Some(node) = worklist.pop_front() {
            // Only control nodes stop here: a Store reached must come live, or
            // `cull_dead` would kill it in use.
            if self.function.node_kind(node).has_control_flow() {
                continue;
            }
            if !self.state.live_nodes.insert(node) {
                continue;
            }
            self.live_entries += 1;
            // A structural twin may have been minted while this node was dead.
            if self.function.node_kind(node).is_cacheable() {
                self.enqueue_for_recanon(node);
            }
            let inputs: smallvec::SmallVec<[ValueId; 4]> =
                self.function.node_inputs(node).into_iter().collect();
            if inputs.is_empty() {
                self.state.roots.insert(node);
            }
            for input in inputs {
                let def = self.function.producer(input);
                if !self.state.live_nodes.contains(def) {
                    worklist.push_back(def);
                }
            }
        }
    }

    /// Enqueues `def` as maybe-dead unless it is side-effecting.
    fn enqueue_killed_def_node(&mut self, def: NodeId) {
        if self.function.node_kind(def).has_side_effects() {
            return;
        }
        self.state.flags[def].insert(NodeFlags::OUTPUT_KILLED);
        self.enqueue(def);
    }

    /// After a detach: enqueues the producer of every value now unused.
    fn enqueue_orphaned_producers(&mut self, values: impl IntoIterator<Item = ValueId>) {
        for value in values {
            if self.value_uses(value).next().is_none() {
                let producer = self.function.producer(value);
                self.enqueue_killed_def_node(producer);
            }
        }
    }

    fn enqueue(&mut self, node: NodeId) {
        if self.state.live_nodes.contains(node) {
            self.state.queue.enqueue(node);
        }
    }

    /// Flags `node` as possibly a structural twin of an existing node.
    fn enqueue_for_recanon(&mut self, node: NodeId) {
        if self.state.live_nodes.contains(node) {
            self.state.flags[node].insert(NodeFlags::NEEDS_RECANON);
            self.enqueue(node);
        }
    }

    /// Merges `node` into its structural twin, or re-enters it into the cache.
    fn canonicalize_node(&mut self, node: NodeId) {
        if let Some(twin) = self.function.graph_mut().canonicalize_node(node) {
            // Every cacheable kind has one output (not always a `Val`).
            let [node_out] = self
                .function
                .node_outputs_exact::<1>(node)
                .expect("a cacheable node flagged for re-canon is single-output");
            let [twin_out] = self
                .function
                .node_outputs_exact::<1>(twin)
                .expect("a cacheable twin is single-output");
            self.replace_value(node_out, twin_out);
        }
    }

    /// Skips nodes that are no longer live.
    fn dequeue(&mut self) -> Option<NodeId> {
        while let Some(node) = self.state.queue.dequeue() {
            #[cfg(test)]
            {
                self.queue_pops += 1;
            }
            if self.state.live_nodes.contains(node) {
                return Some(node);
            }
        }
        None
    }

    /// Non-side-effecting with every output unused.
    fn is_node_dead(&self, node: NodeId) -> bool {
        if self.function.node_kind(node).has_side_effects() {
            return false;
        }
        self.function
            .node_outputs(node)
            .iter()
            .all(|&out| self.value_uses(out).next().is_none())
    }

    /// Detaches `node`'s inputs and evicts it from the live set, roots and dedup
    /// cache, side-effecting or not. Its outputs keep their `ValueId`s, but the
    /// node is invalid; attaching one revives it and `validate` reports its
    /// arity. Operand deadness is checked after the detach, so a value in
    /// several slots (`Add(k, k)`) is still enqueued.
    pub fn kill_node(&mut self, node: NodeId) {
        let inputs: smallvec::SmallVec<[ValueId; 4]> =
            self.function.node_inputs(node).into_iter().collect();
        for &input in &inputs {
            self.note_edge(input);
        }
        self.function.graph_mut().detach_node_inputs(node);
        self.state.live_nodes.remove(node);
        self.state.roots.remove(node);
        self.state.flags[node] = NodeFlags::empty();
        self.generation += 1;
        self.enqueue_orphaned_producers(inputs);
    }

    /// Drains the maybe-dead queue to a fixed point: kills each dead node,
    /// enqueuing its orphaned operands, and merges each mutated node into its
    /// structural twin.
    pub fn clean(&mut self) {
        while let Some(node) = self.dequeue() {
            let flags = std::mem::take(&mut self.state.flags[node]);
            // Deadness first: a dead node is killed, not canonicalized.
            if flags.contains(NodeFlags::OUTPUT_KILLED) && self.is_node_dead(node) {
                self.kill_node(node);
                continue;
            }
            if flags.contains(NodeFlags::NEEDS_RECANON) {
                self.canonicalize_node(node);
            }
        }
    }

    /// Cached live nodes matching `pred`, in `live_nodes` order.
    pub fn live_of_kind<'a>(
        &'a self,
        pred: impl Fn(&NodeKind) -> bool + 'a,
    ) -> impl Iterator<Item = NodeId> + 'a {
        self.state
            .live_nodes
            .iter()
            .filter(move |&n| pred(self.function.node_kind(n)))
    }

    pub fn create_node(
        &mut self,
        kind: NodeKind,
        inputs: impl IntoIterator<Item = ValueId>,
        output_kinds: impl IntoIterator<Item = ValueKind>,
    ) -> NodeId {
        self.create_node_attributed(kind, inputs, output_kinds, &[])
    }

    /// Creates (or dedups to) the node, absorbing every contributor's
    /// asm-fingerprint.
    pub fn create_node_attributed(
        &mut self,
        kind: NodeKind,
        inputs: impl IntoIterator<Item = ValueId>,
        output_kinds: impl IntoIterator<Item = ValueKind>,
        contributors: &[NodeId],
    ) -> NodeId {
        let inputs: smallvec::SmallVec<[ValueId; 4]> = inputs.into_iter().collect();
        for &input in &inputs {
            self.will_attach_value(input);
            self.note_edge(input);
        }
        let node = self
            .function
            .create_node_attributed(kind, inputs, output_kinds, contributors);
        // A dedup onto a live node changes nothing.
        self.live_entries += u64::from(self.state.live_nodes.insert(node));
        if self.node_inputs(node).is_empty() {
            self.state.roots.insert(node);
        }
        node
    }

    /// Points the input edge `input_id` at `output_id`.
    pub fn update_input(&mut self, input_id: UseId, output_id: ValueId) {
        let displaced = self.function.graph().value_of_use(input_id);
        if displaced == output_id {
            return;
        }
        let consumer = self.function.graph().node_of_use(input_id);
        self.will_detach_value(displaced);
        self.will_attach_value(output_id);
        self.note_edge(displaced);
        self.note_edge(output_id);
        self.function.graph_mut().update_input(input_id, output_id);
        #[cfg(any(test, feature = "test-util"))]
        {
            self.uses_moved += 1;
        }
        self.enqueue_for_recanon(consumer);
        self.generation += 1;
    }

    /// Appends an input to a **non-cacheable** node.
    pub fn add_node_input(&mut self, node: NodeId, output_id: ValueId) {
        debug_assert!(
            !self.node_kind(node).is_cacheable(),
            "{node:?} is cacheable"
        );
        let was_input_less = self.node_inputs(node).is_empty();
        self.will_attach_value(output_id);
        self.note_edge(output_id);
        self.function.graph_mut().add_node_input(node, output_id);
        if was_input_less {
            self.state.roots.remove(node);
        }
        self.generation += 1;
    }

    /// Keeps the first `len` inputs of a **non-cacheable** node.
    pub fn truncate_node_inputs(&mut self, node: NodeId, len: usize) {
        debug_assert!(
            !self.node_kind(node).is_cacheable(),
            "{node:?} is cacheable"
        );
        let present = self.function.node_inputs(node).len();
        if present <= len {
            return;
        }
        let indices: smallvec::SmallVec<[u32; 8]> = (len as u32..present as u32).collect();
        self.remove_node_inputs_batch(node, &indices);
    }

    pub fn register_arg_value(&mut self, index: u32, value: ValueId) {
        self.function
            .side_tables_mut()
            .register_arg_value(index, value);
    }

    /// Drops the integer-class argument carriers at `first_index` and above,
    /// keeping the register-passed ones below it.
    pub fn clear_arg_values_from(&mut self, first_index: u32) {
        self.function
            .side_tables_mut()
            .clear_arg_values_from(first_index);
    }

    /// Links `from`'s asm-fingerprint under `into`'s, a live alias once
    /// `from`'s is non-empty.
    pub fn absorb_node_fingerprint(&mut self, into: NodeId, from: NodeId) {
        self.function
            .side_tables_mut()
            .extend_asm_fingerprint_from(into, from);
    }

    /// [`Self::absorb_node_fingerprint`] over the two values' producers.
    pub fn absorb_fingerprint(&mut self, into_value: ValueId, from_value: ValueId) {
        let into = self.function.producer(into_value);
        let from = self.function.producer(from_value);
        self.absorb_node_fingerprint(into, from);
    }

    /// Redirects every use of `old` to `new`. Absorbs `old`'s producer
    /// asm-fingerprint even when no use moved. Returns `true` iff a use moved.
    pub fn replace_value(&mut self, old: ValueId, new: ValueId) -> bool {
        let from = self.function.producer(old);
        self.absorb_fingerprint(new, old);
        // The raw redirect bypasses `update_input`'s hooks.
        let consumers: smallvec::SmallVec<[NodeId; 4]> = if old != new {
            self.function
                .graph()
                .value_uses(old)
                .map(|(consumer, _)| consumer)
                .collect()
        } else {
            smallvec::SmallVec::new()
        };
        let changed = !consumers.is_empty();
        if changed {
            self.will_attach_value(new);
            self.function.graph_mut().replace_all_uses(old, new);
            self.note_edge(old);
            self.generation += 1;
        }
        #[cfg(any(test, feature = "test-util"))]
        {
            self.uses_moved += consumers.len() as u64;
        }
        for consumer in consumers {
            self.enqueue_for_recanon(consumer);
        }
        self.enqueue_killed_def_node(from);
        changed
    }

    /// [`Self::update_input`], absorbing the displaced producer's
    /// asm-fingerprint iff the displaced value is left unused.
    pub fn redirect_input(&mut self, input_id: UseId, new: ValueId) {
        let old_value = self.graph_ref().value_of_use(input_id);
        let only_use = self.value_has_one_use(old_value);
        self.update_input(input_id, new);
        if only_use {
            self.absorb_fingerprint(new, old_value);
        }
    }

    /// Removes predecessor slots from a `Region` and the matching slots from
    /// each of its `Phi`/`MemPhi`s. `pred_indices` may be unsorted and repeat;
    /// pass every dead index in one call.
    pub fn remove_region_predecessors(&mut self, region: NodeId, pred_indices: &[u32]) {
        debug_assert!(
            matches!(self.node_kind(region), NodeKind::Region),
            "remove_region_predecessors: node is not a Region",
        );
        let phi_nodes: Vec<NodeId> = self
            .node_outputs(region)
            .get(NodeKind::REGION_PHI_OUT_SLOT)
            .map_or_else(Vec::new, |&phi_value| {
                self.graph_ref()
                    .value_uses(phi_value)
                    .map(|(n, _)| n)
                    .collect()
            });

        // Phi slot 0 is the region token.
        let phi_idxs: Vec<u32> = pred_indices.iter().map(|&i| i + 1).collect();
        for &phi in &phi_nodes {
            self.remove_node_inputs_batch(phi, &phi_idxs);
        }
        self.remove_node_inputs_batch(region, pred_indices);
    }

    /// [`Graph::remove_node_inputs_batch`] for a **non-cacheable** node.
    /// Out-of-range and duplicate indices are ignored.
    fn remove_node_inputs_batch(&mut self, node: NodeId, indices: &[u32]) {
        let inputs: smallvec::SmallVec<[ValueId; 8]> =
            self.function.node_inputs(node).into_iter().collect();
        let displaced: smallvec::SmallVec<[ValueId; 8]> = indices
            .iter()
            .filter_map(|&i| inputs.get(i as usize).copied())
            .collect();
        for &value in &displaced {
            self.note_edge(value);
        }
        self.function
            .graph_mut()
            .remove_node_inputs_batch(node, indices.iter().map(|&i| i as usize));
        self.generation += u64::from(!displaced.is_empty());
        // After the removal, as in `kill_node`.
        self.enqueue_orphaned_producers(displaced);
    }
}

impl IRBuilder for EditFunction<'_> {
    fn function_mut(&mut self) -> &mut crate::Function {
        EditFunction::function_mut(self)
    }

    fn intern_int_const(
        &mut self,
        val: impl Into<crate::UBig>,
        ty: crate::node::ValueType,
    ) -> crate::node::IntConstId {
        self.function.intern_int_const(val, ty)
    }

    fn create_node_attributed<I, O>(
        &mut self,
        kind: NodeKind,
        inputs: I,
        outputs: O,
        contributors: &[NodeId],
    ) -> NodeId
    where
        I: IntoIterator<Item = ValueId>,
        O: IntoIterator<Item = ValueKind>,
    {
        EditFunction::create_node_attributed(self, kind, inputs, outputs, contributors)
    }
}

#[cfg(test)]
pub(crate) mod test_fixtures {
    //! Fixture builders for the `edit` unit tests. `strider_ir_test_utils`
    //! links a separate compilation of strider-ir, so its `Function` is a
    //! different type here.

    use crate::FunctionBuilder;
    use strider_ir_test_utils::SENTINEL_LIFT_ADDR;

    /// Trivial-convention builder with a single entry region, lift-addr
    /// pre-stamped.
    pub(crate) fn single_region_builder() -> FunctionBuilder {
        let cc = strider_target::BuiltCallingConvention::default();
        let mut b = FunctionBuilder::new(Vec::new(), cc, strider_target::Endianness::Little);
        let region = b.create_region_all().expect("create_region");
        b.set_entry_region_all(region).expect("set_entry_region");
        b.set_region(region);
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        b
    }
}

#[cfg(test)]
mod tests {
    use super::EditFunction;
    use super::test_fixtures::single_region_builder;
    use crate::builder::IRBuilderExt;
    use crate::node::{NodeKind, ValueKind, ValueType};
    use crate::{IRViewer, IntBinaryOp};
    use cranelift_entity::EntityRef;
    use std::collections::BTreeSet;

    #[test]
    fn create_then_kill_tracks_liveness() {
        let mut b = single_region_builder();
        let root = b.build_int_const(1u64, ValueType::int(64)).unwrap();
        b.build_return(Some(root), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let mut ctx = EditFunction::new(&mut function);

        let node = ctx.create_node(
            NodeKind::IntConst(crate::node::IntConstId::new(42_usize)),
            [],
            [ValueKind::Typed(ValueType::int(64))],
        );
        assert!(ctx.is_live(node), "freshly created node is live");
        assert!(ctx.is_root(node), "input-less fresh const is a root");

        ctx.kill_node(node);
        assert!(!ctx.is_live(node), "killed node is no longer live");
        assert!(!ctx.is_root(node), "killed node dropped from roots");
    }

    /// Changing a cacheable node's arity would drop it from dedup.
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "is cacheable")]
    fn adding_an_input_to_a_cacheable_node_panics() {
        let mut b = single_region_builder();
        let one = b.build_int_const(1u64, ValueType::int(64)).unwrap();
        let neg = b
            .build_int_unary_operation(one, crate::IntUnaryOp::Neg, ValueType::int(64))
            .unwrap();
        b.build_return(Some(neg), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();
        let node = function.producer(neg);
        EditFunction::new(&mut function).add_node_input(node, one);
    }

    /// A use moved or a node killed advances the generation; creating a node,
    /// or redirecting an edge onto the value it already reads, does not.
    #[test]
    fn generation_counts_rewires_and_kills_but_not_creation() {
        let mut b = single_region_builder();
        let one = b.build_int_const(1u64, ValueType::int(64)).unwrap();
        let two = b.build_int_const(2u64, ValueType::int(64)).unwrap();
        let sum = b
            .build_int_binary_operation(one, two, IntBinaryOp::Add, ValueType::int(64))
            .unwrap();
        b.build_return(Some(sum), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();
        let add = function.producer(sum);

        let mut ctx = EditFunction::new(&mut function);
        let three = ctx.build_int_const(3u64, ValueType::int(64)).unwrap();
        assert_eq!(ctx.generation(), 0, "creating a node rewires nothing");

        let slot = ctx.function().node_input_id_at(add, 1).unwrap();
        ctx.update_input(slot, two);
        assert_eq!(ctx.generation(), 0, "a self-redirect moves nothing");
        ctx.update_input(slot, three);
        assert_eq!(ctx.generation(), 1);

        assert!(!ctx.replace_value(two, one));
        assert_eq!(ctx.generation(), 1, "a value with no uses moves nothing");

        ctx.kill_node(ctx.producer(two));
        assert_eq!(ctx.generation(), 2);
    }

    /// Data edits share one dominator tree; killing the `Return`, which drops
    /// its control input, and a `function_mut` borrow each rebuild it.
    #[test]
    fn the_dominator_tree_is_rebuilt_only_after_a_control_edit() {
        let mut b = single_region_builder();
        let one = b.build_int_const(1u64, ValueType::int(64)).unwrap();
        let two = b.build_int_const(2u64, ValueType::int(64)).unwrap();
        let sum = b
            .build_int_binary_operation(one, two, IntBinaryOp::Add, ValueType::int(64))
            .unwrap();
        b.build_return(Some(sum), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();
        let add = function.producer(sum);
        let ret = function.value_uses(sum).next().unwrap().0;

        let mut ctx = EditFunction::new(&mut function);
        drop(ctx.control_dominator_tree());
        let three = ctx.build_int_const(3u64, ValueType::int(64)).unwrap();
        let slot = ctx.function().node_input_id_at(add, 1).unwrap();
        ctx.update_input(slot, three);
        ctx.replace_value(sum, one);
        ctx.clean();
        assert!(ctx.control_dominator_tree().span(ret).is_some());
        assert_eq!(ctx.control_dominator_builds(), 1, "no control edge moved");

        let _ = ctx.function_mut();
        drop(ctx.control_dominator_tree());
        assert_eq!(ctx.control_dominator_builds(), 2, "an escape-hatch borrow");

        ctx.kill_node(ret);
        assert!(ctx.control_dominator_tree().span(ret).is_none());
        assert_eq!(ctx.control_dominator_builds(), 3, "the Return left the CFG");
    }

    /// `if (true) {} else {}` joining at `J`: the function, `J`'s node and its
    /// two predecessor edges.
    fn diamond() -> (
        crate::Function,
        crate::node::NodeId,
        [crate::node::ValueId; 2],
    ) {
        use crate::IRWalker;
        let mut b = single_region_builder();
        let (t, e, j) = (
            b.create_region_all().unwrap(),
            b.create_region_all().unwrap(),
            b.create_region_all().unwrap(),
        );
        let c = b.build_boolean_const(true);
        b.build_if(c, t, e).unwrap();
        for arm in [t, e] {
            b.set_region(arm);
            b.build_branch(j).unwrap();
        }
        b.set_region(j);
        b.build_return(None, &[]).unwrap();
        b.set_lift_addr(None);
        let function = b.build().unwrap();
        let join = function
            .walk_kind(|k| matches!(k, NodeKind::Region))
            .find(|&r| function.node_inputs(r).len() == 2)
            .expect("the join");
        let [a, b] = function.node_inputs_exact::<2>(join).unwrap();
        (function, join, [a, b])
    }

    /// Builds the tree, runs `edit`, builds it again, and asserts the second
    /// read rebuilt it.
    fn assert_rebuilds(
        edit: impl FnOnce(&mut EditFunction<'_>, crate::node::NodeId, [crate::node::ValueId; 2]),
    ) {
        let (mut function, join, preds) = diamond();
        let mut ctx = EditFunction::new(&mut function);
        drop(ctx.control_dominator_tree());
        edit(&mut ctx, join, preds);
        drop(ctx.control_dominator_tree());
        assert_eq!(ctx.control_dominator_builds(), 2);
    }

    /// Mutation: drop either `note_edge` in `update_input`.
    #[test]
    fn moving_a_control_edge_rebuilds_the_dominator_tree() {
        assert_rebuilds(|ctx, join, [_, b]| {
            let slot = ctx.function().node_input_id_at(join, 0).unwrap();
            ctx.update_input(slot, b);
        });
    }

    /// Mutation: drop `add_node_input`'s `note_edge`.
    #[test]
    fn adding_a_control_input_rebuilds_the_dominator_tree() {
        assert_rebuilds(|ctx, join, [a, _]| ctx.add_node_input(join, a));
    }

    /// Mutation: drop `create_node_attributed`'s `note_edge`.
    #[test]
    fn creating_a_control_consumer_rebuilds_the_dominator_tree() {
        assert_rebuilds(|ctx, _, [a, _]| {
            ctx.create_node(
                NodeKind::Region,
                [a],
                [ValueKind::Control, ValueKind::PhiToken],
            );
        });
    }

    /// Mutation: drop `remove_node_inputs_batch`'s `note_edge`.
    #[test]
    fn removing_a_predecessor_rebuilds_the_dominator_tree() {
        assert_rebuilds(|ctx, join, _| ctx.remove_region_predecessors(join, &[0]));
    }

    /// Mutation: drop `replace_value`'s `note_edge`.
    #[test]
    fn replacing_a_control_value_rebuilds_the_dominator_tree() {
        assert_rebuilds(|ctx, _, [a, b]| {
            ctx.replace_value(a, b);
        });
    }

    /// Mutation: leave the counter alone in the `IRBuilder` borrow.
    #[test]
    fn a_builder_borrow_rebuilds_the_dominator_tree() {
        assert_rebuilds(|ctx, _, _| {
            let _ = crate::IRBuilder::function_mut(ctx);
        });
    }

    /// A constant is interned without the structural escape hatch, so a
    /// rewrite building one keeps the tree.
    /// Mutation: intern through `function_mut`.
    #[test]
    fn building_a_constant_keeps_the_dominator_tree() {
        let (mut function, _, _) = diamond();
        let mut ctx = EditFunction::new(&mut function);
        drop(ctx.control_dominator_tree());
        ctx.build_int_const(7u64, ValueType::int(64)).unwrap();
        drop(ctx.control_dominator_tree());
        assert_eq!(ctx.control_dominator_builds(), 1);
    }

    /// Every read at one `edits()` reading shares one walk; an edit re-walks.
    #[test]
    fn postorder_is_walked_once_per_edits_reading() {
        let mut b = single_region_builder();
        let one = b.build_int_const(1u64, ValueType::int(64)).unwrap();
        let two = b.build_int_const(2u64, ValueType::int(64)).unwrap();
        let sum = b
            .build_int_binary_operation(one, two, IntBinaryOp::Add, ValueType::int(64))
            .unwrap();
        b.build_return(Some(sum), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();
        let add = function.producer(sum);

        let mut ctx = EditFunction::new(&mut function);
        let first = ctx.live_postorder();
        let second = ctx.live_postorder();
        assert_eq!(first, second);
        assert_eq!(
            ctx.postorder_walks.get(),
            1,
            "two reads with no edit between them must share one walk"
        );

        let three = ctx.build_int_const(3u64, ValueType::int(64)).unwrap();
        assert_eq!(
            ctx.postorder_walks.get(),
            1,
            "a fresh node is not read until the order is asked for again"
        );
        let after_create = ctx.live_postorder();
        assert_eq!(
            ctx.postorder_walks.get(),
            2,
            "a node entering the live set re-walks"
        );
        assert!(
            after_create.contains(&ctx.producer(three)),
            "the re-walk reaches the fresh node"
        );

        let slot = ctx.function().node_input_id_at(add, 1).unwrap();
        ctx.update_input(slot, three);
        ctx.live_postorder();
        assert_eq!(ctx.postorder_walks.get(), 3, "a moved use re-walks");
        let two_node = ctx.producer(two);
        ctx.clean();
        assert!(
            !ctx.live_postorder().contains(&two_node),
            "the re-walk after the orphan is culled drops it"
        );
        assert_eq!(ctx.postorder_walks.get(), 4, "a killed node re-walks");
    }

    /// Modelled with an off-spine data cone that no entry-reachable node
    /// consumes: created nodes are marked cache-live, but a fresh entry walk
    /// never reaches them.
    #[test]
    fn resync_live_set_drops_cache_stale_unreachable_nodes() {
        let mut b = single_region_builder();
        let root = b.build_int_const(1u64, ValueType::int(64)).unwrap();
        b.build_return(Some(root), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let mut ctx = EditFunction::new(&mut function);

        // An off-spine chain x -> y, consumed by nothing entry-reachable.
        let x = ctx.create_node(
            NodeKind::IntConst(crate::node::IntConstId::new(7_usize)),
            [],
            [ValueKind::Typed(ValueType::int(64))],
        );
        let xv = ctx.node_outputs(x)[0];
        let y = ctx.create_node(
            NodeKind::IntUnaryOp(crate::IntUnaryOp::Neg),
            [xv],
            [ValueKind::Typed(ValueType::int(64))],
        );
        assert!(
            ctx.is_live(x) && ctx.is_live(y),
            "freshly created off-spine nodes are cache-live"
        );

        ctx.resync_live_set();

        assert!(
            !ctx.is_live(y),
            "resync drops the unreachable consumer from the live set"
        );
        assert!(
            !ctx.is_live(x),
            "resync drops the unreachable producer from the live set"
        );
    }

    /// Resurrecting a dead cone that is a structural twin of a live cacheable
    /// node must schedule it for re-canonicalization.
    #[test]
    fn will_attach_value_resurrection_re_canonicalizes_twin() {
        let mut b = single_region_builder();
        b.set_lift_addr(Some(0xA));
        let x = b.build_int_const(11u64, ValueType::int(64)).unwrap();
        let y = b.build_int_const(22u64, ValueType::int(64)).unwrap();
        let z = b.build_int_const(33u64, ValueType::int(64)).unwrap();
        // A = Add(x, y): live (consumed by the Return), in the dedup cache.
        let a = b
            .build_int_binary_operation(x, y, IntBinaryOp::Add, ValueType::int(64))
            .unwrap();
        // C = Add(x, z): a different shape, left orphaned so it is dead at
        // populate time.
        b.set_lift_addr(Some(0xC));
        let c = b
            .build_int_binary_operation(x, z, IntBinaryOp::Add, ValueType::int(64))
            .unwrap();
        b.set_lift_addr(Some(0xA));
        b.build_return(Some(a), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let a_node = function.producer(a);
        let c_node = function.producer(c);
        let return_node = function
            .graph()
            .all_node_ids()
            .find(|&n| matches!(function.node_kind(n), NodeKind::Return))
            .expect("a Return node");

        let mut ctx = EditFunction::new(&mut function);
        assert!(!ctx.is_live(c_node), "C starts dead (orphaned)");
        assert!(ctx.is_live(a_node), "A starts live");

        // While C is still DEAD, rewire its slot-1 (z) -> y so it becomes
        // structurally Add(x, y) == A. (Dead nodes aren't enqueued here.)
        let c_use_z = ctx.function().node_input_id_at(c_node, 1).unwrap();
        ctx.update_input(c_use_z, y);

        // Resurrect C by appending its output as an extra Return value: the
        // attach marks C live AND enqueues it for re-canon.
        ctx.add_node_input(return_node, c);
        assert!(ctx.is_live(c_node), "C is resurrected by the attach");

        ctx.clean();

        assert!(
            !ctx.is_live(c_node),
            "the resurrected twin C must be merged into A and culled, not leaked"
        );
        assert!(ctx.is_live(a_node), "the survivor A stays live");
        assert!(
            ctx.function()
                .side_tables()
                .asm_fingerprint(a_node)
                .contains(&0xC),
            "A absorbs C's asm address 0xC on merge (superset contract), got {:?}",
            ctx.function().side_tables().asm_fingerprint(a_node)
        );
    }

    /// `canonicalize_node`'s merge path is `expect`-guarded; a well-formed twin
    /// merge must complete without panicking.
    #[test]
    fn canonicalize_node_merge_is_loud_on_broken_invariant() {
        let mut b = single_region_builder();
        b.set_lift_addr(Some(0x1));
        let x = b.build_int_const(7u64, ValueType::int(64)).unwrap();
        let y = b.build_int_const(8u64, ValueType::int(64)).unwrap();
        let z = b.build_int_const(9u64, ValueType::int(64)).unwrap();
        let a = b
            .build_int_binary_operation(x, y, IntBinaryOp::Add, ValueType::int(64))
            .unwrap();
        let c = b
            .build_int_binary_operation(x, z, IntBinaryOp::Add, ValueType::int(64))
            .unwrap();
        let top = b
            .build_int_binary_operation(a, c, IntBinaryOp::Add, ValueType::int(64))
            .unwrap();
        b.build_return(Some(top), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let a_node = function.producer(a);
        let c_node = function.producer(c);

        let mut ctx = EditFunction::new(&mut function);
        // Rewire C's slot-1 (z) -> y so C becomes structurally Add(x, y) == A;
        // the ensuing `clean()` drives `canonicalize_node`.
        let c_use_z = ctx.function().node_input_id_at(c_node, 1).unwrap();
        ctx.update_input(c_use_z, y);
        ctx.clean();

        assert!(!ctx.is_live(c_node), "the twin is merged + culled");
        assert!(ctx.is_live(a_node), "the survivor stays live");
    }

    /// A node rewired into a structural twin merges at `clean()`, and the
    /// survivor absorbs the duplicate's asm-fingerprint.
    #[test]
    fn clean_merges_a_mutated_twin_and_absorbs_fingerprint() {
        let mut b = single_region_builder();
        // A = Add(x, y) at addr 0xA; C = Add(x, z) at DISTINCT addr 0xC.
        b.set_lift_addr(Some(0xA));
        let x = b.build_int_const(1u64, ValueType::int(64)).unwrap();
        let y = b.build_int_const(2u64, ValueType::int(64)).unwrap();
        let z = b.build_int_const(3u64, ValueType::int(64)).unwrap();
        let a = b
            .build_int_binary_operation(x, y, IntBinaryOp::Add, ValueType::int(64))
            .unwrap();
        b.set_lift_addr(Some(0xC));
        let c = b
            .build_int_binary_operation(x, z, IntBinaryOp::Add, ValueType::int(64))
            .unwrap();
        // top = Add(a, c) keeps BOTH A and C live.
        b.set_lift_addr(Some(0xA));
        let top = b
            .build_int_binary_operation(a, c, IntBinaryOp::Add, ValueType::int(64))
            .unwrap();
        b.build_return(Some(top), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let a_node = function.producer(a);
        let c_node = function.producer(c);
        assert_ne!(a_node, c_node, "A and C start distinct");

        let mut ctx = EditFunction::new(&mut function);
        // Rewire C's slot 1 (z) -> y so C becomes structurally Add(x, y) == A.
        let c_use_z = ctx.function().node_input_id_at(c_node, 1).unwrap();
        ctx.update_input(c_use_z, y);
        ctx.clean();

        assert!(
            !ctx.is_live(c_node),
            "the duplicate C is culled after canonicalization"
        );
        assert!(ctx.is_live(a_node), "the survivor A stays live");
        assert!(
            ctx.function()
                .side_tables()
                .asm_fingerprint(a_node)
                .contains(&0xC),
            "A absorbs C's asm address 0xC (superset contract), got {:?}",
            ctx.function().side_tables().asm_fingerprint(a_node)
        );
    }

    /// The core self-cleaning invariant: after `replace_value` + `clean` the
    /// cached live set equals a fresh entry-reachable walk's.
    #[test]
    fn replace_value_then_clean_keeps_live_eq_reachable() {
        let mut b = single_region_builder();
        b.set_lift_addr(Some(0x10));
        let c1 = b.build_int_const(5u64, ValueType::int(64)).unwrap();
        let c2 = b.build_int_const(6u64, ValueType::int(64)).unwrap();
        let add = b
            .build_int_binary_operation(c1, c2, IntBinaryOp::Add, ValueType::int(64))
            .unwrap();
        b.build_return(Some(add), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let add_node = function.producer(add);

        let mut ctx = EditFunction::new(&mut function);
        ctx.cull_dead();

        let located = ctx
            .live_of_kind(|k| matches!(k, NodeKind::IntBinaryOp(IntBinaryOp::Add)))
            .next()
            .expect("the Add must be live");
        assert_eq!(located, add_node, "live_of_kind located the Add");

        ctx.replace_value(add, c1);
        ctx.clean();

        let entry = ctx.entry();
        let info = crate::walk::GraphWalkInfo::compute_full(ctx.function().graph(), entry);
        let fresh: BTreeSet<usize> = info.live_nodes.iter().map(|n| n.index()).collect();
        let cached: BTreeSet<usize> = ctx.live_snapshot().iter().map(|n| n.index()).collect();
        assert_eq!(
            cached, fresh,
            "cached live_nodes must equal the entry-reachable set after replace + clean"
        );

        assert!(!ctx.is_live(add_node), "replaced-away Add is culled");
        assert!(ctx.is_live(ctx.producer(c1)), "surviving c1 stays live");
    }

    /// Killing a cached node evicts its dedup-cache entry, so re-creating the
    /// same shape mints a FRESH node; the killed id is never resurrected.
    #[test]
    fn kill_cached_node_then_recreate_yields_fresh_live_node() {
        let mut b = single_region_builder();
        let root = b.build_int_const(1u64, ValueType::int(64)).unwrap();
        b.build_return(Some(root), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();
        let mut ctx = EditFunction::new(&mut function);

        let node = ctx.create_node(
            NodeKind::IntConst(crate::node::IntConstId::new(42_usize)),
            [],
            [ValueKind::Typed(ValueType::int(64))],
        );
        ctx.kill_node(node);
        assert!(!ctx.is_live(node));

        let recreated = ctx.create_node(
            NodeKind::IntConst(crate::node::IntConstId::new(42_usize)),
            [],
            [ValueKind::Typed(ValueType::int(64))],
        );
        assert_ne!(
            recreated, node,
            "the killed node's cache entry was evicted, so the same shape mints a fresh node"
        );
        assert!(ctx.is_live(recreated), "re-created node is live");
        assert!(
            ctx.is_root(recreated),
            "input-less re-created const is a root"
        );
    }

    #[test]
    fn cull_dead_twice_is_idempotent() {
        let mut b = single_region_builder();
        let root = b.build_int_const(5u64, ValueType::int(64)).unwrap();
        // A dead consumer of the live const: a Neg whose output nothing uses.
        let dead_neg = b
            .build_int_unary_operation(root, crate::node::IntUnaryOp::Neg, ValueType::int(64))
            .unwrap();
        b.build_return(Some(root), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();
        let dead_neg_node = function.producer(dead_neg);

        let mut ctx = EditFunction::new(&mut function);
        assert!(
            !ctx.is_live(dead_neg_node),
            "the unreachable Neg is excluded from the live set at populate"
        );

        ctx.cull_dead();
        assert!(
            ctx.function().node_inputs(dead_neg_node).is_empty(),
            "first cull detaches the dead consumer's inputs"
        );
        let live_after_first = ctx.live_snapshot();
        let roots_after_first = ctx.roots_snapshot();

        ctx.cull_dead();
        assert_eq!(
            ctx.live_snapshot().iter().collect::<Vec<_>>(),
            live_after_first.iter().collect::<Vec<_>>(),
            "second cull must not change the live set"
        );
        assert_eq!(
            ctx.roots_snapshot().iter().collect::<Vec<_>>(),
            roots_after_first.iter().collect::<Vec<_>>(),
            "second cull must not change the roots set"
        );
    }

    /// `replace_value` onto a value whose producer was dead at `new` time: the
    /// attach resurrects it and its transitive input cone into the cached
    /// live/roots state, and a later `clean` + `cull_dead` leaves it intact.
    #[test]
    fn replace_value_resurrects_previously_dead_producer() {
        let mut b = single_region_builder();
        let c1 = b.build_int_const(5u64, ValueType::int(64)).unwrap();
        let c2 = b.build_int_const(6u64, ValueType::int(64)).unwrap();
        // Orphan producer: a Neg nothing consumes (unreachable at populate).
        let orphan = b
            .build_int_unary_operation(c2, crate::node::IntUnaryOp::Neg, ValueType::int(64))
            .unwrap();
        b.build_return(Some(c1), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();
        let orphan_node = function.producer(orphan);
        let c2_node = function.producer(c2);

        let mut ctx = EditFunction::new(&mut function);
        assert!(
            !ctx.is_live(orphan_node),
            "orphan starts outside the live set"
        );
        assert!(
            !ctx.is_live(c2_node),
            "the orphan's operand starts dead too"
        );

        ctx.replace_value(c1, orphan);

        assert!(
            ctx.is_live(orphan_node),
            "attach resurrects the orphan producer"
        );
        assert!(ctx.is_live(c2_node), "…and its transitive input cone");
        assert!(
            ctx.is_root(c2_node),
            "the resurrected input-less const is a root"
        );
        assert!(
            !ctx.is_root(orphan_node),
            "the Neg has an input, so it is not a root"
        );
        assert!(
            ctx.live_postorder().contains(&orphan_node),
            "the cached postorder visits the resurrected node"
        );

        ctx.clean();
        ctx.cull_dead();
        assert!(
            ctx.is_live(orphan_node),
            "cull_dead keeps the resurrected node"
        );
        let entry = ctx.entry();
        let info = crate::walk::GraphWalkInfo::compute_full(ctx.function().graph(), entry);
        let fresh: BTreeSet<usize> = info.live_nodes.iter().map(|n| n.index()).collect();
        let cached: BTreeSet<usize> = ctx.live_snapshot().iter().map(|n| n.index()).collect();
        assert_eq!(
            cached, fresh,
            "cached live_nodes must equal the entry-reachable set after resurrect + clean + cull"
        );
    }

    /// `update_input` onto a dead producer with a multi-node cone
    /// (`Neg(Neg(k))`) resurrects the WHOLE cone, marking its leaf a root.
    #[test]
    fn update_input_resurrects_previously_dead_producer() {
        let mut b = single_region_builder();
        let c1 = b.build_int_const(5u64, ValueType::int(64)).unwrap();
        // A dead 3-node cone: Neg(Neg(k)), nothing consumes the outer Neg.
        let k = b.build_int_const(7u64, ValueType::int(64)).unwrap();
        let inner = b
            .build_int_unary_operation(k, crate::node::IntUnaryOp::Neg, ValueType::int(64))
            .unwrap();
        let outer = b
            .build_int_unary_operation(inner, crate::node::IntUnaryOp::Neg, ValueType::int(64))
            .unwrap();
        b.build_return(Some(c1), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();
        let k_node = function.producer(k);
        let inner_node = function.producer(inner);
        let outer_node = function.producer(outer);
        let return_node = function
            .graph()
            .all_node_ids()
            .find(|&n| matches!(function.node_kind(n), NodeKind::Return))
            .expect("a Return node");

        let mut ctx = EditFunction::new(&mut function);
        for n in [k_node, inner_node, outer_node] {
            assert!(!ctx.is_live(n), "the whole orphan cone starts dead");
        }

        let slot = ctx
            .function()
            .node_inputs(return_node)
            .into_iter()
            .position(|v| v == c1)
            .expect("Return consumes c1");
        let use_id = ctx.graph_ref().node_input_id_at(return_node, slot).unwrap();
        ctx.update_input(use_id, outer);

        for n in [k_node, inner_node, outer_node] {
            assert!(ctx.is_live(n), "the whole resurrected cone is live");
        }
        assert!(
            ctx.is_root(k_node),
            "the cone's input-less const becomes a root"
        );
        assert!(
            !ctx.is_root(inner_node),
            "inner Neg has an input, so it is not a root"
        );
        assert!(
            !ctx.is_root(outer_node),
            "outer Neg has an input, so it is not a root"
        );

        let post = ctx.live_postorder();
        for n in [k_node, inner_node, outer_node] {
            assert!(
                post.contains(&n),
                "cached postorder visits the resurrected cone"
            );
        }
    }

    /// `add_node_input` of a previously-dead producer's output: the appended
    /// use resurrects the producer and its input cone.
    #[test]
    fn add_node_input_resurrects_previously_dead_producer() {
        let mut b = single_region_builder();
        let c1 = b.build_int_const(5u64, ValueType::int(64)).unwrap();
        let c2 = b.build_int_const(6u64, ValueType::int(64)).unwrap();
        // Orphan producer: a Neg nothing consumes (unreachable at populate).
        let orphan = b
            .build_int_unary_operation(c2, crate::node::IntUnaryOp::Neg, ValueType::int(64))
            .unwrap();
        b.build_return(Some(c1), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();
        let orphan_node = function.producer(orphan);
        let c2_node = function.producer(c2);
        let return_node = function
            .graph()
            .all_node_ids()
            .find(|&n| matches!(function.node_kind(n), NodeKind::Return))
            .expect("a Return node");

        let mut ctx = EditFunction::new(&mut function);
        assert!(
            !ctx.is_live(orphan_node),
            "orphan starts outside the live set"
        );

        ctx.add_node_input(return_node, orphan);

        assert!(
            ctx.is_live(orphan_node),
            "attach resurrects the orphan producer"
        );
        assert!(ctx.is_live(c2_node), "…and its transitive input cone");
        assert!(
            ctx.is_root(c2_node),
            "the resurrected input-less const is a root"
        );
        assert!(
            ctx.live_postorder().contains(&orphan_node),
            "the cached postorder visits the resurrected node"
        );
    }

    /// Attaching an already-live producer is the fast path: one set lookup, no
    /// walk, cached state unchanged.
    #[test]
    fn attach_already_live_value_keeps_cached_state_unchanged() {
        let mut b = single_region_builder();
        b.set_lift_addr(Some(0x10));
        let c1 = b.build_int_const(5u64, ValueType::int(64)).unwrap();
        let c2 = b.build_int_const(6u64, ValueType::int(64)).unwrap();
        let add = b
            .build_int_binary_operation(c1, c2, IntBinaryOp::Add, ValueType::int(64))
            .unwrap();
        b.build_return(Some(add), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();
        let return_node = function
            .graph()
            .all_node_ids()
            .find(|&n| matches!(function.node_kind(n), NodeKind::Return))
            .expect("a Return node");

        let mut ctx = EditFunction::new(&mut function);
        let live_before = ctx.live_snapshot();
        let roots_before = ctx.roots_snapshot();

        let slot = ctx
            .function()
            .node_inputs(return_node)
            .into_iter()
            .position(|v| v == add)
            .expect("Return consumes the Add");
        let use_id = ctx.graph_ref().node_input_id_at(return_node, slot).unwrap();
        ctx.update_input(use_id, c1);

        assert_eq!(
            ctx.live_snapshot().iter().collect::<Vec<_>>(),
            live_before.iter().collect::<Vec<_>>(),
            "attaching an already-live value must not change the live set"
        );
        assert_eq!(
            ctx.roots_snapshot().iter().collect::<Vec<_>>(),
            roots_before.iter().collect::<Vec<_>>(),
            "attaching an already-live value must not change the roots set"
        );
    }

    /// The orphan consumes a LIVE value, so it is raw-reachable from a live
    /// root and `cull_dead`'s walk visits it.
    #[test]
    fn cull_dead_after_resurrect_keeps_node_and_validates() {
        let mut b = single_region_builder();
        let c1 = b.build_int_const(5u64, ValueType::int(64)).unwrap();
        // Orphan consuming the LIVE c1 (raw-reachable from the c1 root).
        let orphan = b
            .build_int_unary_operation(c1, crate::node::IntUnaryOp::Neg, ValueType::int(64))
            .unwrap();
        b.build_return(Some(c1), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();
        let orphan_node = function.producer(orphan);
        let return_node = function
            .graph()
            .all_node_ids()
            .find(|&n| matches!(function.node_kind(n), NodeKind::Return))
            .expect("a Return node");

        let mut ctx = EditFunction::new(&mut function);
        assert!(
            !ctx.is_live(orphan_node),
            "orphan starts outside the live set"
        );

        let slot = ctx
            .function()
            .node_inputs(return_node)
            .into_iter()
            .position(|v| v == c1)
            .expect("Return consumes c1");
        let use_id = ctx.graph_ref().node_input_id_at(return_node, slot).unwrap();
        ctx.update_input(use_id, orphan);
        assert!(
            ctx.is_live(orphan_node),
            "attach resurrects the orphan producer"
        );

        ctx.cull_dead();
        assert!(
            ctx.is_live(orphan_node),
            "cull_dead must not kill the resurrected node"
        );
        assert!(
            !ctx.function().node_inputs(orphan_node).is_empty(),
            "the resurrected node's inputs stay attached"
        );
        crate::validate::validate(ctx.function())
            .expect("graph validates after resurrect + cull_dead");
    }

    /// A value produced by a killed control node must not resurrect it.
    #[test]
    fn attach_output_of_killed_control_node_does_not_resurrect_it() {
        let mut b = single_region_builder();
        let t = b.create_region_all().unwrap();
        let f = b.create_region_all().unwrap();
        let cond = b.build_boolean_const(true);
        b.build_if(cond, t, f).unwrap();
        b.set_region(t);
        b.build_return(None, &[]).unwrap();
        b.set_region(f);
        b.build_return(None, &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let if_node = function
            .graph()
            .all_node_ids()
            .find(|&n| matches!(function.node_kind(n), NodeKind::If))
            .expect("an If node");
        // If outputs are [ctrl_true, ctrl_false].
        let ctrl_false = function.node_outputs(if_node)[1];
        let return_node = function
            .graph()
            .all_node_ids()
            .find(|&n| matches!(function.node_kind(n), NodeKind::Return))
            .expect("a Return node");

        let mut ctx = EditFunction::new(&mut function);
        ctx.kill_node(if_node);
        assert!(!ctx.is_live(if_node), "the killed If leaves the live set");

        // Attach the corpse's dangling control output to a live consumer.
        ctx.add_node_input(return_node, ctrl_false);

        assert!(
            !ctx.is_live(if_node),
            "a control corpse must not be resurrected by an attach"
        );
        assert!(
            !ctx.is_root(if_node),
            "the detached (0-input) corpse must not become a cached root"
        );
    }

    /// Resurrecting a `Load` whose data cone reaches a `Store` on the memory
    /// chain must mark that `Store` live too: `will_attach_value`'s exemption
    /// covers CONTROL corpses only.
    #[test]
    fn resurrect_load_marks_its_memory_store_live() {
        let mut b = single_region_builder();
        b.set_lift_addr(Some(0x10));
        let addr = b.build_int_const(0x1000u64, ValueType::int(64)).unwrap();
        let data = b.build_int_const(0x42u64, ValueType::int(64)).unwrap();
        let init_mem = b.entry_memory;
        // The live spine returns an unrelated const.
        let ret_val = b.build_int_const(1u64, ValueType::int(64)).unwrap();
        b.build_return(Some(ret_val), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        // A dead Store->Load chain hung off the live InitialMemory, consumed by
        // nothing on the live spine. Spliced in after the build, which
        // validates and rejects an unanchored Store on the memory chain.
        let store_node = function.graph_mut().create_node(
            NodeKind::Store(rsleigh::VnSpace::RAM),
            [init_mem, addr, data],
            [ValueKind::Memory],
        );
        let store_mem = function.node_outputs_exact::<1>(store_node).unwrap()[0];
        let load_node = function.graph_mut().create_node(
            NodeKind::Load(rsleigh::VnSpace::RAM),
            [store_mem, addr],
            [ValueKind::Typed(ValueType::int(64))],
        );
        let loaded = function.node_outputs_exact::<1>(load_node).unwrap()[0];
        for n in [store_node, load_node] {
            function
                .side_tables_mut()
                .extend_asm_fingerprint(n, &[0x10]);
        }

        let return_node = function
            .graph()
            .all_node_ids()
            .find(|&n| matches!(function.node_kind(n), NodeKind::Return))
            .expect("a Return node");

        // EditFunction::new does NOT cull, so the dead Store/Load chain is
        // present-but-not-live and still wired together.
        let mut ctx = EditFunction::new(&mut function);
        assert!(!ctx.is_live(load_node), "Load starts dead");
        assert!(!ctx.is_live(store_node), "Store starts dead");

        // Rewire the Return's value slot onto the dead Load's output; the Store
        // on its memory input must come live with it.
        let slot = ctx
            .function()
            .node_inputs(return_node)
            .into_iter()
            .position(|v| v == ret_val)
            .expect("Return consumes ret_val");
        let use_id = ctx.graph_ref().node_input_id_at(return_node, slot).unwrap();
        ctx.update_input(use_id, loaded);

        assert!(ctx.is_live(load_node), "the resurrected Load is live");
        assert!(
            ctx.is_live(store_node),
            "the Store on the resurrected Load's memory input must be live too"
        );

        ctx.cull_dead();
        assert!(
            ctx.is_live(store_node),
            "cull_dead must not kill the in-use memory Store"
        );
        // Read only by the Load, the write is still off the memory chain.
        let mem_slot = ctx.graph_ref().node_input_id_at(return_node, 1).unwrap();
        ctx.update_input(mem_slot, store_mem);
        crate::validate::validate(ctx.function())
            .expect("graph validates after resurrecting a Load over a memory Store");
    }

    /// Side-effecting (`Store`) and control (`Return`) nodes are never
    /// enqueued or culled, even when a maybe-dead drain is forced over them.
    #[test]
    fn clean_keeps_side_effect_node() {
        let mut b = single_region_builder();
        b.set_lift_addr(Some(0x10));
        let addr = b.build_int_const(0x1000u64, ValueType::int(64)).unwrap();
        let data = b.build_int_const(0x42u64, ValueType::int(64)).unwrap();
        b.build_store(addr, data, rsleigh::VnSpace::RAM).unwrap();
        let ret_val = b.build_int_const(1u64, ValueType::int(64)).unwrap();
        b.build_return(Some(ret_val), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let store_node = function
            .graph()
            .all_node_ids()
            .find(|&n| matches!(function.node_kind(n), NodeKind::Store(_)))
            .expect("a Store node");
        let return_node = function
            .graph()
            .all_node_ids()
            .find(|&n| matches!(function.node_kind(n), NodeKind::Return))
            .expect("a Return node");

        let mut ctx = EditFunction::new(&mut function);
        ctx.cull_dead();

        ctx.enqueue_killed_def_node(store_node);
        ctx.enqueue_killed_def_node(return_node);
        ctx.clean();

        assert!(
            ctx.is_live(store_node),
            "Store (side-effecting) never culled"
        );
        assert!(ctx.is_live(return_node), "Return (control) never culled");
    }

    /// Collapsing a wide fan-in in one per-node batch: survivors keep their
    /// original relative order in contiguous slots, and the Region pred `i` to
    /// Phi input `i + 1` correspondence is preserved.
    #[test]
    fn remove_region_predecessors_wide_fanin_is_linear() {
        use crate::node::{NodeKind, ValueKind, ValueType};

        let mut b = single_region_builder();
        b.build_return(None, &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let mut ctx = EditFunction::new(&mut function);

        // One Control input per predecessor, the entry region's control output
        // reused verbatim for each slot.
        let entry = ctx.entry();
        let entry_ctrl = ctx.function().node_outputs(entry)[0];
        const FANIN: usize = 8;
        let region = ctx.create_node(
            NodeKind::Region,
            std::iter::repeat_n(entry_ctrl, FANIN),
            [ValueKind::Control, ValueKind::PhiToken],
        );
        let phi_token = ctx.function().node_outputs(region)[1];

        // A Phi over the Region: [phi_token, v0, ..., v7].  Distinct constants
        // so survivors are identifiable by value after the removal.
        let mut phi_inputs = vec![phi_token];
        let mut consts = Vec::new();
        for i in 0..FANIN {
            let k = ctx.create_node(
                NodeKind::IntConst(crate::node::IntConstId::new((0xC0 + i as u64) as usize)),
                [],
                [ValueKind::Typed(ValueType::int(64))],
            );
            let v = ctx.function().node_outputs(k)[0];
            consts.push(v);
            phi_inputs.push(v);
        }
        let phi = ctx.create_node(
            NodeKind::Phi,
            phi_inputs,
            [ValueKind::Typed(ValueType::int(64))],
        );

        // Remove predecessors {1, 3, 6} in one batch (unsorted and with a
        // duplicate on purpose).
        ctx.remove_region_predecessors(region, &[3, 1, 6, 3]);

        assert_eq!(
            ctx.function().node_inputs(region).len(),
            FANIN - 3,
            "5 of 8 Region predecessors survive"
        );

        // phi_token + the value inputs for the surviving preds {0,2,4,5,7}.
        let surviving: Vec<_> = ctx.function().node_inputs(phi).into_iter().collect();
        let expected: Vec<_> = std::iter::once(phi_token)
            .chain([0usize, 2, 4, 5, 7].into_iter().map(|i| consts[i]))
            .collect();
        assert_eq!(
            surviving, expected,
            "surviving Phi value inputs keep original order, contiguous, token first"
        );
    }
    /// A self-replace moves no use, yet still queues the producer: an unused
    /// value replaced by itself is culled at the next `clean`.
    #[test]
    fn replace_value_onto_itself_moves_nothing_but_queues_the_producer() {
        let mut b = single_region_builder();
        let one = b.build_int_const(1u64, ValueType::int(64)).unwrap();
        b.build_return(Some(one), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let mut ctx = EditFunction::new(&mut function);
        let unused = ctx.build_int_const(9u64, ValueType::int(64)).unwrap();
        let unused_node = ctx.producer(unused);
        assert!(!ctx.replace_value(one, one), "a used value onto itself");
        assert!(
            !ctx.replace_value(unused, unused),
            "an unused value onto itself"
        );
        assert_eq!(ctx.generation(), 0);
        assert_eq!(ctx.uses_moved(), 0);
        ctx.clean();
        assert!(ctx.is_live(ctx.producer(one)));
        assert!(!ctx.is_live(unused_node), "the unused producer is culled");
    }

    /// A node killed while still queued, then resurrected, is popped once at
    /// its old position: the queue holds each node at most once.
    #[test]
    fn kill_then_resurrect_of_a_queued_node_pops_it_once() {
        let mut b = single_region_builder();
        let k = b.build_int_const(6u64, ValueType::int(64)).unwrap();
        let neg = b
            .build_int_unary_operation(k, crate::node::IntUnaryOp::Neg, ValueType::int(64))
            .unwrap();
        b.build_return(Some(neg), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();
        let (k_node, neg_node) = (function.producer(k), function.producer(neg));
        let ret = function
            .graph()
            .all_node_ids()
            .find(|&n| matches!(function.node_kind(n), NodeKind::Return))
            .expect("a Return node");
        let slot_index = function
            .node_inputs(ret)
            .iter()
            .position(|v| v == neg)
            .expect("Return reads the Neg");

        let mut ctx = EditFunction::new(&mut function);
        let zero = ctx.build_int_const(0u64, ValueType::int(64)).unwrap();
        let slot = ctx.function().node_input_id_at(ret, slot_index).unwrap();
        let before = ctx.queue_pops;
        // Queues the Neg (last use gone) and the Return (re-canon).
        ctx.update_input(slot, zero);
        // Kills the queued Neg and queues its operand.
        ctx.kill_node(neg_node);
        // Resurrects the Neg, which is still queued.
        ctx.add_node_input(ret, neg);
        ctx.clean();
        assert_eq!(ctx.queue_pops - before, 3, "Neg, Return, const: once each");
        assert!(ctx.is_live(neg_node));
        assert!(!ctx.is_live(k_node), "the orphaned operand is culled");
    }

    /// Orphaning the head of an `n`-long chain kills it top-down, each node's
    /// operand the next one dequeued. Counts queue pops, not time.
    #[test]
    fn clean_dead_chain_cost_is_linear() {
        fn kill_chain(n: usize) -> u64 {
            let mut b = single_region_builder();
            let three = b.build_int_const(3u64, ValueType::int(64)).unwrap();
            let mut x = b.build_int_const(1u64, ValueType::int(64)).unwrap();
            for _ in 0..n {
                x = b
                    .build_int_binary_operation(x, three, IntBinaryOp::Add, ValueType::int(64))
                    .unwrap();
            }
            b.build_return(Some(x), &[]).unwrap();
            b.set_lift_addr(None);
            let mut function = b.build().unwrap();
            let ret = function
                .graph()
                .all_node_ids()
                .find(|&n| matches!(function.node_kind(n), NodeKind::Return))
                .expect("a Return node");
            let slot_index = function
                .node_inputs(ret)
                .iter()
                .position(|v| v == x)
                .expect("Return reads the chain head");

            let mut ctx = EditFunction::new(&mut function);
            let zero = ctx.build_int_const(0u64, ValueType::int(64)).unwrap();
            let slot = ctx.function().node_input_id_at(ret, slot_index).unwrap();
            let before = ctx.queue_pops;
            ctx.update_input(slot, zero);
            ctx.clean();
            assert!(
                !ctx.is_live(ctx.function().producer(x)),
                "chain head culled"
            );
            ctx.queue_pops - before
        }
        let small = kill_chain(1_000);
        let large = kill_chain(8_000);
        assert!(small >= 1_000, "every chain node is dequeued ({small})");
        // Linear would be 8x; quadratic 64x.
        assert!(
            large < small * 9,
            "8x the chain cost {:.1}x the pops ({small} -> {large})",
            large as f64 / small as f64,
        );
    }
}

#[cfg(test)]
mod function_state_tests {
    use super::FunctionState;
    use super::test_fixtures::single_region_builder;
    use crate::builder::IRBuilderExt;
    use crate::node::NodeKind;
    use crate::{IRViewer, IntBinaryOp, ValueType};

    /// `roots` gets exactly the input-less reachable nodes (`Entry` + the two
    /// operand consts); a dangling unreachable const stays out of the live set.
    #[test]
    fn populate_seeds_roots_and_live_set() {
        let mut b = single_region_builder();

        b.set_lift_addr(Some(0x10));
        let k1 = b.build_int_const(7u64, ValueType::int(64)).unwrap();
        let k2 = b.build_int_const(11u64, ValueType::int(64)).unwrap();
        let sum = b
            .build_int_binary_operation(k1, k2, IntBinaryOp::Add, ValueType::int(64))
            .unwrap();
        b.build_return(Some(sum), &[]).unwrap();
        // Created but never wired into anything.
        let dangling = b.build_int_const(0xDEAD_u64, ValueType::int(64)).unwrap();
        b.set_lift_addr(None);
        let function = b.build().unwrap();

        let k1_node = function.producer(k1);
        let k2_node = function.producer(k2);
        let dangling_node = function.producer(dangling);
        let entry = function.entry();

        let mut state = FunctionState::populate(&function);

        for r in &state.roots {
            assert!(
                function.graph().node_inputs(r).is_empty(),
                "root {r:?} must be input-less"
            );
        }

        assert!(state.roots.contains(entry), "Entry must be a root");
        assert!(state.roots.contains(k1_node), "k1 const must be a root");
        assert!(state.roots.contains(k2_node), "k2 const must be a root");

        assert!(
            !state.live_nodes.contains(dangling_node),
            "dangling unreachable const must not be live"
        );
        // A distinct const node, not deduped with k1/k2.
        assert!(
            matches!(function.node_kind(dangling_node), NodeKind::IntConst(_)),
            "dangling node is an IntConst"
        );

        assert_eq!(state.queue.dequeue(), None, "queue starts empty");
    }
}
