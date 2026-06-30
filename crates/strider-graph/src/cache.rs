//! The dedup cache: the stateless node-creation *policy* ([`NodeCacheable`])
//! plus the generic *mechanism* ([`NodeCache`]) it drives.
//!
//! [`NodeCacheable`] is the strider-agnostic policy through which
//! [`crate::graph::Graph::create_node`] turns a `(payload, inputs, outputs)`
//! triple into a [`NodeId`]. The cache mechanism — the table, per-node hashes,
//! eviction, and rebuild — lives below in [`NodeCache`]; the policy supplies
//! only the three decisions that mechanism cannot make for itself: whether a
//! kind caches, how to hash a structural key, and how to compare a stored
//! candidate against a query.
//!
//! All three hooks are ASSOCIATED FUNCTIONS (no `self`): the policy is a
//! stateless ZST, and every bit of state (the dedup table) is owned by
//! [`NodeCache`].
//!
//! CRITICAL: the `Graph<N, V, C>` struct imposes NO `Hash`/`Eq`/`Copy` bound
//! on `N`/`V`, and neither does this trait. A *caching* impl's [`hash`] needs
//! `N, V: Hash` and its [`eq`] needs `N, V: PartialEq`, but those bounds live
//! ONLY inside that concrete impl's method bodies — never as trait bounds,
//! never on `Graph`. That is what lets payloads containing e.g. `Box<dyn Fn>`
//! still be stored, as long as they go through [`NeverCacheable`] (whose
//! [`should_cache`] is `false`, so [`hash`]/[`eq`] are never reached).
//!
//! # `NodeCache` data structure: hash-on-demand
//!
//! [`NodeCache`] stores no owned key payloads. It is a [`hashbrown::HashTable`]
//! of bare [`NodeId`]s located by their structural hash, paired with a
//! [`SecondaryMap`] caching each cacheable node's hash (so eviction is O(1) —
//! no need to re-read and re-hash the node's structure to find its bucket).
//! Equality is resolved by *re-reading* the candidate's `(kind, inputs,
//! output-kinds)` back out of the [`RawStore`] (via [`NodeCacheable::eq`]) and
//! comparing against the query, so two structurally-distinct nodes that collide
//! on the same hash coexist peacefully (lookup walks the bucket and re-reads
//! each candidate). This mirrors the cranelift / spidir `NodeCache`
//! (`HashTable<Node>` + `SecondaryMap<Node, hash>`).
//!
//! [`hash`]: NodeCacheable::hash
//! [`eq`]: NodeCacheable::eq
//! [`should_cache`]: NodeCacheable::should_cache

use cranelift_entity::SecondaryMap;
use hashbrown::HashTable;
use smallvec::SmallVec;

use crate::ids::{NodeId, ValueId};
use crate::storage::RawStore;

/// The node-creation policy: three stateless hooks (all with defaults) that the
/// generic `NodeCache` consults to decide dedup-or-create.
///
/// A dedup cache keyed on a node's structure goes STALE when the graph mutates
/// that structure underneath it (an input is rewritten, an edge is detached) or
/// when ids are renumbered by compaction. The generic cache evicts and rebuilds
/// using [`hash`](Self::hash)/[`eq`](Self::eq); a non-caching policy whose
/// [`should_cache`](Self::should_cache) is `false` pays nothing for any of it.
///
/// The default impls make a *non-caching* policy a single empty `impl` block:
/// [`should_cache`] is `false`, and [`hash`]/[`eq`] are `unreachable!` (never
/// reached because `should_cache` gates them).
///
/// [`should_cache`]: Self::should_cache
/// [`hash`]: Self::hash
/// [`eq`]: Self::eq
pub trait NodeCacheable<N, V> {
    /// Whether this kind participates in dedup. Default: `false` (never cache).
    ///
    /// When this returns `false`, [`hash`](Self::hash)/[`eq`](Self::eq) are
    /// never reached for the kind, so their `unreachable!` defaults are safe.
    fn should_cache(_kind: &N) -> bool {
        false
    }

    /// Structural hash of a `(kind, inputs, output-kinds)` key.
    ///
    /// Default: `unreachable!` — a non-caching policy never reaches it because
    /// [`should_cache`](Self::should_cache) gates the call. A caching impl
    /// supplies a real hash here (its `N, V: Hash` bound lives on THIS method,
    /// not on the trait). The returned value may be any `u64`, including
    /// `u64::MAX`; sentinel avoidance is the generic cache's concern, not the
    /// policy's.
    fn hash(_kind: &N, _inputs: &[ValueId], _outputs: &[V]) -> u64 {
        unreachable!("hash() called on a policy whose should_cache() returned false")
    }

