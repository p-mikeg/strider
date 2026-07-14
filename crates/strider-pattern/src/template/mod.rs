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
pub use graph::{Template, TmplNode, TmplNodeKind, TmplOutput, TmplValue};

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
use strider_ir::node::{NodeId, NodeKind, ValueId, ValueKind, ValueType};
use strider_ir::{Function, IRBuilder, IRViewer};

use crate::bindings::Bindings;
use crate::graph_ext::{PatGraphRead, reachable_topo};
use crate::matcher::OutputKindSpec;

/// Type alias for the [`TemplateKind::Fn`] closure shape. Factored out
/// to keep [`TemplateKind`] legible under clippy's `type_complexity`
/// lint. `Box` (single-threaded; no `Arc` / `Rc` / `Send` / `Sync` in
/// the core).
pub type TemplateKindFn = Box<dyn Fn(&TemplateCtx<'_>) -> anyhow::Result<NodeKind>>;

/// Type alias for the [`TemplateKind::FnIntConst`] closure shape.
/// Returns a `u128` value; the instantiator interns it as a `ConstId` via
/// `intern_int_const` (≤I128) or `intern_int_const_limbs` (I256/I512).
///
/// The `u128` return caps the expressible range: for an I256/I512 output
/// type only the low 128 bits are materialised (the high limbs are zero).
/// That suffices for every current rewrite (folds operate at ≤128 bits);
/// a full-range I256/I512 rewrite constant would need a wider closure.
pub type TemplateKindFnIntConst = Box<dyn Fn(&TemplateCtx<'_>) -> anyhow::Result<u128>>;

/// How a template node materialises into fresh IR during instantiation.
pub enum TemplateKind {
    /// Emit a node with the given exact [`NodeKind`].
    Exact(NodeKind),
    /// Dynamic-kind closure variant. The closure receives a
    /// [`TemplateCtx`] — exposing the captured LHS [`Bindings`], the
    /// matched-root `NodeId` / output type, and a shared
    /// [`Function`](strider_ir::Function) — and returns the `NodeKind` to materialise. Used
    /// by the `*_const_with` family of builders to emit constants whose
    /// value is computed from captured operand values at rewrite time.
    Fn(TemplateKindFn),
    /// Dynamic `IntConst` variant: the closure computes a `u128` value
    /// at rewrite time, and the instantiator interns it via the unified
    /// `ConstId` interner — `intern_int_const` for ≤128-bit types,
    /// `intern_int_const_limbs` for I256/I512 — without ever truncating
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
    /// Inherit the width of a **bound LHS capture**, resolved at
    /// instantiation time from the value the capture matched.  Binding-relative
    /// (not root-relative like [`InheritRoot`]): use it when a materialised
    /// interior node's width comes from a captured operand that the rewrite
    /// root's shape does not expose — e.g. the `And(x, mask)` / mask in
    /// `Sless(x<<C, 0) → Xor(Equal(And(x,mask),0),1)`, whose `I1` `Xor` root has
    /// no `x`-wide input.
    InheritBinding(crate::Capture),
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
/// every node's declared output signature matches its `NodeKind`'s
/// `expected_signature`. Input-slot wiring **is** checked here: a gap in a
/// node's input slots (non-contiguous `0..n`) or a duplicate slot is
/// rejected with an error rather than being silently closed / overwritten.
/// The typed `template::` builders wire contiguous single-occupancy slots
/// by construction; a [`Template`] hand-built via the raw
/// [`TemplateBuilder`] node / output verbs is validated against this here.
///
/// # Errors
///
/// Returns an error if the template is rootless, references an unbound
/// capture, has a [`TemplateKind::Fn`] closure that itself errors, has a
/// node with gapped / duplicate input slots, or if the underlying
/// `create_node_attributed` call fails to produce a value output.
/// `proof_nodes` is the attribution set unioned into the asm-fingerprint of
/// **every** node this function creates (via `create_node_attributed`): the
/// caller passes the matched LHS footprint so each fresh RHS node — root and
/// interior alike — carries the whole rewrite's proof, not just the matched
/// root. `lhs_root` is kept separately because the dynamic-`Fn` template
/// closures need the single matched-root node for `InheritRoot` typing.
pub fn instantiate<B: IRBuilder>(
    template: &Template,
    builder: &mut B,
    bindings: &Bindings,
    lhs_root: NodeId,
    proof_nodes: &[NodeId],
    root_ty: ValueType,
) -> anyhow::Result<ValueId> {
    let root = template.root()?;
    let order = reachable_topo(&template.graph, root)?;

    // Widths for `TemplateTy::InheritBinding(cap)` nodes: resolved once from the
    // value each referenced capture matched.  Scanned before the mutating build
    // loop (needs an immutable `function` borrow).  A capture bound to a
    // non-typed value simply doesn't enter the map and falls back to `root_ty`.
    let binding_tys = resolve_binding_tys(template, bindings, builder.function());

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
                let value_ty = node_value_ty(template, vtx, root_ty, &binding_tys);
                let ctx = TemplateCtx {
                    function: builder.function(),
                    bindings,
                    root: lhs_root,
                    root_ty: value_ty,
                };
                f(&ctx)?
            }
            TmplNodeKind::Build(TemplateKind::FnIntConst(f)) => {
                // Compute the u128 value via the closure, then intern it
                // (see `intern_fn_int_const` for the per-width interning).
                let value_ty = node_value_ty(template, vtx, root_ty, &binding_tys);
                let ctx = TemplateCtx {
                    function: builder.function(),
                    bindings,
                    root: lhs_root,
                    root_ty: value_ty,
                };
                let v = f(&ctx)?;
                intern_fn_int_const(builder, value_ty, v)
            }
        };

        let inputs = collect_inputs(template, vtx, &materialised)?;

        // Declare the node's output signature from its template output
        // vertices. The common path is a single value output; a
        // multi-output node (e.g. a `Store` declaring a memory output a
        // later node consumes) declares that signature instead. Each value
        // output resolves its own [`TemplateTy`] against the rewrite root.
        let outputs = output_kinds_for(template, vtx, root_ty, &binding_tys);

        let node = builder.create_node_attributed(kind, inputs, outputs, proof_nodes);

        // Map each template output vertex to the IR output at the
        // matching slot, so multi-output consumers wire the right edge.
        let ir_outputs = builder.function().node_outputs(node);
        for out_vtx in template.graph.produced_outputs(vtx).iter().copied() {
            // A built node's outputs are all `TmplOutput`; a `ValueCapture`
            // never hangs off a `Build` node.
            let TmplValue::TmplOutput(o) = template.graph.output_weight(out_vtx) else {
                continue;
            };
            let ir_value = *ir_outputs.get(o.slot).ok_or_else(|| {
                anyhow!(
                    "template output slot {} out of range for instantiated node",
                    o.slot
                )
            })?;
            materialised.insert(out_vtx, ir_value);
        }

        if vtx == root {
            root_value = Some(
                builder
                    .function()
                    .first_value_output_of(node)
                    .ok_or_else(|| anyhow!("instantiated root node has no value output"))?,
            );
        }
    }

    root_value.ok_or_else(|| anyhow!("root template node never materialised"))
}

