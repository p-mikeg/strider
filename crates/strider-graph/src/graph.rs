//! [`Graph`] — the generic bipartite sea-of-nodes graph plus its structural
//! verbs.
//!
//! Generalized from `strider-ir`'s `graph/{mod,store,uses,access,rewrite,
//! compact}.rs` over the node payload `N` and value payload `V`, with the
//! dedup-or-create policy hoisted into the `C: NodeCacheable<N, V>` parameter.
//!
//! The struct imposes NO `Hash`/`Eq` bound on `N`/`V`: deduplication, if any,
//! is entirely the cacher's concern (see the `cache` module).

use std::marker::PhantomData;

use anyhow::anyhow;
use cranelift_entity::{EntityRef, ListPool, PrimaryMap, SecondaryMap};
use smallvec::SmallVec;

use crate::cache::{NodeCache, NodeCacheable};
use crate::ids::{NodeId, UseId, UseIdList, ValueId, ValueIdList};
use crate::iter::{InputCursor, Inputs};
use crate::storage::{Node, RawStore, UseData, ValueData};

/// The core generic graph structure.
///
/// Stores nodes, their input/output slots, the generic dedup `NodeCache`
/// driven by the stateless policy `C`, and a generation counter bumped on every
/// arena-reshuffling operation.
///
/// The policy `C` is a stateless ZST consulted only through its associated
/// functions, so it is held as a `PhantomData<C>` marker — all cache state
/// lives in the `NodeCache`.
///
/// `Graph` is the pure structural arena. Any payload-specific side-tables a
/// consumer maintains (keyed by `NodeId` / `ValueId`) live on the consumer,
/// not here; [`Graph::retain_reachable`] returns the old→new remap so the
/// consumer can fix those up.
///
/// The struct imposes NO `Hash`/`Eq` bound on `N`/`V`: deduplication, if any,
/// is entirely the policy's concern (see the `cache` module).
pub struct Graph<N, V, C: NodeCacheable<N, V>> {
    pub(crate) store: RawStore<N, V>,
    pub(crate) cache: NodeCache,
    pub(crate) _policy: PhantomData<C>,
    pub(crate) generation: u64,
}

impl<N, V, C: NodeCacheable<N, V>> Default for Graph<N, V, C> {
    fn default() -> Self {
        Self::new()
    }
}

