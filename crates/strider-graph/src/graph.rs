use std::marker::PhantomData;

use anyhow::anyhow;
use cranelift_entity::{EntityRef, ListPool, PrimaryMap, SecondaryMap};
use smallvec::SmallVec;

use crate::cache::{NodeCache, NodeCacheable};
use crate::ids::{NodeId, UseId, UseIdList, ValueId, ValueIdList};
use crate::iter::{InputCursor, Inputs};
use crate::storage::{Node, RawStore, UseData, ValueData};

/// Nodes, their input/output slots, the dedup cache, and a generation counter
/// bumped on every arena-reshuffling operation.
pub struct Graph<N, V, C: NodeCacheable<N, V>> {
    pub(crate) store: RawStore<N, V>,
    cache: NodeCache,
    _policy: PhantomData<C>,
    generation: u64,
}

impl<N, V, C: NodeCacheable<N, V>> Default for Graph<N, V, C> {
    fn default() -> Self {
        Self::new()
    }
}

// Manual `Clone` so the bound is `N: Clone, V: Clone` only: the derive would
// also demand `C: Clone`, spurious for a `PhantomData` ZST. The clone is deep
// and independent.
impl<N: Clone, V: Clone, C: NodeCacheable<N, V>> Clone for Graph<N, V, C> {
    fn clone(&self) -> Self {
        Graph {
            store: self.store.clone(),
            cache: self.cache.clone(),
            _policy: PhantomData,
            generation: self.generation,
        }
    }
}

impl<N, V, C: NodeCacheable<N, V>> Graph<N, V, C> {
    pub fn new() -> Self {
        Graph {
            store: RawStore::new(),
            cache: NodeCache::default(),
            _policy: PhantomData,
            generation: 0,
        }
    }

    /// Delegates the dedup-or-create decision to the cache, driven by `C`.
    pub fn create_node(
        &mut self,
        kind: N,
        inputs: impl IntoIterator<Item = ValueId>,
        outputs: impl IntoIterator<Item = V>,
    ) -> NodeId {
        let inputs: SmallVec<[ValueId; 4]> = inputs.into_iter().collect();
        let outputs: SmallVec<[V; 4]> = outputs.into_iter().collect();
        self.cache
            .get_or_alloc::<N, V, C>(&mut self.store, kind, inputs, outputs)
    }

    #[inline]
    pub fn node_kind(&self, node_id: NodeId) -> &N {
        self.store.kind_of(node_id)
    }

    #[inline]
    pub fn value_kind(&self, value_id: ValueId) -> V
    where
        V: Copy,
    {
        *self.store.value_kind(value_id)
    }

    /// By-reference companion to [`Self::value_kind`], for a `V` that is not
    /// `Copy`.
    #[inline]
    pub fn value_kind_ref(&self, value_id: ValueId) -> &V {
        self.store.value_kind(value_id)
    }

    #[inline]
    pub fn value_definition(&self, value_id: ValueId) -> (NodeId, u32) {
        let data = &self.store.outputs[value_id];
        (data.source_id, data.output_index)
    }

    #[inline]
    pub fn producer(&self, value_id: ValueId) -> NodeId {
        self.store.producer(value_id)
    }

    #[inline]
    pub fn node_outputs(&self, node_id: NodeId) -> &[ValueId] {
        self.store.node_outputs(node_id)
    }

