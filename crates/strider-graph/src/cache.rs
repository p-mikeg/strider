use cranelift_entity::SecondaryMap;
use hashbrown::HashTable;
use smallvec::SmallVec;

use crate::ids::{NodeId, ValueId};
use crate::storage::RawStore;

/// The node-creation policy: three stateless hooks deciding dedup-or-create.
/// A caching impl's `Hash`/`PartialEq` bounds live inside its own method
/// bodies, never on the trait or on `Graph`.
///
/// [`should_cache`](Self::should_cache) defaults to `false`, which gates
/// [`hash`](Self::hash)/[`eq`](Self::eq) so their `unreachable!` defaults are
/// never reached: a non-caching policy is a single empty `impl` block.
pub trait NodeCacheable<N, V> {
    fn should_cache(_kind: &N) -> bool {
        false
    }

    /// Structural hash of a `(kind, inputs, output-kinds)` key. May return any
    /// `u64` including `u64::MAX`; sentinel avoidance is the cache's concern.
    fn hash(_kind: &N, _inputs: &[ValueId], _outputs: &[V]) -> u64 {
        unreachable!("hash() called on a policy whose should_cache() returned false")
    }

    /// Whether `cand` equals the `(kind, inputs, outputs)` key, resolved by
    /// re-reading `cand`'s current structure out of the store.
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
            // `SecondaryMap::clear` preserves this default, so a cleared map
            // still reports `HASH_NONE` for every (re-)defaulted slot.
            node_hashes: SecondaryMap::with_default(HASH_NONE),
        }
    }
}

impl NodeCache {
    /// Remaps a real hash that collides with [`HASH_NONE`] to `0`, so it can
    /// never be read back as "absent". Stays deterministic (equal keys still
    /// hash equal) at the cost of one extra collision, which the re-read eq
    /// absorbs.
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

        // Probe by hash, then re-read each candidate from the store for an
        // exact structural compare, so no owned key payloads are ever stored.
        // The sentinel is remapped here, inside the mechanism: the policy's
        // `hash` knows nothing about it.
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

    /// Re-canonicalize an existing node whose inputs may have changed.
    ///
    /// `Some(twin)` means a structurally-equal other cacheable node is already
    /// cached and the caller should merge `node` into it. `None` means the node
    /// is not cacheable, or has no twin and was (re-)inserted as its own
    /// canonical representative. Touches no edges; the merge is the caller's.
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
            return Some(twin);
        }
        // No twin: `node` becomes its own canonical entry. Changing its inputs
        // evicted it (hash == HASH_NONE), so re-insert. A stored hash instead
        // means the structure is unchanged since it was last cached, because
        // every mutation verb invalidates first; one that did not would
        // mislocate the node here and only blow up later in `invalidate`.
        debug_assert!(
            self.node_hashes[node] == HASH_NONE || self.node_hashes[node] == h,
            "canonicalize: node {node:?} has a stale stored hash \
             (a mutation verb changed its structure without invalidating)"
        );
        if self.node_hashes[node] == HASH_NONE {
            self.insert_hashed(node, h);
        }
        None
    }

    /// Drops the dedup entry for a node whose structure is about to change.
    pub(crate) fn invalidate(&mut self, node: NodeId) {
        let hash = self.node_hashes[node];
        if hash == HASH_NONE {
            return;
        }
        // The stored hash is the SSoT for membership: non-sentinel means `node`
        // is in `table` under exactly that hash (every insert sets it, and the
        // only other place it is cleared is `rebuild`, which drops the table
        // with it), so the bucket walk cannot miss.
        self.table
            .find_entry(hash, |&n| n == node)
            .expect("node_hashes records a hash ⇒ the node is in the dedup table")
            .remove();
        self.node_hashes[node] = HASH_NONE;
    }

    /// Re-keys the whole cache over the surviving nodes after ids are
    /// renumbered (compaction) or cacheable payloads are rewritten.
    pub(crate) fn rebuild<N, V, C: NodeCacheable<N, V>>(&mut self, store: &RawStore<N, V>)
    where
        V: Clone,
    {
        self.table.clear();
        // `clear` preserves the `HASH_NONE` default: every slot reverts to
        // "absent" until re-inserted below.
        self.node_hashes.clear();
        for node in store.node_ids() {
            let kind = store.kind_of(node);
            if !C::should_cache(kind) {
                continue;
            }
            let hash = Self::avoid_sentinel(C::hash(
                kind,
                &store.input_values(node),
                &store.output_kinds(node),
            ));
            // Inserted unconditionally: "at most one node per structural key"
            // is NOT an invariant of the table. Both multi-occupancy cases are
            // sound:
            //   * distinct nodes colliding on one hash. The bucket holds each
            //     and lookup re-reads structure.
            //   * structurally equal twins, which rewiring a live node's inputs
            //     creates until the next canonicalize. A lookup resolves to
            //     whichever the walk hits first, and twins compute the same
            //     value.
            self.insert_hashed(node, hash);
        }
    }
}
