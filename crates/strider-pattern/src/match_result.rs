//! Public [`Match`] result type returned by every successful pattern
//! match.  Wraps a root [`NodeId`] and the accumulated
//! [`Bindings`] journal; per-capture value reads go through
//! [`Match::bindings`] (the typed accessors live on [`Bindings`]).

use strider_ir::Graph;
use strider_ir::node::{NodeId, NodeKind, ValueId};

use crate::bindings::{Binding, Bindings};
use crate::capture::Capture;

/// The result of a successful pattern match against a single root node.
///
/// Exposes the matched root, the captured bindings (via
/// [`bindings`](Self::bindings) for the typed value/op accessors on
/// [`Bindings`]), and two match-level helpers that need the owning
/// [`strider_ir::Function`]: [`get_vn`](Self::get_vn) and
/// [`asm_fingerprint`](Self::asm_fingerprint).
#[derive(Clone)]
pub struct Match {
    pub(crate) root: NodeId,
    pub(crate) bindings: Bindings,
}

impl Match {
    /// Construct a [`Match`] from a root [`NodeId`] and the
    /// accumulated bindings.  `pub(crate)` because [`Bindings`] is
    /// constructed only by the matcher.
    pub(crate) fn from_root(root: NodeId, bindings: Bindings) -> Self {
        Self { root, bindings }
    }

    /// The root node where the top-level pattern matched.
    pub fn root(&self) -> NodeId {
        self.root
    }

    /// The captured [`Bindings`].  All typed value / op accessors
    /// (`get_uint` / `get_int` / `get_bool` / `get_float_bits` /
    /// `get_*_op`, …) live on [`Bindings`]; read them as
    /// `m.bindings().get_uint(c, graph)`.
    pub fn bindings(&self) -> &Bindings {
        &self.bindings
    }

    /// Returns the `NodeId` bound to `c`, or `None` if `c` was not
    /// captured in this match.  Every successful capture binds at
    /// least the matched node id; for value-producing captures the
    /// owning node is recovered from the bound `ValueId` via
    /// [`strider_ir::Graph::producer`], hence the `&Graph` arg.
    pub fn node(&self, c: Capture, graph: &Graph) -> Option<NodeId> {
        self.bindings.get_node(c, graph)
    }

    /// Returns the value `ValueId` bound to `c`, or `None` if
    /// `c` was not captured or the binding was control-flow.
    /// Multi-output nodes (e.g. `Load = [Memory, Value]`) bind the
    /// value slot.
    pub fn value(&self, c: Capture) -> Option<ValueId> {
        self.bindings.get_value(c)
    }

    /// Whether `c` is bound in this match (either variant of the
    /// internal `Binding` — value or node-only).  Graph-free — useful
    /// for `c in m` containment checks where the only question is
    /// "did this capture fire?".
    pub fn is_bound(&self, c: Capture) -> bool {
        self.bindings.is_bound(c)
    }

    /// Returns the [`rsleigh::Vn`] associated with the binding, if one
    /// can be determined.  The output-to-varnode mapping is well-defined
    /// only for a handful of producer kinds:
    ///
    /// * `InitialVar(vn)` — the varnode whose function-entry value is
    ///   read.
    /// * `Call` / `CallOther` clobber output values — the register the
    ///   call clobbers, recovered with a single
    ///   [`strider_ir::Function::clobbered_vn`] lookup keyed by the bound
    ///   value.  Every clobber output is tagged at build time (both the
    ///   function-default and the override / implicit-write paths), so the
    ///   lookup needs no slot arithmetic and works uniformly for Call and
    ///   CallOther.
    ///
    /// Returns `None` for unbound captures or producers without a
    /// well-defined varnode mapping.
    pub fn get_vn(&self, c: Capture, function: &strider_ir::Function) -> Option<rsleigh::Vn> {
        let binding = self.bindings.get_binding(c)?;
        if let Binding::Value(value) = binding {
            let (node, _slot) = function.value_definition(value);
            let kind = function.node_kind(node);
            // Call / CallOther clobber outputs carry their clobbered
            // varnode directly on the value via `value_vn`.  Control /
            // Memory / value outputs are absent from `value_vn`, so a
            // missing entry correctly falls through to `None`.
            if matches!(kind, NodeKind::Call | NodeKind::CallOther { .. })
                && let Some(vn) = function.clobbered_vn(value)
            {
                return Some(vn);
            }
        }
        // Fallback: an `InitialVar` carries its varnode tag on the
        // owning node — recover the node id (directly for a
        // [`Binding::Node`], via `producer` for a
        // [`Binding::Value`]) and inspect the kind.
        let node = self.bindings.get_node(c, function.graph())?;
        match function.node_kind(node) {
            NodeKind::InitialVar(vn) => Some(*vn),
            _ => None,
        }
    }

    /// Returns the asm-instruction-address fingerprint of the node bound
    /// to `c`, as a sorted-deduplicated slice.  Returns an empty slice
    /// when the capture is unbound or when the bound node has no
    /// recorded contributors (legitimately empty for region / phi /
    /// initial-state kinds — see
    /// [`strider_ir::Function::asm_fingerprint`] for the documented exempt set).
    ///
    /// This is the proof-of-correctness aid: when a pattern query
    /// captures a value node, this slice lists the machine
    /// instructions whose lifting (or subsequent rewrite) contributed
    /// to that node's value.  See
    /// `docs/superpowers/specs/2026-05-03-asm-fingerprints-design.md`
    /// for the full contract.
    pub fn asm_fingerprint<'g>(&self, c: Capture, graph: &'g strider_ir::Function) -> &'g [u64] {
        match self.bindings.get_node(c, graph.graph()) {
            Some(node) => graph.asm_fingerprint(node),
            None => &[],
        }
    }

    /// Returns an owned copy of the full [`Bindings`] captured by this match.
    /// Used by the rewrite-rule interpreter (drops the `Matcher` borrow
    /// before mutating the graph) and by tests.
    pub fn bindings_clone(&self) -> Bindings {
        self.bindings.clone()
    }
}
