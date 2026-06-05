//! The node-creation policy: four stateless hooks that drive the generic
//! dedup cache.
//!
//! [`NodeCacheable`] is the strider-agnostic *policy* through which
//! [`crate::graph::Graph::create_node`] turns a `(payload, inputs, outputs)`
//! triple into a [`NodeId`]. The cache *mechanism* — the table, per-node
//! hashes, eviction, and rebuild — lives entirely in the `node_cache` module;
//! the policy supplies only the four decisions that mechanism cannot make for
//! itself: how to canonicalize a kind, whether a kind caches, how to hash a
//! structural key, and how to compare a stored candidate against a query.
//!
//! All four hooks are ASSOCIATED FUNCTIONS (no `self`): the policy is a
//! stateless ZST, and every bit of state (the dedup table) is owned by the
//! generic `NodeCache` (in the `node_cache` module).
//!
//! CRITICAL: the `Graph<N, V, C>` struct imposes NO `Hash`/`Eq`/`Copy` bound
//! on `N`/`V`, and neither does this trait. A *caching* impl's [`hash`] needs
//! `N, V: Hash` and its [`eq`] needs `N, V: PartialEq`, but those bounds live
//! ONLY inside that concrete impl's method bodies — never as trait bounds,
//! never on `Graph`. That is what lets payloads containing e.g. `Box<dyn Fn>`
//! still be stored, as long as they go through [`NeverCacheable`] (whose
//! [`should_cache`] is `false`, so [`hash`]/[`eq`] are never reached).
//!
//! [`hash`]: NodeCacheable::hash
//! [`eq`]: NodeCacheable::eq
//! [`should_cache`]: NodeCacheable::should_cache

use crate::ids::{NodeId, ValueId};
use crate::storage::RawStore;

/// The node-creation policy: four stateless hooks (all with defaults) that the
/// generic `NodeCache` consults to decide dedup-or-create.
///
/// A dedup cache keyed on a node's structure goes STALE when the graph mutates
/// that structure underneath it (an input is rewritten, an edge is detached) or
/// when ids are renumbered by compaction. The generic cache evicts and rebuilds
/// using [`hash`](Self::hash)/[`eq`](Self::eq); a non-caching policy whose
/// [`should_cache`](Self::should_cache) is `false` pays nothing for any of it.
///
/// The default impls make a *non-caching* policy a single empty `impl` block:
/// [`canonicalize`](Self::canonicalize) is identity, [`should_cache`] is
/// `false`, and [`hash`]/[`eq`] are `unreachable!` (never reached because
/// `should_cache` gates them).
///
/// [`should_cache`]: Self::should_cache
/// [`hash`]: Self::hash
/// [`eq`]: Self::eq
pub trait NodeCacheable<N, V> {
    /// Canonicalizes a kind before it is hashed/stored, so semantically-equal
    /// nodes minted by different paths share one canonical form and therefore
    /// dedup together.
    ///
    /// Applied by `NodeCache::get_or_alloc` to EVERY node (cacheable or not)
    /// before it is allocated. Default: identity.
    fn canonicalize(kind: N, _inputs: &[ValueId], _outputs: &[V]) -> N {
        kind
    }

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
/// Imposes no bound on `N`/`V` whatsoever (all four trait hooks keep their
/// defaults, and `should_cache` is `false` so `hash`/`eq` are never reached),
/// so a graph parameterised with `NeverCacheable` can hold payloads that are
/// neither `Hash` nor `Eq` (e.g. a pattern payload carrying `Box<dyn Fn>`).
#[derive(Clone, Copy, Default)]
pub struct NeverCacheable;

impl<N, V> NodeCacheable<N, V> for NeverCacheable {}