/// Resolve a build [`TemplateTy`] against the rewrite root's output type
/// (`root_ty`) and the per-capture width map (`binding_tys`).  An
/// `InheritBinding` whose capture didn't resolve to a typed value falls back
/// to `root_ty`.
fn resolve_ty(
    ty: TemplateTy,
    root_ty: ValueType,
    binding_tys: &FxHashMap<crate::Capture, ValueType>,
) -> ValueType {
    match ty {
        TemplateTy::Fixed(t) => t,
        TemplateTy::InheritRoot => root_ty,
        TemplateTy::InheritBinding(cap) => binding_tys.get(&cap).copied().unwrap_or(root_ty),
    }
}

/// Resolve the width of every `InheritBinding(cap)` referenced anywhere in
/// `template`, once, from the value each capture matched in `bindings`.  A
/// capture bound to a non-typed value (or unbound) is simply omitted — the
/// consumer falls back to the root type.
fn resolve_binding_tys(
    template: &Template,
    bindings: &Bindings,
    function: &Function,
) -> FxHashMap<crate::Capture, ValueType> {
    let mut out: FxHashMap<crate::Capture, ValueType> = FxHashMap::default();
    for vtx in template.graph.all_node_ids() {
        for out_vtx in template.graph.produced_outputs(vtx).iter().copied() {
            let TmplValue::TmplOutput(o) = template.graph.output_weight(out_vtx) else {
                continue;
            };
            let TemplateTy::InheritBinding(cap) = o.ty else {
                continue;
            };
            if let std::collections::hash_map::Entry::Vacant(e) = out.entry(cap)
                && let Some(v) = bindings.get_value(cap)
                && let ValueKind::Typed(t) = function.value_kind(v)
            {
                e.insert(t);
            }
        }
    }
    out
}

/// Intern a dynamic-`FnIntConst` value `v` as an `IntConst` of `value_ty`.
///
/// The closure value is a `u128`, so it always fits within 128 bits — even for
/// I256/I512 the high limbs are zero, so the limb path would canonicalise back
/// through `intern_int_const` anyway. `intern_int_const` masks `v` to
/// `value_ty`'s width and stores it as `Bits`.
fn intern_fn_int_const<B: IRBuilder>(builder: &mut B, value_ty: ValueType, v: u128) -> NodeKind {
    NodeKind::IntConst(builder.function_mut().intern_int_const(v, value_ty))
}

