//! Construction verbs for a [`Template`]. A [`TmplNodeKind::Build`] node is
//! built from an exact `NodeKind`, or from a rewrite-time closure through
//! [`dynamic_leaf`](TemplateBuilder::dynamic_leaf).

use strider_ir::IntBinaryOp;
use strider_ir::node::{NodeKind, ValueType};

use crate::staging::{SealNode, StagedGraph};
use crate::template::graph::{Template, TmplNode, TmplNodeKind, TmplOutput, TmplValue};
use crate::template::{TemplateKind, TemplateTy};

/// A staged node's output, by position.
#[derive(Clone, Copy)]
pub struct TmplValueRef {
    node: usize,
    output: usize,
}

/// A staged node, by position.
#[derive(Clone, Copy)]
pub struct TmplNodeRef(pub(crate) usize);

impl SealNode for TmplNodeKind {
    type Sealed = TmplNode;
    type Slot = usize;
    fn seal(self, input_slots: Vec<usize>) -> TmplNode {
        TmplNode {
            kind: self,
            input_slots,
        }
    }
}

/// # The raw verbs are checked at instantiation
///
/// The high-level verbs (`leaf`, `unary`, `binary`) declare canonical output
/// signatures and wire each input slot exactly once. The raw verbs
/// ([`node`](Self::node), [`input`](Self::input), the `*_output` slot verbs)
/// enforce nothing when called; [`instantiate`](crate::template::instantiate)
/// checks every node against its node signature and refuses a gapped or
/// duplicate input slot. No verb declares a control output, so a node whose
/// signature has one (`Call`, `Region`, `If`, ...) cannot be built.
pub struct TemplateBuilder {
    core: StagedGraph<TmplNodeKind, TmplValue>,
    declared_captures: Vec<crate::Capture>,
    optional_captures: Vec<crate::Capture>,
    /// The first refused verb, reported by [`Template::root`].
    rejected: Option<String>,
}

impl Default for TemplateBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl TemplateBuilder {
    pub fn new() -> Self {
        Self {
            core: StagedGraph::new(),
            declared_captures: Vec::new(),
            optional_captures: Vec::new(),
            rejected: None,
        }
    }

    /// Declares a capture a dynamic-kind closure reads at instantiation. It has
    /// no vertex, so without this the construction-time coverage check cannot
    /// see it and an LHS that never binds it errors mid-rewrite instead.
    pub fn declare_capture(&mut self, c: crate::Capture) {
        self.declared_captures.push(c);
    }

    /// Declares a capture the closure reads as an `Option`, so it may bind on
    /// only some alternation arms. The coverage check still refuses one the
    /// LHS mentions nowhere.
    pub fn declare_optional_capture(&mut self, c: crate::Capture) {
        self.declared_captures.push(c);
        self.optional_captures.push(c);
    }

    /// One value output at slot 0. A template node is built, so it names a
    /// kind, not a match spec:
    ///
    /// ```compile_fail,E0308
    /// use strider_pattern::matcher::KindSpec;
    /// use strider_pattern::template::TemplateBuilder;
    /// TemplateBuilder::new().leaf(KindSpec::Any);
    /// ```
    pub fn leaf(&mut self, kind: NodeKind) -> TmplValueRef {
        let n = self.node(kind);
        self.add_built_output(n, TmplOutput::value(0))
    }

    /// A leaf whose kind or constant a closure computes at rewrite time, from
    /// [`TemplateKind::Fn`] or [`TemplateKind::FnIntConst`];
    /// `TemplateKind::Exact(k)` builds what [`leaf`](Self::leaf)`(k)` does.
    pub fn dynamic_leaf(&mut self, kind: TemplateKind) -> TmplValueRef {
        let n = TmplNodeRef(self.core.add_node(TmplNodeKind::Build(kind)));
        self.add_built_output(n, TmplOutput::value(0))
    }

    /// Consumes `inner` at input slot 0, with one value output at slot 0.
    pub fn unary(&mut self, kind: NodeKind, inner: TmplValueRef) -> TmplValueRef {
        let n = self.node(kind);
        self.input(n, 0, inner);
        self.add_built_output(n, TmplOutput::value(0))
    }

    /// Consumes `l` at input slot 0 and `r` at slot 1, with one value output
    /// at slot 0.
    pub fn binary(&mut self, op: IntBinaryOp, l: TmplValueRef, r: TmplValueRef) -> TmplValueRef {
        let n = self.node(NodeKind::IntBinaryOp(op));
        self.input(n, 0, l);
        self.input(n, 1, r);
        self.add_built_output(n, TmplOutput::value(0))
    }

    /// Bare: no inputs or outputs yet.
    pub fn node(&mut self, kind: NodeKind) -> TmplNodeRef {
        TmplNodeRef(
            self.core
                .add_node(TmplNodeKind::Build(TemplateKind::Exact(kind))),
        )
    }

    pub fn input(&mut self, node: TmplNodeRef, slot: usize, prod: TmplValueRef) {
        self.core.add_input(node.0, slot, prod.node, prod.output);
    }

