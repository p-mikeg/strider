//! The build-side imperative builder, [`TemplateBuilder`].
//!
//! [`TemplateBuilder`] is the single lowering target for every
//! build-side ([`TemplatePat`](crate::template::template_pat::TemplatePat)) typed
//! struct. It exposes **construction verbs only** — `leaf` / `unary` /
//! `binary` / `node` / `input` + the `*_output` slot verbs + `capture`
//! (for `var(c)` template leaves) + the dynamic-kind
//! `set_template_kind` / `set_template_ty` setters (for the
//! `*_const_with` materialiser path). It deliberately exposes **no match
//! verbs** (`set_node_predicate` / `set_value_width` / `set_force_ordered` /
//! `set_post_match` / predicate kindspecs): a [`Template`] is a build
//! recipe, not a query.
//!
//! Each node it creates is either a [`TmplNodeKind::Build`] carrying a
//! [`TemplateKind`] build spec (an exact `NodeKind` by default; a kind
//! computed at rewrite time overwrites it via
//! [`set_template_kind`](TemplateBuilder::set_template_kind)) or a
//! [`TmplNodeKind::Capture`] leaf (via [`capture`](TemplateBuilder::capture)),
//! so a finished [`Template`] is materialisable by
//! [`instantiate`](crate::template::instantiate) by construction.
//!
//! Like the match-side [`MatcherBuilder`](crate::matcher::MatcherBuilder),
//! it stages every node into the shared [`StagedGraph`] core and materialises
//! the whole DAG into the generic [`strider_graph::Graph`] at
//! [`finish`](TemplateBuilder::finish), in producer-before-consumer order — the generic
//! graph creates a node with all its outputs and resolved inputs at once, so
//! the incremental `node()` / `*_output()` / `input()` verbs cannot write
//! straight through.

use strider_ir::IntBinaryOp;
use strider_ir::node::{IntPayload, NodeKind, ValueType};

use crate::matcher::KindSpec;
use crate::staging::{SealNode, StagedGraph};
use crate::template::graph::{Template, TmplNode, TmplNodeKind, TmplOutput, TmplValue};
use crate::template::{TemplateKind, TemplateTy};

/// Handle to a template **output** vertex (a staged node's output by
/// position).
#[derive(Clone, Copy)]
pub struct TmplValueRef {
    pub(crate) node: usize,
    pub(crate) output: usize,
}

/// Handle to a template **node** vertex (a staged node by position).
#[derive(Clone, Copy)]
pub struct TmplNodeRef(pub(crate) usize);

/// Seals a staged [`TmplNodeKind`] build spec into a materialised
/// [`TmplNode`], pairing it with the recovered consumer-slot list.
impl SealNode for TmplNodeKind {
    type Sealed = TmplNode;
    fn seal(self, input_slots: Vec<usize>) -> TmplNode {
        TmplNode {
            kind: self,
            input_slots,
        }
    }
}

/// Imperative builder for a build-side [`Template`].
///
/// Stages each node verb into a shared `StagedGraph` and materialises the
/// whole DAG at [`finish`](TemplateBuilder::finish). The build root is derived
/// structurally.
///
/// The returned [`TmplValueRef`] / [`TmplNodeRef`] handles are scoped to
/// the builder that produced them, and are a distinct type from the match
/// side's `PatValueRef` / `PatNodeRef`, so the two builders' handles cannot
/// be crossed.
///
/// # Output-signature validity is author-owned
///
/// The high-level construction verbs (`leaf` / `unary` / `binary`) and the
/// typed `template::` free functions built on top of them declare canonical
/// output signatures and wire each input slot exactly once, so a
/// [`Template`] built that way always materialises a structurally-valid IR
/// node. The **raw** verbs — [`node`](Self::node) plus
/// [`input`](Self::input) and the `*_output` slot verbs — do not enforce
/// this: a hand-built node can declare an output signature that does not
/// match its `NodeKind`'s `expected_signature`. Because
/// [`instantiate`](crate::template::instantiate) does **not** run
/// [`strider_ir::validate`], such an output-signature mismatch is not
/// caught — authors using the raw verbs own that invariant. Input-slot
/// wiring, by contrast, **is** validated at instantiation: gapped
/// (non-contiguous) or duplicate input slots are rejected with an error
/// rather than silently closed / overwritten.
pub struct TemplateBuilder {
    core: StagedGraph<TmplNodeKind, TmplValue>,
}