    /// Whether the stored candidate node `cand` equals the
    /// `(kind, inputs, outputs)` key, resolved by RE-READING `cand`'s current
    /// structure out of the store.
    ///
    /// Default: `unreachable!` — same gating as [`hash`](Self::hash). A caching
    /// impl supplies a real structural compare here (its `N, V: PartialEq`
    /// bound lives on THIS method, not on the trait).
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

/// A policy that never deduplicates: every node allocates fresh.
///
/// Imposes no bound on `N`/`V` whatsoever (all three trait hooks keep their
/// defaults, and `should_cache` is `false` so `hash`/`eq` are never reached),
/// so a graph parameterised with `NeverCacheable` can hold payloads that are
/// neither `Hash` nor `Eq` (e.g. a pattern payload carrying `Box<dyn Fn>`).
#[derive(Clone, Copy, Default)]
pub struct NeverCacheable;

impl<N, V> NodeCacheable<N, V> for NeverCacheable {}

/// Sentinel `node_hashes` value meaning "this node is not in the dedup table".
/// `u64::MAX` is used rather than `0` because `0` is a perfectly valid hash for
/// a cached node, so it can't double as "absent".
const HASH_NONE: u64 = u64::MAX;

/// The generic deduplication cache: a hash-on-demand table of [`NodeId`]s plus
/// a per-node cached structural hash. Owns no key payloads.
///
/// The cache is NOT parameterised by the policy `C` — every method that needs
/// the policy takes `C` as a method-level type parameter instead, so one
/// `NodeCache` value serves a graph regardless of which (stateless) policy ZST
/// drives it.
#[derive(Clone)]
pub(crate) struct NodeCache {
    /// Deduplicated `NodeId`s, located by their structural hash. A bucket can
    /// hold several distinct nodes that collide on the same hash; equality is
    /// resolved by re-reading each candidate from the store via the policy's
    /// [`NodeCacheable::eq`].
    table: HashTable<NodeId>,
    /// Per-node cached structural hash, defaulting to [`HASH_NONE`] for nodes
    /// not in `table` (non-cacheable kinds, or cacheable nodes that were
    /// evicted by [`invalidate`](Self::invalidate)). Lets `invalidate` locate a
    /// node's bucket in O(1) without re-reading and re-hashing its structure.
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
    /// Remaps the lone [`HASH_NONE`] value to `0`, so a real structural hash can
    /// never collide with the "absent" sentinel.
    ///
    /// A cacheable node's stored hash doubles as its "present in the table"
    /// flag, so a real hash that equalled the sentinel would make `invalidate`
    /// skip the node's eviction. Remapping keeps the hash deterministic (equal
    /// keys still hash equal) at the cost of one extra collision in the
    /// vanishingly rare case, which the re-read equality check absorbs.
    #[inline]
    fn avoid_sentinel(h: u64) -> u64 {
        if h == HASH_NONE { 0 } else { h }
    }

    /// Records `node` in the dedup table under structural hash `h` and caches
    /// `h` in `node_hashes`.  The shared miss-insert tail of `get_or_alloc`,
    /// `canonicalize`, and `rebuild`.
    ///
    /// The rehash closure recovers an existing entry's hash from
    /// `node_hashes` (every entry already in the table has a non-sentinel hash
    /// there).  Disjoint-field borrow: `self.table` is `&mut` while the
    /// closure borrows `self.node_hashes` — proven safe by the borrow checker.
    #[inline]
    fn insert_hashed(&mut self, node: NodeId, h: u64) {
        self.table
            .insert_unique(h, node, |&existing| self.node_hashes[existing]);
        self.node_hashes[node] = h;
    }

    /// Gate on `should_cache`, hash (sentinel-avoided), probe via `C::eq`,
    /// then either return the existing structurally-equal node or allocate a
    /// fresh one and insert it.
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