    pub fn value_output(&mut self, node: TmplNodeRef, slot: usize) -> TmplValueRef {
        self.add_built_output(node, TmplOutput::value(slot))
    }

    pub fn memory_output(&mut self, node: TmplNodeRef, slot: usize) -> TmplValueRef {
        self.add_built_output(node, TmplOutput::memory(slot))
    }

    /// Types the materialised node independently of the rewrite root. On a
    /// capture, which keeps its binding's type, the template is refused.
    pub fn set_value_ty(&mut self, out: TmplValueRef, ty: ValueType) {
        if let Some(o) = self.out_data_of(out) {
            o.ty = TemplateTy::Fixed(ty);
        }
    }

    /// Types `out` from the width of a bound LHS capture. On a capture, which
    /// keeps its binding's type, the template is refused.
    pub fn set_value_ty_of_binding(&mut self, out: TmplValueRef, cap: crate::Capture) {
        if let Some(o) = self.out_data_of(out) {
            o.ty = TemplateTy::InheritBinding(cap);
        }
    }

    /// Adds a payload-less `TmplNodeKind::Capture` marker producing a
    /// `TmplValue::ValueCapture`. At instantiation it resolves to the LHS
    /// binding for `c`, reused verbatim. Returns only a [`TmplValueRef`], so
    /// an input can never be wired into a capture node.
    pub fn capture(&mut self, c: crate::capture::Capture) -> TmplValueRef {
        let n = TmplNodeRef(self.core.add_node(TmplNodeKind::Capture));
        self.add_value(n, TmplValue::ValueCapture(c))
    }

    /// Materialises every staged node in producer-before-consumer order. A
    /// multi-sink template surfaces as an
    /// [`instantiate`](crate::template::instantiate) error.
    ///
    /// # Panics
    /// On a cyclic staged graph (a builder bug).
    pub fn finish(self) -> Template {
        let graph = self.core.seal().expect("cyclic staged template graph");
        Template::from_graph(
            graph,
            self.declared_captures,
            self.optional_captures,
            self.rejected,
        )
    }

    fn add_built_output(&mut self, node: TmplNodeRef, out: TmplOutput) -> TmplValueRef {
        self.add_value(node, TmplValue::TmplOutput(out))
    }

    fn add_value(&mut self, node: TmplNodeRef, out: TmplValue) -> TmplValueRef {
        let output = self.core.add_output(node.0, out);
        TmplValueRef {
            node: node.0,
            output,
        }
    }

    /// `None`, recording the refusal, when `out` is a capture.
    fn out_data_of(&mut self, out: TmplValueRef) -> Option<&mut TmplOutput> {
        match self.core.output_mut(out.node, out.output) {
            TmplValue::TmplOutput(o) => Some(o),
            TmplValue::ValueCapture(_) => {
                self.rejected.get_or_insert_with(|| {
                    "a template type setter (capture_typed, set_value_ty) targets a \
                     capture, which keeps the type of the value it bound"
                        .to_string()
                });
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use strider_ir::IntBinaryOp;

    #[test]
    fn binary_builder_wires_two_inputs_and_one_output() {
        let mut b = TemplateBuilder::new();
        let x = b.dynamic_leaf(TemplateKind::FnIntConst(Box::new(|_| Ok(5))));
        let k = b.dynamic_leaf(TemplateKind::FnIntConst(Box::new(|_| Ok(1))));
        let _sum = b.binary(IntBinaryOp::Add, x, k);
        let t = b.finish();
        assert_eq!(t.graph.all_node_ids().count(), 3);
        assert_eq!(t.graph.all_value_ids().count(), 3);
        assert!(t.root().is_ok());
    }

    #[test]
    fn capture_is_a_node_marker_with_a_value_capture_output() {
        // A capture leaf splits across both vertex enums: a payload-less
        // marker node, and a `ValueCapture(c)` output carrying the id. A
        // buildable node stays `Build(_)` producing a `TmplOutput(_)`.
        let c = crate::capture::Capture::new();
        let mut b = TemplateBuilder::new();
        let _built = b.dynamic_leaf(TemplateKind::FnIntConst(Box::new(|_| Ok(5))));
        let _cap = b.capture(c);
        let t = b.finish();

        // Exactly one Build node and one Capture node, the id on the
        // capture's output.
        let mut saw_build = false;
        let mut saw_capture = false;
        for node in t.graph.all_node_ids() {
            match &t.graph.node_kind(node).kind {
                TmplNodeKind::Build(_) => {
                    saw_build = true;
                    let out = t.graph.node_outputs(node)[0];
                    assert!(matches!(
                        t.graph.value_kind_ref(out),
                        TmplValue::TmplOutput(_)
                    ));
                }
                TmplNodeKind::Capture => {
                    saw_capture = true;
                    assert_eq!(t.graph.node_inputs(node).into_iter().count(), 0);
                    let out = t.graph.node_outputs(node)[0];
                    assert!(matches!(
                        t.graph.value_kind_ref(out),
                        TmplValue::ValueCapture(cc) if *cc == c
                    ));
                }
            }
        }
        assert!(saw_build && saw_capture);
    }
}
