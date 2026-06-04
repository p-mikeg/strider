//! [`Graph`] — the generic bipartite sea-of-nodes graph plus its structural
//! verbs.
//!
//! Generalized from `strider-ir`'s `graph/{mod,store,uses,access,rewrite,
//! compact}.rs` over the node payload `N` and value payload `V`, with the
//! dedup-or-create policy hoisted into the `C: NodeCacheable<N, V>` parameter.
//!
//! The struct imposes NO `Hash`/`Eq` bound on `N`/`V`: deduplication, if any,
//! is entirely the cacher's concern (see [`crate::cache`]).

use cranelift_entity::{EntityRef, ListPool, PrimaryMap, SecondaryMap};
use smallvec::SmallVec;

use crate::cache::NodeCacheable;
use crate::ids::{NodeId, UseId, UseIdList, ValueId, ValueIdList};
use crate::iter::{Inputs, InputCursor};
use crate::storage::{Node, RawStore, UseData, ValueData};

/// The core generic graph structure.
///
/// Stores nodes, their input/output slots, a node-creation policy (`cacher`),
/// and a generation counter bumped on every arena-reshuffling operation.
///
/// `Graph` is the pure structural arena. Any payload-specific side-tables a
/// consumer maintains (keyed by `NodeId` / `ValueId`) live on the consumer,
/// not here; [`Graph::retain_reachable`] returns the old→new remap so the
/// consumer can fix those up.
pub struct Graph<N, V, C: NodeCacheable<N, V>> {
    pub(crate) store: RawStore<N, V>,
    pub(crate) cacher: C,
    pub(crate) generation: u64,
}

impl<N, V, C: NodeCacheable<N, V> + Default> Default for Graph<N, V, C> {
    fn default() -> Self {
        Self::new()
    }
}

impl<N, V, C: NodeCacheable<N, V> + Default> Graph<N, V, C> {
    /// Creates an empty graph with a default-constructed cacher.
    pub fn new() -> Self {
        Self::with_cacher(C::default())
    }
}

impl<N, V, C: NodeCacheable<N, V>> Graph<N, V, C> {
    /// Creates an empty graph with the given cacher.
    pub fn with_cacher(cacher: C) -> Self {
        Graph {
            store: RawStore::new(),
            cacher,
            generation: 0,
        }
    }

    // ── creation ────────────────────────────────────────────────────────────

    /// Creates a node with the given payload, input values, and output
    /// payloads, delegating the dedup-or-create decision to the cacher.
    pub fn create_node(
        &mut self,
        kind: N,
        inputs: impl IntoIterator<Item = ValueId>,
        outputs: impl IntoIterator<Item = V>,
    ) -> NodeId {
        let inputs: SmallVec<[ValueId; 4]> = inputs.into_iter().collect();
        let outputs: SmallVec<[V; 4]> = outputs.into_iter().collect();
        self.cacher.create(&mut self.store, kind, inputs, outputs)
    }

    // ── read-only accessors ─────────────────────────────────────────────────

    /// Returns a reference to the payload of `node_id`.
    #[inline]
    pub fn node_kind(&self, node_id: NodeId) -> &N {
        self.store.node_kind(node_id)
    }

    /// Returns a reference to the payload of `value_id`.
    #[inline]
    pub fn value_kind(&self, value_id: ValueId) -> &V {
        self.store.value_kind(value_id)
    }

    /// Returns the `(NodeId, output_index)` pair that defines `value_id`.
    #[inline]
    pub fn value_definition(&self, value_id: ValueId) -> (NodeId, u32) {
        let data = &self.store.outputs[value_id];
        (data.source_id, data.output_index)
    }

    /// Returns the [`NodeId`] that produces `value_id`.
    #[inline]
    pub fn producer(&self, value_id: ValueId) -> NodeId {
        self.store.producer(value_id)
    }

    /// Returns the slice of output ids for `node_id`.
    #[inline]
    pub fn node_outputs(&self, node_id: NodeId) -> &[ValueId] {
        self.store.node_outputs(node_id)
    }

    /// Returns exactly `N` output ids for `node_id`, or `None` if the count
    /// differs.
    #[inline]
    pub fn node_outputs_exact<const K: usize>(&self, node_id: NodeId) -> Option<[ValueId; K]> {
        let outputs = self.node_outputs(node_id);
        if outputs.len() != K {
            return None;
        }
        let mut result = [ValueId::default(); K];
        result.copy_from_slice(outputs);
        Some(result)
    }

