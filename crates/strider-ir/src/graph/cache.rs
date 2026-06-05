//! [`IrCacheable`] — the IR's dedup-or-create policy for the generic
//! [`strider_graph::Graph`].
//!
//! Ports the former `Graph::create_node` dedup cache (the
//! `(NodeKind, inputs, output_kinds)` → `NodeId` map plus the `is_cacheable`
//! gate) onto the generic [`strider_graph::NodeCacheable`] hooks:
//!
//! - `create` — normalises an `IntConst` payload to its declared width, then
//!   either returns an existing structurally-equal node or allocates a fresh
//!   one. This is the single source of truth for `IntConst` payload masking
//!   (a big-endian read can mint `IntConst(0xff..fc):I64` while another path
//!   mints the 64-bit-masked form; both are `-4:I64` and must dedup).
//! - `invalidate` — drops the dedup entry for a node whose input/output
//!   structure is about to change (so a `create` with the pre-change key can't
//!   resurrect the now-different node). Called by the generic mutation verbs
//!   before they mutate.
//! - `rebuild` — re-keys the whole cache over the surviving nodes after
//!   [`strider_graph::Graph::retain_reachable`] renumbers ids, and is re-run by
//!   [`crate::Function::compact`] after the wide-const GC rewrites
//!   `IntConstWide` payloads (which change those nodes' cache keys).
//!
//! # Data structure: hash-on-demand
//!
//! The cache stores no owned key payloads.  It is a [`hashbrown::HashTable`] of
//! bare [`NodeId`]s located by their structural hash, paired with a
//! [`SecondaryMap`] caching each cacheable node's hash (so eviction is O(1) —
//! no need to re-read and re-hash the node's structure to find its bucket).
//! Equality is resolved by *re-reading* the candidate's `(kind, inputs,
//! output_kinds)` back out of the [`RawStore`] and comparing against the query,
//! so two structurally-distinct nodes that collide on the same hash coexist
//! peacefully (lookup walks the bucket and re-reads each candidate).  This
//! mirrors the cranelift / spidir `NodeCache` (`HashTable<Node>` +
//! `SecondaryMap<Node, hash>`).

use std::hash::{Hash, Hasher};

use cranelift_entity::SecondaryMap;
use hashbrown::HashTable;
use rustc_hash::FxHasher;
use smallvec::SmallVec;
use strider_graph::{NodeCacheable, NodeId, RawStore, ValueId};

use crate::node::{NodeKind, ValueKind, ValueType};

/// Sentinel `node_hashes` value meaning "this node is not in the dedup table".
/// `u64::MAX` is used rather than `0` because `0` is a perfectly valid hash for
/// a cached node, so it can't double as "absent".
const HASH_NONE: u64 = u64::MAX;

/// Hashes a `(kind, inputs, output_kinds)` structural key into a `u64`.
///
/// The fields are hashed in declaration order (`kind`, then the input-value
/// slice, then the output-kind slice).  `[T]: Hash` hashes the length followed
/// by each element, so hashing a borrowed query slice and hashing a node's
/// re-read `SmallVec` of the same contents agree element-for-element — which is
/// what lets a query probe land in the same bucket the node was inserted under.
///
/// The result is guaranteed never to equal [`HASH_NONE`]: a cacheable node's
/// stored hash doubles as its "present in the table" flag, so a real hash that
/// collided with the sentinel would make `invalidate` skip the node's eviction.
/// Remapping the lone `HASH_NONE` value to `0` keeps the hash deterministic
/// (equal keys still hash equal) at the cost of one extra collision in the
/// vanishingly rare case, which the re-read equality check absorbs.
#[inline]
fn hash_key(kind: &NodeKind, inputs: &[ValueId], outputs: &[ValueKind]) -> u64 {
    let mut h = FxHasher::default();
    kind.hash(&mut h);
    inputs.hash(&mut h);
    outputs.hash(&mut h);
    let hash = h.finish();
    if hash == HASH_NONE { 0 } else { hash }
}

/// The IR's deduplication cache: deduplicates cacheable node kinds (see
/// [`NodeKind::is_cacheable`]) by their `(NodeKind, inputs, output_kinds)`
/// structure, storing no owned key payloads.
///
/// Non-cacheable kinds (`Region`, `Phi`, `MemPhi`, `Call`, …) are never
/// inserted — they always allocate a fresh node.
#[derive(Clone)]
pub struct IrCacheable {
    /// Deduplicated `NodeId`s, located by their structural hash.  A bucket can
    /// hold several distinct nodes that collide on the same hash; equality is
    /// resolved by re-reading each candidate from the store.
    table: HashTable<NodeId>,
    /// Per-node cached structural hash, defaulting to [`HASH_NONE`] for nodes
    /// not in `table` (non-cacheable kinds, or cacheable nodes that were
    /// evicted by `invalidate`).  Lets `invalidate` locate a node's bucket in
    /// O(1) without re-reading and re-hashing its structure.
    node_hashes: SecondaryMap<NodeId, u64>,
}