impl Default for TemplateBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl TemplateBuilder {
    /// A builder over an empty template.
    pub fn new() -> Self {
        Self {
            core: StagedGraph::new(),
        }
    }

    // ── construction verbs ───────────────────────────────────────────

    /// A leaf node materialising the given exact `kind`, with one value
    /// output at slot `0`.
    pub fn leaf(&mut self, kind: KindSpec) -> TmplValueRef {
        let n = self.add_buildable(kind);
        self.add_built_output(n, TmplOutput::value(0))
    }

    /// A unary node materialising `kind`, consuming `inner` at slot `0`,
    /// with one value output at slot `0`.
    pub fn unary(&mut self, kind: KindSpec, inner: TmplValueRef) -> TmplValueRef {
        let n = self.add_buildable(kind);
        self.input(n, 0, inner);
        self.add_built_output(n, TmplOutput::value(0))
    }

    /// A binary [`IntBinaryOp`] node consuming `l` at slot `0` and `r`
    /// at slot `1`, with one value output at slot `0`.
    pub fn binary(&mut self, op: IntBinaryOp, l: TmplValueRef, r: TmplValueRef) -> TmplValueRef {
        let n = self.add_buildable(KindSpec::Exact(NodeKind::IntBinaryOp(op)));
        self.input(n, 0, l);
        self.input(n, 1, r);
        self.add_built_output(n, TmplOutput::value(0))
    }

    /// A bare node materialising `kind`, with no inputs/outputs yet.
    pub fn node(&mut self, kind: KindSpec) -> TmplNodeRef {
        self.add_buildable(kind)
    }

    /// Wires `prod` into `node`'s input `slot`.
    pub fn input(&mut self, node: TmplNodeRef, slot: usize, prod: TmplValueRef) {
        self.core.add_input(node.0, slot, prod.node, prod.output);
    }

    /// Adds a value output at `slot` to `node`.
    pub fn value_output(&mut self, node: TmplNodeRef, slot: usize) -> TmplValueRef {
        self.add_built_output(node, TmplOutput::value(slot))
    }

    /// Adds a memory-token output at `slot` to `node`.
    pub fn memory_output(&mut self, node: TmplNodeRef, slot: usize) -> TmplValueRef {
        self.add_built_output(node, TmplOutput::memory(slot))
    }

    /// Adds a control output at `slot` to `node`.
    pub fn control_output(&mut self, node: TmplNodeRef, slot: usize) -> TmplValueRef {
        self.add_built_output(node, TmplOutput::control(slot))
    }

    // ── annotators ───────────────────────────────────────────────────

    /// Pins `out`'s value output to a fixed build type (so the
    /// materialised node is typed independently of the rewrite root).
    pub fn set_value_ty(&mut self, out: TmplValueRef, ty: ValueType) {
        self.out_data_of(out).ty = TemplateTy::Fixed(ty);
    }

    /// Overwrites the build spec of the node producing `out` with a
    /// dynamic-kind closure (the `*_const_with` materialiser path).
    pub fn set_template_kind(&mut self, out: TmplValueRef, kind: TemplateKind) {
        *self.core.kind_mut(out.node) = TmplNodeKind::Build(kind);
    }

    /// Records `out`'s value output as inheriting the rewrite root's
    /// output type at instantiation time (the default).
    pub fn set_inherit_root_ty(&mut self, out: TmplValueRef) {
        self.out_data_of(out).ty = TemplateTy::InheritRoot;
    }

    /// Adds a fresh capture leaf — a payload-less [`TmplNodeKind::Capture`]
    /// marker node producing a [`TmplValue::ValueCapture`] — and
    /// returns its value handle. At instantiation the value capture resolves
    /// to the LHS binding for `c` (the captured value re-used verbatim, the
    /// `add(x, 0) → x` shape).
    ///
    /// A capture is a **leaf by construction**: this verb returns only the
    /// value handle (a [`TmplValueRef`]), never a node handle, so there is
    /// no way to wire inputs into a capture node. The "captures are leaves"
    /// invariant therefore holds structurally, not by convention.
    pub fn capture(&mut self, c: crate::capture::Capture) -> TmplValueRef {
        let n = TmplNodeRef(self.core.add_node(TmplNodeKind::Capture));
        self.add_value(n, TmplValue::ValueCapture(c))
    }