    #[inline]
    pub fn node_inputs(&self, node_id: NodeId) -> Inputs<'_, N, V, C> {
        Inputs {
            graph: self,
            use_list: self.store.node_input_uses(node_id),
        }
    }

    /// `None` if `idx` is past the input count.
    #[inline]
    pub fn nth_input(&self, node: NodeId, idx: usize) -> Option<ValueId> {
        let use_id = *self.store.node_input_uses(node).get(idx)?;
        Some(self.store.inputs[use_id].value_id)
    }

    /// # Errors
    ///
    /// If `idx` is past the node's current input count.
    pub fn node_input_id_at(&self, node: NodeId, idx: usize) -> crate::Result<UseId> {
        self.store
            .node_input_uses(node)
            .get(idx)
            .copied()
            .ok_or_else(|| {
                let len = self.node_inputs(node).len();
                anyhow!("input index {idx} out of bounds for node {node:?} (len={len})")
            })
    }

    /// # Errors
    ///
    /// If the node does not have exactly `M` inputs.
    pub fn node_inputs_exact<const M: usize>(
        &self,
        node_id: NodeId,
    ) -> crate::Result<[ValueId; M]> {
        let inputs = self.node_inputs(node_id);
        if inputs.len() != M {
            let actual = inputs.len();
            return Err(anyhow!(
                "node {node_id:?} does not have exactly {M} inputs (has {actual})"
            ));
        }
        let mut result = [ValueId::default(); M];
        for (i, v) in inputs.into_iter().enumerate() {
            result[i] = v;
        }
        Ok(result)
    }

    /// # Errors
    ///
    /// If the node does not have exactly `M` outputs.
    pub fn node_outputs_exact<const M: usize>(
        &self,
        node_id: NodeId,
    ) -> crate::Result<[ValueId; M]> {
        let outputs = self.node_outputs(node_id);
        if outputs.len() != M {
            let actual = outputs.len();
            return Err(anyhow!(
                "node {node_id:?} does not have exactly {M} outputs (has {actual})"
            ));
        }
        let mut result = [ValueId::default(); M];
        result.copy_from_slice(outputs);
        Ok(result)
    }

    #[inline]
    pub fn kind_of_value(&self, value_id: ValueId) -> &N {
        self.node_kind(self.producer(value_id))
    }

    #[inline]
    pub fn value_of_use(&self, use_id: UseId) -> ValueId {
        self.store.inputs[use_id].value_id
    }

    /// The consumer that owns input slot `use_id`.
    #[inline]
    pub fn node_of_use(&self, use_id: UseId) -> NodeId {
        self.store.inputs[use_id].node_id
    }

    /// Re-canonicalize `node` against the dedup cache after its inputs changed.
    /// `Some(twin)` is an existing structurally-equal node the caller should
    /// merge `node` into; `None` means `node` is now the canonical
    /// representative, or is a non-cacheable kind.
    pub fn canonicalize_node(&mut self, node: NodeId) -> Option<NodeId>
    where
        V: Clone,
    {
        self.cache.canonicalize::<N, V, C>(&self.store, node)
    }

    #[inline]
    #[cfg(any(test, feature = "test-injectors"))]
    pub fn next_node_id(&self) -> NodeId {
        self.store.nodes.next_key()
    }

    #[inline]
    pub fn has_node(&self, id: NodeId) -> bool {
        self.store.nodes.is_valid(id)
    }

    /// `None` if no node with that index exists.
    ///
    /// `u32::MAX` is `cranelift-entity`'s reserved sentinel and `NodeId::new`
    /// only debug-asserts on it, so it is rejected before the id is built: a raw id from
    /// outside (a Python caller) must produce `None`, never a panic.
    #[inline]
    pub fn node_id_from_u32(&self, raw: u32) -> Option<NodeId> {
        if raw == u32::MAX {
            return None;
        }
        let id = NodeId::new(raw as usize);
        self.has_node(id).then_some(id)
    }

    #[inline]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Marks the graph as changed without invalidating any id.
    #[inline]
    pub fn bump_generation(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    /// Includes unreachable nodes.
    pub fn all_node_ids(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.store.nodes.keys()
    }

    /// Includes the outputs of unreachable nodes.
    pub fn all_value_ids(&self) -> impl Iterator<Item = ValueId> + '_ {
        self.store.outputs.keys()
    }

    /// The cache is NOT notified: if the payload feeds the cache key, call
    /// [`rebuild_cache`](Self::rebuild_cache) afterwards.
    #[inline]
    pub fn node_kind_mut(&mut self, node_id: NodeId) -> &mut N {
        self.store.node_kind_mut(node_id)
    }

    /// Re-keys the cache after node payloads feeding the cache key were
    /// rewritten in place.
    pub fn rebuild_cache(&mut self)
    where
        V: Clone,
    {
        self.cache.rebuild::<N, V, C>(&self.store);
    }

    /// Walks the per-value use-list, yielding `(consumer, input_index)`.
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

    #[inline]
    pub fn value_has_one_use(&self, value: ValueId) -> bool {
        let mut uses = self.value_uses(value);
        uses.next().is_some() && uses.next().is_none()
    }

    #[inline]
    #[cfg(any(test, feature = "test-injectors"))]
    pub fn value_first_use_id(&self, value: ValueId) -> Option<UseId> {
        self.store.outputs[value].first_use.expand()
    }

    #[inline]
    pub fn value_use_cursor(&mut self, value_id: ValueId) -> InputCursor<'_, N, V, C> {
        let first_use = self.store.outputs[value_id].first_use.expand();
        InputCursor {
            graph: self,
            current: first_use,
        }
    }

    /// Appends `value_id` as the node's last input.
    pub fn add_node_input(&mut self, node_id: NodeId, value_id: ValueId) {
        self.cache.invalidate(node_id);
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

    /// Compacts the remaining inputs' indices. `false` if `index` is out of
    /// bounds.
    pub fn remove_node_input(&mut self, node_id: NodeId, index: u32) -> bool {
        let index = index as usize;
        let inputs = &self.store.nodes[node_id].inputs;
        let slice = inputs.as_slice(&self.store.input_pool);
        let Some(&delete_use_id) = slice.get(index) else {
            return false;
        };
        // After the bounds check: invalidating first would evict a node whose
        // structure never changed, and this path never re-inserts it.
        self.cache.invalidate(node_id);

        self.store.nodes[node_id]
            .inputs
            .remove(index, &mut self.store.input_pool);
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

    /// Equivalent to [`Self::remove_node_input`] per index, but removing K of a
    /// node's D inputs is O(D), not O(K*D). Out-of-bounds indices and
    /// duplicates are ignored.
    pub fn remove_node_inputs_batch(
        &mut self,
        node_id: NodeId,
        indices: impl IntoIterator<Item = usize>,
    ) {
        // Degree-bounded, so a bitset over the current input count is O(D).
        let len = self.store.node_input_uses(node_id).len();
        let mut drop_slot = vec![false; len];
        let mut any = false;
        for idx in indices {
            if idx < len {
                drop_slot[idx] = true;
                any = true;
            }
        }
        if !any {
            return;
        }
        // After the early return: invalidating first would evict a node whose
        // structure never changed, and this path never re-inserts it.
        self.cache.invalidate(node_id);

        // Partition into survivors (reindexed) and victims (unlinked).
        let old_uses: SmallVec<[UseId; 4]> = self.store.node_input_uses(node_id).into();
        let mut survivors: SmallVec<[UseId; 4]> = SmallVec::with_capacity(old_uses.len());
        for (slot, use_id) in old_uses.into_iter().enumerate() {
            if drop_slot[slot] {
                self.store.unlink_use_from_value_list(use_id);
            } else {
                self.store.inputs[use_id].input_index = survivors.len() as u32;
                survivors.push(use_id);
            }
        }

        let inputs = &mut self.store.nodes[node_id].inputs;
        inputs.clear(&mut self.store.input_pool);
        *inputs = UseIdList::from_iter(survivors, &mut self.store.input_pool);
    }

    /// Keeps both affected use-lists consistent. A self-redirect is a no-op.
    pub fn update_input(&mut self, input_id: UseId, value_id: ValueId) {
        if self.store.inputs[input_id].value_id == value_id {
            return;
        }
        let node_id = self.store.inputs[input_id].node_id;
        self.cache.invalidate(node_id);
        self.store.unlink_use_from_value_list(input_id);
        self.store.inputs[input_id].value_id = value_id;
        self.store.link_use_to_value_list(input_id);
    }

    /// Unlinks each from its value's use-list.
    pub fn detach_node_inputs(&mut self, node_id: NodeId) {
        self.cache.invalidate(node_id);
        let use_ids: SmallVec<[UseId; 4]> = self.store.nodes[node_id]
            .inputs
            .as_slice(&self.store.input_pool)
            .into();
        for use_id in use_ids {
            self.store.unlink_use_from_value_list(use_id);
        }
        self.store.nodes[node_id]
            .inputs
            .clear(&mut self.store.input_pool);
    }

    /// `false` if `old` had no uses, or if `old == new_val` since a
    /// self-redirect changes nothing.
    pub fn replace_all_uses(&mut self, old: ValueId, new_val: ValueId) -> bool {
        if old == new_val {
            return false;
        }
        let mut cursor = self.value_use_cursor(old);
        if cursor.current().is_none() {
            return false;
        }
        while cursor.replace_current_with(new_val) {}
        true
    }

    /// Clears `value`'s use-list head, severing the producer's forward link to
    /// its consumers WITHOUT touching the consumers' input edges. Leaves the
    /// graph inconsistent, for tests that need a broken graph.
    #[doc(hidden)]
    #[cfg(any(test, feature = "test-injectors"))]
    pub fn corrupt_clear_first_use(&mut self, value: ValueId) {
        self.store.outputs[value].first_use = None.into();
    }

    /// Rebuilds the arena to retain exactly the nodes in `reachable`, dropping
    /// every other node. Returns the old-to-new id translation table.
    ///
    /// `reachable` MUST be backward-input-closed, i.e. every input's producing
    /// node is present, and duplicate-free. Both violations panic; unchecked, a
    /// repeated id would push two new nodes, the remap would keep the second,
    /// and the relink would splice the orphaned first into its values'
    /// use-lists, so `value_uses` reported consumers no node owns.
    ///
    /// Invalidates every pre-compaction `NodeId` / `ValueId` / `UseId`, and
    /// bumps the generation counter.
    pub fn retain_reachable(&mut self, reachable: impl IntoIterator<Item = NodeId>) -> NodeIdRemap
    where
        N: Clone,
        V: Clone,
    {
        self.generation = self.generation.wrapping_add(1);

        let reachable: Vec<NodeId> = reachable.into_iter().collect();

        let mut new_nodes: PrimaryMap<NodeId, Node<N>> = PrimaryMap::new();
        let mut new_outputs: PrimaryMap<ValueId, ValueData<V>> = PrimaryMap::new();
        let mut new_inputs: PrimaryMap<UseId, UseData> = PrimaryMap::new();
        let mut new_output_pool = ListPool::<ValueId>::new();
        let mut new_input_pool = ListPool::<UseId>::new();

        let mut remap = NodeIdRemap::default();

        // Copy nodes (placeholder slot lists) and outputs first, so every new
        // NodeId / ValueId exists before the second pass rewrites edges.
        for &old_node_id in &reachable {
            // Not a `debug_assert`: violating it corrupts the arena silently, and
            // the slot this reads is written on the next line either way.
            assert!(
                remap.nodes[old_node_id].is_none(),
                "`reachable` repeated {old_node_id:?}: the second copy orphans \
                 the first, whose outputs stay spliced into the use-lists"
            );
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
                let new_value_id =
                    new_outputs.push(ValueData::new(kind, new_node_id, output_index));
                remap.outputs[old_value_id] = Some(new_value_id);
                new_value_ids.push(new_value_id);
            }
            new_nodes[new_node_id].outputs =
                ValueIdList::from_iter(new_value_ids, &mut new_output_pool);
        }

        // Copy inputs, rewriting each value_id through the remap.
        for &old_node_id in &reachable {
            let new_node_id =
                remap.nodes[old_node_id].expect("reachable node missing from pass-1 remap");
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
                let new_use_id =
                    new_inputs.push(UseData::new(new_value_id, new_node_id, input_index));
                new_use_ids.push(new_use_id);
            }
            new_nodes[new_node_id].inputs = UseIdList::from_iter(new_use_ids, &mut new_input_pool);
        }

        // Swap the arenas in before rebuilding use-lists.
        self.store.nodes = new_nodes;
        self.store.outputs = new_outputs;
        self.store.inputs = new_inputs;
        self.store.output_pool = new_output_pool;
        self.store.input_pool = new_input_pool;

        let all_use_ids: SmallVec<[UseId; 16]> = self.store.inputs.keys().collect();
        for use_id in all_use_ids {
            self.store.link_use_to_value_list(use_id);
        }

        self.cache.rebuild::<N, V, C>(&self.store);

        remap
    }
}

/// Sparse old-to-new id table from [`Graph::retain_reachable`]: only surviving
/// ids are populated, dropped ids return `None`.
#[derive(Debug, Clone, Default)]
pub struct NodeIdRemap {
    nodes: SecondaryMap<NodeId, Option<NodeId>>,
    outputs: SecondaryMap<ValueId, Option<ValueId>>,
}

impl NodeIdRemap {
    #[inline]
    pub fn node_old_to_new(&self, old: NodeId) -> Option<NodeId> {
        self.nodes[old]
    }

    /// `None` if `old`'s producing node was dropped.
    #[inline]
    pub fn value_old_to_new(&self, old: ValueId) -> Option<ValueId> {
        self.outputs[old]
    }
}
