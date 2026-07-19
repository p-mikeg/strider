//! Exposes construction verbs only, and deliberately no match verbs
//! (`set_node_predicate`, `set_value_width`, `set_force_ordered`,
//! `set_post_match`, predicate kindspecs): a [`Template`] is a build recipe,
//! not a query.
//!
//! Every node it creates is either a [`TmplNodeKind::Build`] carrying a
//! [`TemplateKind`], exact by default and overwritable at rewrite time via
//! [`set_template_kind`](TemplateBuilder::set_template_kind), or a
//! [`TmplNodeKind::Capture`] leaf. A finished [`Template`] is therefore
//! materialisable by construction.

use strider_ir::node::{NodeKind, ValueType};
use strider_ir::{ConstId, IntBinaryOp};

use crate::matcher::KindSpec;
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
    fn seal(self, input_slots: Vec<usize>) -> TmplNode {
        TmplNode {
            kind: self,
            input_slots,
        }
    }
}

/// # Output-signature validity is author-owned
///
/// The high-level verbs (`leaf`, `unary`, `binary`) declare canonical output
/// signatures and wire each input slot exactly once. The raw verbs
/// ([`node`](Self::node), [`input`](Self::input), the `*_output` slot verbs)
/// enforce nothing, and [`instantiate`](crate::template::instantiate) does not
/// run [`strider_ir::validate`], so an author using them owns output-signature
/// validity. Input-slot wiring IS validated at instantiation: gapped or
/// duplicate slots error out.
pub struct TemplateBuilder {
    core: StagedGraph<TmplNodeKind, TmplValue>,
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
        }
    }

    /// One value output at slot 0.
    pub fn leaf(&mut self, kind: KindSpec) -> TmplValueRef {
        let n = self.add_buildable(kind);
        self.add_built_output(n, TmplOutput::value(0))
    }

    /// Consumes `inner` at input slot 0, with one value output at slot 0.
    pub fn unary(&mut self, kind: KindSpec, inner: TmplValueRef) -> TmplValueRef {
        let n = self.add_buildable(kind);
        self.input(n, 0, inner);
        self.add_built_output(n, TmplOutput::value(0))
    }

    /// Consumes `l` at input slot 0 and `r` at slot 1, with one value output
    /// at slot 0.
    pub fn binary(&mut self, op: IntBinaryOp, l: TmplValueRef, r: TmplValueRef) -> TmplValueRef {
        let n = self.add_buildable(KindSpec::Exact(NodeKind::IntBinaryOp(op)));
        self.input(n, 0, l);
        self.input(n, 1, r);
        self.add_built_output(n, TmplOutput::value(0))
    }

    /// Bare: no inputs or outputs yet.
    pub fn node(&mut self, kind: KindSpec) -> TmplNodeRef {
        self.add_buildable(kind)
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

    /// Types the materialised node independently of the rewrite root.
    pub fn set_value_ty(&mut self, out: TmplValueRef, ty: ValueType) {
        self.out_data_of(out).ty = TemplateTy::Fixed(ty);
    }

    /// Types `out` from the width of a bound LHS capture.
    pub fn set_value_ty_of_binding(&mut self, out: TmplValueRef, cap: crate::Capture) {
        self.out_data_of(out).ty = TemplateTy::InheritBinding(cap);
    }

    /// Replaces the producing node's build spec with a dynamic-kind closure.
    pub fn set_template_kind(&mut self, out: TmplValueRef, kind: TemplateKind) {
        *self.core.kind_mut(out.node) = TmplNodeKind::Build(kind);
    }

    /// Adds a payload-less [`TmplNodeKind::Capture`] marker producing a
    /// [`TmplValue::ValueCapture`]. At instantiation it resolves to the LHS
    /// binding for `c`, reused verbatim. Returns only a [`TmplValueRef`], so
    /// an input can never be wired into a capture node.
    pub fn capture(&mut self, c: crate::capture::Capture) -> TmplValueRef {
        let n = TmplNodeRef(self.core.add_node(TmplNodeKind::Capture));
        self.add_value(n, TmplValue::ValueCapture(c))
    }

    /// Materialises every staged node in producer-before-consumer order,
    /// with no structural validation: a multi-sink template surfaces as an
    /// [`instantiate`](crate::template::instantiate) error, not here.
    ///
    /// # Panics
    /// On a cyclic staged graph (a builder bug).
    #[allow(clippy::expect_used)]
    pub fn finish(self) -> Template {
        let graph = self.core.seal().expect("cyclic staged template graph");
        Template::from_graph(graph)
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

    /// A non-exact spec (`Variant`, `Any`, `VariantWith`) has no concrete
    /// `NodeKind` and MUST be overwritten via
    /// [`set_template_kind`](Self::set_template_kind) before sealing.
    fn add_buildable(&mut self, kind: KindSpec) -> TmplNodeRef {
        // A template only ever passes `Exact`, stamped directly, or a
        // `Variant`-shaped placeholder from `node()`.
        let spec = match kind {
            KindSpec::Exact(k) => TemplateKind::Exact(k),
            // Overwritten by `set_template_kind` before sealing. Captures go
            // through `capture`, not through overwriting a build node.
            _ => TemplateKind::Exact(NodeKind::IntConst(ConstId::from_u32(0))),
        };
        TmplNodeRef(self.core.add_node(TmplNodeKind::Build(spec)))
    }

    #[allow(clippy::unreachable)]
    fn out_data_of(&mut self, out: TmplValueRef) -> &mut TmplOutput {
        match self.core.output_mut(out.node, out.output) {
            TmplValue::TmplOutput(o) => o,
            TmplValue::ValueCapture(_) => {
                unreachable!("type setter targets a built output, not a capture")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use strider_ir::IntBinaryOp;
    use strider_ir::node::NodeKind;

    #[test]
    fn binary_builder_wires_two_inputs_and_one_output() {
        let mut b = TemplateBuilder::new();
        let x = b.leaf(KindSpec::Exact(NodeKind::IntConst(ConstId::from_u32(5))));
        let k = b.leaf(KindSpec::Exact(NodeKind::IntConst(ConstId::from_u32(1))));
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
        let _built = b.leaf(KindSpec::Exact(NodeKind::IntConst(ConstId::from_u32(5))));
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
