//! Every template node is either a [`Build`](TmplNodeKind::Build) declaring a
//! [`TemplateKind`], or a [`Capture`](TmplNodeKind::Capture) leaf whose one
//! value output is typed [`TemplateTy::InheritBinding`] and resolves through
//! the LHS [`Bindings`](crate::Bindings) at instantiation, so a `Template` is
//! buildable by construction.

use rustc_hash::FxHashSet;
use strider_graph::{Graph, NeverCacheable, NodeId};

use crate::capture::Capture;
use crate::graph_ext::HasInputSlots;
use crate::template::{TemplateKind, TemplateTy};

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
    /// A leaf standing for the LHS-bound value, as in `int_add(x, 0) -> x`,
    /// the capture its output's type names. Never synthesised, never has
    /// inputs.
    Capture,
}

/// An output at a slot of its node. A built node's output declares its slot,
/// which is what lets a multi-output interior node, such as a `Store` or
/// `Call` whose memory token a later node consumes, wire the right slot.
#[derive(Clone, Copy)]
pub(crate) struct TmplValue {
    pub(crate) slot: usize,
    /// `None` for a memory token.
    pub(crate) ty: Option<TemplateTy>,
}

/// Materialised by [`instantiate`](crate::template::instantiate).
pub struct Template {
    pub(crate) graph: Graph<TmplNode, TmplValue, NeverCacheable>,
    /// Captures a `*_const_with!` closure reads at instantiation and cannot
    /// do without. They have no vertex, so only the declaration makes them
    /// visible to the coverage check.
    pub(crate) declared_captures: Vec<Capture>,
    /// Captures a closure reads as an `Option`, disjoint from
    /// `declared_captures`, so the coverage check asks only that the LHS
    /// mention them.
    pub(crate) optional_captures: Vec<Capture>,
    /// Why the builder refused the template, reported by [`Self::root`].
    pub(crate) rejected: Option<String>,
}

impl Template {
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
            // A capture leaf's output and a `capture_typed` output alike: an
            // unbound one would resolve to `root_ty` and build a node at the
            // wrong width instead of failing the check.
            .filter_map(|v| match self.graph.value_kind_ref(v).ty {
                Some(TemplateTy::InheritBinding(cap)) => Some(cap),
                _ => None,
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
