//! Every template node is either a [`Build`](TmplNodeKind::Build) declaring a
//! [`TemplateKind`], or a [`Capture`](TmplNodeKind::Capture) leaf marker whose
//! [`ValueCapture`](TmplValue::ValueCapture) output resolves through the LHS
//! [`Bindings`](crate::Bindings) at instantiation, so a `Template` is
//! buildable by construction.

use rustc_hash::FxHashSet;
use strider_graph::{Graph, NeverCacheable, NodeId};

use crate::capture::Capture;
use crate::graph_ext::HasInputSlots;
use crate::template::{TemplateKind, TemplateTy};

/// A capture is split across both vertex enums. The node side is a
/// payload-less marker saying only "this leaf is a capture, do not synthesise
/// it"; the capture id and its value resolution live on the produced
/// [`TmplValue::ValueCapture`].
pub(crate) struct TmplNode {
    pub(crate) kind: TmplNodeKind,
    /// Parallel to the generic graph's input order; see `graph_ext` for why
    /// the slot rides on the node payload.
    pub(crate) input_slots: Vec<usize>,
}

impl HasInputSlots for TmplNode {
    type Slot = usize;
    fn input_slots(&self) -> &[usize] {
        &self.input_slots
    }
}

pub(crate) enum TmplNodeKind {
    /// Synthesised as fresh IR from a concrete `NodeKind` or a dynamic `Fn`.
    Build(TemplateKind),
    /// A leaf resolving through its
    /// [`ValueCapture`](TmplValue::ValueCapture) output to the LHS-bound
    /// value, as in `int_add(x, 0) -> x`. Never synthesised, never has inputs.
    Capture,
}

/// The value side of the build graph. A built node's output declares its
/// slot and kind, which is what lets a multi-output interior node, such as a
/// `Store` or `Call` whose memory token a later node consumes, wire the right
/// slot.
pub(crate) enum TmplValue {
    /// A built node's value output, typed at instantiation.
    Value { slot: usize, ty: TemplateTy },
    /// A built node's memory output.
    Memory { slot: usize },
    /// Resolves to `bindings.get_value(c)` at instantiation.
    ValueCapture(Capture),
}

impl TmplValue {
    /// `None` for a capture, which no node builds.
    pub(crate) fn built_slot(&self) -> Option<usize> {
        match *self {
            Self::Value { slot, .. } | Self::Memory { slot } => Some(slot),
            Self::ValueCapture(_) => None,
        }
    }
}

/// Materialised by [`instantiate`](crate::template::instantiate).
pub struct Template {
    pub(crate) graph: Graph<TmplNode, TmplValue, NeverCacheable>,
    /// Captures a `*_const_with!` closure reads at instantiation and cannot
    /// do without. They have no vertex, so only the declaration makes them
    /// visible to the coverage check.
    declared_captures: Vec<Capture>,
    /// Captures a closure reads as an `Option`, disjoint from
    /// `declared_captures`, so the coverage check asks only that the LHS
    /// mention them.
    optional_captures: Vec<Capture>,
    /// Why the builder refused the template, reported by [`Self::root`].
    rejected: Option<String>,
}

impl Template {
    /// For `TemplateBuilder::finish`.
    pub(crate) fn from_graph(
        graph: Graph<TmplNode, TmplValue, NeverCacheable>,
        declared_captures: Vec<Capture>,
        optional_captures: Vec<Capture>,
        rejected: Option<String>,
    ) -> Self {
        Self {
            graph,
            declared_captures,
            optional_captures,
            rejected,
        }
    }

    /// The build root is the unique sink, recovered structurally.
    ///
    /// # Errors
    /// Unless there is exactly one sink: zero means an empty template, more
    /// than one means multi-rooted. Also if the builder refused a verb.
    pub fn root(&self) -> anyhow::Result<NodeId> {
        if let Some(why) = &self.rejected {
            anyhow::bail!("{why}");
        }
        crate::graph_ext::derive_root(&self.graph)
    }

    /// Captures the RHS cannot instantiate without: a value vertex, an output
    /// type inheriting a binding's width, or a non-`Option` declaration.
    fn required_captures(&self) -> impl Iterator<Item = Capture> + '_ {
        self.graph
            .all_value_ids()
            .filter_map(|v| match self.graph.value_kind_ref(v) {
                // A capture reached only through `capture_typed` lives in the
                // output's TYPE, not as a value vertex. Unreported, an
                // unbound one resolves to `root_ty` and the rewrite builds a
                // node at the wrong width instead of failing the check.
                TmplValue::ValueCapture(cap)
                | TmplValue::Value {
                    ty: TemplateTy::InheritBinding(cap),
                    ..
                } => Some(*cap),
                TmplValue::Value { .. } | TmplValue::Memory { .. } => None,
            })
            .chain(self.declared_captures.iter().copied())
    }

    /// Drives the rewrite engine's construction-time coverage check, which
    /// confirms the LHS binds every capture the RHS references.
    pub fn referenced_captures(&self) -> impl Iterator<Item = Capture> + '_ {
        self.required_captures()
            .chain(self.optional_captures.iter().copied())
    }

    /// The referenced captures no use needs bound. The coverage check asks
    /// only that the LHS bind these somewhere, not on every alternation arm.
    ///
    /// A declaration relaxes one closure's own read, so a capture some OTHER
    /// use needs is subtracted here: an arm that leaves it unbound would pass
    /// construction and then fail `instantiate`, which is not a skip.
    pub fn optional_captures(&self) -> impl Iterator<Item = Capture> + '_ {
        let required: FxHashSet<Capture> = self.required_captures().collect();
        self.optional_captures
            .iter()
            .copied()
            .filter(move |c| !required.contains(c))
    }
}

#[cfg(test)]
mod tests {
    /// `bool_not(var(c))` seals into `int_xor(var(c), IntConst(1)):I1`: the xor,
    /// its const operand and the captured var, so three node vertices.
    #[test]
    fn bool_not_template_builds_three_node_graph() {
        use crate::{Capture, TemplatePat, var};
        let c = Capture::new();
        let tpl = crate::template::bool_not(var(c)).into_template();
        assert!(tpl.root().is_ok(), "sealed template must have a root");
        assert_eq!(tpl.graph.all_node_ids().count(), 3);
    }
}
