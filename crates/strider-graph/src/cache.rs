//! The node-creation policy: dedup-or-create.
//!
//! [`NodeCacheable`] is the single hook through which
//! [`crate::graph::Graph::create_node`] turns a `(payload, inputs, outputs)`
//! triple into a [`NodeId`]. An implementation may return an existing
//! structurally-equal node (dedup) or allocate a fresh one via
//! [`RawStore::alloc_node`].
//!
//! CRITICAL: the `Graph<N, V, C>` struct imposes NO `Hash`/`Eq` bound on
//! `N`/`V`. [`NeverCacheable`] adds none either — it always allocates. Only a
//! *caching* impl needs `N, V: Hash + Eq`, and that bound lives on THAT impl
//! (not here, not on `Graph`), so payloads containing e.g. `Box<dyn Fn>` can
//! still be stored as long as they go through [`NeverCacheable`] (or a custom
//! policy that doesn't compare them).

use smallvec::SmallVec;

use crate::ids::{NodeId, ValueId};
use crate::storage::RawStore;

/// The node-creation policy.
///
/// `create` either returns an existing structurally-equal node OR allocates a
/// fresh one via [`RawStore::alloc_node`]. Any `Hash`/`Eq` requirement lives
/// in the implementation, never on this trait or on [`crate::graph::Graph`].
pub trait NodeCacheable<N, V> {
    /// Returns an existing structurally-equal node, or allocates a fresh one
    /// via `store.alloc_node`.
    fn create(
        &mut self,
        store: &mut RawStore<N, V>,
        kind: N,
        inputs: SmallVec<[ValueId; 4]>,
        outputs: SmallVec<[V; 4]>,
    ) -> NodeId;
}

/// A policy that never deduplicates: every `create` allocates a fresh node.
///
/// Imposes no bound on `N`/`V` whatsoever, so a graph parameterised with
/// `NeverCacheable` can hold payloads that are neither `Hash` nor `Eq`.
#[derive(Clone, Copy, Default)]
pub struct NeverCacheable;

impl<N, V> NodeCacheable<N, V> for NeverCacheable {
    fn create(
        &mut self,
        store: &mut RawStore<N, V>,
        kind: N,
        inputs: SmallVec<[ValueId; 4]>,
        outputs: SmallVec<[V; 4]>,
    ) -> NodeId {
        store.alloc_node(kind, inputs, outputs)
    }
}