    /// Returns an iterable view over the values consumed by `node_id`'s inputs.
    #[inline]
    pub fn node_inputs(&self, node_id: NodeId) -> Inputs<'_, N, V, C> {
        Inputs {
            graph: self,
            use_list: self.store.node_input_uses(node_id),
        }
    }

    /// Returns exactly `K` input values for `node_id`, or `None` if the count
    /// differs.
    #[inline]
    pub fn node_inputs_exact<const K: usize>(&self, node_id: NodeId) -> Option<[ValueId; K]> {
        let inputs = self.node_inputs(node_id);
        if inputs.len() != K {
            return None;
        }
        let mut result = [ValueId::default(); K];
        for (i, v) in inputs.into_iter().enumerate() {
            result[i] = v;
        }
        Some(result)
    }

    /// Returns the [`ValueId`] driving the `idx`-th input slot of `node`, or
    /// `None` if `idx` is past the input count.
    #[inline]
    pub fn nth_input(&self, node: NodeId, idx: usize) -> Option<ValueId> {
        let use_id = *self.store.node_input_uses(node).get(idx)?;
        Some(self.store.inputs[use_id].value_id)
    }

    /// Returns the [`UseId`] of the input slot at position `idx` of `node`, or
    /// `None` if out of bounds.
    #[inline]
    pub fn node_input_id_at(&self, node: NodeId, idx: usize) -> Option<UseId> {
        self.store.node_input_uses(node).get(idx).copied()
    }

    /// Returns the [`ValueId`] that `use_id` currently references.
    #[inline]
    pub fn value_of_use(&self, use_id: UseId) -> ValueId {
        self.store.inputs[use_id].value_id
    }

    /// Returns the [`NodeId`] that the next freshly-allocated node would
    /// receive.
    #[inline]
    pub fn next_node_id(&self) -> NodeId {
        self.store.nodes.next_key()
    }

    /// Returns `true` if `id` is a live entry in the node arena.
    #[inline]
    pub fn has_node(&self, id: NodeId) -> bool {
        self.store.nodes.is_valid(id)
    }

    /// Validated construction of a [`NodeId`] from a raw `u32` index. Returns
    /// `None` if no node with that index exists.
    #[inline]
    pub fn node_id_from_u32(&self, raw: u32) -> Option<NodeId> {
        let id = NodeId::new(raw as usize);
        self.has_node(id).then_some(id)
    }

    /// Returns the current generation counter, bumped by every
    /// arena-reshuffling operation ([`Self::retain_reachable`]).
    #[inline]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Iterates over every node id in the arena, including unreachable nodes.
    pub fn all_node_ids(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.store.nodes.keys()
    }

    // ── use-list queries ────────────────────────────────────────────────────

    /// Returns an iterator over all inputs that consume `value_id`, each as
    /// `(consumer_node_id, input_index)`, following the per-value use-list.
    #[inline]
    pub fn value_uses(&self, value_id: ValueId) -> impl Iterator<Item = (NodeId, u32)> + '_ {
        let first_use = self.store.outputs[value_id].first_use.expand();
        core::iter::successors(first_use, move |id| self.store.inputs[*id].next.expand()).map(
            move |id| {
                let use_data = &self.store.inputs[id];
                (use_data.node_id, use_data.input_index)
            },
        )
    }

    /// Returns `true` if `value` is consumed by exactly one input.
    #[inline]
    pub fn value_has_one_use(&self, value: ValueId) -> bool {
        let mut uses = self.value_uses(value);
        uses.next().is_some() && uses.next().is_none()
    }

    /// Returns the head of `value`'s use-list as a raw [`UseId`].
    #[inline]
    pub fn value_first_use_id(&self, value: ValueId) -> Option<UseId> {
        self.store.outputs[value].first_use.expand()
    }

    /// Returns the `next` pointer of `use_id` in its use-list.
    #[inline]
    pub fn next_use(&self, use_id: UseId) -> Option<UseId> {
        self.store.inputs[use_id].next.expand()
    }

    /// Returns a cursor over the use-list of `value_id`.
    #[inline]
    pub fn value_use_cursor(&mut self, value_id: ValueId) -> InputCursor<'_, N, V, C> {
        let first_use = self.store.outputs[value_id].first_use.expand();
        InputCursor {
            graph: self,
            current: first_use,
        }
    }

    // ── mutation ────────────────────────────────────────────────────────────

    /// Appends a new input to `node_id` referencing `value_id`.
    ///
    /// Mutates UNCONDITIONALLY: the generic graph has no cacheability
    /// knowledge. The consumer's invariant is "only mutate nodes the cacher
    /// does not cache" — mutating a cached node would leave a stale dedup
    /// entry pointing at the now-different node.
    pub fn add_node_input(&mut self, node_id: NodeId, value_id: ValueId) {
        let input_index = self.store.nodes[node_id].inputs.len(&self.store.input_pool) as u32;
        let use_id = self
            .store
            .inputs
            .push(UseData::new(value_id, node_id, input_index));
        self.store.nodes[node_id]
            .inputs
            .push(use_id, &mut self.store.input_pool);
        self.store.link_use_to_value_list(use_id);
    }

    /// Removes the input at position `index` from `node_id`, compacting the
    /// remaining inputs' indices. Returns `false` if `index` is out of bounds.
    ///
    /// Mutates unconditionally (see [`Self::add_node_input`]).
    pub fn remove_node_input(&mut self, node_id: NodeId, index: u32) -> bool {
        let index = index as usize;
        let inputs = &mut self.store.nodes[node_id].inputs;
        let slice = inputs.as_slice(&self.store.input_pool);
        let Some(&delete_use_id) = slice.get(index) else {
            return false;
        };

        inputs.remove(index, &mut self.store.input_pool);
        let tail: SmallVec<[UseId; 4]> = self.store.nodes[node_id]
            .inputs
            .as_slice(&self.store.input_pool)[index..]
            .into();
        for use_id in tail {
            self.store.inputs[use_id].input_index -= 1;
        }
        self.store.unlink_use_from_value_list(delete_use_id);
        true
    }

    /// Redirects `input_id` to reference `value_id` instead of its current
    /// value, keeping both affected use-lists consistent. A self-redirect is a
    /// no-op.
    pub fn update_input(&mut self, input_id: UseId, value_id: ValueId) {
        if self.store.inputs[input_id].value_id == value_id {
            return;
        }
        self.store.unlink_use_from_value_list(input_id);
        self.store.inputs[input_id].value_id = value_id;
        self.store.link_use_to_value_list(input_id);
    }

    /// Removes all inputs from `node_id`, unlinking each from its value's
    /// use-list. After this call `node_id` has no inputs.
    pub fn detach_node_inputs(&mut self, node_id: NodeId) {
        let use_ids: SmallVec<[UseId; 4]> = self.store.nodes[node_id]
            .inputs
            .as_slice(&self.store.input_pool)
            .into();
        for use_id in use_ids {
            self.store.unlink_use_from_value_list(use_id);
        }
        self.store.nodes[node_id].inputs.clear(&mut self.store.input_pool);
    }

    /// Redirects every consumer of `old` to `new_val`.
    ///
    /// Returns `true` if at least one use was replaced, `false` if `old` had
    /// no uses.
    pub fn replace_all_uses(&mut self, old: ValueId, new_val: ValueId) -> bool {
        let mut cursor = self.value_use_cursor(old);
        if cursor.current().is_none() {
            return false;
        }
        while cursor.replace_current_with(new_val) {}
        true
    }

    // ── compaction ──────────────────────────────────────────────────────────

    /// Rebuilds the arena to retain only nodes reachable from `roots` by
    /// following input edges backward (def→use closure). Returns the old→new
    /// id translation table.
    ///
    /// Pre-compaction `NodeId` / `ValueId` / `UseId` values are invalidated by
    /// this call; callers holding any such ids MUST rewrite them through the
    /// returned [`NodeIdRemap`] (or drop them). The generation counter is
    /// bumped so a captured snapshot can detect the reshuffle.
    ///
    /// The generic graph compacts only the structural arena (nodes, values,
    /// uses). Any consumer side-tables are the consumer's concern; they remap
    /// via the returned table.
    pub fn retain_reachable(
        &mut self,
        roots: impl IntoIterator<Item = NodeId>,
    ) -> NodeIdRemap
    where
        N: Clone,
        V: Clone,
    {
        self.generation = self.generation.wrapping_add(1);

        // 1. Reachable set: backward closure over input producers.
        let reachable = self.reachable_by_inputs(roots);

        // 2. Build fresh arenas.
        let mut new_nodes: PrimaryMap<NodeId, Node<N>> = PrimaryMap::new();
        let mut new_outputs: PrimaryMap<ValueId, ValueData<V>> = PrimaryMap::new();
        let mut new_inputs: PrimaryMap<UseId, UseData> = PrimaryMap::new();
        let mut new_output_pool = ListPool::<ValueId>::new();
        let mut new_input_pool = ListPool::<UseId>::new();

        let mut remap = NodeIdRemap::default();

        // 3. First pass: copy nodes (placeholder slot lists) and outputs so
        // every new NodeId / ValueId exists before pass 2 rewrites edges.
        for &old_node_id in &reachable {
            let new_kind = self.store.nodes[old_node_id].kind.clone();
            let new_node_id = new_nodes.push(Node::new(new_kind));
            remap.nodes[old_node_id] = Some(new_node_id);

            let old_value_ids: SmallVec<[ValueId; 4]> = self.store.nodes[old_node_id]
                .outputs
                .as_slice(&self.store.output_pool)
                .into();
            let mut new_value_ids: SmallVec<[ValueId; 4]> =
                SmallVec::with_capacity(old_value_ids.len());
            for old_value_id in old_value_ids {
                let old_out = &self.store.outputs[old_value_id];
                let kind = old_out.kind.clone();
                let output_index = old_out.output_index;
                let new_value_id = new_outputs.push(ValueData::new(kind, new_node_id, output_index));
                remap.outputs[old_value_id] = Some(new_value_id);
                new_value_ids.push(new_value_id);
            }
            new_nodes[new_node_id].outputs =
                ValueIdList::from_iter(new_value_ids, &mut new_output_pool);
        }

        // 4. Second pass: copy inputs, rewriting value_id through the remap.
        for &old_node_id in &reachable {
            let new_node_id = remap.nodes[old_node_id]
                .expect("reachable node missing from pass-1 remap");
            let old_use_ids: SmallVec<[UseId; 4]> = self.store.nodes[old_node_id]
                .inputs
                .as_slice(&self.store.input_pool)
                .into();
            let mut new_use_ids: SmallVec<[UseId; 4]> = SmallVec::with_capacity(old_use_ids.len());
            for old_use_id in old_use_ids {
                let old_input = &self.store.inputs[old_use_id];
                let new_value_id = remap.outputs[old_input.value_id].expect(
                    "input references an output whose producing node is unreachable \
                     (use-list invariant violation)",
                );
                let input_index = old_input.input_index;
                let new_use_id = new_inputs.push(UseData::new(new_value_id, new_node_id, input_index));
                remap.inputs[old_use_id] = Some(new_use_id);
                new_use_ids.push(new_use_id);
            }
            new_nodes[new_node_id].inputs = UseIdList::from_iter(new_use_ids, &mut new_input_pool);
        }

        // 5. Swap the arenas in before rebuilding use-lists.
        self.store.nodes = new_nodes;
        self.store.outputs = new_outputs;
        self.store.inputs = new_inputs;
        self.store.output_pool = new_output_pool;
        self.store.input_pool = new_input_pool;

        // 6. Rebuild use-list pointers via the link helper.
        let all_use_ids: SmallVec<[UseId; 16]> = self.store.inputs.keys().collect();
        for use_id in all_use_ids {
            self.store.link_use_to_value_list(use_id);
        }

        remap
    }

    /// Backward closure over input-producer edges from `roots`.
    fn reachable_by_inputs(&self, roots: impl IntoIterator<Item = NodeId>) -> Vec<NodeId> {
        let mut visited: SecondaryMap<NodeId, bool> = SecondaryMap::new();
        let mut order: Vec<NodeId> = Vec::new();
        let mut stack: Vec<NodeId> = roots.into_iter().collect();
        while let Some(node) = stack.pop() {
            if visited[node] {
                continue;
            }
            visited[node] = true;
            order.push(node);
            for input in self.node_inputs(node) {
                stack.push(self.producer(input));
            }
        }
        order
    }
}

/// Old→new id translation table produced by [`Graph::retain_reachable`].
///
/// Sparse: only surviving ids are populated; dropped ids return `None`.
#[derive(Debug, Clone, Default)]
pub struct NodeIdRemap {
    nodes: SecondaryMap<NodeId, Option<NodeId>>,
    outputs: SecondaryMap<ValueId, Option<ValueId>>,
    inputs: SecondaryMap<UseId, Option<UseId>>,
}

impl NodeIdRemap {
    /// The post-compaction `NodeId` for `old`, or `None` if dropped.
    #[inline]
    pub fn node_old_to_new(&self, old: NodeId) -> Option<NodeId> {
        self.nodes[old]
    }

    /// The post-compaction `ValueId` for `old`, or `None` if its producing
    /// node was dropped.
    #[inline]
    pub fn value_old_to_new(&self, old: ValueId) -> Option<ValueId> {
        self.outputs[old]
    }

    /// The post-compaction `UseId` for `old`, or `None` if its consuming node
    /// was dropped.
    #[inline]
    pub fn use_old_to_new(&self, old: UseId) -> Option<UseId> {
        self.inputs[old]
    }
}
