//! Template instantiation: materialising a [`Template`] as fresh IR.
//!
//! A [`Template`] is the build-side counterpart of
//! [`Pattern`](crate::pattern::Pattern): every node either declares a
//! [`TemplateKind`] (an exact `NodeKind` or a dynamic `Fn`) plus an
//! output [`TemplateTy`], or is capture-only. [`instantiate`] walks the
//! bipartite store in topological order, resolves capture-only nodes
//! through the LHS [`Bindings`], synthesises every buildable node via
//! [`strider_ir::Graph::create_node`], and returns the root's
//! materialised value output.

mod builder;
mod ctx;
mod graph;

pub use builder::{TemplateBuilder, TmplNodeRef, TmplOutRef};
pub use ctx::TemplateCtx;
pub use graph::{Template, TmplNode, TmplOutput};

// Build-side twin value-op factories (`template::add`, `template::sub`,
// …). These share the typed structs of the bare match-side factories but
// carry `TemplatePat` constructor bounds, so the match/template boundary
// is enforced at the construction call site. Re-exported here so callers
// write `strider_pattern::template::add(...)` on a rewrite RHS.
pub use crate::typed::value_ops::template::*;

use std::collections::BTreeMap;

use anyhow::anyhow;
use petgraph::stable_graph::NodeIndex;
use rustc_hash::FxHashMap;
use strider_ir::Function;
use strider_ir::node::{NodeId, NodeKind, ValueId, ValueKind, ValueType};

use crate::bigraph::reachable_topo;
use crate::bindings::Bindings;
use crate::pattern::OutputKindSpec;