impl Default for IrCacheable {
    fn default() -> Self {
        Self {
            table: HashTable::new(),
            // `SecondaryMap::clear` preserves this default, so a cleared map
            // still reports `HASH_NONE` for every (re-)defaulted slot.
            node_hashes: SecondaryMap::with_default(HASH_NONE),
        }
    }
}

impl IrCacheable {
    /// Normalises an `IntConst` payload by masking it to its declared integer
    /// output type's bit width, so every creation path (lifter sub-register
    /// read, rewrite closure, `build_int_const`, …) keys the cache on the same
    /// canonical narrow payload.
    ///
    /// Only the narrow integer `Typed` case is touched: wide constants
    /// (`I256`/`I512`) flow through `IntConstWide`, and non-integer /
    /// non-value outputs are left alone.
    fn normalize_kind(kind: NodeKind, outputs: &[ValueKind]) -> NodeKind {
        match (kind, outputs) {
            (NodeKind::IntConst(v), [ValueKind::Typed(ty)])
                if ty.is_integer() && !matches!(ty, ValueType::I256 | ValueType::I512) =>
            {
                NodeKind::IntConst(v & ty.bit_mask_u128())
            }
            (kind, _) => kind,
        }
    }

    /// Re-reads candidate node `cand` from the store and reports whether its
    /// stored `(kind, inputs, output_kinds)` structure equals the query.  This
    /// is the equality half of the hash-on-demand probe: no owned key payloads
    /// are kept, so structural identity is recomputed from the live store.
    #[inline]
    fn eq_key(
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

impl NodeCacheable<NodeKind, ValueKind> for IrCacheable {
    fn create(
        &mut self,
        store: &mut RawStore<NodeKind, ValueKind>,
        kind: NodeKind,
        inputs: SmallVec<[ValueId; 4]>,
        outputs: SmallVec<[ValueKind; 4]>,
    ) -> NodeId {
        // IR-specific normalisation FIRST, so equal-mod-width `IntConst`s hash
        // (and therefore dedup) identically regardless of which creation path
        // minted the raw payload.
        let kind = Self::normalize_kind(kind, &outputs);

        if !kind.is_cacheable() {
            return store.alloc_node(kind, inputs, outputs);
        }

        // Hash the borrowed query, then probe the bucket.  On a hit the
        // candidate is re-read from the store for an exact structural compare,
        // so no owned key payloads are ever allocated or stored.
        let hash = hash_key(&kind, &inputs, &outputs);
        if let Some(&cand) = self
            .table
            .find(hash, |&cand| Self::eq_key(store, cand, &kind, &inputs, &outputs))
        {
            return cand;
        }

        // Miss: allocate a fresh node and record it under its hash.  The
        // rehash closure recovers an existing entry's hash from `node_hashes`
        // (every entry already in the table has a non-sentinel hash there).
        let node = store.alloc_node(kind, inputs, outputs);
        self.table
            .insert_unique(hash, node, |&existing| self.node_hashes[existing]);
        self.node_hashes[node] = hash;
        node
    }

    fn invalidate(&mut self, node: NodeId, _store: &RawStore<NodeKind, ValueKind>) {
        let hash = self.node_hashes[node];
        if hash == HASH_NONE {
            // Not in the table (non-cacheable kind, or already evicted) — the
            // stored hash is the single source of truth for membership, so a
            // sentinel here means there is nothing to remove.
            return;
        }
        // Invariant: a non-sentinel `node_hashes[node]` means `node` is present
        // in `table` under exactly that hash (every `create` insert sets the
        // hash, and `invalidate` is the only place it is cleared).  So the
        // bucket walk for `n == node` cannot miss.
        self.table
            .find_entry(hash, |&n| n == node)
            .expect("node_hashes records a hash ⇒ the node is in the dedup table")
            .remove();
        self.node_hashes[node] = HASH_NONE;
    }

    fn rebuild(&mut self, store: &RawStore<NodeKind, ValueKind>) {
        self.table.clear();
        // `clear` preserves the `HASH_NONE` default, so every slot reverts to
        // "absent" until re-inserted below.
        self.node_hashes.clear();
        for node in store.node_ids() {
            let kind = *store.kind_of(node);
            if !kind.is_cacheable() {
                continue;
            }
            let hash = hash_key(&kind, &store.input_values(node), &store.output_kinds(node));
            // Structurally-distinct nodes that share a hash coexist: the bucket
            // holds each one and lookup re-reads for equality.  (The old owned-
            // key map used `or_insert`, which silently dropped a colliding
            // distinct key; this is strictly more correct, and identical for
            // structurally-equal nodes — reachable duplicates are already
            // deduped, so no real collision survives into here.)
            self.table
                .insert_unique(hash, node, |&existing| self.node_hashes[existing]);
            self.node_hashes[node] = hash;
        }
    }
}
