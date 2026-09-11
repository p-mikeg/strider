//! [`instantiate`] synthesises every `Build` node through the
//! [`strider_ir::IRBuilder::create_node_attributed`] seam, so each implementor
//! keeps its own attribution and liveness policy.

mod builder;
mod ctx;
mod graph;
pub(crate) mod template_pat;

pub use builder::{TemplateBuilder, TmplNodeRef, TmplValueRef};
pub use ctx::TemplateCtx;
pub use graph::{Template, TmplNode, TmplNodeKind, TmplOutput, TmplValue};

// Build-side twins of the match-side value-op factories. They share the same
// typed structs but carry `TemplatePat` constructor bounds, so the
// match/template boundary is enforced at the construction call site.
// Re-exported so a rewrite RHS reads `strider_pattern::template::add(...)`.
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

pub type TemplateKindFn = Box<dyn Fn(&TemplateCtx<'_>) -> anyhow::Result<NodeKind>>;

/// The `u128` return caps the expressible range: an I256/I512 output
/// materialises only the low 128 bits, leaving the high limbs zero.
pub type TemplateKindFnIntConst = Box<dyn Fn(&TemplateCtx<'_>) -> anyhow::Result<u128>>;

/// How a template node materialises into fresh IR.
pub enum TemplateKind {
    Exact(NodeKind),
    /// The closure returns the `NodeKind` to materialise, given a
    /// [`TemplateCtx`].
    Fn(TemplateKindFn),
    /// The closure computes a `u128` at rewrite time, interned at the
    /// resolved output width.
    FnIntConst(TemplateKindFnIntConst),
}

/// The output type a template node declares for its value output. All three
/// resolve at instantiation time.
#[derive(Clone, Copy)]
pub enum TemplateTy {
    /// The rewrite root's output type.
    InheritRoot,
    /// The width of the value a bound LHS capture matched, for an interior
    /// node whose width comes from a captured operand the root does not
    /// expose (`Sless(x<<C, 0) -> Xor(Equal(And(x,mask),0),1)`: the `I1` root
    /// has no `x`-wide input for `And` and its mask to inherit).
    InheritBinding(crate::Capture),
    /// Independent of the root.
    Fixed(ValueType),
}

/// Materialises `template` as an IR sub-graph rooted at the returned output.
///
/// `root_ty` types any node whose [`TemplateTy`] is
/// [`TemplateTy::InheritRoot`]. `lhs_root` reaches [`TemplateKind::Fn`]
/// closures as [`TemplateCtx::root`]; a pure-`Exact` template ignores it.
///
/// `proof_nodes` is unioned into the asm-fingerprint of every node created
/// here.
///
/// Interior nodes may be multi-output; the root always yields a single value
/// output.
///
/// # Author-owned output-signature validity
///
/// Nodes are created with their **declared** output signature, and
/// [`strider_ir::validate`] is not run: matching each declared signature to
/// its `NodeKind`'s `expected_signature` is the author's responsibility.
/// Input-slot wiring IS checked here: a gap or a duplicate errors out.
///
/// # Errors
///
/// If the template is rootless, references an unbound capture, has a
/// [`TemplateKind::Fn`] closure that itself errors, has gapped or duplicate
/// input slots, or if node creation yields no value output.
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

    // Scanned before the mutating build loop, which is what the immutable
    // `function` borrow requires.
    let binding_tys = resolve_binding_tys(template, bindings, builder.function());

    // Keyed on the output VERTEX rather than the producer node, so a
    // multi-output interior node feeds the right slot to each consumer: a
    // `Store`'s memory output and a sibling value output must resolve to
    // distinct IR outputs.
    let mut materialised: FxHashMap<TmplValueId, ValueId> = FxHashMap::default();
    let mut root_value: Option<ValueId> = None;

    for vtx in order {
        let nd = template.graph.node_weight(vtx);

        // A `Capture` leaf has no build kind: it IS the materialisation. Its
        // `ValueCapture` output resolves to the LHS binding, reusing that
        // value verbatim as in `int_add(x, 0) -> x`, and is never synthesised.
        // The capture id lives on the output, not the marker node.
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
                // The closure computes its constant against this node's own
                // declared output type, not the rewrite root's.
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
                let value_ty = node_value_ty(template, vtx, root_ty, &binding_tys);
                let ctx = TemplateCtx {
                    function: builder.function(),
                    bindings,
                    root: lhs_root,
                    root_ty: value_ty,
                };
                let v = f(&ctx)?;
                intern_fn_int_const(builder, value_ty, v)?
            }
        };

        // A template `FloatConst` carries raw IEEE bits with no width; the
        // width is resolved here, so bits above it are dropped here.
        let kind = match kind {
            NodeKind::FloatConst(bits) => {
                let ty = node_value_ty(template, vtx, root_ty, &binding_tys);
                NodeKind::FloatConst(if ty.is_float() {
                    ty.mask_float_bits(bits)
                } else {
                    bits
                })
            }
            other => other,
        };

        let inputs = collect_inputs(template, vtx, &materialised)?;

        // Usually a single value output; a multi-output node such as a
        // `Store` declares its memory output here too.
        let outputs = output_kinds_for(template, vtx, root_ty, &binding_tys);

        let node = builder.create_node_attributed(kind, inputs, outputs, proof_nodes);

        // Map each template output vertex onto the IR output at the same
        // slot, so multi-output consumers wire the right edge.
        let ir_outputs = builder.function().node_outputs(node);
        for out_vtx in template.graph.produced_outputs(vtx).iter().copied() {
            // A `ValueCapture` never hangs off a `Build` node.
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

/// An `InheritBinding` whose capture never resolved to a typed value falls
/// back to `root_ty`.
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

/// Resolves every `InheritBinding(cap)` width, from the value each capture
/// matched. An unbound or non-typed capture is omitted.
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

/// Masks `v` to `value_ty`'s width and stores it.
/// The closure computed `v` in `u128`, so a carry or borrow out of bit 127 is
/// lost. That is the declared width's own modulus up to `I128`, and the WRONG
/// one past it: `2^127 + 2^127` reads back as `0` rather than `2^128`. Skip the
/// rewrite instead of interning a truncated constant.
fn intern_fn_int_const<B: IRBuilder>(
    builder: &mut B,
    value_ty: ValueType,
    v: u128,
) -> anyhow::Result<NodeKind> {
    if value_ty.bit_width() > 128 {
        return Err(crate::skip());
    }
    Ok(NodeKind::IntConst(
        builder.function_mut().intern_int_const(v, value_ty),
    ))
}

/// In slot order, reading each producer's already-materialised IR output from
/// `materialised`. A gapped or duplicate input slot is rejected here rather
/// than silently closed or overwritten.
fn collect_inputs(
    template: &Template,
    node_vtx: NodeId,
    materialised: &FxHashMap<TmplValueId, ValueId>,
) -> anyhow::Result<Vec<ValueId>> {
    let mut inputs_by_slot: BTreeMap<usize, ValueId> = BTreeMap::new();
    for (slot, producer_out_vtx) in template.graph.consumed_inputs(node_vtx) {
        let producer_value = *materialised.get(&producer_out_vtx).ok_or_else(|| {
            anyhow!("producer output not materialised before consumer (topo order bug)")
        })?;
        if inputs_by_slot.insert(slot, producer_value).is_some() {
            return Err(anyhow!(
                "template node wires two producers into input slot {slot} \
                 (raw-builder mis-wire)"
            ));
        }
    }
    // The keys must be exactly `0..len`, or the dense `into_values()` shifts
    // later slots down onto the wrong IR input index.
    if inputs_by_slot
        .keys()
        .enumerate()
        .any(|(i, &slot)| i != slot)
    {
        let slots: Vec<usize> = inputs_by_slot.keys().copied().collect();
        return Err(anyhow!(
            "template node has non-contiguous input slots {slots:?}, \
             expected 0..{} (raw-builder mis-wire)",
            inputs_by_slot.len()
        ));
    }
    Ok(inputs_by_slot.into_values().collect())
}

/// Maps an `OutputKindSpec` to its [`ValueKind`].
fn resolved_output_kind(
    o: &TmplOutput,
    root_ty: ValueType,
    binding_tys: &FxHashMap<crate::Capture, ValueType>,
) -> ValueKind {
    match o.kind {
        OutputKindSpec::Memory => ValueKind::Memory,
        OutputKindSpec::Control => ValueKind::Control,
        OutputKindSpec::PhiToken => ValueKind::PhiToken,
        // Every value shape uses this output's own resolved type. `Any` is
        // match-only and no template builder emits it; resolved defensively.
        OutputKindSpec::Value(_) | OutputKindSpec::AnyValue | OutputKindSpec::Any => {
            ValueKind::Typed(resolve_ty(o.ty, root_ty, binding_tys))
        }
    }
}

/// The [`TemplateTy`] of the node's first value output vertex, resolved
/// against `root_ty`. A node with no value output vertex falls back to
/// `root_ty`.
fn node_value_ty(
    template: &Template,
    node_vtx: NodeId,
    root_ty: ValueType,
    binding_tys: &FxHashMap<crate::Capture, ValueType>,
) -> ValueType {
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

/// Each value output resolves its own [`TemplateTy`] against `root_ty`. A
/// node with no explicit output vertex falls back to a single value output of
/// the root's type.
fn output_kinds_for(
    template: &Template,
    node_vtx: NodeId,
    root_ty: ValueType,
    binding_tys: &FxHashMap<crate::Capture, ValueType>,
) -> Vec<ValueKind> {
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
