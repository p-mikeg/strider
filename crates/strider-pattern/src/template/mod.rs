//! Template instantiation: materialising a [`Template`] as fresh IR.
//!
//! A [`Template`] is the build-side counterpart of
//! [`Pattern`](crate::matcher::Pattern): a node is either a
//! [`Build`](TmplNodeKind::Build) (declaring a [`TemplateKind`] — an exact
//! `NodeKind` or a dynamic `Fn`) or a [`Capture`](TmplNodeKind::Capture) leaf
//! marker. The value side lives on the [`TmplValue`]: a built node's
//! [`TmplOutput`] carries the output [`TemplateTy`], and a capture leaf's
//! [`ValueCapture`](TmplValue::ValueCapture) carries the capture id.
//! [`instantiate`] walks the bipartite store in topological order,
//! resolves each capture leaf's `ValueCapture` through the LHS
//! [`Bindings`] (the captured value re-used verbatim), synthesises every
//! `Build` node via the generic [`strider_ir::IRBuilder::create_node_attributed`] seam
//! (so each implementor applies its own attribution / liveness policy),
//! and returns the root's materialised value output.

mod builder;
mod ctx;
mod graph;
pub(crate) mod template_pat;

pub use builder::{TemplateBuilder, TmplNodeRef, TmplValueRef};
pub use ctx::TemplateCtx;
pub use graph::{TmplValue, Template, TmplNode, TmplNodeKind, TmplOutput};

// Build-side twin value-op factories (`template::add`, `template::sub`,
// …). These share the typed structs of the bare match-side factories but
// carry `TemplatePat` constructor bounds, so the match/template boundary
// is enforced at the construction call site. Re-exported here so callers
// write `strider_pattern::template::add(...)` on a rewrite RHS.
pub use crate::typed::value_ops::template::*;

use std::collections::BTreeMap;

use anyhow::anyhow;
use rustc_hash::FxHashMap;
use strider_graph::ValueId as TmplValueId;
use strider_ir::IRBuilder;
use strider_ir::IRViewer;
use strider_ir::Function;
use strider_ir::node::{NodeId, NodeKind, ValueId, ValueKind, ValueType};

use crate::bindings::Bindings;
use crate::graph_ext::{reachable_topo, PatGraphRead};
use crate::matcher::OutputKindSpec;

