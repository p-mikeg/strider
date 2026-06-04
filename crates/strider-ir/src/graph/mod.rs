//! Sea-of-nodes graph storage, dedup cache, use-list, and typed accessors.
//!
//! The implementation is split into three submodules along the contracts
//! that the validator's three checks each protect:
//!
//! - `store` — node arena, dedup cache, side-tables. Local-typing's input.
//! - `uses`  — bidirectional use-list bookkeeping. Use-list-consistency's contract.
//! - `access` — read-only typed accessors. Local-typing's lookup surface.
//!
//! All public API names live in this module via the original paths:
//! `ir::graph::Graph`, `ir::graph::Graph::create_node`, etc., regardless of
//! which submodule's `impl Graph { ... }` block defines each method.

use cranelift_entity::{ListPool, PrimaryMap};
use hashbrown::HashMap;

use crate::node::{
    Node, NodeId, UseData, UseId, ValueData, ValueId, ValueKind,
};

mod access;
mod compact;
pub(crate) mod iterators;
mod rewrite;
mod store;
mod uses;

pub use compact::NodeIdRemap;
pub(crate) use compact::SideTableRemap;

#[cfg(test)]
mod tests;

/// Bidirectional tracked-variable table (`VarId ↔ Vn`): the forward
/// `VarId → Vn` map plus its `Vn → VarId` reverse index, kept consistent by
/// construction.  An [`entity_utils::EntityInterner`] — `intern` is the sole
/// mutator (writes both halves), `key_of`/`get` resolve either direction in
/// O(1), and `keys()`/`values()` iterate in insertion (`VarId`) order for
/// the consumers that need ABI slot order.
///
/// This is a **build-time-only** type: it lives on the
/// [`crate::FunctionBuilder`] for SSA bookkeeping while the function is
/// being constructed.  It is **not** stored on the finished
/// [`crate::Function`] — the post-build varnode record is the ordered
/// `crate::Function::all_vns` list (snapshotted from this table in
/// `new`, one entry per tracked variable) instead.
pub(crate) type VarTable = entity_utils::EntityInterner<crate::builder::VarId, rsleigh::Vn>;

/// The core IR graph structure.
///
/// Stores nodes, their input/output slots, and a deduplication cache for
/// cacheable node kinds.  All ids (node, output, input) are small integers
/// allocated from dense entity maps, so they can be used as cheap, copyable
/// handles.
///
/// `Graph` is the pure structural arena: nodes, edges, wide-const interning,
/// the dedup cache, and the generation counter.  Per-function overlay state
/// (the six `NodeId`-keyed side tables: asm fingerprints, phi var tags,
/// stack offsets, call-other names, call-clobbered overrides, and
/// call-stack-arg-offset overrides) lives on [`crate::Function`].
#[derive(Clone)]
pub struct Graph {
    /// Dense map from [`NodeId`] to [`Node`] metadata.
    pub(crate) nodes: PrimaryMap<NodeId, Node>,
    /// Dense map from [`ValueId`] to [`ValueData`] metadata.
    pub(crate) outputs: PrimaryMap<ValueId, ValueData>,
    /// Dense map from [`UseId`] to [`UseData`] metadata.
    pub(crate) inputs: PrimaryMap<UseId, UseData>,
    /// Pool backing the per-node output id lists.
    pub(crate) output_pool: ListPool<ValueId>,
    /// Pool backing the per-node input id lists.
    pub(crate) input_pool: ListPool<UseId>,
    /// Deduplication cache: maps `(Node, inputs, output_kinds)` → `NodeId`
    /// for cacheable node kinds.
    pub(crate) node_to_id: HashMap<(Node, Vec<ValueId>, Vec<ValueKind>), NodeId>,
    /// Monotonic version counter incremented by every operation that
    /// invalidates pre-existing `NodeId` / `ValueId` /
    /// `UseId` values — currently [`Self::retain_reachable`] (and
    /// transitively [`crate::Function::compact`]).  External callers that captured
    /// node ids before the arena was reshuffled compare snapshots via
    /// [`Self::generation`] to detect staleness instead of dereferencing
    /// a recycled id into the wrong node.
    pub(crate) generation: u64,
}

