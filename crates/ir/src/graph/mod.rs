//! Sea-of-nodes graph storage, dedup cache, use-list, and typed accessors.
//!
//! The implementation is split into three submodules along the contracts
//! that the validator's three layers each protect:
//!
//! - [`store`] — node arena, dedup cache, side-tables. Layer A's input.
//! - [`uses`]  — bidirectional use-list bookkeeping. Layer B's contract.
//! - [`access`] — read-only typed accessors. Layer A's lookup surface.
//!
//! All public API names live in this module via the original paths:
//! `ir::graph::Graph`, `ir::graph::Graph::create_node`, etc., regardless of
//! which submodule's `impl Graph { ... }` block defines each method.

use std::collections::HashMap;

use cranelift_entity::{ListPool, PrimaryMap, SecondaryMap};

use crate::node::{
    Node, NodeId, NodeInput, NodeInputId, NodeOutput, NodeOutputId, NodeOutputKind,
};

mod access;
mod store;
mod uses;

#[cfg(test)]
mod tests;

/// The core IR graph structure.
///
/// Stores nodes, their input/output slots, and a deduplication cache for
/// cacheable node kinds.  All ids (node, output, input) are small integers
/// allocated from dense entity maps, so they can be used as cheap, copyable
/// handles.
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
    /// Side-map from [`crate::node::NodeKind::StackStorePhi`] nodes to their
    /// per-predecessor SP-relative offsets.  Kept external so that
    /// `NodeKind` stays `Copy`.
    ///
    /// Stored as a `SecondaryMap<NodeId, Vec<i64>>` (dense entity-indexed
    /// array) instead of a `HashMap` for O(1) cache-local lookup with no
    /// hashing.  The default value is an empty `Vec`, which is the same
    /// "no entry" sentinel the previous `HashMap`-keyed accessor returned.
    pub(crate) stack_phi_offsets: SecondaryMap<NodeId, Vec<i64>>,
    /// Side-map from [`crate::node::NodeKind::CallOther`] nodes to the user-op
    /// name resolved from Sleigh.  Kept external so that `NodeKind::CallOther`
    /// keeps its single-`u64` payload (and stays `Copy`).  `CallOther` is
    /// non-cacheable, so the dedup-cache concern that motivates the side-map
    /// shape for cacheable kinds doesn't apply here — the choice is purely to
    /// keep the kind enum small and `Copy`.
    ///
    /// Populated at IR construction time by the analyzer.  Not all `CallOther`
    /// nodes are guaranteed to have an entry — e.g. nodes synthesised by tests
    /// that don't go through the analyzer.  Use [`Graph::call_other_name`].
    ///
    /// Stored as a `SecondaryMap<NodeId, Option<String>>`: O(1) array index
    /// without hashing.  The `Option` distinguishes "name not set" from
    /// "name set to empty string"; the previous `HashMap` accessor returned
    /// `None` for the former and `Some("")` for the latter.
    pub(crate) call_other_names: SecondaryMap<NodeId, Option<String>>,
    /// Side-map from every [`NodeId`] to a sorted-deduped list of the
    /// machine-instruction addresses ("asm addresses") whose lifting or
    /// subsequent rewrite contributed to the node's value — its
    /// **fingerprint**.
    ///
    /// The contract is **superset-only**:
    /// - The fingerprint may overstate (extra ancestors are tolerated).
    /// - It must never *omit* a contributing address — every optimisation
    ///   pass that folds `old → new` must absorb `old`'s fingerprint into
    ///   `new` via [`Graph::extend_asm_fingerprint_from`].
    /// - Two structurally identical nodes share one entry on the
    ///   side-table; [`Graph::create_node`]'s callers union additional
    ///   contributors via the same `extend_*` helper.
    ///
    /// Stored as `SecondaryMap<NodeId, Vec<u64>>` for O(1) array indexing
    /// and small-set merge — the typical fingerprint is 1–4 entries.
    /// The default value is the empty `Vec`, which represents "no
    /// contributors recorded".  Region nodes (`ControlState`, phis,
    /// `Entry`, `InitialMemory`, `InitialVar`, `FunctionArg`, `IfCase`)
    /// legitimately stay empty; the validator's opt-in fingerprint check
    /// exempts those kinds and flags any other reachable empty entry.
    pub(crate) asm_fingerprints: SecondaryMap<NodeId, Vec<u64>>,
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
            stack_phi_offsets: SecondaryMap::new(),
            call_other_names: SecondaryMap::new(),
            asm_fingerprints: SecondaryMap::new(),
        }
    }

    /// Returns an iterator that visits all reachable nodes in pre-order,
    /// starting from `entry`.  Callers that hold a `BuiltFunctionGraph`
    /// can use the wrapping [`crate::function::BuiltFunctionGraph::preorder`]
    /// shortcut instead; opt passes that take `(graph, entry)` directly use
    /// this method to walk reachable nodes without needing a wrapper.
    #[must_use]
    pub fn preorder(&self, entry: crate::node::NodeId) -> crate::walk::GraphWalk<'_> {
        crate::walk::walk_graph(self, entry)
    }

    /// Iterates over **every** node id in the graph, including nodes that are
    /// not reachable from any entry (e.g. detached zombies left behind by
    /// optimizer passes).
    pub fn all_node_ids(&self) -> impl Iterator<Item = crate::node::NodeId> + '_ {
        self.nodes.keys()
    }

}