/// Type alias for the [`TemplateKind::Fn`] closure shape. Factored out
/// to keep [`TemplateKind`] legible under clippy's `type_complexity`
/// lint. `Box` (single-threaded; no `Arc` / `Rc` / `Send` / `Sync` in
/// the core).
pub type TemplateKindFn = Box<dyn Fn(&TemplateCtx<'_>) -> anyhow::Result<NodeKind>>;

/// Type alias for the [`TemplateKind::FnIntConst`] closure shape.
/// Returns a `u128` value; the instantiator routes it to the correct
/// `IntPayload` form (Small or Wide) based on the output type.
pub type TemplateKindFnIntConst = Box<dyn Fn(&TemplateCtx<'_>) -> anyhow::Result<u128>>;

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
    /// Dynamic `IntConst` variant: the closure computes a `u128` value
    /// at rewrite time, and the instantiator routes it to
    /// `IntPayload::Small` (≤I64) or the wide interner (I80/I128/I256/I512)
    /// based on the node's resolved output type — without ever truncating
    /// to `u64` prematurely.  Used by [`int_const_with_fn`](crate::int_const_with_fn)
    /// so that I128 rewrites preserve values wider than 64 bits.
    FnIntConst(TemplateKindFnIntConst),
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
/// `create_node_attributed` is called with each template node's **declared** output
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
/// the underlying `create_node_attributed` call fails to produce a value output.
pub fn instantiate<B: IRBuilder>(
    template: &Template,
    builder: &mut B,
    bindings: &Bindings,
    lhs_root: NodeId,
    root_ty: ValueType,
) -> anyhow::Result<ValueId> {
    let root = template.root()?;
    let order = reachable_topo(&template.graph, root)?;

    // Map from a template *output vertex* (a generic-graph ValueId) → its
    // materialised IR ValueId. Keying on the output vertex (not the producer
    // node) lets a multi-output interior node feed the right slot to each
    // consumer: a `Store`'s memory output and a sibling value output
    // resolve to distinct IR outputs.
    let mut materialised: FxHashMap<TmplValueId, ValueId> = FxHashMap::default();
    // The root node's value output, captured as the root materialises.
    let mut root_value: Option<ValueId> = None;

    for vtx in order {
        let nd = template.graph.node_weight(vtx);

        // Resolve this node's build kind. A `Capture` leaf is a marker with
        // no build kind: it *is* the materialisation — its `ValueCapture`
        // output resolves to the LHS binding (the captured value re-used
        // verbatim in the RHS, e.g. `add(x, 0) → x`) and is never
        // synthesised. The capture id lives on the value (output), not the
        // marker node.
        let kind = match &nd.kind {
            TmplNodeKind::Capture => {
                let outs = template.graph.produced_outputs(vtx);
                let out_vtx = *outs
                    .first()
                    .ok_or_else(|| anyhow!("capture leaf has no value-capture output"))?;
                let TmplValue::ValueCapture(cap) = template.graph.output_weight(out_vtx) else {
                    return Err(anyhow!("capture leaf output is not a ValueCapture"));
                };
                let bound_value = bindings.get_value(*cap).ok_or_else(|| {
                    anyhow!("capture {cap:?} referenced in template but unbound by LHS")
                })?;
                materialised.insert(out_vtx, bound_value);
                if vtx == root {
                    root_value = Some(bound_value);
                }
                continue;
            }
            TmplNodeKind::Build(TemplateKind::Exact(k)) => *k,
            TmplNodeKind::Build(TemplateKind::Fn(f)) => {
                // The node's declared value-output type (resolved against
                // the rewrite root for `InheritRoot`), read from the node's
                // value output vertex. Exposed to the dynamic closure as
                // the `root_ty` it computes its constant against.
                let value_ty = node_value_ty(template, vtx, root_ty);
                let ctx = TemplateCtx {
                    function: builder.function(),
                    bindings,
                    root: lhs_root,
                    root_ty: value_ty,
                };
                f(&ctx)?
            }
            TmplNodeKind::Build(TemplateKind::FnIntConst(f)) => {
                // Compute the u128 value via the closure, then route it to
                // the correct IntPayload form based on the output type.
                // wide types use the interner; ≤I64 truncate safely to u64.
                let value_ty = node_value_ty(template, vtx, root_ty);
                let ctx = TemplateCtx {
                    function: builder.function(),
                    bindings,
                    root: lhs_root,
                    root_ty: value_ty,
                };
                let v = f(&ctx)?;
                use strider_ir::node::IntPayload;
                use strider_ir::wide_const::WideConstStorage;
                match value_ty {
                    strider_ir::node::ValueType::I80 => {
                        let id = builder.function_mut().intern_wide_const(
                            WideConstStorage::I80(v & strider_ir::node::ValueType::I80.bit_mask_u128()),
                        );
                        strider_ir::node::NodeKind::IntConst(IntPayload::Wide(id))
                    }
                    strider_ir::node::ValueType::I128 => {
                        let id = builder.function_mut().intern_wide_const(
                            WideConstStorage::I128(v),
                        );
                        strider_ir::node::NodeKind::IntConst(IntPayload::Wide(id))
                    }
                    strider_ir::node::ValueType::I256 => {
                        #[allow(clippy::cast_possible_truncation)]
                        let low64 = v as u64;
                        #[allow(clippy::cast_possible_truncation)]
                        let high64 = (v >> 64) as u64;
                        let id = builder.function_mut().intern_wide_const(
                            WideConstStorage::I256([low64, high64, 0, 0]),
                        );
                        strider_ir::node::NodeKind::IntConst(IntPayload::Wide(id))
                    }
                    strider_ir::node::ValueType::I512 => {
                        #[allow(clippy::cast_possible_truncation)]
                        let low64 = v as u64;
                        #[allow(clippy::cast_possible_truncation)]
                        let high64 = (v >> 64) as u64;
                        let id = builder.function_mut().intern_wide_const(
                            WideConstStorage::I512([low64, high64, 0, 0, 0, 0, 0, 0]),
                        );
                        strider_ir::node::NodeKind::IntConst(IntPayload::Wide(id))
                    }
                    _ => {
                        // For ≤I64 output types the mask is at most u64::MAX,
                        // so the cast to u64 is lossless after masking.
                        let mask = value_ty.bit_mask_u128();
                        debug_assert!(
                            mask <= u128::from(u64::MAX),
                            "non-wide output type must have a ≤u64 bit mask; \
                             got mask {mask:#x} for {value_ty:?}"
                        );
                        #[allow(clippy::cast_possible_truncation)]
                        strider_ir::node::NodeKind::IntConst(IntPayload::Small((v & mask) as u64))
                    }
                }
            }
        };

        // Collect inputs in slot order: each `Consumes` edge names the
        // producer output vertex feeding this node's slot; read its
        // already-materialised IR output.
        let mut inputs_by_slot: BTreeMap<usize, ValueId> = BTreeMap::new();
        for (slot, producer_out_vtx) in template.graph.consumed_inputs(vtx) {
            let producer_value = *materialised.get(&producer_out_vtx).ok_or_else(|| {
                anyhow!("producer output not materialised before consumer — topo order bug")
            })?;
            inputs_by_slot.insert(slot, producer_value);
        }
        let inputs: Vec<ValueId> = inputs_by_slot.into_values().collect();

        // Declare the node's output signature from its template output
        // vertices. The common path is a single value output; a
        // multi-output node (e.g. a `Store` declaring a memory output a
        // later node consumes) declares that signature instead. Each value
        // output resolves its own [`TemplateTy`] against the rewrite root.
        let outputs = output_kinds_for(template, vtx, root_ty);

        let node = builder.create_node_attributed(kind, inputs, outputs, &[lhs_root]);

        // Map each template output vertex to the IR output at the
        // matching slot, so multi-output consumers wire the right edge.
        let ir_outputs = builder.function().node_outputs(node);
        for out_vtx in template.graph.produced_outputs(vtx) {
            // A built node's outputs are all `TmplOutput`; a `ValueCapture`
            // never hangs off a `Build` node.
            let TmplValue::TmplOutput(o) = template.graph.output_weight(out_vtx) else {
                continue;
            };
            let ir_value = *ir_outputs.get(o.slot).ok_or_else(|| {
                anyhow!("template output slot {} out of range for instantiated node", o.slot)
            })?;
            materialised.insert(out_vtx, ir_value);
        }

        if vtx == root {
            root_value = Some(
                first_value_output(builder.function(), node)
                    .ok_or_else(|| anyhow!("instantiated root node has no value output"))?,
            );
        }
    }

    root_value.ok_or_else(|| anyhow!("root template node never materialised"))
}

/// Resolve a build [`TemplateTy`] against the rewrite root's output type.
fn resolve_ty(ty: TemplateTy, root_ty: ValueType) -> ValueType {
    match ty {
        TemplateTy::Fixed(t) => t,
        TemplateTy::InheritRoot => root_ty,
    }
}

/// The resolved value-output type a template node declares: the
/// [`TemplateTy`] of its first value output vertex, resolved against
/// `root_ty`. Falls back to `root_ty` when the node has no value output
/// vertex (the dynamic-`Fn` `root_ty` for such a node is then the root's).
fn node_value_ty(template: &Template, node_vtx: NodeId, root_ty: ValueType) -> ValueType {
    template
        .graph
        .produced_outputs(node_vtx)
        .into_iter()
        .find_map(|out_vtx| {
            let TmplValue::TmplOutput(o) = template.graph.output_weight(out_vtx) else {
                return None;
            };
            matches!(
                o.kind,
                OutputKindSpec::Value(_) | OutputKindSpec::AnyValue | OutputKindSpec::Any
            )
            .then(|| resolve_ty(o.ty, root_ty))
        })
        .unwrap_or(root_ty)
}

/// The full output-kind signature a template node declares, derived from
/// its template output vertices; each value output resolves its own
/// [`TemplateTy`] against `root_ty`. Falls back to a single value output
/// of the root's type when the node has no explicit output vertex (the
/// common value-expression case).
fn output_kinds_for(
    template: &Template,
    node_vtx: NodeId,
    root_ty: ValueType,
) -> Vec<ValueKind> {
    let mut by_slot: BTreeMap<usize, ValueKind> = BTreeMap::new();
    for out_vtx in template.graph.produced_outputs(node_vtx) {
        if let TmplValue::TmplOutput(o) = template.graph.output_weight(out_vtx) {
            let kind = match o.kind {
                OutputKindSpec::Memory => ValueKind::Memory,
                OutputKindSpec::Control => ValueKind::Control,
                OutputKindSpec::PhiToken => ValueKind::PhiToken,
                // Value (typed or not) — use this output's own resolved
                // type. The unconstrained `Any` wildcard is a match-only
                // kind (no template builder emits it); resolve it
                // defensively.
                OutputKindSpec::Value(_) | OutputKindSpec::AnyValue | OutputKindSpec::Any => {
                    ValueKind::Typed(resolve_ty(o.ty, root_ty))
                }
            };
            by_slot.insert(o.slot, kind);
        }
    }
    if by_slot.is_empty() {
        vec![ValueKind::Typed(root_ty)]
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
        .find(|&value| function.value_kind(value).as_value().is_some())
}