// Manual `Clone` (not derived) so the bound is `N: Clone, V: Clone` only — the
// policy `C` is a `PhantomData` ZST, so requiring `C: Clone` (as the derive
// would) is spurious. A cloned graph is a deep, independent copy: `RawStore` and
// `NodeCache` both clone their owned state.
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
    /// Creates an empty graph.
    ///
    /// The policy `C` is stateless, so no instance is constructed — only its
    /// associated functions are ever called.
    pub fn new() -> Self {
        Graph {
            store: RawStore::new(),
            cache: NodeCache::default(),
            _policy: PhantomData,
            generation: 0,
        }
    }

    // ── creation ────────────────────────────────────────────────────────────

    /// Creates a node with the given payload, input values, and output
    /// payloads, delegating the dedup-or-create decision to the cache (driven
    /// by the policy `C`).
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

    // ── read-only accessors ─────────────────────────────────────────────────

    /// Returns a reference to the payload of `node_id`.
    #[inline]
    pub fn node_kind(&self, node_id: NodeId) -> &N {
        self.store.kind_of(node_id)
    }

    /// Returns the payload of `value_id` by value.
    ///
    /// Value payloads are small `Copy` discriminants in every consumer (the
    /// IR's `ValueKind`, the tests' `TestVal`), so returning by value keeps
    /// the common `graph.value_kind(v) == V::Foo` comparison ergonomic. The
    /// `V: Copy` bound lives only on this method — the struct itself imposes no
    /// bound on `V`.
    #[inline]
    pub fn value_kind(&self, value_id: ValueId) -> V
    where
        V: Copy,
    {
        *self.store.value_kind(value_id)
    }

    /// Returns a reference to the payload of `value_id`.
    ///
    /// The by-reference companion to [`Self::value_kind`]. Imposes no `Copy`
    /// bound on `V`, so it serves consumers whose value payload is a non-`Copy`
    /// type (e.g. one carrying `Box<dyn Fn>` predicates). The IR's `Copy`
    /// `ValueKind` keeps using the by-value getter for ergonomic comparisons.
    #[inline]
    pub fn value_kind_ref(&self, value_id: ValueId) -> &V {
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

    /// Returns an iterable view over the values consumed by `node_id`'s inputs.
    #[inline]
    pub fn node_inputs(&self, node_id: NodeId) -> Inputs<'_, N, V, C> {
        Inputs {
            graph: self,
            use_list: self.store.node_input_uses(node_id),
        }
    }

    /// Returns the [`ValueId`] driving the `idx`-th input slot of `node`, or
    /// `None` if `idx` is past the input count.
    #[inline]
    pub fn nth_input(&self, node: NodeId, idx: usize) -> Option<ValueId> {
        let use_id = *self.store.node_input_uses(node).get(idx)?;
        Some(self.store.inputs[use_id].value_id)
    }

    /// Returns the [`UseId`] of the input slot at position `idx` of `node`.
    ///
    /// # Errors
    ///
    /// Returns an error if `idx` is past the node's current input count.
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

    /// Returns exactly `M` input values for `node_id`.
    ///
    /// # Errors
    ///
    /// Returns an error if the node does not have exactly `M` inputs.
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

    /// Returns exactly `M` output ids for `node_id`.
    ///
    /// # Errors
    ///
    /// Returns an error if the node does not have exactly `M` outputs.
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

    /// Returns the node payload of the node that produces `value_id`.
    #[inline]
    pub fn kind_of_value(&self, value_id: ValueId) -> &N {
        self.node_kind(self.producer(value_id))
    }

    /// Returns the [`ValueId`] that `use_id` currently references.
    #[inline]
    pub fn value_of_use(&self, use_id: UseId) -> ValueId {
        self.store.inputs[use_id].value_id
    }

    /// Returns the [`NodeId`] that owns input slot `use_id` (the consumer of
    /// that edge).
    #[inline]
    pub fn node_of_use(&self, use_id: UseId) -> NodeId {
        self.store.inputs[use_id].node_id
    }

    /// Re-canonicalize `node` against the dedup cache after its inputs changed.
    /// `Some(twin)` => an existing structurally-equal node the caller should
    /// merge `node` into; `None` => `node` is now the canonical representative
    /// (or is a non-cacheable kind). See [`NodeCache::canonicalize`].
    pub fn canonicalize_node(&mut self, node: NodeId) -> Option<NodeId>
    where
        V: Clone,
    {
        self.cache.canonicalize::<N, V, C>(&self.store, node)
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

    /// Bump the generation counter without reshuffling the arena.
    ///
    /// `retain_reachable` bumps the counter implicitly because it
    /// invalidates ids; an in-place mutation (a rewrite that replaces or
    /// detaches nodes without compacting) leaves ids valid but changes the
    /// graph a captured snapshot was taken against. Callers that perform
    /// such a mutation invoke this so any snapshot taken beforehand can
    /// detect the change.
    #[inline]
    pub fn bump_generation(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    /// Iterates over every node id in the arena, including unreachable nodes.
    pub fn all_node_ids(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.store.nodes.keys()
    }

    /// Iterates over every value (node-output) id in the arena, including the
    /// outputs of unreachable nodes.
    pub fn all_value_ids(&self) -> impl Iterator<Item = ValueId> + '_ {
        self.store.outputs.keys()
    }

    /// Returns a mutable reference to the payload of `node_id`.
    ///
    /// Rewriting a node's payload in place does NOT change its input/output
    /// structure, so the use-lists stay valid. A cacher keyed on the payload
    /// (e.g. a dedup cache) is NOT notified — the caller must
    /// [`rebuild_cache`](Self::rebuild_cache) afterwards if the payload feeds
    /// the cache key. Used by consumers that renumber an interned-id payload
    /// after compaction.
    #[inline]
    pub fn node_kind_mut(&mut self, node_id: NodeId) -> &mut N {
        self.store.node_kind_mut(node_id)
    }

    /// Rebuilds the dedup cache over the current arena.
    ///
    /// Call after mutating node payloads that feed the cache key in a way the
    /// mutation verbs don't observe — e.g. after an interned-id renumber that
    /// rewrites cacheable nodes' payloads post-compaction.
    pub fn rebuild_cache(&mut self)
    where
        V: Clone,
    {
        self.cache.rebuild::<N, V, C>(&self.store);
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
    /// The dedup cache is told to invalidate `node_id` before the structure
    /// changes, so a dedup entry keyed on its old shape is dropped rather than
    /// left pointing at the now-different node.
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

    /// Removes the input at position `index` from `node_id`, compacting the
    /// remaining inputs' indices. Returns `false` if `index` is out of bounds.
    ///
    /// The cacher is invalidated for `node_id` before the structure changes
    /// (see [`Self::add_node_input`]). A no-op out-of-bounds call still
    /// invalidates, which is harmless: a re-create restores the entry.
    pub fn remove_node_input(&mut self, node_id: NodeId, index: u32) -> bool {
        self.cache.invalidate(node_id);
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

    /// Removes the inputs at the given positions from `node_id` in a SINGLE
    /// O(degree) filter-rebuild, compacting the surviving inputs' indices.
    ///
    /// Equivalent to calling [`Self::remove_node_input`] for each index (the
    /// surviving inputs end up in the same order with contiguous
    /// `input_index`es), but does it in one linear pass over the node's input
    /// list instead of K independent O(tail) shifts — so removing K of a node's
    /// D inputs is O(D), not O(K·D). Out-of-bounds indices and duplicates are
    /// ignored.
    ///
    /// The cacher is invalidated for `node_id` before the structure changes (see
    /// [`Self::add_node_input`]).
    pub fn remove_node_inputs_batch(
        &mut self,
        node_id: NodeId,
        indices: impl IntoIterator<Item = usize>,
    ) {
        self.cache.invalidate(node_id);

        // Mark the slots to drop. A node's degree is the natural bound here, so
        // a bitset keyed on the current input count is O(degree) space.
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

        // Single pass: partition the existing use ids into survivors (reindexed)
        // and victims (unlinked from their value's use-list).
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

    /// Redirects `input_id` to reference `value_id` instead of its current
    /// value, keeping both affected use-lists consistent. A self-redirect is a
    /// no-op.
    pub fn update_input(&mut self, input_id: UseId, value_id: ValueId) {
        if self.store.inputs[input_id].value_id == value_id {
            return;
        }
        // Invalidate the consuming node before its input set changes.
        let node_id = self.store.inputs[input_id].node_id;
        self.cache.invalidate(node_id);
        self.store.unlink_use_from_value_list(input_id);
        self.store.inputs[input_id].value_id = value_id;
        self.store.link_use_to_value_list(input_id);
    }

    /// Removes all inputs from `node_id`, unlinking each from its value's
    /// use-list. After this call `node_id` has no inputs.
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

    /// Redirects every consumer of `old` to `new_val`.
    ///
    /// Returns `true` if at least one use was replaced, `false` if `old` had
    /// no uses or `old == new_val` (a self-redirect changes nothing).
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

    // ── corruption injectors (consistency-check tests only) ─────────────────
    //
    // These deliberately leave the use-list in an inconsistent state so a
    // downstream validator's use-list-consistency check has a corrupted graph
    // to detect. They are NOT part of the normal mutation vocabulary.

    /// Forcibly clears the use-list head of `value`, severing the forward link
    /// from its producer to its consumers WITHOUT touching the consumers' input
    /// edges. Leaves the graph in a deliberately inconsistent state for
    /// use-list-consistency tests.
    ///
    /// `#[doc(hidden)]`: this is a test-only corruption injector, never part of
    /// the production mutation vocabulary. It is reachable only when the
    /// `test-injectors` feature is enabled (a dev-dependency feature of the
    /// consuming test crate), and is hidden from docs so it can never surface as
    /// a discoverable API.
    #[doc(hidden)]
    #[cfg(feature = "test-injectors")]
    pub fn corrupt_clear_first_use(&mut self, value: ValueId) {
        self.store.outputs[value].first_use = None.into();
    }

    /// Forcibly retargets `use_id` to reference `new_target` WITHOUT updating
    /// either the old or new value's use-list. Leaves the graph in a
    /// deliberately inconsistent state for use-list-consistency tests.
    ///
    /// `#[doc(hidden)]`: see [`Self::corrupt_clear_first_use`] — a test-only
    /// corruption injector, hidden from docs and gated behind `test-injectors`.
    #[doc(hidden)]
    #[cfg(feature = "test-injectors")]
    pub fn corrupt_retarget_input(&mut self, use_id: UseId, new_target: ValueId) {
        self.store.inputs[use_id].value_id = new_target;
    }

    // ── compaction ──────────────────────────────────────────────────────────

    /// Rebuilds the arena to retain exactly the nodes in `reachable`, dropping
    /// every other node. Returns the old→new id translation table.
    ///
    /// The graph does **not** traverse to compute reachability — it cannot know
    /// the right reachability for the payload's edge semantics (e.g. the IR
    /// follows forward-control + backward-data; a pure backward-input closure
    /// would miss a `Region` reached only via control). The caller supplies the
    /// set. It **must** be backward-input-closed: every input's producing node
    /// must be present, or pass 2 panics on a dangling edge. Use
    /// [`Self::reachable_by_inputs`] for the backward-input closure of some
    /// roots when that is the reachability you want.
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
        reachable: impl IntoIterator<Item = NodeId>,
    ) -> NodeIdRemap
    where
        N: Clone,
        V: Clone,
    {
        self.generation = self.generation.wrapping_add(1);

        // The caller-supplied reachable set, retained verbatim.
        let reachable: Vec<NodeId> = reachable.into_iter().collect();

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
                let new_value_id =
                    new_outputs.push(ValueData::new(kind, new_node_id, output_index));
                remap.outputs[old_value_id] = Some(new_value_id);
                new_value_ids.push(new_value_id);
            }
            new_nodes[new_node_id].outputs =
                ValueIdList::from_iter(new_value_ids, &mut new_output_pool);
        }

        // 4. Second pass: copy inputs, rewriting value_id through the remap.
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

        // 7. Re-key the dedup cache over the renumbered survivors.
        self.cache.rebuild::<N, V, C>(&self.store);

        remap
    }

}