/// Collect a template node's inputs in slot order: each `Consumes` edge names
/// the producer output vertex feeding this node's slot; its already-materialised
/// IR output is read from `materialised`.
///
/// The raw `TemplateBuilder` verbs do not enforce contiguous,
/// single-occupancy slots; a gap (slots 0 and 2 but not 1) would be
/// silently CLOSED by `into_values()` and a duplicate slot would
/// silently overwrite the earlier edge — both producing wrong IR with
/// no diagnostic on the validate-skipping rewrite path. Reject both
/// here (the typed builders always wire `0..n` once, so they never
/// trip this).
fn collect_inputs(
    template: &Template,
    node_vtx: NodeId,
    materialised: &FxHashMap<TmplValueId, ValueId>,
) -> anyhow::Result<Vec<ValueId>> {
    let mut inputs_by_slot: BTreeMap<usize, ValueId> = BTreeMap::new();
    for (slot, producer_out_vtx) in template.graph.consumed_inputs(node_vtx) {
        let producer_value = *materialised.get(&producer_out_vtx).ok_or_else(|| {
            anyhow!("producer output not materialised before consumer — topo order bug")
        })?;
        if inputs_by_slot.insert(slot, producer_value).is_some() {
            return Err(anyhow!(
                "template node wires two producers into input slot {slot} \
                 (raw-builder mis-wire)"
            ));
        }
    }
    // Reject a gap: the keys must be exactly the contiguous range
    // `0..len`, else the dense `into_values()` would shift later slots
    // down onto the wrong IR input index.
    if inputs_by_slot
        .keys()
        .enumerate()
        .any(|(i, &slot)| i != slot)
    {
        let slots: Vec<usize> = inputs_by_slot.keys().copied().collect();
        return Err(anyhow!(
            "template node has non-contiguous input slots {slots:?} \
             (expected 0..{}) — raw-builder mis-wire",
            inputs_by_slot.len()
        ));
    }
    Ok(inputs_by_slot.into_values().collect())
}

/// Resolve one template output vertex's [`OutputKindSpec`] (+ its
/// [`TemplateTy`]) into the concrete IR [`ValueKind`], against `root_ty`.
/// Single source of truth for the `OutputKindSpec → ValueKind` mapping
/// shared by [`node_value_ty`] and [`output_kinds_for`].
fn resolved_output_kind(o: &TmplOutput, root_ty: ValueType, binding_tys: &FxHashMap<crate::Capture, ValueType>) -> ValueKind {
    match o.kind {
        OutputKindSpec::Memory => ValueKind::Memory,
        OutputKindSpec::Control => ValueKind::Control,
        OutputKindSpec::PhiToken => ValueKind::PhiToken,
        // Value (typed or not) — use this output's own resolved type. The
        // unconstrained `Any` wildcard is a match-only kind (no template
        // builder emits it); resolve it defensively.
        OutputKindSpec::Value(_) | OutputKindSpec::AnyValue | OutputKindSpec::Any => {
            ValueKind::Typed(resolve_ty(o.ty, root_ty, binding_tys))
        }
    }
}

/// The resolved value-output type a template node declares: the
/// [`TemplateTy`] of its first value output vertex, resolved against
/// `root_ty`. Falls back to `root_ty` when the node has no value output
/// vertex (the dynamic-`Fn` `root_ty` for such a node is then the root's).
fn node_value_ty(template: &Template, node_vtx: NodeId, root_ty: ValueType, binding_tys: &FxHashMap<crate::Capture, ValueType>) -> ValueType {
    template
        .graph
        .produced_outputs(node_vtx)
        .iter()
        .copied()
        .find_map(|out_vtx| {
            let TmplValue::TmplOutput(o) = template.graph.output_weight(out_vtx) else {
                return None;
            };
            match resolved_output_kind(o, root_ty, binding_tys) {
                ValueKind::Typed(t) => Some(t),
                _ => None,
            }
        })
        .unwrap_or(root_ty)
}

/// The full output-kind signature a template node declares, derived from
/// its template output vertices; each value output resolves its own
/// [`TemplateTy`] against `root_ty`. Falls back to a single value output
/// of the root's type when the node has no explicit output vertex (the
/// common value-expression case).
fn output_kinds_for(template: &Template, node_vtx: NodeId, root_ty: ValueType, binding_tys: &FxHashMap<crate::Capture, ValueType>) -> Vec<ValueKind> {
    let mut by_slot: BTreeMap<usize, ValueKind> = BTreeMap::new();
    for out_vtx in template.graph.produced_outputs(node_vtx).iter().copied() {
        if let TmplValue::TmplOutput(o) = template.graph.output_weight(out_vtx) {
            by_slot.insert(o.slot, resolved_output_kind(o, root_ty, binding_tys));
        }
    }
    if by_slot.is_empty() {
        vec![ValueKind::Typed(root_ty)]
    } else {
        by_slot.into_values().collect()
    }
}
