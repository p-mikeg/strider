//! Template instantiation: materialising a buildable [`Pattern`] as
//! fresh IR.
//!
//! A buildable pattern node carries a [`TemplateKind`] in its
//! `PatNode.build` slot plus an output [`TemplateTy`]. [`instantiate`]
//! walks the bipartite store in topological order, resolves capture-only
//! nodes through the LHS [`Bindings`], synthesises every buildable node
//! via [`strider_ir::Function::create_node`], and returns the root's
//! materialised value output.

mod ctx;

pub use ctx::TemplateCtx;

use std::collections::BTreeMap;

use anyhow::anyhow;
use petgraph::stable_graph::NodeIndex;
use rustc_hash::FxHashMap;
use strider_ir::Function;
use strider_ir::node::{NodeId, NodeKind, NodeOutputId, NodeOutputKind, NodeOutputType};

use crate::bigraph::reachable_topo;
use crate::bindings::Bindings;
use crate::pattern::{OutputKindSpec, Pattern};

/// Type alias for the [`TemplateKind::Fn`] closure shape. Factored out
/// to keep [`TemplateKind`] legible under clippy's `type_complexity`
/// lint. `Box` (single-threaded; no `Arc` / `Rc` / `Send` / `Sync` in
/// the core).
pub type TemplateKindFn = Box<dyn Fn(&TemplateCtx<'_>) -> anyhow::Result<NodeKind>>;

/// How a buildable pattern node materialises into fresh IR during
/// template instantiation.
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

/// The output type a buildable node declares for its value output.
#[derive(Clone, Copy)]
pub enum TemplateTy {
    /// Inherit the rewrite root's output type (resolved at
    /// instantiation time).
    InheritRoot,
    /// A fixed output type, independent of the root.
    Fixed(NodeOutputType),
}

/// Materialise the buildable `template` as an IR sub-graph rooted at the
/// returned output.
///
/// Captures are resolved from `bindings`; `root_ty` is the output type
/// used for any node whose [`TemplateTy`] is [`TemplateTy::InheritRoot`].
/// `lhs_root` is the matched LHS root `NodeId` exposed to
/// [`TemplateKind::Fn`] closures via [`TemplateCtx::root`] — pure-`Exact`
/// templates ignore it, so standalone callers may pass any valid
/// `NodeId` from `function`.
///
/// # Errors
///
/// Returns an error if the template is rootless, references an unbound
/// capture, contains a node without a build path (a match-only
/// `KindSpec::Any` / predicate shape that should not appear in a
/// buildable RHS), has a [`TemplateKind::Fn`] closure that itself
/// errors, or if the underlying `create_node` call fails to produce
/// exactly one value output.
pub fn instantiate(
    template: &Pattern,
    function: &mut Function,
    bindings: &Bindings,
    lhs_root: NodeId,
    root_ty: NodeOutputType,
) -> anyhow::Result<NodeOutputId> {
    let Some(root) = template.root() else {
        return Err(anyhow!("rootless template pattern"));
    };
    let order = reachable_topo(&template.graph, root)?;

    // Map from pattern node vertex → materialised IR NodeOutputId.
    let mut materialised: FxHashMap<NodeIndex, NodeOutputId> = FxHashMap::default();

    for vtx in order {
        // Only node vertices materialise; output vertices are wiring.
        let Some(nd) = template.graph.node_weight(vtx) else {
            continue;
        };

        // 1. Capture-bearing node: resolve through the LHS bindings.
        //    The capture *is* the materialisation (a captured LHS value
        //    re-used verbatim in the RHS).
        if let Some(cap) = nd.capture {
            let bound_out = bindings.get_output(cap).ok_or_else(|| {
                anyhow!("capture {cap:?} referenced in template but unbound by LHS")
            })?;
            materialised.insert(vtx, bound_out);
            continue;
        }

        // 2. Buildable node: synthesise fresh IR.
        let spec = nd.build.as_ref().ok_or_else(|| {
            anyhow!(
                "template node has no build spec and no capture — \
                 a rewrite RHS must consist of buildable nodes \
                 (e.g. int_const(0), add(...)) and LHS-bound captures"
            )
        })?;

        // The node's declared output value type (resolved against the
        // rewrite root for `InheritRoot`).
        let ty = match nd.build_ty {
            TemplateTy::Fixed(t) => t,
            TemplateTy::InheritRoot => root_ty,
        };

        let kind = match spec {
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

        // Collect inputs in slot order: walk this node's Consumes edges
        // (source = a producer output vertex), step through its
        // Produces edge to the producer node, and read that node's
        // materialised output.
        let mut inputs_by_slot: BTreeMap<usize, NodeOutputId> = BTreeMap::new();
        for (slot, producer_out_vtx) in template.graph.consumed_inputs(vtx) {
            let producer_node = producer_node_of(template, producer_out_vtx)?;
            let producer_out = *materialised.get(&producer_node).ok_or_else(|| {
                anyhow!("producer node not materialised before consumer — topo order bug")
            })?;
            inputs_by_slot.insert(slot, producer_out);
        }
        let inputs: Vec<NodeOutputId> = inputs_by_slot.into_values().collect();

        // Declare the node's output signature. Rewrite RHSs are value
        // expressions, so the common path is a single value output; a
        // node whose template output vertex is a memory / control /
        // phi-token kind declares that signature instead, so this never
        // hardcodes "value output" in a way that blocks memory nodes.
        let outputs = output_kinds_for(template, vtx, ty);

        let node = function.create_node(kind, inputs, outputs);
        let out_id = first_value_output(function, node)
            .ok_or_else(|| anyhow!("instantiated node has no value output"))?;
        materialised.insert(vtx, out_id);
    }

    materialised
        .remove(&root)
        .ok_or_else(|| anyhow!("root template node never materialised"))
}

/// The producer node vertex of an output vertex (its lone incoming
/// `Produces` edge source).
fn producer_node_of(template: &Pattern, output_vtx: NodeIndex) -> anyhow::Result<NodeIndex> {
    template
        .graph
        .producer_of(output_vtx)
        .ok_or_else(|| anyhow!("template output vertex has no producer node"))
}

/// The full output-kind signature a buildable node declares, derived
/// from its template output vertices. Falls back to a single value
/// output of type `ty` when the node has no explicit output vertex
/// (the common value-expression case).
fn output_kinds_for(template: &Pattern, node_vtx: NodeIndex, ty: NodeOutputType) -> Vec<NodeOutputKind> {
    let mut by_slot: BTreeMap<usize, NodeOutputKind> = BTreeMap::new();
    for out_vtx in template.graph.produced_outputs(node_vtx) {
        if let Some(o) = template.graph.output_weight(out_vtx) {
            let kind = match o.kind {
                OutputKindSpec::Memory => NodeOutputKind::Memory,
                OutputKindSpec::Control => NodeOutputKind::Control,
                OutputKindSpec::PhiToken => NodeOutputKind::PhiToken,
                // Value (typed or not) — use the resolved value type.
                OutputKindSpec::Value(_) | OutputKindSpec::AnyValue => {
                    NodeOutputKind::OutputType(ty)
                }
            };
            by_slot.insert(o.slot, kind);
        }
    }
    if by_slot.is_empty() {
        vec![NodeOutputKind::OutputType(ty)]
    } else {
        by_slot.into_values().collect()
    }
}

/// The first value output of `node`, if any.
fn first_value_output(function: &Function, node: NodeId) -> Option<NodeOutputId> {
    function
        .node_outputs(node)
        .iter()
        .copied()
        .find(|&out| function.output_kind(out).as_value().is_some())
}
