//! The [`Template`] graph: [`Build`](TmplNodeKind::Build) nodes and
//! [`Capture`](TmplNodeKind::Capture) leaves resolved through the LHS
//! [`Bindings`](crate::Bindings).

use rustc_hash::FxHashSet;
use strider_graph::{Graph, NeverCacheable, NodeId};

use crate::capture::Capture;
use crate::graph_ext::HasInputSlots;
use crate::template::{TemplateKind, TemplateTy};

pub(crate) struct TmplNode {
    pub(crate) kind: TmplNodeKind,
    /// Parallel to the graph's input order; see `graph_ext`.
    pub(crate) input_slots: Vec<usize>,
}

impl HasInputSlots for TmplNode {
    type Slot = usize;
    fn input_slots(&self) -> &[usize] {
        &self.input_slots
    }
}

pub(crate) enum TmplNodeKind {
    /// Built as fresh IR.
    Build(TemplateKind),
    /// A leaf standing for the LHS value bound to the capture its output's type
    /// names (`int_add(x, 0) -> x`). Has no inputs.
    Capture,
}

/// An output at a slot of its node.
#[derive(Clone, Copy)]
pub(crate) struct TmplValue {
    pub(crate) slot: usize,
    /// `None` for a memory token.
    pub(crate) ty: Option<TemplateTy>,
}

/// A rewrite's build side, materialised by [`instantiate`](crate::template::instantiate).
pub struct Template {
    pub(crate) graph: Graph<TmplNode, TmplValue, NeverCacheable>,
    /// Captures a closure requires; they have no vertex.
    pub(crate) declared_captures: Vec<Capture>,
    /// Captures a closure reads as an `Option`.
    pub(crate) optional_captures: Vec<Capture>,
    /// The builder's refusal, reported by [`Self::root`].
    pub(crate) rejected: Option<String>,
}

impl Template {
    /// The build root: the unique sink.
    ///
    /// # Errors
    /// If the builder refused a verb, or there is not exactly one sink.
    pub fn root(&self) -> anyhow::Result<NodeId> {
        if let Some(why) = &self.rejected {
            anyhow::bail!("{why}");
        }
        crate::graph_ext::derive_root(&self.graph)
    }

    /// Captures the RHS cannot instantiate without: those an output's type
    /// inherits from (incl. capture leaves) and the non-`Option` declarations.
    fn required_captures(&self) -> impl Iterator<Item = Capture> + '_ {
        self.graph
            .all_value_ids()
            .filter_map(|v| match self.graph.value_kind_ref(v).ty {
                Some(TemplateTy::InheritBinding(cap)) => Some(cap),
                _ => None,
            })
            .chain(self.declared_captures.iter().copied())
    }

    /// Every capture the RHS references, for the coverage check.
    pub fn referenced_captures(&self) -> impl Iterator<Item = Capture> + '_ {
        self.required_captures()
            .chain(self.optional_captures.iter().copied())
    }

    /// The referenced captures no use requires, which the LHS need bind on only
    /// some alternation arms.
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
    /// `bool_not(var(c))` is `int_xor(var(c), IntConst(1)):I1`: three nodes.
    #[test]
    fn bool_not_template_builds_three_node_graph() {
        use crate::{Capture, TemplatePat, var};
        let c = Capture::new();
        let tpl = crate::template::bool_not(var(c)).into_template();
        assert!(tpl.root().is_ok(), "sealed template must have a root");
        assert_eq!(tpl.graph.all_node_ids().count(), 3);
    }
}
