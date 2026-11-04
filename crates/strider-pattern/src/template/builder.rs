//! Construction verbs for a [`Template`].

use strider_ir::node::{NodeKind, ValueType};

use crate::build_graph::BuildGraph;
use crate::template::graph::{Template, TmplNode, TmplNodeKind, TmplValue};
use crate::template::{TemplateKind, TemplateTy};
use strider_graph::{NodeId, ValueId};

/// A template node's output. A match-side output is not one:
///
/// ```compile_fail,E0308
/// use strider_ir::node::NodeKind;
/// use strider_pattern::matcher::{KindSpec, MatcherBuilder};
/// use strider_pattern::template::TemplateBuilder;
/// let v = MatcherBuilder::new().leaf(KindSpec::Any);
/// let mut b = TemplateBuilder::new();
/// let n = b.node(NodeKind::IntUnaryOp(strider_ir::IntUnaryOp::Neg));
/// b.input(n, 0, v);
/// ```
#[derive(Clone, Copy)]
pub struct TmplValueRef(ValueId);

/// A template node. A capture's node is never handed out, so it takes no input:
///
/// ```compile_fail,E0308
/// use strider_pattern::Capture;
/// use strider_pattern::template::TemplateBuilder;
/// let mut b = TemplateBuilder::new();
/// let x = b.capture(Capture::new());
/// b.input(strider_ir::node::NodeId::from_u32(0), 0, x);
/// ```
#[derive(Clone, Copy)]
pub struct TmplNodeRef(NodeId);

/// Builds a [`Template`]. The raw verbs ([`node`](Self::node),
/// [`input`](Self::input), the `*_output` verbs) are checked against each node's
/// signature only at [`instantiate`](crate::template::instantiate). No verb
/// declares a control output, so `Call`, `Region`, `If`, ... cannot be built.
#[derive(Default)]
pub struct TemplateBuilder {
    core: BuildGraph<TmplNode, TmplValue>,
    declared_captures: Vec<crate::Capture>,
    optional_captures: Vec<crate::Capture>,
}

