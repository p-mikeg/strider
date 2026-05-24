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

use cranelift_entity::{ListPool, PrimaryMap, SecondaryMap};
use hashbrown::HashMap;

use crate::node::{
    Node, NodeId, NodeInput, NodeInputId, NodeOutput, NodeOutputId, NodeOutputKind,
};

mod access;
mod compact;
mod store;
mod uses;

pub use compact::NodeIdRemap;

#[cfg(test)]
mod tests;

/// Calling-convention metadata captured at build time.
///
/// `None` on a `Graph` while it is being constructed by
/// [`crate::FunctionBuilder`]; populated to `Some(_)` by
/// [`crate::FunctionBuilder::build`] before the graph is returned to
/// consumers.  After build, [`Graph::cc_metadata`] unwraps the option;
/// pre-build code paths must use the field directly.
///
/// The four `Box<[rsleigh::Vn]>` lists' element-ordering invariants
/// correspond to slot positions on `Call` / `CallOther` / `Return`
/// nodes — `call_clobbered[i]` is the varnode for the `i`-th clobbered
/// output slot (slot `i + 2`); `ret_val_regs[i]` is the i-th ABI
/// return register; `call_other_clobbered[i]` is the i-th CallOther
/// clobber slot.
#[derive(Clone, Debug)]
pub struct CcMetadata {
    /// Map from [`crate::builder::VarId`] to the corresponding [`rsleigh::Vn`]
    /// varnode.  Indexed by the same `VarId` keys the builder used.
    pub(crate) variables: PrimaryMap<crate::builder::VarId, rsleigh::Vn>,
    /// Ordered list of varnodes clobbered by every `Call` node.  The
    /// `i`-th clobbered output (slot `i + 2`) corresponds to
    /// `call_clobbered[i]`.
    pub(crate) call_clobbered: Box<[rsleigh::Vn]>,
    /// The calling convention's return-value registers, in ABI order.
    pub(crate) ret_val_regs: Box<[rsleigh::Vn]>,
    /// Function-default clobber list for every `CallOther` node:
    /// every tracked variable except the stack pointer.
    pub(crate) call_other_clobbered: Box<[rsleigh::Vn]>,
    /// Function-default `no_memory_clobber` flag — whether calls under
    /// this convention preserve the memory chain.  `true` for
    /// zero-side-effect hooks (`__fentry__` / `mcount` /
    /// `x86_64_all_preserving`).
    pub(crate) no_memory_clobber: bool,
}

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
    /// Populated at IR construction time by the strider lifter.  Not all `CallOther`
    /// nodes are guaranteed to have an entry — e.g. nodes synthesised by tests
    /// that don't go through the strider lifter.  Use [`Graph::call_other_name`].
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
    /// contributors recorded".  Structural nodes — those whose
    /// [`NodeKind::category`] is `Region`, `InitialState`, or `Phi` —
    /// legitimately stay empty; the validator's fingerprint check
    /// (`asm_fingerprint_exempt` in `validate/graph_invariants.rs`)
    /// derives its exempt set from the same category predicate and
    /// flags any other reachable empty entry.
    pub(crate) asm_fingerprints: SecondaryMap<NodeId, Vec<u64>>,
    /// Per-Call clobber-list override.
    ///
    /// `None` (the default) means the Call uses the function-default
    /// clobber list at [`CcMetadata::call_clobbered`];
    /// `Some(list)` shadows the function-default for this one Call —
    /// the i-th value-typed output (slot `i + 2`) corresponds to
    /// `list[i]` instead of the function-default.  Populated by
    /// [`crate::FunctionBuilder::build_call_with_cc`] when the call
    /// site uses a per-address calling-convention override (e.g.
    /// Linux-kernel `__fentry__` / `mcount` callbacks that preserve
    /// every register).
    ///
    /// Stored as `SecondaryMap<NodeId, Option<Vec<rsleigh::Vn>>>` so
    /// the default `None` is the "no override" sentinel; the previous
    /// `HashMap`-keyed shape isn't used because the override is
    /// per-NodeId and benefits from the `SecondaryMap`'s O(1) array
    /// lookup with no hashing.
    pub(crate) call_clobbered_overrides: SecondaryMap<NodeId, Option<Vec<rsleigh::Vn>>>,
    /// Source-level varnode tag for [`crate::node::NodeKind::Phi`] nodes
    /// created at lift time.  `Some(vn)` marks the phi as the SSA φ for
    /// varnode `vn` (carries register-identity semantics — the
    /// indirect-branch classifier's soundness gate refuses to walk
    /// through such phis because doing so would erase that identity).
    /// `None` (the default) marks an anonymous value phi — synthesised
    /// by opt passes like `StackLoadForward` when forwarding a load
    /// across a `MemPhi`.  Non-`Phi` kinds always store `None`; readers
    /// must always pair the tag query with a `NodeKind::Phi` match.
    ///
    /// Stored as `SecondaryMap<NodeId, Option<rsleigh::Vn>>` for O(1)
    /// array indexing without hashing.
    pub(crate) phi_var_tag: SecondaryMap<NodeId, Option<rsleigh::Vn>>,
    /// Wide-integer constant values (U256, U512) referenced by
    /// [`crate::node::NodeKind::IntConstWide`].
    ///
    /// Wide values don't fit in `IntConst`'s `u128` payload; the IR
    /// stores them off-side here and the node carries a
    /// [`crate::wide_const::WideConstId`] index instead.  Interning
    /// (via [`Self::intern_wide_const`]) dedups by value so two
    /// `IntConstWide(id)` nodes referencing the same id are
    /// structurally equal under [`Self::create_node`]'s dedup cache.
    pub(crate) wide_consts:
        PrimaryMap<crate::wide_const::WideConstId, crate::wide_const::WideConstStorage>,
    /// Reverse-dedup index for [`Self::wide_consts`]: value → id.
    /// Owned by [`Self::intern_wide_const`]; never read directly by
    /// other code.
    pub(crate) wide_const_dedup: rustc_hash::FxHashMap<
        crate::wide_const::WideConstStorage,
        crate::wide_const::WideConstId,
    >,
    /// Maps each `InitialVar(vn)`'s varnode to the [`NodeId`] of that
    /// node, providing O(1) lookup at indirect-resolve sites that
    /// previously scanned `preorder()` to find the matching
    /// `InitialVar`.  Maintained at every canonical `InitialVar`
    /// creation site; remapped through [`NodeIdRemap`] by
    /// [`Self::retain_reachable`].
    pub(crate) initial_var_index: rustc_hash::FxHashMap<rsleigh::Vn, NodeId>,
    /// Monotonic version counter incremented by every operation that
    /// invalidates pre-existing `NodeId` / `NodeOutputId` /
    /// `NodeInputId` values — currently [`Self::retain_reachable`] (and
    /// transitively [`Self::compact`]).  External callers that captured
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
            stack_phi_offsets: SecondaryMap::new(),
            call_other_names: SecondaryMap::new(),
            asm_fingerprints: SecondaryMap::new(),
            call_clobbered_overrides: SecondaryMap::new(),
            phi_var_tag: SecondaryMap::new(),
            wide_consts: PrimaryMap::new(),
            wide_const_dedup: rustc_hash::FxHashMap::default(),
            initial_var_index: rustc_hash::FxHashMap::default(),
            generation: 0,
        }
    }

    /// Returns the current generation counter.  Bumped by every
    /// arena-reshuffling operation ([`Self::retain_reachable`] and
    /// transitively [`Self::compact`]); external callers that captured
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

    /// Interns `value` and returns its [`crate::wide_const::WideConstId`].
    /// Subsequent calls with an equal value return the same id — the
    /// dedup invariant the [`Self::create_node`] cache relies on so
    /// two `IntConstWide(id)` nodes referencing the same logical value
    /// share a single `NodeId`.
    pub fn intern_wide_const(
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
    /// produced by [`Self::intern_wide_const`] on this graph; ids
    /// from other graphs are not portable.
    #[must_use]
    pub fn wide_const(
        &self,
        id: crate::wide_const::WideConstId,
    ) -> &crate::wide_const::WideConstStorage {
        &self.wide_consts[id]
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

    /// Identity self-reference — kept so call sites written against the
    /// old `Graph`-as-wrapper shape continue to compile while callers
    /// migrate to `Function`.
    #[doc(hidden)]
    #[inline]
    #[must_use]
    pub fn graph(&self) -> &Graph {
        self
    }

    /// Identity self-reference (mut).  See [`Self::graph`].
    #[doc(hidden)]
    #[inline]
    #[must_use]
    pub fn graph_mut(&mut self) -> &mut Graph {
        self
    }
}
