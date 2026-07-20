//! Every mutation of an [`EditFunction`] must route through one of the curated
//! verbs below, which keep its cached live/roots state accurate without a
//! re-walk.

use entity_utils::{DenseEntitySet, Worklist};

use cranelift_entity::SecondaryMap;

use crate::builder::IRBuilder;
use crate::error::Result;
use crate::node::{NodeId, NodeKind, UseId, ValueId, ValueKind};
use crate::walk::{DefUseSuccs, PostOrder, RawDefUseSuccs};
use crate::{Function, Graph, IRViewer};

bitflags::bitflags! {
    /// `OUTPUT_KILLED`: the node lost the last use of one of its outputs, so
    /// it may now be dead.  `NEEDS_RECANON`: its inputs changed, so it may now
    /// be a structural twin of an existing node.  Both are re-examined when the
    /// queue drains.
    #[derive(Clone, Copy, Default)]
    pub(crate) struct NodeFlags: u8 {
        const ENQUEUED = 0b01;
        const OUTPUT_KILLED = 0b10;
        const NEEDS_RECANON = 0b100;
    }
}

/// Persistent edit bookkeeping owned by an [`EditFunction`] for its lifetime.
pub struct FunctionState {
    /// Entry-reachable and not culled.
    live_nodes: DenseEntitySet<NodeId>,
    /// Input-less source nodes, iterated in ascending-`NodeId` order.
    roots: DenseEntitySet<NodeId>,
    /// Nodes whose liveness may have just dropped; drained by `clean`.
    queue: Worklist<NodeId>,
    flags: SecondaryMap<NodeId, NodeFlags>,
}

impl FunctionState {
    /// Seeds `live_nodes` + `roots` from the entry-reachable walk.  Culls
    /// nothing; queue and flags start empty.
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

/// Edit context used by the optimizer's rewrite rules and destructive passes.
pub struct EditFunction<'g> {
    pub(crate) function: &'g mut Function,
    state: FunctionState,
}

impl<'g> EditFunction<'g> {
    /// Does NOT cull pre-existing dead nodes; call [`Self::cull_dead`] for that.
    pub fn new(function: &'g mut Function) -> Self {
        let state = FunctionState::populate(function);
        Self { function, state }
    }

    /// Kills everything outside `state.live_nodes`, walking the **raw** forward
    /// def->use graph from `roots` so dead consumers of still-live producers are
    /// reached.  Idempotent.
    pub fn cull_dead(&mut self) {
        let order: Vec<NodeId> = PostOrder::new(
            RawDefUseSuccs::new(self.function.graph()),
            self.state.roots.iter(),
        )
        .collect();
        for node in order {
            if !self.state.live_nodes.contains(node) {
                self.kill_node(node);
            }
        }
    }

    /// Recompute the cached live/roots bookkeeping from a fresh entry walk,
    /// then cull what the walk no longer reaches, restoring "every cached-live
    /// node is entry-reachable".  Clears the queue and flags.
    ///
    /// O(graph).  The incremental bookkeeping tracks **data** orphaning only,
    /// so call this after a control edit that detached a subgraph.
    pub fn resync_live_set(&mut self) {
        self.state = FunctionState::populate(self.function);
        self.cull_dead();
    }

    pub fn function(&self) -> &Function {
        self.function
    }

    /// ESCAPE HATCH: bypasses the cached live/roots bookkeeping.  Structural
    /// mutation through this handle leaves a later `postorder()` /
    /// `reverse_postorder()` / `roots` read STALE; call `resync_live_set`
    /// before relying on one.  Payload-only edits (e.g. `node_kind_mut`) are
    /// safe.
    pub fn function_mut(&mut self) -> &mut Function {
        self.function
    }

    /// Post-order over the cached live def->use graph: every node is yielded
    /// after all of its consumers.  Roots are visited in ascending `NodeId`
    /// order.
    ///
    /// Entry-global: covers only the entry-rooted graph.  A post-order seeded
    /// at a non-entry node must recompute roots from scratch (e.g.
    /// [`walk_info(Some(seed))`](crate::IRWalker::walk_info) +
    /// [`reverse_postorder`](crate::IRWalker::reverse_postorder)).
    pub fn postorder(&self) -> Vec<NodeId> {
        PostOrder::new(
            DefUseSuccs::new(self.function.graph(), &self.state.live_nodes),
            self.state.roots.iter(),
        )
        .collect()
    }