impl TemplateBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Declares a capture a closure reads, so the coverage check sees it.
    pub fn declare_capture(&mut self, c: crate::Capture) {
        self.declared_captures.push(c);
    }

    /// Declares a capture a closure reads as an `Option`, so it may bind on only
    /// some alternation arms. The LHS must still mention it, and another use
    /// that needs it bound overrides this.
    pub fn declare_optional_capture(&mut self, c: crate::Capture) {
        self.optional_captures.push(c);
    }

    /// A node with one value output at slot 0. It takes a kind, not a match spec:
    ///
    /// ```compile_fail,E0308
    /// use strider_pattern::matcher::KindSpec;
    /// use strider_pattern::template::TemplateBuilder;
    /// TemplateBuilder::new().leaf(KindSpec::Any);
    /// ```
    pub fn leaf(&mut self, kind: NodeKind) -> TmplValueRef {
        self.build(TemplateKind::Exact(kind), &[], TemplateTy::InheritRoot)
    }

    /// A leaf from any [`TemplateKind`], such as a rewrite-time closure.
    pub fn dynamic_leaf(&mut self, kind: TemplateKind) -> TmplValueRef {
        self.build(kind, &[], TemplateTy::InheritRoot)
    }

    /// One node over `inputs` at slots `0..`, its value output at slot 0 typed `ty`.
    pub(crate) fn build(
        &mut self,
        kind: TemplateKind,
        inputs: &[TmplValueRef],
        ty: TemplateTy,
    ) -> TmplValueRef {
        let n = self.add_node(TmplNodeKind::Build(kind));
        for (slot, &prod) in inputs.iter().enumerate() {
            self.input(n, slot, prod);
        }
        let ty = Some(ty);
        self.add_value(n, TmplValue { slot: 0, ty })
    }

    /// A node with no inputs or outputs yet.
    pub fn node(&mut self, kind: NodeKind) -> TmplNodeRef {
        self.add_node(TmplNodeKind::Build(TemplateKind::Exact(kind)))
    }

    /// Wires `prod` into `node`'s `slot`. `prod` must come from a node built
    /// before `node`, or from a [`capture`](Self::capture).
    pub fn input(&mut self, node: TmplNodeRef, slot: usize, prod: TmplValueRef) {
        if self.core.add_input(node.0, prod.0, self.is_capture(prod)) {
            self.core.graph.node_kind_mut(node.0).input_slots.push(slot);
        }
    }

    /// A value output, inheriting the rewrite root's type unless one is set.
    pub fn value_output(&mut self, node: TmplNodeRef, slot: usize) -> TmplValueRef {
        let ty = Some(TemplateTy::InheritRoot);
        self.add_value(node, TmplValue { slot, ty })
    }

    /// A memory output.
    pub fn memory_output(&mut self, node: TmplNodeRef, slot: usize) -> TmplValueRef {
        self.add_value(node, TmplValue { slot, ty: None })
    }

    /// Fixes `out`'s type. Refused on a capture.
    pub fn set_value_ty(&mut self, out: TmplValueRef, ty: ValueType) {
        self.set_ty(out, TemplateTy::Fixed(ty));
    }

    /// Types `out` from a bound LHS capture's value. Refused on a capture.
    pub fn set_value_ty_of_binding(&mut self, out: TmplValueRef, cap: crate::Capture) {
        self.set_ty(out, TemplateTy::InheritBinding(cap));
    }

    /// A leaf that instantiates as the LHS binding for `c`.
    pub fn capture(&mut self, c: crate::capture::Capture) -> TmplValueRef {
        let n = self.add_node(TmplNodeKind::Capture);
        let ty = Some(TemplateTy::InheritBinding(c));
        self.add_value(n, TmplValue { slot: 0, ty })
    }

    /// See [`BuildGraph::enter_nesting`]; a template is charged to the stack alone.
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

    /// The finished template; a refusal surfaces through [`Template::root`].
    pub fn finish(self) -> Template {
        Template {
            graph: self.core.graph,
            declared_captures: self.declared_captures,
            optional_captures: self.optional_captures,
            rejected: self.core.refusal,
        }
    }

    fn add_node(&mut self, kind: TmplNodeKind) -> TmplNodeRef {
        TmplNodeRef(self.core.add_node(TmplNode {
            kind,
            input_slots: Vec::new(),
        }))
    }

    fn add_value(&mut self, node: TmplNodeRef, out: TmplValue) -> TmplValueRef {
        TmplValueRef(self.core.add_output(node.0, out))
    }

    fn is_capture(&self, v: TmplValueRef) -> bool {
        let node = self.core.graph.producer(v.0);
        matches!(self.core.graph.node_kind(node).kind, TmplNodeKind::Capture)
    }

    /// Refused on a capture; a no-op on a memory output.
    fn set_ty(&mut self, out: TmplValueRef, ty: TemplateTy) {
        if self.is_capture(out) {
            return self.core.reject(
                "a template type setter (capture_typed, set_value_ty) targets a capture, \
                 which keeps the type of the value it bound"
                    .to_string(),
            );
        }
        if let Some(t) = &mut self.core.graph.value_kind_mut(out.0).ty {
            *t = ty;
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
        let n = b.node(NodeKind::IntBinaryOp(IntBinaryOp::Add));
        b.input(n, 0, x);
        b.input(n, 1, k);
        b.value_output(n, 0);
        let t = b.finish();
        assert_eq!(t.graph.all_node_ids().count(), 3);
        assert_eq!(t.graph.all_value_ids().count(), 3);
        assert!(t.root().is_ok());
    }

    /// The first refusal is the one reported, ahead of a later wiring
    /// refusal and a later explicit one.
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
    fn capture_is_a_node_whose_output_inherits_its_binding() {
        let c = crate::capture::Capture::new();
        let mut b = TemplateBuilder::new();
        let _built = b.dynamic_leaf(TemplateKind::FnIntConst(Box::new(|_| {
            Ok(strider_ir::UBig::from(5u8))
        })));
        let _cap = b.capture(c);
        let t = b.finish();

        let mut saw_build = false;
        let mut saw_capture = false;
        for node in t.graph.all_node_ids() {
            match &t.graph.node_kind(node).kind {
                TmplNodeKind::Build(_) => {
                    saw_build = true;
                    let out = t.graph.node_outputs(node)[0];
                    assert!(t.graph.value_kind_ref(out).ty.is_some());
                }
                TmplNodeKind::Capture => {
                    saw_capture = true;
                    assert_eq!(t.graph.node_inputs(node).into_iter().count(), 0);
                    let out = t.graph.node_outputs(node)[0];
                    assert!(matches!(
                        t.graph.value_kind_ref(out).ty,
                        Some(TemplateTy::InheritBinding(cc)) if cc == c
                    ));
                }
            }
        }
        assert!(saw_build && saw_capture);
    }
}