impl Default for Graph {
    fn default() -> Self {
        Self::new()
    }
}

impl Graph {
    /// Creates an empty graph.
    pub fn new() -> Self {
        Graph {
            nodes: PrimaryMap::new(),
            outputs: PrimaryMap::new(),
            inputs: PrimaryMap::new(),
            output_pool: ListPool::new(),
            input_pool: ListPool::new(),
            node_to_id: HashMap::new(),
            generation: 0,
        }
    }

    /// Returns the current generation counter.  Bumped by every
    /// arena-reshuffling operation ([`Self::retain_reachable`] and
    /// transitively [`crate::Function::compact`]); external callers that captured
    /// a node id before the bump should not dereference it on the
    /// post-bump graph.  See the field-level doc on `generation` for
    /// the lifecycle.
    #[inline]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Returns `true` if `id` corresponds to a live entry in this
    /// graph's node arena.
    ///
    /// Cheap arena-membership check (a `cranelift-entity` PrimaryMap
    /// lookup).  Used by dump APIs (`dump_neighborhood`) to surface a
    /// typed error on a stale / foreign node id instead of panicking
    /// inside the renderer.  Note: a `true` result only proves the id
    /// is *currently* valid; if the graph is later compacted, the same
    /// id may map to a different node — compare [`Self::generation`]
    /// across the boundary if that matters.
    #[inline]
    pub fn has_node(&self, id: crate::node::NodeId) -> bool {
        self.nodes.is_valid(id)
    }

    /// Validated construction of a [`NodeId`] from a raw `u32` index supplied
    /// by an external caller (e.g. the Python bindings).
    /// Returns `None` if no node with that index exists in this graph.  O(1):
    /// `NodeId`s are dense arena indices, so this is a bounds check, not a scan.
    pub fn node_id_from_u32(&self, raw: u32) -> Option<crate::node::NodeId> {
        use cranelift_entity::EntityRef;
        let id = crate::node::NodeId::new(raw as usize);
        self.has_node(id).then_some(id)
    }

    /// Returns an iterator that visits all reachable nodes in pre-order,
    /// starting from the given `entry`.
    /// Used by opt passes that take `(graph, entry)` explicitly.
    pub fn walk_from(&self, entry: crate::node::NodeId) -> crate::walk::GraphWalk<'_> {
        crate::walk::walk_graph(self, entry)
    }

    /// Real reverse-post-order of every node reachable from `seed`.
    ///
    /// A post-order over the forward def→use graph (from the input-less
    /// roots), reversed, so every producer is yielded strictly before its
    /// consumers (defs-before-uses); the input-less roots come first.  The
    /// reachable SET is identical to [`Self::walk_from`]'s; only the ORDER is
    /// canonicalised to RPO.  See [`crate::walk::GraphWalkInfo`] for the
    /// construction.
    ///
    /// This is the single graph-level walk-ordering primitive.  For a value
    /// cone, seed the value's producer
    /// (`graph.reverse_postorder(graph.producer(value))`); for a kind-filtered
    /// global RPO over a function, use
    /// [`Function::rpo_filter`](crate::Function::rpo_filter).
    pub fn reverse_postorder(&self, seed: crate::node::NodeId) -> Vec<NodeId> {
        crate::walk::GraphWalkInfo::compute_full(self, seed).reverse_postorder(self)
    }

    /// Iterates over **every** node id in the graph, including nodes that are
    /// not reachable from any entry (e.g. detached zombies left behind by
    /// optimizer passes).
    pub fn all_node_ids(&self) -> impl Iterator<Item = crate::node::NodeId> + '_ {
        self.nodes.keys()
    }

}
