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
    /// contributors recorded".  Structural nodes — `Entry`,
    /// `InitialMemory`, `InitialVar`, `FunctionArg`, `ControlState`,
    /// `MemPhi`, `Phi`, `StackStorePhi` — legitimately
    /// stay empty; the validator's opt-in fingerprint check
    /// (`asm_fingerprint_exempt` in `validate/graph_invariants.rs`) exempts those
    /// kinds and flags any other reachable empty entry.  (`IfCase` is
    /// not a `NodeKind` — it's a CFG edge label only.)
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
    /// The `Entry` node of the function, once
    /// [`crate::FunctionBuilder::build`] has finalised the graph.
    /// `None` during build; `Some(_)` after.  Consumers go through
    /// [`Self::entry`], which unwraps and panics on the un-built case.
    pub(crate) entry: Option<NodeId>,
    /// Calling-convention metadata captured at build time.  `None`
    /// during build; `Some(_)` after.  See [`CcMetadata`].
    pub(crate) cc_metadata: Option<CcMetadata>,
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
            wide_consts: PrimaryMap::new(),
            wide_const_dedup: rustc_hash::FxHashMap::default(),
            initial_var_index: rustc_hash::FxHashMap::default(),
            entry: None,
            cc_metadata: None,
        }
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

    /// Returns the `Entry` node id of the function, or `None` if the
    /// graph has not yet been finalised by
    /// [`crate::FunctionBuilder::build`].
    ///
    /// Callers in `Result`-returning code typically bubble via `?`:
    /// ```ignore
    /// let entry = graph.entry().ok_or_else(|| anyhow::anyhow!(
    ///     "Graph is not fully built: entry node missing"
    /// ))?;
    /// ```
    ///
    /// Opt passes and analyses run only against built graphs, so the
    /// `None` arm represents an invariant violation at their boundary;
    /// returning `Option` (instead of panicking) keeps the failure mode
    /// honest and typed.
    #[inline]
    #[must_use]
    pub fn entry(&self) -> Option<crate::node::NodeId> {
        self.entry
    }

    /// Read-only access to the calling-convention metadata captured at
    /// build time, or `None` if the graph has not yet been finalised by
    /// [`crate::FunctionBuilder::build`].  See [`CcMetadata`].
    ///
    /// Callers in `Result`-returning code typically bubble via `?`:
    /// ```ignore
    /// let cc = graph.cc_metadata().ok_or_else(|| anyhow::anyhow!(
    ///     "Graph is not fully built: cc_metadata missing"
    /// ))?;
    /// ```
    #[inline]
    #[must_use]
    pub fn cc_metadata(&self) -> Option<&CcMetadata> {
        self.cc_metadata.as_ref()
    }

    /// Read the calling convention's call-clobbered varnode list.
    /// Convenience for `graph.cc_metadata().call_clobbered`.  Returns
    /// an empty slice on a pre-build graph (when `cc_metadata` is
    /// `None`); callers needing to distinguish should use
    /// [`Self::cc_metadata`] directly.
    #[must_use]
    pub fn call_clobbered_regs(&self) -> &[rsleigh::Vn] {
        self.cc_metadata
            .as_ref()
            .map_or(&[], |cc| &cc.call_clobbered)
    }

    /// Function-default `no_memory_clobber` flag — whether calls under
    /// this convention preserve memory (zero-side-effect hooks like
    /// `__fentry__` / `mcount`).  When `true`, `LoadReadOnly` and
    /// `StackLoadForward` may forward across calls.  Returns `false`
    /// on a pre-build graph (the safe, memory-clobbering default).
    #[must_use]
    pub fn no_memory_clobber(&self) -> bool {
        self.cc_metadata
            .as_ref()
            .is_some_and(|cc| cc.no_memory_clobber)
    }

    /// Read the function-default CallOther clobber list.
    /// Convenience for `graph.cc_metadata().call_other_clobbered`.
    /// Returns an empty slice on a pre-build graph.
    #[must_use]
    pub fn call_other_clobbered_regs(&self) -> &[rsleigh::Vn] {
        self.cc_metadata
            .as_ref()
            .map_or(&[], |cc| &cc.call_other_clobbered)
    }

    /// Read the `VarId → Vn` map for tracked variables.
    /// Convenience for `graph.cc_metadata().variables`.  Returns a
    /// reference to a shared empty map on a pre-build graph.
    #[must_use]
    pub fn variables_map(&self) -> &PrimaryMap<crate::builder::VarId, rsleigh::Vn> {
        use std::sync::OnceLock;
        static EMPTY: OnceLock<PrimaryMap<crate::builder::VarId, rsleigh::Vn>> = OnceLock::new();
        self.cc_metadata
            .as_ref()
            .map_or_else(|| EMPTY.get_or_init(PrimaryMap::new), |cc| &cc.variables)
    }

    /// Returns an iterator that visits all reachable nodes in pre-order,
    /// starting from [`Self::entry`].  Yields an empty walk on a graph
    /// that has not yet been built (i.e. `entry` is `None`).
    #[must_use]
    pub fn preorder(&self) -> crate::walk::GraphWalk<'_> {
        crate::walk::walk_graph_opt(self, self.entry)
    }

    /// Returns an iterator that visits all reachable nodes in pre-order,
    /// starting from the given `entry` (which need not be `self.entry`).
    /// Used by opt passes that take `(graph, entry)` explicitly.
    #[must_use]
    pub fn walk_from(&self, entry: crate::node::NodeId) -> crate::walk::GraphWalk<'_> {
        crate::walk::walk_graph(self, entry)
    }

    /// Reachable preorder filtered by a predicate over the node's
    /// [`crate::node::NodeKind`].  Convenience for the common
    /// `.preorder().filter(|n| matches!(graph.node_kind(n), …))` pattern.
    pub fn preorder_kind<'a, P>(
        &'a self,
        mut pred: P,
    ) -> impl Iterator<Item = crate::node::NodeId> + 'a
    where
        P: FnMut(&crate::node::NodeKind) -> bool + 'a,
    {
        self.preorder().filter(move |&n| pred(self.node_kind(n)))
    }

    /// Iterates over **every** node id in the graph, including nodes that are
    /// not reachable from any entry (e.g. detached zombies left behind by
    /// optimizer passes).
    pub fn all_node_ids(&self) -> impl Iterator<Item = crate::node::NodeId> + '_ {
        self.nodes.keys()
    }

    /// Rebuilds the graph to retain only nodes reachable from
    /// [`Self::entry`] via [`crate::walk::walk_graph`].  The entry node
    /// id is remapped; CC metadata is vn-keyed and stays valid as-is.
    ///
    /// External callers that hold any pre-compaction `NodeId` /
    /// `NodeOutputId` / `NodeInputId` MUST rewrite them through the
    /// returned remap (or drop them).
    ///
    /// # Errors
    ///
    /// Returns an error if the graph has not been built (i.e. `entry`
    /// is `None`), or if `retain_reachable`'s remap doesn't contain
    /// the entry node.  The latter can never fire by construction —
    /// `retain_reachable` walks forward from `entry`, so the entry is
    /// always reachable from itself — but propagating as `Err` rather
    /// than panicking keeps every error path typed so Python users see
    /// a clean exception.
    pub fn compact(&mut self) -> crate::Result<NodeIdRemap> {
        let entry = self.entry.ok_or_else(|| {
            anyhow::anyhow!("Graph::compact: graph has not been built (entry is None)")
        })?;
        let remap = self.retain_reachable(entry)?;
        let new_entry = remap.node_old_to_new(entry).ok_or_else(|| {
            anyhow::anyhow!(
                "Graph::compact: entry {:?} missing from retain_reachable remap (invariant violation)",
                entry
            )
        })?;
        self.entry = Some(new_entry);
        Ok(remap)
    }

    /// Returns a [`crate::graph_dot::GraphDotDumper`] that can render
    /// this function graph to a `.dot` / `.html` file.
    ///
    /// # Errors
    ///
    /// Returns an error if the graph has not been built (i.e. `entry`
    /// or `cc_metadata` is `None`).
    pub fn dot_dumper<'a, R: rsleigh::MemReader>(
        &'a self,
        sleigh: &'a rsleigh::Sleigh<R>,
    ) -> crate::Result<crate::graph_dot::GraphDotDumper<'a, R>> {
        let entry = self.entry.ok_or_else(|| {
            anyhow::anyhow!("Graph::dot_dumper: graph has not been built (entry is None)")
        })?;
        let cc = self.cc_metadata.as_ref().ok_or_else(|| {
            anyhow::anyhow!("Graph::dot_dumper: graph has not been built (cc_metadata is None)")
        })?;
        Ok(crate::graph_dot::GraphDotDumper {
            entry,
            graph: self,
            sleigh,
            call_clobbered: &cc.call_clobbered,
            ret_val_regs: &cc.ret_val_regs,
        })
    }

    /// Identity self-reference — no-op now that the
    /// `BuiltFunctionGraph` wrapper was collapsed into `Graph`.  Kept
    /// so call sites that were written against the wrapper continue to
    /// compile.
    #[doc(hidden)]
    #[must_use]
    pub fn graph(&self) -> &Graph {
        self
    }

    /// Identity self-reference (mut).  See [`Self::graph`].
    #[doc(hidden)]
    #[must_use]
    pub fn graph_mut(&mut self) -> &mut Graph {
        self
    }
}