    // ── sealing ──────────────────────────────────────────────────────

    /// Seals the staged graph into a [`Template`].
    ///
    /// Materialises every staged node into the generic graph in
    /// producer-before-consumer order. Performs no structural validation:
    /// the build root is derived structurally (the unique sink) at
    /// instantiation time, so a malformed template (multiple sinks, a
    /// cycle) surfaces as an error from
    /// [`instantiate`](crate::template::instantiate) rather than panicking.
    ///
    /// # Panics
    /// Panics on a cyclic staged graph (a template is always a DAG).
    #[allow(clippy::expect_used)]
    pub fn finish(self) -> Template {
        let graph = self.core.seal().expect("cyclic staged template graph");
        Template::from_graph(graph)
    }

    // ── internal plumbing ────────────────────────────────────────────

    /// Adds a built output (`TmplValue::TmplOutput`) produced by `node`.
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

    /// Stages a buildable node stamped with a [`TemplateKind::Exact`] build
    /// spec derived from `kind` (for the exact-kind case). Non-exact
    /// specs (`Variant` / `Any` / `VariantWith`) have no concrete
    /// `NodeKind`, so the caller must overwrite the spec via
    /// [`set_template_kind`](Self::set_template_kind) before sealing.
    fn add_buildable(&mut self, kind: KindSpec) -> TmplNodeRef {
        // The only kinds a template ever passes are `Exact` (stamped
        // directly) and the `Variant`-shaped placeholders for `node()`
        // (whose `set_template_kind` overwrite lands before sealing).
        let spec = match kind {
            KindSpec::Exact(k) => TemplateKind::Exact(k),
            // Placeholder; overwritten by `set_template_kind` before
            // sealing. (Captures are created directly via `capture`, not
            // by overwriting a build node.)
            _ => TemplateKind::Exact(NodeKind::IntConst(IntPayload::Small(0))),
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
    use strider_ir::node::{IntPayload, NodeKind};

    #[test]
    fn binary_builder_wires_two_inputs_and_one_output() {
        let mut b = TemplateBuilder::new();
        let x = b.leaf(KindSpec::Exact(NodeKind::IntConst(IntPayload::Small(5))));
        let k = b.leaf(KindSpec::Exact(NodeKind::IntConst(IntPayload::Small(1))));
        let _sum = b.binary(IntBinaryOp::Add, x, k);
        let t = b.finish();
        assert_eq!(t.node_count(), 3);
        assert_eq!(t.output_count(), 3);
        assert!(t.root().is_ok());
    }

    #[test]
    fn capture_is_a_node_marker_with_a_value_capture_output() {
        // A capture leaf is split across both vertex enums: the node is a
        // payload-less `TmplNodeKind::Capture` marker (the opaque node
        // side), and its output is an `TmplValue::ValueCapture(c)` that
        // carries the capture id and resolves to a value (the value side).
        // A buildable node stays `TmplNodeKind::Build(_)` producing an
        // `TmplValue::TmplOutput(_)`. A capture is a leaf by construction —
        // the verb returns only the value handle.
        let c = crate::capture::Capture::new();
        let mut b = TemplateBuilder::new();
        let _built = b.leaf(KindSpec::Exact(NodeKind::IntConst(IntPayload::Small(5))));
        let _cap = b.capture(c);
        let t = b.finish();

        // Walk the materialised graph: exactly one Build node and one
        // Capture node, the capture id on the capture node's output.
        let mut saw_build = false;
        let mut saw_capture = false;
        for node in t.graph.all_node_ids() {
            match &t.graph.node_kind(node).kind {
                TmplNodeKind::Build(_) => {
                    saw_build = true;
                    // Built node produces a TmplOutput.
                    let out = t.graph.node_outputs(node)[0];
                    assert!(matches!(
                        t.graph.value_kind_ref(out),
                        TmplValue::TmplOutput(_)
                    ));
                }
                TmplNodeKind::Capture => {
                    saw_capture = true;
                    // Capture leaf: no inputs, output is the ValueCapture.
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
