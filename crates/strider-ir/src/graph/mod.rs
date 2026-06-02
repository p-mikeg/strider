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
/// `new_raw`, one entry per tracked variable) instead.
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
    /// Wide-integer constant values (I256, I512) referenced by
    /// [`crate::node::NodeKind::IntConstWide`].
    ///
    /// Wide values don't fit in `IntConst`'s `u128` payload; the IR
    /// stores them off-side here and the node carries a
    /// `crate::wide_const::WideConstId` index instead.  Interning
    /// (via `Self::intern_wide_const`) dedups by value so two
    /// `IntConstWide(id)` nodes referencing the same id are
    /// structurally equal under [`Self::create_node`]'s dedup cache.
    /// An [`entity_utils::EntityInterner`] owns both the forward
    /// `WideConstId → value` map and the reverse value-dedup index.
    pub(crate) wide_const_interner: entity_utils::EntityInterner<
        crate::wide_const::WideConstId,
        crate::wide_const::WideConstStorage,
    >,
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
    #[must_use]
    pub fn new() -> Self {
        Graph {
            nodes: PrimaryMap::new(),
            outputs: PrimaryMap::new(),
            inputs: PrimaryMap::new(),
            output_pool: ListPool::new(),
            input_pool: ListPool::new(),
            node_to_id: HashMap::new(),
            wide_const_interner: entity_utils::EntityInterner::default(),
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
    #[must_use]
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
    #[must_use]
    pub fn has_node(&self, id: crate::node::NodeId) -> bool {
        self.nodes.is_valid(id)
    }

    /// Validated construction of a [`NodeId`] from a raw `u32` index supplied
    /// by an external caller (e.g. the Python bindings).
    /// Returns `None` if no node with that index exists in this graph.  O(1):
    /// `NodeId`s are dense arena indices, so this is a bounds check, not a scan.
    #[must_use]
    pub fn node_id_from_u32(&self, raw: u32) -> Option<crate::node::NodeId> {
        use cranelift_entity::EntityRef;
        let id = crate::node::NodeId::new(raw as usize);
        self.has_node(id).then_some(id)
    }

    /// Interns `value` and returns its `crate::wide_const::WideConstId`.
    /// Subsequent calls with an equal value return the same id — the
    /// dedup invariant the [`Self::create_node`] cache relies on so
    /// two `IntConstWide(id)` nodes referencing the same logical value
    /// share a single `NodeId`.
    pub(crate) fn intern_wide_const(
        &mut self,
        value: crate::wide_const::WideConstStorage,
    ) -> crate::wide_const::WideConstId {
        self.wide_const_interner.intern(value)
    }

    /// Looks up a wide-const value by id.  The id must have been
    /// produced by `Self::intern_wide_const` on this graph; ids
    /// from other graphs are not portable.
    #[must_use]
    pub fn wide_const(
        &self,
        id: crate::wide_const::WideConstId,
    ) -> &crate::wide_const::WideConstStorage {
        &self.wide_const_interner[id]
    }

    /// Non-panicking variant of [`Self::wide_const`]: returns `None` for a
    /// dangling id rather than panicking.  The debug renderers use this so
    /// they can label a malformed graph (e.g. one inspected mid-rewrite)
    /// instead of aborting.
    #[must_use]
    pub fn wide_const_opt(
        &self,
        id: crate::wide_const::WideConstId,
    ) -> Option<&crate::wide_const::WideConstStorage> {
        self.wide_const_interner.get(id)
    }

    /// Returns an iterator that visits all reachable nodes in pre-order,
    /// starting from the given `entry`.
    /// Used by opt passes that take `(graph, entry)` explicitly.
    #[must_use]
    pub fn walk_from(&self, entry: crate::node::NodeId) -> crate::walk::GraphWalk<'_> {
        crate::walk::walk_graph(self, entry)
    }

    /// Reverse-post-order walk of the data-input cone reachable from `seed`.
    ///
    /// Yields every producer node before the node that consumes it
    /// (defs-before-uses); the producer of `seed` is yielded last.  Follows
    /// only data inputs (value, memory, dispatch) — never forward control
    /// edges; see [`crate::walk::InputSuccs`] for the successor relation.
    /// Used by value-cone analyses (e.g. SP-expression decomposition) that
    /// need each operand classified before the node that uses it.
    #[must_use]
    pub fn rpo(&self, seed: crate::node::ValueId) -> crate::walk::RpoWalk<'_> {
        crate::walk::rpo_walk(self, seed)
    }

    /// Returns the entry-reachable nodes in **global reverse-post-order**
    /// (entry-first), filtered to those whose [`crate::node::NodeKind`]
    /// satisfies `pred`.
    ///
    /// RPO is the deterministic, entry-first topological-ish order: the
    /// reverse of a post-order over the reachable graph (data-input
    /// producers + forward control), so every producer precedes its
    /// consumers.  The reachable SET is identical to
    /// [`Self::walk_from`]; only the ORDER is canonicalised.  Filtering
    /// is applied after ordering, so the relative RPO order of the kept
    /// nodes is preserved.
    pub fn rpo_filter<'a>(
        &'a self,
        entry: crate::node::NodeId,
        pred: impl Fn(&crate::node::NodeKind) -> bool + 'a,
    ) -> impl Iterator<Item = crate::node::NodeId> + 'a {
        crate::walk::rpo_reachable(self, entry)
            .into_iter()
            .filter(move |&n| pred(self.node_kind(n)))
    }

    /// Iterates over **every** node id in the graph, including nodes that are
    /// not reachable from any entry (e.g. detached zombies left behind by
    /// optimizer passes).
    pub fn all_node_ids(&self) -> impl Iterator<Item = crate::node::NodeId> + '_ {
        self.nodes.keys()
    }

}
