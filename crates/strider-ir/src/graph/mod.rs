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
    Node, NodeId, NodeInput, NodeInputId, NodeOutput, NodeOutputId, NodeOutputKind,
};

mod access;
mod compact;
mod store;
mod uses;

pub use compact::NodeIdRemap;
pub(crate) use compact::SideTableRemap;

#[cfg(test)]
mod tests;

/// Calling-convention metadata captured at build time.
///
/// Always present on every [`crate::Function`] (possibly with every
/// field empty / default while the [`crate::FunctionBuilder`] is still
/// populating it).  [`crate::Function::cc_metadata`] is the read entry
/// point; [`crate::Function::cc_metadata_mut`] the write entry point.
///
/// The three ordered `Vec<rsleigh::Vn>` lists' element-ordering
/// invariants correspond to slot positions on `Call` / `CallOther` /
/// `Return` nodes — `call_clobbered[i]` is the varnode for the `i`-th
/// clobbered output slot (slot `i + 2`); `ret_val_regs[i]` is the i-th
/// ABI return register; `call_other_clobbered[i]` is the i-th
/// CallOther clobber slot.
///
/// Pure ABI declarations (the stack pointer varnode, `ret_stack_pop`,
/// `no_memory_clobber`, link register, etc.) live on the embedded
/// `Self::cc` copy rather than being mirrored here — they are read
/// through `cc.as_ref().map(...)` and surfaced by the
/// [`crate::Function`] accessors (`crate::Function::stack_vn` /
/// `crate::Function::ret_stack_pop` /
/// `crate::Function::no_memory_clobber`).  The fields below are the
/// per-function-effective lists, which differ from the raw ABI lists
/// after dedup / `upgrade_to_tracked_for`.
#[derive(Clone, Debug, Default)]
pub struct CcMetadata {
    /// Map from [`crate::builder::VarId`] to the corresponding [`rsleigh::Vn`]
    /// varnode.  Indexed by the same `VarId` keys the builder used.
    pub(crate) variables: PrimaryMap<crate::builder::VarId, rsleigh::Vn>,
    /// Reverse index of [`Self::variables`]: `Vn → VarId`.  Maintained
    /// alongside `variables` (every insert into `variables` must also
    /// insert here) so SSA reads / writes can resolve a tracked varnode
    /// to its dense [`crate::builder::VarId`] in O(1).
    pub(crate) variable_to_id: rustc_hash::FxHashMap<rsleigh::Vn, crate::builder::VarId>,
    /// Ordered list of varnodes clobbered by every `Call` node.  The
    /// `i`-th clobbered output (slot `i + 2`) corresponds to
    /// `call_clobbered[i]`.
    pub(crate) call_clobbered: Vec<rsleigh::Vn>,
    /// The calling convention's return-value registers, in ABI order.
    /// Post-`upgrade_to_tracked_for`, so may differ from the raw
    /// `cc.ret_val_regs` (e.g. when a function uses a sub-register view
    /// of an ABI ret slot).
    pub(crate) ret_val_regs: Vec<rsleigh::Vn>,
    /// Function-default clobber list for every `CallOther` node:
    /// every tracked variable except the stack pointer.
    pub(crate) call_other_clobbered: Vec<rsleigh::Vn>,
    /// Calling convention's arg-passing registers, filtered through the
    /// function's tracked-variable set (and through
    /// `upgrade_to_tracked_for` for register aliasing).  May differ
    /// from the raw `cc.arg_passing_regs`.
    pub(crate) arg_passing_vars: Vec<rsleigh::Vn>,
    /// Embedded copy of the calling convention this function was built
    /// under, when one was provided.  `None` for synthetic test
    /// functions constructed via [`crate::FunctionBuilder::new_raw`]
    /// without a real CC.
    ///
    /// Reads of pure ABI facts (`stack_vn`, `ret_stack_pop`,
    /// `no_memory_clobber`, `link_register_vn`) delegate here rather
    /// than duplicating those scalars on this struct.
    pub(crate) cc: Option<strider_target::BuiltCallingConvention>,
}