/// Old→new id translation table produced by [`Graph::retain_reachable`].
///
/// Sparse: only surviving ids are populated; dropped ids return `None`.
#[derive(Debug, Clone, Default)]
pub struct NodeIdRemap {
    nodes: SecondaryMap<NodeId, Option<NodeId>>,
    outputs: SecondaryMap<ValueId, Option<ValueId>>,
}

impl NodeIdRemap {
    /// The post-compaction `NodeId` for `old`, or `None` if dropped.
    #[inline]
    pub fn node_old_to_new(&self, old: NodeId) -> Option<NodeId> {
        self.nodes[old]
    }

    /// Iterates over every `(old, new)` node-id pair that survived
    /// compaction, in ascending old-id order. Dropped ids are skipped.
    ///
    /// Lets a consumer remap a `NodeId`-keyed side-table by draining each
    /// surviving slot from its old key to its new key.
    pub fn surviving_node_pairs(&self) -> impl Iterator<Item = (NodeId, NodeId)> + '_ {
        self.nodes
            .iter()
            .filter_map(|(old, new)| new.map(|n| (old, n)))
    }

    /// The post-compaction `ValueId` for `old`, or `None` if its producing
    /// node was dropped.
    #[inline]
    pub fn value_old_to_new(&self, old: ValueId) -> Option<ValueId> {
        self.outputs[old]
    }
}