/// Type alias for the [`TemplateKind::Fn`] closure shape. Factored out
/// to keep [`TemplateKind`] legible under clippy's `type_complexity`
/// lint. `Box` (single-threaded; no `Arc` / `Rc` / `Send` / `Sync` in
/// the core).
pub type TemplateKindFn = Box<dyn Fn(&TemplateCtx<'_>) -> anyhow::Result<NodeKind>>;

/// How a template node materialises into fresh IR during instantiation.
pub enum TemplateKind {
    /// Emit a node with the given exact [`NodeKind`].
    Exact(NodeKind),
    /// Dynamic-kind closure variant. The closure receives a
    /// [`TemplateCtx`] — exposing the captured LHS [`Bindings`], the
    /// matched-root `NodeId` / output type, and a shared
    /// [`Function`] — and returns the `NodeKind` to materialise. Used
    /// by the `*_const_with` family of builders to emit constants whose
    /// value is computed from captured operand values at rewrite time.
    Fn(TemplateKindFn),
}

/// The output type a template node declares for its value output.
#[derive(Clone, Copy)]
pub enum TemplateTy {
    /// Inherit the rewrite root's output type (resolved at
    /// instantiation time).
    InheritRoot,
    /// A fixed output type, independent of the root.
    Fixed(ValueType),
}

/// Materialise `template` as an IR sub-graph rooted at the returned
/// output.
///
/// Captures are resolved from `bindings`; `root_ty` is the output type
/// used for any node whose [`TemplateTy`] is [`TemplateTy::InheritRoot`].
/// `lhs_root` is the matched LHS root `NodeId` exposed to
/// [`TemplateKind::Fn`] closures via [`TemplateCtx::root`] — pure-`Exact`
/// templates ignore it, so standalone callers may pass any valid
/// `NodeId` from `function`.
///
/// Interior nodes may be multi-output (a built `Store` / `Call`
/// producing a memory token a later node consumes); the **root** yields a
/// single value output — the contract the single-value rewrite rule
/// relies on.
///
/// # Author-owned output-signature validity
///
/// `create_node` is called with each template node's **declared** output
/// signature; this function does **not** run [`strider_ir::validate`] on
/// the materialised sub-graph, and the rewrite path never validates
/// afterward either. It is the [`Template`] author's responsibility that
/// (a) every node's declared output signature matches its `NodeKind`'s
/// `expected_signature`, and (b) no two producers are wired into the same
/// input slot — inputs are collected into a `BTreeMap` keyed by slot, so a
/// duplicate slot silently overwrites the earlier edge. The typed
/// `template::` builders guarantee both by construction; a [`Template`]
/// hand-built via the raw [`TemplateBuilder`]
/// node / output verbs does not.
///
/// # Errors
///
/// Returns an error if the template is rootless, references an unbound
/// capture, has a [`TemplateKind::Fn`] closure that itself errors, or if
/// the underlying `create_node` call fails to produce a value output.
pub fn instantiate(
    template: &Template,
    function: &mut Function,
    bindings: &Bindings,
    lhs_root: NodeId,
    root_ty: ValueType,
) -> anyhow::Result<ValueId> {
    let Some(root) = template.root() else {
        return Err(anyhow!("rootless template"));
    };
    let order = reachable_topo(&template.graph, root)?;

    // Map from a template *output vertex* → its materialised IR
    // ValueId. Keying on the output vertex (not the producer node)
    // lets a multi-output interior node feed the right slot to each
    // consumer: a `Store`'s memory output and a sibling value output
    // resolve to distinct IR outputs.
    let mut materialised: FxHashMap<NodeIndex, ValueId> = FxHashMap::default();
    // The root node's value output, captured as the root materialises.
    let mut root_value: Option<ValueId> = None;

    for vtx in order {
        // Only node vertices materialise; output vertices are wiring.
        let Some(nd) = template.graph.node_weight(vtx) else {
            continue;
        };

        // 1. Capture-bearing node: resolve through the LHS bindings.
        //    The capture *is* the materialisation (a captured LHS value
        //    re-used verbatim in the RHS). A captured node has a single
        //    value output vertex; map it to the bound output.
        if let Some(cap) = nd.capture {
            let bound_out = bindings.get_output(cap).ok_or_else(|| {
                anyhow!("capture {cap:?} referenced in template but unbound by LHS")
            })?;
            for out_vtx in template.graph.produced_outputs(vtx) {
                materialised.insert(out_vtx, bound_out);
            }
            if vtx == root {
                root_value = Some(bound_out);
            }
            continue;
        }

        // The node's declared output value type (resolved against the
        // rewrite root for `InheritRoot`).
        let ty = match nd.ty {
            TemplateTy::Fixed(t) => t,
            TemplateTy::InheritRoot => root_ty,
        };

        // 2. Buildable node: synthesise fresh IR.
        let kind = match &nd.kind {
            TemplateKind::Exact(k) => *k,
            TemplateKind::Fn(f) => {
                let ctx = TemplateCtx {
                    function,
                    bindings,
                    root: lhs_root,
                    root_ty: ty,
                };
                f(&ctx)?
            }
        };

        // Collect inputs in slot order: each `Consumes` edge names the
        // producer output vertex feeding this node's slot; read its
        // already-materialised IR output.
        let mut inputs_by_slot: BTreeMap<usize, ValueId> = BTreeMap::new();
        for (slot, producer_out_vtx) in template.graph.consumed_inputs(vtx) {
            let producer_out = *materialised.get(&producer_out_vtx).ok_or_else(|| {
                anyhow!("producer output not materialised before consumer — topo order bug")
            })?;
            inputs_by_slot.insert(slot, producer_out);
        }
        let inputs: Vec<ValueId> = inputs_by_slot.into_values().collect();

        // Declare the node's output signature from its template output
        // vertices. The common path is a single value output; a
        // multi-output node (e.g. a `Store` declaring a memory output a
        // later node consumes) declares that signature instead.
        let outputs = output_kinds_for(template, vtx, ty);

        let node = function.graph_mut().create_node(kind, inputs, outputs);

        // Map each template output vertex to the IR output at the
        // matching slot, so multi-output consumers wire the right edge.
        let ir_outputs = function.node_outputs(node);
        for out_vtx in template.graph.produced_outputs(vtx) {
            let Some(o) = template.graph.output_weight(out_vtx) else {
                continue;
            };
            let ir_out = *ir_outputs.get(o.slot).ok_or_else(|| {
                anyhow!("template output slot {} out of range for instantiated node", o.slot)
            })?;
            materialised.insert(out_vtx, ir_out);
        }

        if vtx == root {
            root_value = Some(
                first_value_output(function, node)
                    .ok_or_else(|| anyhow!("instantiated root node has no value output"))?,
            );
        }
    }

    root_value.ok_or_else(|| anyhow!("root template node never materialised"))
}

/// The full output-kind signature a template node declares, derived from
/// its template output vertices. Falls back to a single value output of
/// type `ty` when the node has no explicit output vertex (the common
/// value-expression case).
fn output_kinds_for(
    template: &Template,
    node_vtx: NodeIndex,
    ty: ValueType,
) -> Vec<ValueKind> {
    let mut by_slot: BTreeMap<usize, ValueKind> = BTreeMap::new();
    for out_vtx in template.graph.produced_outputs(node_vtx) {
        if let Some(o) = template.graph.output_weight(out_vtx) {
            let kind = match o.kind {
                OutputKindSpec::Memory => ValueKind::Memory,
                OutputKindSpec::Control => ValueKind::Control,
                OutputKindSpec::PhiToken => ValueKind::PhiToken,
                // Value (typed or not) — use the resolved value type. The
                // unconstrained `Any` wildcard is a match-only kind (no
                // template builder emits it); resolve it to the value
                // type defensively.
                OutputKindSpec::Value(_) | OutputKindSpec::AnyValue | OutputKindSpec::Any => {
                    ValueKind::Typed(ty)
                }
            };
            by_slot.insert(o.slot, kind);
        }
    }
    if by_slot.is_empty() {
        vec![ValueKind::Typed(ty)]
    } else {
        by_slot.into_values().collect()
    }
}

/// The first value output of `node`, if any.
fn first_value_output(function: &Function, node: NodeId) -> Option<ValueId> {
    function
        .node_outputs(node)
        .iter()
        .copied()
        .find(|&out| function.value_kind(out).as_value().is_some())
}