/// The core IR graph structure.
///
/// Stores nodes, their input/output slots, and a deduplication cache for
/// cacheable node kinds.  All ids (node, output, input) are small integers
/// allocated from dense entity maps, so they can be used as cheap, copyable
/// handles.
///
/// `Graph` is the pure structural arena: nodes, edges, wide-const interning,
/// the dedup cache, and the generation counter.  Per-function overlay state
/// (asm fingerprints, phi var tags, stack-phi offsets, call-other names, and
/// call-clobbered overrides) lives on [`crate::Function`].
#[derive(Clone)]
pub struct Graph {
    /// Dense map from [`NodeId`] to [`Node`] metadata.
    pub(crate) nodes: PrimaryMap<NodeId, Node>,
    /// Dense map from [`NodeOutputId`] to [`NodeOutput`] metadata.
    pub(crate) outputs: PrimaryMap<NodeOutputId, NodeOutput>,
    /// Dense map from [`NodeInputId`] to [`NodeInput`] metadata.
    pub(crate) inputs: PrimaryMap<NodeInputId, NodeInput>,
    /// Pool backing the per-node output id lists.
    pub(crate) output_pool: ListPool<NodeOutputId>,
    /// Pool backing the per-node input id lists.
    pub(crate) input_pool: ListPool<NodeInputId>,
    /// Deduplication cache: maps `(Node, inputs, output_kinds)` → `NodeId`
    /// for cacheable node kinds.
    pub(crate) node_to_id: HashMap<(Node, Vec<NodeOutputId>, Vec<NodeOutputKind>), NodeId>,
    /// Wide-integer constant values (I256, I512) referenced by
    /// [`crate::node::NodeKind::IntConstWide`].
    ///
    /// Wide values don't fit in `IntConst`'s `u128` payload; the IR
    /// stores them off-side here and the node carries a
    /// `crate::wide_const::WideConstId` index instead.  Interning
    /// (via `Self::intern_wide_const`) dedups by value so two
    /// `IntConstWide(id)` nodes referencing the same id are
    /// structurally equal under [`Self::create_node`]'s dedup cache.
    pub(crate) wide_consts:
        PrimaryMap<crate::wide_const::WideConstId, crate::wide_const::WideConstStorage>,
    /// Reverse-dedup index for [`Self::wide_consts`]: value → id.
    /// Owned by `Self::intern_wide_const`; never read directly by
    /// other code.
    pub(crate) wide_const_dedup: rustc_hash::FxHashMap<
        crate::wide_const::WideConstStorage,
        crate::wide_const::WideConstId,
    >,
    /// Monotonic version counter incremented by every operation that
    /// invalidates pre-existing `NodeId` / `NodeOutputId` /
    /// `NodeInputId` values — currently [`Self::retain_reachable`] (and
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
            wide_consts: PrimaryMap::new(),
            wide_const_dedup: rustc_hash::FxHashMap::default(),
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

    /// Validated construction of a [`NodeId`](crate::node::NodeId) from a raw
    /// `u32` index supplied by an external caller (e.g. the Python bindings).
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
        if let Some(&id) = self.wide_const_dedup.get(&value) {
            return id;
        }
        let id = self.wide_consts.push(value.clone());
        self.wide_const_dedup.insert(value, id);
        id
    }

    /// Looks up a wide-const value by id.  The id must have been
    /// produced by `Self::intern_wide_const` on this graph; ids
    /// from other graphs are not portable.
    #[must_use]
    pub fn wide_const(
        &self,
        id: crate::wide_const::WideConstId,
    ) -> &crate::wide_const::WideConstStorage {
        &self.wide_consts[id]
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
        self.wide_consts.get(id)
    }

    /// Returns an iterator that visits all reachable nodes in pre-order,
    /// starting from the given `entry`.
    /// Used by opt passes that take `(graph, entry)` explicitly.
    #[must_use]
    pub fn walk_from(&self, entry: crate::node::NodeId) -> crate::walk::GraphWalk<'_> {
        crate::walk::walk_graph(self, entry)
    }

    /// Iterates over **every** node id in the graph, including nodes that are
    /// not reachable from any entry (e.g. detached zombies left behind by
    /// optimizer passes).
    pub fn all_node_ids(&self) -> impl Iterator<Item = crate::node::NodeId> + '_ {
        self.nodes.keys()
    }

}
