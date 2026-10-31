//! Construction verbs for a [`Template`]. A [`TmplNodeKind::Build`] node is
//! built from an exact `NodeKind`, or from a rewrite-time closure through
//! [`dynamic_leaf`](TemplateBuilder::dynamic_leaf).

use strider_ir::IntBinaryOp;
use strider_ir::node::{NodeKind, ValueType};

use crate::staging::{SealNode, StagedGraph};
use crate::template::graph::{Template, TmplNode, TmplNodeKind, TmplValue};
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
    /// LHS mentions nowhere, and the relaxation drops away if another use in
    /// the same template needs `c` bound.
    pub fn declare_optional_capture(&mut self, c: crate::Capture) {
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
        self.value_output(n, 0)
    }

    /// A leaf whose kind or constant a closure computes at rewrite time, from
    /// [`TemplateKind::Fn`] or [`TemplateKind::FnIntConst`];
    /// `TemplateKind::Exact(k)` builds what [`leaf`](Self::leaf)`(k)` does.
    pub fn dynamic_leaf(&mut self, kind: TemplateKind) -> TmplValueRef {
        let n = TmplNodeRef(self.core.add_node(TmplNodeKind::Build(kind)));
        self.value_output(n, 0)
    }

    /// Consumes `inner` at input slot 0, with one value output at slot 0.
    pub fn unary(&mut self, kind: NodeKind, inner: TmplValueRef) -> TmplValueRef {
        let n = self.node(kind);
        self.input(n, 0, inner);
        self.value_output(n, 0)
    }

    /// Consumes `l` at input slot 0 and `r` at slot 1, with one value output
    /// at slot 0.
    pub fn binary(&mut self, op: IntBinaryOp, l: TmplValueRef, r: TmplValueRef) -> TmplValueRef {
        let n = self.node(NodeKind::IntBinaryOp(op));
        self.input(n, 0, l);
        self.input(n, 1, r);
        self.value_output(n, 0)
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

    /// Inherits the rewrite root's type unless one is set.
    pub fn value_output(&mut self, node: TmplNodeRef, slot: usize) -> TmplValueRef {
        let ty = TemplateTy::InheritRoot;
        self.add_value(node, TmplValue::Value { slot, ty })
    }

    pub fn memory_output(&mut self, node: TmplNodeRef, slot: usize) -> TmplValueRef {
        self.add_value(node, TmplValue::Memory { slot })
    }

    /// Types the materialised node independently of the rewrite root. On a
    /// capture, which keeps its binding's type, the template is refused.
    pub fn set_value_ty(&mut self, out: TmplValueRef, ty: ValueType) {
        self.set_ty(out, TemplateTy::Fixed(ty));
    }

    /// Types `out` from the width of a bound LHS capture. On a capture, which
    /// keeps its binding's type, the template is refused.
    pub fn set_value_ty_of_binding(&mut self, out: TmplValueRef, cap: crate::Capture) {
        self.set_ty(out, TemplateTy::InheritBinding(cap));
    }

    /// Adds a payload-less `TmplNodeKind::Capture` marker producing a
    /// `TmplValue::ValueCapture`. At instantiation it resolves to the LHS
    /// binding for `c`, reused verbatim. Returns only a [`TmplValueRef`], so
    /// an input can never be wired into a capture node.
    pub fn capture(&mut self, c: crate::capture::Capture) -> TmplValueRef {
        let n = TmplNodeRef(self.core.add_node(TmplNodeKind::Capture));
        self.add_value(n, TmplValue::ValueCapture(c))
    }

    /// See [`StagedGraph::enter_nesting`]: a template is charged to the stack
    /// alone.
    pub(crate) fn enter_nesting(&mut self) -> bool {
        self.core.enter_nesting(false)
    }

    pub(crate) fn leave_nesting(&mut self) {
        self.core.leave_nesting(false);
    }

    /// Records `why` and lowers a placeholder in place of what was refused.
    pub(crate) fn refuse(&mut self, why: &str) -> TmplValueRef {
        self.core.reject(why.to_string());
        self.placeholder()
    }

    /// Stands for an operand that was refused.
    pub(crate) fn placeholder(&mut self) -> TmplValueRef {
        self.capture(crate::Capture::internal())
    }

    /// Materialises every staged node in producer-before-consumer order. A
    /// refused, cyclic or multi-sink template surfaces through
    /// [`Template::root`].
    pub fn finish(self) -> Template {
        let (graph, refusal) = self.core.seal();
        Template::from_graph(
            graph,
            self.declared_captures,
            self.optional_captures,
            refusal,
        )
    }

    fn add_value(&mut self, node: TmplNodeRef, out: TmplValue) -> TmplValueRef {
        let output = self.core.add_output(node.0, out);
        TmplValueRef {
            node: node.0,
            output,
        }
    }

    /// Refused when `out` is a capture; a memory output has no type to set.
    fn set_ty(&mut self, out: TmplValueRef, ty: TemplateTy) {
        match self.core.output_mut(out.node, out.output) {
            TmplValue::Value { ty: t, .. } => *t = ty,
            TmplValue::Memory { .. } => {}
            TmplValue::ValueCapture(_) => self.core.reject(
                "a template type setter (capture_typed, set_value_ty) targets a capture, \
                 which keeps the type of the value it bound"
                    .to_string(),
            ),
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
        let x = b.dynamic_leaf(TemplateKind::FnIntConst(Box::new(|_| {
            Ok(strider_ir::UBig::from(5u8))
        })));
        let k = b.dynamic_leaf(TemplateKind::FnIntConst(Box::new(|_| {
            Ok(strider_ir::UBig::ONE)
        })));
        let _sum = b.binary(IntBinaryOp::Add, x, k);
        let t = b.finish();
        assert_eq!(t.graph.all_node_ids().count(), 3);
        assert_eq!(t.graph.all_value_ids().count(), 3);
        assert!(t.root().is_ok());
    }

    /// The first refusal is the one reported, ahead of a later one and of the
    /// cycle it sits in.
    #[test]
    fn the_first_template_refusal_wins() {
        let mut b = TemplateBuilder::new();
        let cap = b.capture(crate::Capture::new());
        b.set_value_ty(cap, ValueType::I1);
        b.core.reject("second".into());
        let n = b.node(NodeKind::IntBinaryOp(IntBinaryOp::Add));
        let out = b.value_output(n, 0);
        b.input(n, 0, out);
        let err = b.finish().root().unwrap_err().to_string();
        assert!(err.contains("targets a capture"), "{err}");
    }

    #[test]
    fn capture_is_a_node_marker_with_a_value_capture_output() {
        // A capture leaf splits across both vertex enums: a payload-less
        // marker node, and a `ValueCapture(c)` output carrying the id. A
        // buildable node stays `Build(_)` producing a `Value { .. }`.
        let c = crate::capture::Capture::new();
        let mut b = TemplateBuilder::new();
        let _built = b.dynamic_leaf(TemplateKind::FnIntConst(Box::new(|_| {
            Ok(strider_ir::UBig::from(5u8))
        })));
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
                        TmplValue::Value { .. }
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
