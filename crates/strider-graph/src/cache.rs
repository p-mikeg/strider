//! The dedup cache: the stateless node-creation policy ([`NodeCacheable`]) plus
//! the generic mechanism ([`NodeCache`]) it drives.
//!
//! The mechanism owns the table, per-node hashes, eviction, and rebuild. The
//! policy supplies only the three decisions it cannot make for itself: whether
//! a kind caches, how to hash a structural key, and how to compare a stored
//! candidate against a query. All three hooks are associated functions (no
//! `self`): the policy is a stateless ZST.
//!
//! `Graph<N, V, C>` imposes NO `Hash`/`Eq`/`Copy` bound on `N`/`V`, and neither
//! does this trait. A caching impl's [`hash`] needs `N, V: Hash` and its [`eq`]
//! needs `N, V: PartialEq`, but those bounds live inside that concrete impl's
//! method bodies, never as trait bounds and never on `Graph`. That is what lets
//! a payload containing e.g. `Box<dyn Fn>` be stored, as long as it goes
//! through [`NeverCacheable`].
//!
//! # `NodeCache`: hash-on-demand
//!
//! The cache stores no owned key payloads: a [`hashbrown::HashTable`] of bare
//! [`NodeId`]s located by structural hash, plus a [`SecondaryMap`] caching each
//! cacheable node's hash so eviction is O(1) (no re-read and re-hash to find
//! the bucket). Equality is resolved by re-reading the candidate's `(kind,
//! inputs, output-kinds)` back out of the [`RawStore`], so structurally
//! distinct nodes that collide on one hash coexist: lookup walks the bucket and
//! re-reads each candidate. Mirrors the cranelift / spidir `NodeCache`.
//!
//! [`hash`]: NodeCacheable::hash
//! [`eq`]: NodeCacheable::eq

use cranelift_entity::SecondaryMap;
use hashbrown::HashTable;
use smallvec::SmallVec;

use crate::ids::{NodeId, ValueId};
use crate::storage::RawStore;

/// The node-creation policy: three stateless hooks the generic `NodeCache`
/// consults to decide dedup-or-create.
///
/// The defaults make a non-caching policy a single empty `impl` block:
/// [`should_cache`](Self::should_cache) is `false`, which gates
/// [`hash`](Self::hash)/[`eq`](Self::eq) so their `unreachable!` defaults are
/// never reached.
pub trait NodeCacheable<N, V> {
    fn should_cache(_kind: &N) -> bool {
        false
    }

    /// Structural hash of a `(kind, inputs, output-kinds)` key.
    ///
    /// A caching impl's `N, V: Hash` bound lives on THIS method, not the trait.
    /// May return any `u64` including `u64::MAX`; sentinel avoidance is the
    /// cache's concern, not the policy's.
    fn hash(_kind: &N, _inputs: &[ValueId], _outputs: &[V]) -> u64 {
        unreachable!("hash() called on a policy whose should_cache() returned false")
    }

    /// Whether `cand` equals the `(kind, inputs, outputs)` key, resolved by
    /// re-reading `cand`'s current structure out of the store. A caching impl's
    /// `N, V: PartialEq` bound lives on THIS method, not the trait.
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

/// Never deduplicates: every node allocates fresh.
///
/// Imposes no bound on `N`/`V` at all, so a graph parameterised with it can
/// hold payloads that are neither `Hash` nor `Eq` (e.g. one carrying
/// `Box<dyn Fn>`).
#[derive(Clone, Copy, Default)]
pub struct NeverCacheable;

impl<N, V> NodeCacheable<N, V> for NeverCacheable {}

/// `node_hashes` sentinel for "not in the dedup table". `u64::MAX` rather than
/// `0` because `0` is a valid hash, so it cannot double as "absent".
const HASH_NONE: u64 = u64::MAX;

/// Hash-on-demand dedup table of [`NodeId`]s plus a per-node cached structural
/// hash. Owns no key payloads.
///
/// Not parameterised by the policy `C`: every method that needs it takes `C` as
/// a method-level type parameter, so one `NodeCache` serves a graph regardless
/// of which stateless policy ZST drives it.
#[derive(Clone)]
pub(crate) struct NodeCache {
    /// A bucket can hold several distinct nodes colliding on one hash;
    /// equality is resolved by re-reading each candidate from the store.
    table: HashTable<NodeId>,
    /// Defaults to [`HASH_NONE`] for nodes not in `table` (non-cacheable kinds,
    /// or entries evicted by [`invalidate`](Self::invalidate)). Lets
    /// `invalidate` locate a bucket in O(1) without re-hashing the structure.
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
    /// A stored hash doubles as the "present in the table" flag, so a real hash
    /// equal to [`HASH_NONE`] would make `invalidate` skip the eviction.
    /// Remapping it to `0` stays deterministic (equal keys still hash equal) at
    /// the cost of one extra collision, which the re-read eq absorbs.
    #[inline]
    fn avoid_sentinel(h: u64) -> u64 {
        if h == HASH_NONE { 0 } else { h }
    }

    /// Shared miss-insert tail of `get_or_alloc`, `canonicalize`, and
    /// `rebuild`. The rehash closure recovers an existing entry's hash from
    /// `node_hashes`, where every table entry already has a non-sentinel hash.
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

    /// Re-canonicalize an existing node whose inputs may have changed: the dual
    /// of [`get_or_alloc`](Self::get_or_alloc) for a node already in the store.
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
        // Exclude `node` itself.
        if let Some(&twin) = self.table.find(h, |&cand| {
            cand != node && C::eq(store, cand, kind, &inputs, &outputs)
        }) {
            return Some(twin);
        }
        // No twin: `node` becomes its own canonical entry. Changing its inputs
        // evicted it (hash == HASH_NONE), so re-insert.
        //
        // Reaching here with a hash still stored means the structure is
        // unchanged since it was last cached, because every mutation verb
        // invalidates first. A future verb that mutates inputs WITHOUT
        // invalidating would land here with a stale hash != h, mislocate the
        // node, and only blow up later in `invalidate`'s `expect`; the assert
        // turns that silent breach into an immediate failure.
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

    /// Drops the dedup entry for a node whose structure is about to change, so
    /// a later `get_or_alloc` of the pre-change key cannot resurrect the
    /// now-different node.
    ///
    /// O(1): the bucket is found via the cached hash, with no need to re-read
    /// the (possibly already-mutated) structure.
    pub(crate) fn invalidate(&mut self, node: NodeId) {
        let hash = self.node_hashes[node];
        if hash == HASH_NONE {
            return;
        }
        // The stored hash is the SSoT for membership: non-sentinel means `node`
        // is in `table` under exactly that hash (every insert sets it, and this
        // is the only place it is cleared), so the bucket walk cannot miss.
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
        // `clear` preserves the `HASH_NONE` default, so every slot reverts to
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
            // is NOT an invariant of the table. Two multi-occupancy cases are
            // both sound:
            //   * distinct nodes colliding on one hash. The bucket holds each
            //     and lookup re-reads structure (an owned-key map using
            //     `or_insert` would silently drop the colliding distinct key).
            //   * structurally equal twins. Rewiring a live node's inputs can
            //     transiently make it a twin of an existing node until the next
            //     canonicalize. A lookup resolves to whichever the walk hits
            //     first, which is equivalent since twins compute the same value.
            self.insert_hashed(node, hash);
        }
    }
}