        // Hash the borrowed query, then probe the bucket. On a hit the
        // candidate is re-read from the store for an exact structural compare,
        // so no owned key payloads are ever allocated or stored. The lone
        // `u64::MAX` sentinel is remapped here, INSIDE the mechanism — the
        // policy's `hash` knows nothing about it.
        let h = Self::avoid_sentinel(C::hash(&kind, &inputs, &outputs));
        if let Some(&cand) = self
            .table
            .find(h, |&cand| C::eq(store, cand, &kind, &inputs, &outputs))
        {
            return cand;
        }

        // Miss: allocate a fresh node and record it under its hash.
        let node = store.alloc_node(kind, inputs, outputs);
        self.insert_hashed(node, h);
        node
    }

    /// Re-canonicalize an EXISTING node whose inputs may have changed — the
    /// dual of [`get_or_alloc`](Self::get_or_alloc) for a node already in the
    /// store.
    ///
    /// Returns `Some(twin)` if a structurally-equal OTHER cacheable node is
    /// already cached (the caller merges `node` into `twin`). Returns `None` if
    /// the node is not cacheable, or if no twin exists — in which case `node` is
    /// (re-)inserted as its own canonical representative. Touches no edges; the
    /// merge itself is the caller's job.
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
        // Probe for a structurally-equal OTHER node (exclude `node` itself).
        if let Some(&twin) = self.table.find(h, |&cand| {
            cand != node && C::eq(store, cand, kind, &inputs, &outputs)
        }) {
            return Some(twin);
        }
        // No twin: ensure `node` is its own canonical entry. It was evicted when
        // its inputs changed (hash == HASH_NONE), so (re-)insert it now.
        //
        // If the node is still present (hash != HASH_NONE), every structural
        // mutation verb must have invalidated it first, which drives the stored
        // hash to HASH_NONE — so the only way to reach here with a stored hash is
        // for that hash to still equal the freshly-recomputed `h` (i.e. the
        // structure is unchanged from when it was last cached). A future verb
        // that mutates inputs without invalidating would land here with a STALE
        // stored hash != h, mislocate the node, and only blow up later in
        // `invalidate`'s `expect`. This debug-assert turns that silent
        // invariant breach into a loud, immediate dev/test failure.
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

    /// Drops the dedup entry for a node whose input/output structure is about
    /// to change, so a later `get_or_alloc` of the pre-change key can't
    /// resurrect the now-different node.
    ///
    /// O(1): the node's bucket is located via its cached hash in `node_hashes`,
    /// with no need to re-read or re-hash its (possibly already-mutated)
    /// structure.
    pub(crate) fn invalidate(&mut self, node: NodeId) {
        let hash = self.node_hashes[node];
        if hash == HASH_NONE {
            // Not in the table (non-cacheable kind, or already evicted) — the
            // stored hash is the single source of truth for membership, so a
            // sentinel here means there is nothing to remove.
            return;
        }
        // Invariant: a non-sentinel `node_hashes[node]` means `node` is present
        // in `table` under exactly that hash (every insert sets the hash, and
        // `invalidate` is the only place it is cleared). So the bucket walk for
        // `n == node` cannot miss.
        self.table
            .find_entry(hash, |&n| n == node)
            .expect("node_hashes records a hash ⇒ the node is in the dedup table")
            .remove();
        self.node_hashes[node] = HASH_NONE;
    }

    /// Re-keys the whole cache over the surviving nodes after the graph
    /// renumbers ids (e.g. compaction) or rewrites cacheable payloads.
    ///
    /// Clears the table + per-node hashes, then re-hashes and re-inserts every
    /// cacheable node from its current stored structure.
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
            // Every cacheable survivor is inserted unconditionally — `rebuild`
            // does NOT enforce "at most one node per structural key". Two kinds
            // of multi-occupancy are possible and both are sound:
            //   * hash collision between structurally-DISTINCT nodes — the bucket
            //     holds each, and lookup re-reads structure for equality (an
            //     owned-key map using `or_insert` would silently drop a colliding
            //     distinct key; re-reading is strictly more correct);
            //   * structurally-EQUAL twins — a rewrite that rewires a live node's
            //     inputs can transiently turn it into a twin of an existing node
            //     (the consumer is re-canonicalized at the next `clean()` drain,
            //     but until then both live in the bucket). A lookup resolves to
            //     whichever the walk hits first, which is semantically identical
            //     since twins compute the same value. So single-key-uniqueness is
            //     NOT an invariant of the table itself.
            self.insert_hashed(node, hash);
        }
    }
}