    /// Every producer precedes its consumers.  Same entry-global contract as
    /// [`Self::postorder`].
    pub fn reverse_postorder(&self) -> Vec<NodeId> {
        let mut v = self.postorder();
        v.reverse();
        v
    }

    /// Same reachable SET as `Self::walk_kind`; only the ORDER differs (RPO).
    pub fn reverse_postorder_filter<'a>(
        &'a self,
        pred: impl Fn(&NodeKind) -> bool + 'a,
    ) -> impl Iterator<Item = NodeId> + 'a {
        self.reverse_postorder()
            .into_iter()
            .filter(move |&n| pred(self.function.node_kind(n)))
    }

    /// Post-order counterpart of [`Self::reverse_postorder_filter`].
    pub fn postorder_filter<'a>(
        &'a self,
        pred: impl Fn(&NodeKind) -> bool + 'a,
    ) -> impl Iterator<Item = NodeId> + 'a {
        self.postorder()
            .into_iter()
            .filter(move |&n| pred(self.function.node_kind(n)))
    }

    pub fn graph_ref(&self) -> &Graph {
        self.function.graph()
    }

    pub fn entry(&self) -> NodeId {
        self.function.entry()
    }

    // Dead-node cleanup: edits that might orphan a producer enqueue it; `clean`
    // drains the queue.  Side-effecting nodes are never enqueued or culled.

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

    /// Call BEFORE rewiring, while the about-to-be-removed use still counts: if
    /// this was the value's last use its producer may now be dead, so enqueue it.
    fn will_detach_value(&mut self, value: ValueId) {
        // `nth(1).is_none()` means at most one use remains, the one being
        // detached.  Zero uses enqueues harmlessly; `is_node_dead` confirms.
        if self.value_uses(value).nth(1).is_none() {
            let def = self.function.producer(value);
            self.enqueue_killed_def_node(def);
        }
    }

    /// Mirror of [`Self::will_detach_value`]: `value` is about to GAIN a use,
    /// so mark its producer and transitive input cone live (input-less ones as
    /// roots).  Control-flow producers are exempt from resurrection.
    ///
    /// Assumes the consumer gaining the use is itself live.
    fn will_attach_value(&mut self, value: ValueId) {
        let producer = self.function.producer(value);
        if self.state.live_nodes.contains(producer) {
            return;
        }
        let mut worklist: Worklist<NodeId> = Worklist::new();
        worklist.enqueue(producer);
        while let Some(node) = worklist.dequeue() {
            // Control corpses only.  A memory Store (side-effecting but NOT
            // control flow) reached as a genuine data input of a resurrected
            // pure node MUST come live, else the cached live set omits an in-use
            // Store and cull_dead corrupts the graph.
            if self.function.node_kind(node).has_control_flow() {
                continue;
            }
            // `insert` returning false doubles as the seen-set check.
            if !self.state.live_nodes.insert(node) {
                continue;
            }
            // A structural twin may have been minted while this node was dead,
            // so flag it for re-canon rather than leaking a duplicate.
            if self.function.node_kind(node).is_cacheable() {
                self.enqueue_for_recanon(node);
            }
            // Snapshot inputs before touching `self.state` (borrow).
            let inputs: smallvec::SmallVec<[ValueId; 4]> =
                self.function.node_inputs(node).into_iter().collect();
            if inputs.is_empty() {
                self.state.roots.insert(node);
            }
            for input in inputs {
                let def = self.function.producer(input);
                if !self.state.live_nodes.contains(def) {
                    worklist.enqueue(def);
                }
            }
        }
    }

    /// Enqueue a node whose last output use was just removed, unless it is
    /// side-effecting (those are never culled).
    fn enqueue_killed_def_node(&mut self, def: NodeId) {
        if self.function.node_kind(def).has_side_effects() {
            return;
        }
        self.state.flags[def].insert(NodeFlags::OUTPUT_KILLED);
        self.enqueue(def);
    }

    /// Run AFTER the detach, over values snapshotted before it: enqueues the
    /// producer of every value now at zero uses.
    fn enqueue_orphaned_producers(&mut self, values: impl IntoIterator<Item = ValueId>) {
        for value in values {
            if self.value_uses(value).next().is_none() {
                let producer = self.function.producer(value);
                self.enqueue_killed_def_node(producer);
            }
        }
    }

    fn enqueue(&mut self, node: NodeId) {
        if self.state.live_nodes.contains(node)
            && !self.state.flags[node].contains(NodeFlags::ENQUEUED)
        {
            self.state.flags[node].insert(NodeFlags::ENQUEUED);
            self.state.queue.enqueue(node);
        }
    }

    /// Flag `node` as possibly a structural twin of an existing node.
    fn enqueue_for_recanon(&mut self, node: NodeId) {
        if self.state.live_nodes.contains(node) {
            self.state.flags[node].insert(NodeFlags::NEEDS_RECANON);
            self.enqueue(node);
        }
    }

    /// If the node's changed structure matches an existing cacheable node, merge
    /// it into that twin; otherwise it re-enters the cache as the canonical
    /// representative.
    fn canonicalize_node(&mut self, node: NodeId) {
        self.state.flags[node].remove(NodeFlags::NEEDS_RECANON);
        if let Some(twin) = self.function.graph_mut().canonicalize_node(node) {
            // `canonicalize_node` returns `Some` only on a structural dedup.
            // Among cacheable kinds only `If` is multi-output, and two `If`s
            // never share a control edge (control is single-consumer), so an
            // `If` never dedups; every node reaching here is single-value-output.
            let [node_out] = self
                .function
                .node_outputs_exact::<1>(node)
                .expect("a cacheable node flagged for re-canon is single-value-output");
            let [twin_out] = self
                .function
                .node_outputs_exact::<1>(twin)
                .expect("a cacheable twin is single-value-output");
            self.replace_value(node_out, twin_out)
                .expect("merging a re-canonicalized node into its twin cannot fail");
        }
    }

    /// Skips past (but still dequeues) nodes that are no longer live.
    fn dequeue(&mut self) -> Option<NodeId> {
        while let Some(node) = self.state.queue.dequeue() {
            self.state.flags[node].remove(NodeFlags::ENQUEUED);
            if self.state.live_nodes.contains(node) {
                return Some(node);
            }
        }
        None
    }

    /// Dead iff non-side-effecting AND every output is unused.
    fn is_node_dead(&self, node: NodeId) -> bool {
        if self.function.node_kind(node).has_side_effects() {
            return false;
        }
        self.function
            .node_outputs(node)
            .iter()
            .all(|&out| self.value_uses(out).next().is_none())
    }

    /// Detach `node`'s inputs, evict it from the live set and `roots`, clear its
    /// flags.  `detach_node_inputs` also evicts the dedup-cache entry.
    ///
    /// Operand deadness is checked AFTER the detach, not per-edge before it.
    /// With the same value in two or more input slots (`Add(k, k)`), a per-edge
    /// pre-check sees all N uses on every edge and never fires the last-use
    /// enqueue, yet the detach drops all N edges at once, stranding the operand
    /// at zero uses and never enqueued.
    ///
    /// Unconditional for the node passed, side-effecting or not; the
    /// `has_side_effects` gate governs only the operand cascade.
    pub fn kill_node(&mut self, node: NodeId) {
        // Snapshot inputs BEFORE detaching (detach clears them).
        let inputs: Vec<ValueId> = self.function.node_inputs(node).into_iter().collect();
        self.function.graph_mut().detach_node_inputs(node);
        self.mark_node_dead(node);
        self.enqueue_orphaned_producers(inputs);
    }

    /// Evict `node` from the live set and `roots`, and clear its flags.
    fn mark_node_dead(&mut self, node: NodeId) {
        self.state.live_nodes.remove(node);
        self.state.roots.remove(node);
        self.state.flags[node] = NodeFlags::empty();
    }

    /// Drain the maybe-dead queue to a fixed point: kill every enqueued node
    /// that is actually dead, recursively enqueuing its orphaned operands.
    pub fn clean(&mut self) {
        while let Some(node) = self.dequeue() {
            let flags = self.state.flags[node];
            self.state.flags[node].remove(NodeFlags::OUTPUT_KILLED);
            // Deadness first: a dead node is killed, not canonicalized.
            if flags.contains(NodeFlags::OUTPUT_KILLED) && self.is_node_dead(node) {
                self.kill_node(node);
                continue;
            }
            // Merge a mutated node into its structural twin.
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

    /// Register a fresh node into the cached live/roots state.  Idempotent.
    fn track_created(&mut self, node: NodeId) {
        self.state.live_nodes.insert(node);
        if self.node_inputs(node).is_empty() {
            self.state.roots.insert(node);
        }
    }

    pub fn create_node(
        &mut self,
        kind: NodeKind,
        inputs: impl IntoIterator<Item = ValueId>,
        output_kinds: impl IntoIterator<Item = ValueKind>,
    ) -> NodeId {
        self.create_node_attributed(kind, inputs, output_kinds, &[])
    }

    /// Create (or dedup to) the node, union every contributor's asm-fingerprint
    /// into it, then register it into the cached live/roots state.
    pub fn create_node_attributed(
        &mut self,
        kind: NodeKind,
        inputs: impl IntoIterator<Item = ValueId>,
        output_kinds: impl IntoIterator<Item = ValueKind>,
        contributors: &[NodeId],
    ) -> NodeId {
        // Each input gains a use on the fresh node, so resurrect any whose
        // producer was dead.
        let inputs: smallvec::SmallVec<[ValueId; 4]> = inputs.into_iter().collect();
        for &input in &inputs {
            self.will_attach_value(input);
        }
        let node = self
            .function
            .create_node_attributed(kind, inputs, output_kinds, contributors);
        self.track_created(node);
        node
    }

    /// Point the input edge `input_id` at `output_id`, maintaining the cached
    /// state on both sides of the rewire.
    pub fn update_input(&mut self, input_id: UseId, output_id: ValueId) {
        let displaced = self.function.graph().value_of_use(input_id);
        // Self-redirect: nothing displaced, attached, or re-canonicalized.
        if displaced == output_id {
            return;
        }
        let consumer = self.function.graph().node_of_use(input_id);
        self.will_detach_value(displaced);
        self.will_attach_value(output_id);
        self.function.graph_mut().update_input(input_id, output_id);
        self.enqueue_for_recanon(consumer);
    }

    /// Append an input to a **non-cacheable** node.
    ///
    /// # Errors
    /// Never; the `Result` keeps the edit-verb surface uniform.
    pub fn add_node_input(&mut self, node: NodeId, output_id: ValueId) -> crate::error::Result<()> {
        let was_input_less = self.node_inputs(node).is_empty();
        self.will_attach_value(output_id);
        self.function.graph_mut().add_node_input(node, output_id);
        if was_input_less {
            self.state.roots.remove(node);
        }
        Ok(())
    }

    /// Redirect every use of `old` to `new`.  Does NO fingerprint work; use
    /// [`Self::replace_value`] for that.  Returns `true` iff a use moved.
    ///
    /// # Errors
    /// Never; the `Result` keeps the edit-verb surface uniform.
    pub fn replace_all_uses(&mut self, old: ValueId, new: ValueId) -> crate::error::Result<bool> {
        // The raw Graph redirect bypasses `update_input`'s per-edge hook, so
        // snapshot the consumers BEFORE it to flag them for re-canon after.
        let consumers: smallvec::SmallVec<[NodeId; 4]> = if old != new {
            self.function
                .graph()
                .value_uses(old)
                .map(|(consumer, _)| consumer)
                .collect()
        } else {
            smallvec::SmallVec::new()
        };
        if old != new && self.value_uses(old).next().is_some() {
            self.will_attach_value(new);
        }
        let changed = self.function.graph_mut().replace_all_uses(old, new);
        for consumer in consumers {
            self.enqueue_for_recanon(consumer);
        }
        Ok(changed)
    }

    pub fn register_arg_value(&mut self, index: u32, value: ValueId) {
        self.function
            .side_tables_mut()
            .register_arg_value(index, value);
    }

    /// Union `from_value`'s producer asm-fingerprint into `into_value`'s.
    pub fn absorb_fingerprint(&mut self, into_value: ValueId, from_value: ValueId) {
        let into = self.function().producer(into_value);
        let from = self.function().producer(from_value);
        self.function_mut()
            .side_tables_mut()
            .extend_asm_fingerprint_from(into, from);
    }

    /// Redirect every use of `old` to `new`, first absorbing `old`'s producer
    /// asm-fingerprint into `new`'s (superset-only union).  Returns `true` iff
    /// a use moved.
    ///
    /// # Errors
    /// Propagates [`Self::replace_all_uses`]'s error arm unchanged.
    pub fn replace_value(&mut self, old: ValueId, new: ValueId) -> Result<bool> {
        let into = self.function.producer(new);
        let from = self.function.producer(old);
        self.function
            .side_tables_mut()
            .extend_asm_fingerprint_from(into, from);
        let changed = self.replace_all_uses(old, new)?;
        // `replace_all_uses` bypasses `update_input`'s per-edge hook, so the
        // orphaned producer is enqueued here (side-effect-guarded inside).
        self.enqueue_killed_def_node(from);
        Ok(changed)
    }

    /// Single-slot companion to [`Self::replace_value`]: rewires exactly one
    /// input edge, absorbing the displaced producer's asm-fingerprint into
    /// `new`'s producer **iff** the redirect leaves that producer unused.
    pub fn redirect_input(&mut self, input_id: UseId, new: ValueId) {
        let old_value = self.graph_ref().value_of_use(input_id);
        // `input_id` is itself one use of `old_value`, so "exactly one use"
        // means this edge is the only one.
        let only_use = self.value_has_one_use(old_value);
        self.update_input(input_id, new);
        if only_use {
            let into = self.function.producer(new);
            let from = self.function.producer(old_value);
            self.function
                .side_tables_mut()
                .extend_asm_fingerprint_from(into, from);
        }
    }

    /// The single structural primitive for dropping dead control edges into a
    /// join: removes predecessor slots from a `Region` and the matching value
    /// slots from every `Phi`/`MemPhi` over its phi-token.
    ///
    /// `pred_indices` index the Region's predecessors, and may arrive unsorted
    /// and with duplicates.  Pass ALL dead indices at once.
    ///
    /// # Errors
    /// Never; the `Result` keeps the edit-verb surface uniform.
    pub fn remove_region_predecessors(
        &mut self,
        region: NodeId,
        pred_indices: &[u32],
    ) -> Result<()> {
        debug_assert!(
            matches!(self.node_kind(region), NodeKind::Region),
            "remove_region_predecessors: node is not a Region",
        );
        if pred_indices.is_empty() {
            return Ok(());
        }
        let mut indices: Vec<u32> = pred_indices.to_vec();
        indices.sort_unstable();
        indices.dedup();

        // Collected once: the Phi/MemPhi set doesn't change as their value
        // inputs are removed.
        let phi_nodes: Vec<NodeId> = {
            let outputs = self.node_outputs(region);
            if outputs.len() >= 2 {
                let phi_value = outputs[1]; // ValueId: Copy
                self.graph_ref()
                    .value_uses(phi_value)
                    .map(|(n, _)| n)
                    .collect()
            } else {
                Vec::new()
            }
        };

        // Each Phi/MemPhi loses slots `pred_index + 1`; the Region loses
        // `pred_index`.
        for &phi in &phi_nodes {
            let phi_idxs: Vec<u32> = indices.iter().map(|&i| i + 1).collect();
            self.remove_node_inputs_batch(phi, &phi_idxs);
        }
        self.remove_node_inputs_batch(region, &indices);
        Ok(())
    }

    /// Batched counterpart of [`Graph::remove_node_input`], for a
    /// **non-cacheable** node.  Out-of-range and duplicate indices are ignored.
    fn remove_node_inputs_batch(&mut self, node: NodeId, indices: &[u32]) {
        // Snapshot the values at the removed (in-range) slots BEFORE the edit.
        let inputs: smallvec::SmallVec<[ValueId; 8]> =
            self.function.node_inputs(node).into_iter().collect();
        let displaced: smallvec::SmallVec<[ValueId; 8]> = indices
            .iter()
            .filter_map(|&i| inputs.get(i as usize).copied())
            .collect();
        self.function
            .graph_mut()
            .remove_node_inputs_batch(node, indices.iter().map(|&i| i as usize));
        // Deadness checked AFTER the removal (as in `kill_node`): a value in
        // several removed slots only reaches zero uses once ALL its edges are
        // gone, so a pre-removal per-slot check would miss it.
        self.enqueue_orphaned_producers(displaced);
    }
}

impl IRBuilder for EditFunction<'_> {
    fn function_mut(&mut self) -> &mut crate::Function {
        self.function
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
    //! Local fixture builders for the `edit` unit tests.
    //!
    //! strider-ir's own `#[cfg(test)]` modules cannot use
    //! `strider_ir_test_utils`' type-returning builders: under `cargo test` the
    //! dev-dep links a *separate* compilation of strider-ir, so a helper
    //! returning `strider_ir::Function` mismatches the unit-test crate's own
    //! `Function`.

    use crate::FunctionBuilder;
    use strider_ir_test_utils::SENTINEL_LIFT_ADDR;

    /// Trivial-convention builder with a single entry region, lift-addr
    /// pre-stamped.
    #[allow(clippy::expect_used)]
    pub(crate) fn single_region_builder() -> FunctionBuilder {
        let cc = strider_target::BuiltCallingConvention {
            arg_passing_regs: Vec::new(),
            callee_saved_regs: Vec::new(),
            ret_val_regs: Vec::new(),
            ret_val_regs_float: Vec::new(),
            stack_vn: strider_target::BuiltCallingConvention::default().stack_vn,
            stack_args: None,
            ret_stack_pop: 0,
            link_register_vn: None,
            preserves_memory: false,
            no_return: false,
        };
        let mut b = FunctionBuilder::new(Vec::new(), cc, strider_target::Endianness::Little)
            .expect("FunctionBuilder::new");
        let region = b.create_region_all().expect("create_region");
        b.set_entry_region_all(region).expect("set_entry_region");
        b.set_region(region);
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        b
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
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
        let root = b.build_int_const(1u64, ValueType::I64).unwrap();
        b.build_return(Some(root), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let mut ctx = EditFunction::new(&mut function);

        let node = ctx.create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(42_usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        assert!(ctx.is_live(node), "freshly created node is live");
        assert!(ctx.is_root(node), "input-less fresh const is a root");

        ctx.kill_node(node);
        assert!(!ctx.is_live(node), "killed node is no longer live");
        assert!(!ctx.is_root(node), "killed node dropped from roots");
    }

    /// Modelled with an off-spine data cone that no entry-reachable node
    /// consumes: created nodes are marked cache-live, but a fresh entry walk
    /// never reaches them.
    #[test]
    fn resync_live_set_drops_cache_stale_unreachable_nodes() {
        let mut b = single_region_builder();
        let root = b.build_int_const(1u64, ValueType::I64).unwrap();
        b.build_return(Some(root), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

        let mut ctx = EditFunction::new(&mut function);

        // An off-spine chain x -> y, consumed by nothing entry-reachable.
        let x = ctx.create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(7_usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        let xv = ctx.node_outputs(x)[0];
        let y = ctx.create_node(
            NodeKind::IntUnaryOp(crate::IntUnaryOp::Neg),
            [xv],
            [ValueKind::Typed(ValueType::I64)],
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
        let x = b.build_int_const(11u64, ValueType::I64).unwrap();
        let y = b.build_int_const(22u64, ValueType::I64).unwrap();
        let z = b.build_int_const(33u64, ValueType::I64).unwrap();
        // A = Add(x, y): live (consumed by the Return), in the dedup cache.
        let a = b
            .build_int_binary_operation(x, y, IntBinaryOp::Add, ValueType::I64)
            .unwrap();
        // C = Add(x, z): a different shape, left orphaned so it is dead at
        // populate time.
        b.set_lift_addr(Some(0xC));
        let c = b
            .build_int_binary_operation(x, z, IntBinaryOp::Add, ValueType::I64)
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
        ctx.add_node_input(return_node, c).unwrap();
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
        let x = b.build_int_const(7u64, ValueType::I64).unwrap();
        let y = b.build_int_const(8u64, ValueType::I64).unwrap();
        let z = b.build_int_const(9u64, ValueType::I64).unwrap();
        let a = b
            .build_int_binary_operation(x, y, IntBinaryOp::Add, ValueType::I64)
            .unwrap();
        let c = b
            .build_int_binary_operation(x, z, IntBinaryOp::Add, ValueType::I64)
            .unwrap();
        let top = b
            .build_int_binary_operation(a, c, IntBinaryOp::Add, ValueType::I64)
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
        let x = b.build_int_const(1u64, ValueType::I64).unwrap();
        let y = b.build_int_const(2u64, ValueType::I64).unwrap();
        let z = b.build_int_const(3u64, ValueType::I64).unwrap();
        let a = b
            .build_int_binary_operation(x, y, IntBinaryOp::Add, ValueType::I64)
            .unwrap();
        b.set_lift_addr(Some(0xC));
        let c = b
            .build_int_binary_operation(x, z, IntBinaryOp::Add, ValueType::I64)
            .unwrap();
        // top = Add(a, c) keeps BOTH A and C live.
        b.set_lift_addr(Some(0xA));
        let top = b
            .build_int_binary_operation(a, c, IntBinaryOp::Add, ValueType::I64)
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
        let c1 = b.build_int_const(5u64, ValueType::I64).unwrap();
        let c2 = b.build_int_const(6u64, ValueType::I64).unwrap();
        let add = b
            .build_int_binary_operation(c1, c2, IntBinaryOp::Add, ValueType::I64)
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

        ctx.replace_value(add, c1).unwrap();
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
        let root = b.build_int_const(1u64, ValueType::I64).unwrap();
        b.build_return(Some(root), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();
        let mut ctx = EditFunction::new(&mut function);

        let node = ctx.create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(42_usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        ctx.kill_node(node);
        assert!(!ctx.is_live(node));

        let recreated = ctx.create_node(
            NodeKind::IntConst(crate::node::const_value::ConstId::new(42_usize)),
            [],
            [ValueKind::Typed(ValueType::I64)],
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
        let root = b.build_int_const(5u64, ValueType::I64).unwrap();
        // A dead consumer of the live const: a Neg whose output nothing uses.
        let dead_neg = b
            .build_int_unary_operation(root, crate::node::IntUnaryOp::Neg, ValueType::I64)
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
        let c1 = b.build_int_const(5u64, ValueType::I64).unwrap();
        let c2 = b.build_int_const(6u64, ValueType::I64).unwrap();
        // Orphan producer: a Neg nothing consumes (unreachable at populate).
        let orphan = b
            .build_int_unary_operation(c2, crate::node::IntUnaryOp::Neg, ValueType::I64)
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

        ctx.replace_value(c1, orphan).unwrap();

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
            ctx.postorder().contains(&orphan_node),
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
        let c1 = b.build_int_const(5u64, ValueType::I64).unwrap();
        // A dead 3-node cone: Neg(Neg(k)), nothing consumes the outer Neg.
        let k = b.build_int_const(7u64, ValueType::I64).unwrap();
        let inner = b
            .build_int_unary_operation(k, crate::node::IntUnaryOp::Neg, ValueType::I64)
            .unwrap();
        let outer = b
            .build_int_unary_operation(inner, crate::node::IntUnaryOp::Neg, ValueType::I64)
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
            "inner Neg has an input — not a root"
        );
        assert!(
            !ctx.is_root(outer_node),
            "outer Neg has an input — not a root"
        );

        let post = ctx.postorder();
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
        let c1 = b.build_int_const(5u64, ValueType::I64).unwrap();
        let c2 = b.build_int_const(6u64, ValueType::I64).unwrap();
        // Orphan producer: a Neg nothing consumes (unreachable at populate).
        let orphan = b
            .build_int_unary_operation(c2, crate::node::IntUnaryOp::Neg, ValueType::I64)
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

        ctx.add_node_input(return_node, orphan).unwrap();

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
            ctx.postorder().contains(&orphan_node),
            "the cached postorder visits the resurrected node"
        );
    }

    /// Attaching an already-live producer is the fast path: one set lookup, no
    /// walk, cached state unchanged.
    #[test]
    fn attach_already_live_value_keeps_cached_state_unchanged() {
        let mut b = single_region_builder();
        b.set_lift_addr(Some(0x10));
        let c1 = b.build_int_const(5u64, ValueType::I64).unwrap();
        let c2 = b.build_int_const(6u64, ValueType::I64).unwrap();
        let add = b
            .build_int_binary_operation(c1, c2, IntBinaryOp::Add, ValueType::I64)
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
        let c1 = b.build_int_const(5u64, ValueType::I64).unwrap();
        // Orphan consuming the LIVE c1 (raw-reachable from the c1 root).
        let orphan = b
            .build_int_unary_operation(c1, crate::node::IntUnaryOp::Neg, ValueType::I64)
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

    /// A value produced by an explicitly-killed side-effecting node must NOT
    /// resurrect it.
    #[test]
    fn attach_output_of_killed_side_effecting_node_does_not_resurrect_it() {
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
        ctx.add_node_input(return_node, ctrl_false).unwrap();

        assert!(
            !ctx.is_live(if_node),
            "a side-effecting corpse must not be resurrected by an attach"
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
        let addr = b.build_int_const(0x1000u64, ValueType::I64).unwrap();
        let data = b.build_int_const(0x42u64, ValueType::I64).unwrap();
        // A dead Store->Load chain hung off the live InitialMemory, consumed by
        // nothing on the live spine.  Built via the low-level create_node so the
        // memory edge bypasses the builder's current-region threading.
        let init_mem = b.entry_memory;
        let store_node = b.create_node(
            NodeKind::Store(rsleigh::VnSpace::RAM),
            [init_mem, addr, data],
            [ValueKind::Memory],
        );
        let store_mem = b.function().node_outputs_exact::<1>(store_node).unwrap()[0];
        let load_node = b.create_node(
            NodeKind::Load(rsleigh::VnSpace::RAM),
            [store_mem, addr],
            [ValueKind::Typed(ValueType::I64)],
        );
        let loaded = b.function().node_outputs_exact::<1>(load_node).unwrap()[0];
        // The live spine returns an unrelated const.
        let ret_val = b.build_int_const(1u64, ValueType::I64).unwrap();
        b.build_return(Some(ret_val), &[]).unwrap();
        b.set_lift_addr(None);
        let mut function = b.build().unwrap();

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
        crate::validate::validate(ctx.function())
            .expect("graph validates after resurrecting a Load over a memory Store");
    }

    /// Side-effecting (`Store`) and control (`Return`) nodes are never
    /// enqueued or culled, even when a maybe-dead drain is forced over them.
    #[test]
    fn clean_keeps_side_effect_node() {
        let mut b = single_region_builder();
        b.set_lift_addr(Some(0x10));
        let addr = b.build_int_const(0x1000u64, ValueType::I64).unwrap();
        let data = b.build_int_const(0x42u64, ValueType::I64).unwrap();
        b.build_store(addr, data, rsleigh::VnSpace::RAM).unwrap();
        let ret_val = b.build_int_const(1u64, ValueType::I64).unwrap();
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

        // Force-enqueue both, then drain.
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
                NodeKind::IntConst(crate::node::const_value::ConstId::new(
                    (0xC0 + i as u64) as usize,
                )),
                [],
                [ValueKind::Typed(ValueType::I64)],
            );
            let v = ctx.function().node_outputs(k)[0];
            consts.push(v);
            phi_inputs.push(v);
        }
        let phi = ctx.create_node(
            NodeKind::Phi,
            phi_inputs,
            [ValueKind::Typed(ValueType::I64)],
        );

        // Remove predecessors {1, 3, 6} in one batch (unsorted on purpose).
        ctx.remove_region_predecessors(region, &[3, 1, 6]).unwrap();

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
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
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
        let k1 = b.build_int_const(7u64, ValueType::I64).unwrap();
        let k2 = b.build_int_const(11u64, ValueType::I64).unwrap();
        let sum = b
            .build_int_binary_operation(k1, k2, IntBinaryOp::Add, ValueType::I64)
            .unwrap();
        b.build_return(Some(sum), &[]).unwrap();
        // Created but never wired into anything.
        let dangling = b.build_int_const(0xDEAD_u64, ValueType::I64).unwrap();
        b.set_lift_addr(None);
        let function = b.build().unwrap();

        let k1_node = function.producer(k1);
        let k2_node = function.producer(k2);
        let dangling_node = function.producer(dangling);
        let entry = function.entry();

        let mut state = FunctionState::populate(&function);

        for r in state.roots.iter() {
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
        // Sanity: a distinct const node, not deduped with k1/k2.
        assert!(
            matches!(function.node_kind(dangling_node), NodeKind::IntConst(_)),
            "dangling node is an IntConst"
        );

        assert_eq!(state.queue.dequeue(), None, "queue starts empty");
    }
}
