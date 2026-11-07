use cranelift_entity::SecondaryMap;
use hashbrown::HashTable;
use smallvec::SmallVec;

use crate::ids::{NodeId, ValueId};
use crate::storage::RawStore;

/// The node-creation policy: stateless hooks deciding dedup-or-create.
///
/// [`hash`](Self::hash) and [`eq`](Self::eq) are only called when
/// [`should_cache`](Self::should_cache) (default `false`) is true, so a
/// non-caching policy is an empty `impl` block.
pub trait NodeCacheable<N, V> {
    fn should_cache(_kind: &N) -> bool {
        false
    }

    /// Structural hash of a `(kind, inputs, output-kinds)` key; any `u64`.
    fn hash(_kind: &N, _inputs: &[ValueId], _outputs: &[V]) -> u64 {
        unreachable!("hash() called on a policy whose should_cache() returned false")
    }

    /// Whether `cand`'s current structure in `store` equals the key.
    fn eq(
        _store: &RawStore<N, V>,
        _cand: NodeId,
        _kind: &N,
        _inputs: &[ValueId],
        _outputs: &[V],
    ) -> bool {
        unreachable!("eq() called on a policy whose should_cache() returned false")
    }
}

/// Never deduplicates: every node allocates fresh. Imposes no bound on `N`/`V`.
#[derive(Clone, Copy, Default)]
pub struct NeverCacheable;

impl<N, V> NodeCacheable<N, V> for NeverCacheable {}

/// `node_hashes` sentinel for "not in the dedup table".
const HASH_NONE: u64 = u64::MAX;

/// Maps a node's structural key to the canonical [`NodeId`] holding it.
#[derive(Clone)]
pub(crate) struct NodeCache {
    table: HashTable<NodeId>,
    node_hashes: SecondaryMap<NodeId, u64>,
}

impl Default for NodeCache {
    fn default() -> Self {
        Self {
            table: HashTable::new(),
            // `SecondaryMap::clear` keeps this default.
            node_hashes: SecondaryMap::with_default(HASH_NONE),
        }
    }
}

impl NodeCache {
    /// Remaps a real hash equal to [`HASH_NONE`] to `0`.
    #[inline]
    fn avoid_sentinel(h: u64) -> u64 {
        if h == HASH_NONE { 0 } else { h }
    }

    #[inline]
    fn insert_hashed(&mut self, node: NodeId, h: u64) {
        self.table
            .insert_unique(h, node, |&existing| self.node_hashes[existing]);
        self.node_hashes[node] = h;
    }

    pub(crate) fn get_or_alloc<N, V, C: NodeCacheable<N, V>>(
        &mut self,
        store: &mut RawStore<N, V>,
        kind: N,
        inputs: SmallVec<[ValueId; 4]>,
        outputs: SmallVec<[V; 4]>,
    ) -> NodeId {
        if !C::should_cache(&kind) {
            return store.alloc_node(kind, inputs, outputs);
        }

        // Candidates are compared by re-reading the store, so no key is stored.
        let h = Self::avoid_sentinel(C::hash(&kind, &inputs, &outputs));
        if let Some(&cand) = self
            .table
            .find(h, |&cand| C::eq(store, cand, &kind, &inputs, &outputs))
        {
            return cand;
        }

        let node = store.alloc_node(kind, inputs, outputs);
        self.insert_hashed(node, h);
        node
    }

    /// Re-canonicalizes a node whose inputs may have changed: `Some(twin)` is a
    /// cached structurally-equal node for the caller to merge `node` into;
    /// `None` means not cacheable, or `node` is now its own canonical entry.
    /// Touches no edges.
    pub(crate) fn canonicalize<N, V, C: NodeCacheable<N, V>>(
        &mut self,
        store: &RawStore<N, V>,
        node: NodeId,
    ) -> Option<NodeId>
    where
        V: Clone,
    {
        let kind = store.kind_of(node);
        if !C::should_cache(kind) {
            return None;
        }
        let inputs = store.input_values(node);
        let outputs = store.output_kinds(node);
        let h = Self::avoid_sentinel(C::hash(kind, &inputs, &outputs));
        if let Some(&twin) = self.table.find(h, |&cand| {
            cand != node && C::eq(store, cand, kind, &inputs, &outputs)
        }) {
            // A twin `rebuild` keyed alongside `node` would outlive the merge.
            self.invalidate(node);
            return Some(twin);
        }
        // Every edge or output edit invalidates first; a payload rewrite needs
        // `rebuild_cache`.
        debug_assert!(
            self.node_hashes[node] == HASH_NONE || self.node_hashes[node] == h,
            "canonicalize: node {node:?} has a stale stored hash \
             (a mutation verb changed its structure without invalidating)"
        );
        // Release builds re-key a stale hash.
        if self.node_hashes[node] != h {
            self.invalidate(node);
            self.insert_hashed(node, h);
        }
        None
    }

    #[cfg(any(test, feature = "test-injectors"))]
    pub(crate) fn contains(&self, node: NodeId) -> bool {
        self.node_hashes[node] != HASH_NONE
    }

    /// Drops the dedup entry for a node whose structure is about to change.
    pub(crate) fn invalidate(&mut self, node: NodeId) {
        let hash = self.node_hashes[node];
        if hash == HASH_NONE {
            return;
        }
        // A non-sentinel stored hash means `node` is in `table` under it.
        self.table
            .find_entry(hash, |&n| n == node)
            .expect("node_hashes records a hash ⇒ the node is in the dedup table")
            .remove();
        self.node_hashes[node] = HASH_NONE;
    }

    /// Re-keys the whole cache, after compaction or a payload rewrite.
    pub(crate) fn rebuild<N, V, C: NodeCacheable<N, V>>(&mut self, store: &RawStore<N, V>)
    where
        V: Clone,
    {
        self.table.clear();
        self.node_hashes.clear();
        for node in store.nodes.keys() {
            let kind = store.kind_of(node);
            if !C::should_cache(kind) {
                continue;
            }
            let hash = Self::avoid_sentinel(C::hash(
                kind,
                &store.input_values(node),
                &store.output_kinds(node),
            ));
            // Unconditional: structurally equal twins (left by rewiring until
            // the next canonicalize) may share a key; a lookup takes either.
            self.insert_hashed(node, hash);
        }
    }
}
