mod builder;
mod ctx;
mod graph;
pub(crate) mod template_pat;
#[cfg(test)]
mod tests;

pub use builder::{TemplateBuilder, TmplNodeRef, TmplValueRef};
pub use ctx::TemplateCtx;
pub use graph::Template;
pub(crate) use graph::{TmplNodeKind, TmplValue};

pub use crate::typed::value_ops::ops::*;

use std::collections::BTreeMap;

use anyhow::anyhow;
use petgraph::unionfind::UnionFind;
use rustc_hash::{FxHashMap, FxHashSet};
use strider_graph::ValueId as TmplValueId;
use strider_ir::node::{
    Conversion, NodeId, NodeKind, OperandWidth, ValueId, ValueKind, ValueType, WidthTie, width_tie,
};
use strider_ir::{Function, IRBuilder, IRViewer};

use crate::bindings::{Binding, Bindings};
use crate::graph_ext::consumed_inputs;

pub type TemplateKindFn = Box<dyn Fn(&TemplateCtx<'_>) -> anyhow::Result<NodeKind> + Send>;

pub type TemplateKindFnIntConst =
    Box<dyn Fn(&TemplateCtx<'_>) -> anyhow::Result<strider_ir::UBig> + Send>;

/// How a template node materialises into fresh IR.
pub enum TemplateKind {
    Exact(NodeKind),
    /// The closure returns the `NodeKind`; an `IntConst` is refused, as under `Exact`.
    Fn(TemplateKindFn),
    /// The closure computes a [`strider_ir::UBig`], interned at the resolved output width.
    FnIntConst(TemplateKindFnIntConst),
}

/// The output type a template node declares, resolved at instantiation.
#[derive(Clone, Copy)]
pub(crate) enum TemplateTy {
    /// The width the node is evaluated at: the root's type unless an operand
    /// group pins another. A shift count takes the shift's width.
    InheritRoot,
    /// The width of the value a bound LHS capture matched
    /// (`Sless(x<<C, 0) -> Xor(Equal(And(x,mask),0),1)`: `And` is `x`-wide, not `I1`).
    InheritBinding(crate::Capture),
    Fixed(ValueType),
}

/// Materialises `template` as an IR sub-graph and returns the root node's first value output.
///
/// `root_ty` types the value the root replaces, `lhs_root` is [`TemplateCtx::root`] for
/// [`TemplateKind::Fn`] closures, and `proof_nodes` join every created node's asm-fingerprint.
/// Each node is checked against its `expected_signature`, not by [`strider_ir::validate`].
///
/// # Errors
///
/// If the template is malformed, a capture is unbound, a closure errors, or no width reaches a
/// node, or a node's inputs or outputs refuse its signature. Every refusal comes before
/// the first node is built.
pub fn instantiate<B: IRBuilder>(
    template: &Template,
    builder: &mut B,
    bindings: &Bindings,
    lhs_root: NodeId,
    proof_nodes: &[NodeId],
    root_ty: ValueType,
) -> anyhow::Result<ValueId> {
    instantiate_guarded(
        template,
        builder,
        bindings,
        lhs_root,
        proof_nodes,
        root_ty,
        |_| Ok(()),
    )
}

/// [`instantiate`], calling `before_build` after every kind is resolved and
/// before the first node is created, only if the template builds one.
///
/// # Errors
///
/// As [`instantiate`], and whatever `before_build` returns.
pub fn instantiate_guarded<B: IRBuilder>(
    template: &Template,
    builder: &mut B,
    bindings: &Bindings,
    lhs_root: NodeId,
    proof_nodes: &[NodeId],
    root_ty: ValueType,
    before_build: impl FnOnce(&Function) -> anyhow::Result<()>,
) -> anyhow::Result<ValueId> {
    let root = template.root()?;
    // Producers first, except captures, which materialise before any node is built.
    let order: Vec<NodeId> = template.graph.all_node_ids().collect();

    let value_tys = resolve_value_tys(
        template,
        &order,
        bindings,
        builder.function(),
        root,
        root_ty,
    )?;

    // Keyed by output vertex, so a multi-output node feeds each consumer its own slot.
    let mut materialised: Vec<Option<ValueId>> = vec![None; value_tys.by_vertex.len()];
    let mut root_value: Option<ValueId> = None;

    // Plan and check first, so every refusal comes before any node exists.
    let mut plan: Vec<(NodeId, NodeKind)> = Vec::with_capacity(order.len());
    let mut unbound = None;
    for vtx in order {
        let nd = template.graph.node_kind(vtx);
        if matches!(nd.kind, TmplNodeKind::Build(_)) && template.graph.node_outputs(vtx).is_empty()
        {
            anyhow::bail!(
                "template node {} declares no output, so nothing gives it a type: \
                 declare its outputs (value_output / memory_output)",
                node_name(template, vtx)
            );
        }
        let TmplNodeKind::Build(build) = &nd.kind else {
            // A capture leaf's output is its LHS binding.
            let out_vtx = template.graph.node_outputs(vtx)[0];
            let Some(TemplateTy::InheritBinding(cap)) = template.graph.value_kind_ref(out_vtx).ty
            else {
                unreachable!("a capture leaf's one output is typed from its binding")
            };
            match bindings.get_binding(cap) {
                Some(Binding::Value(value)) => {
                    materialised[idx(out_vtx)] = Some(value);
                    if vtx == root {
                        root_value = Some(value);
                    }
                }
                Some(Binding::Node(_)) => anyhow::bail!(
                    "capture {cap:?} bound a node, not a value; a template can only reuse a value"
                ),
                None => unbound = unbound.or(Some(cap)),
            }
            continue;
        };
        let value_ty = node_value_ty(template, vtx, &value_tys);
        let ctx = TemplateCtx {
            function: builder.function(),
            bindings,
            root: lhs_root,
            root_ty: value_ty,
        };
        let kind = match build {
            TemplateKind::Exact(NodeKind::IntConst(_)) => return Err(raw_int_const()),
            TemplateKind::Exact(k) => *k,
            TemplateKind::Fn(f) => match f(&ctx)? {
                NodeKind::IntConst(_) => return Err(raw_int_const()),
                kind => kind,
            },
            TemplateKind::FnIntConst(f) => {
                let mut v = f(&ctx)?;
                // An inferred shift count that does not fit becomes `mask(v) | width`,
                // which is >= width, so a shift by >= width keeps its p-code result
                // instead of wrapping into range.
                if first_value_output_vtx(template, vtx)
                    .is_some_and(|o| value_tys.saturated_counts.contains(&o))
                    && strider_ir::eval::mask(&v, value_ty) != v
                {
                    v = strider_ir::eval::mask(&v, value_ty)
                        | strider_ir::UBig::from(value_ty.bit_width());
                }
                intern_fn_int_const(builder, value_ty, v)?
            }
        };

        // Raw IEEE bits; bits above the resolved width are refused.
        if let NodeKind::FloatConst(bits) = kind {
            let ty = value_ty;
            anyhow::ensure!(
                !(ty.is_float() && ty.byte_size() > 8),
                "template float const at {ty}: the payload is a u64"
            );
            anyhow::ensure!(
                !ty.is_float() || ty.mask_float_bits(bits) == bits,
                "template float const {bits:#x} has bits above {ty}; give the \
                 IEEE pattern at the width it is evaluated at"
            );
        }
        plan.push((vtx, kind));
    }
    if let Some(cap) = unbound {
        anyhow::bail!("capture {cap:?} referenced in template but unbound by LHS");
    }

    if !plan.is_empty() {
        before_build(builder.function())?;
    }

    let mut checked = Vec::with_capacity(plan.len());
    for (vtx, kind) in plan {
        let outputs = output_kinds_for(template, vtx, &value_tys)?;
        for (slot, got) in outputs.iter().enumerate() {
            let want = kind.expected_output_kind(slot).ok_or_else(|| {
                anyhow::anyhow!(
                    "template node {kind:?} declares output slot {slot}, past what \
                     its node signature admits"
                )
            })?;
            if !want.admits(*got) {
                anyhow::bail!(
                    "template node {kind:?} declares {got} at output slot {slot}, \
                     where its node signature expects {want:?}"
                );
            }
        }
        let input_kinds =
            input_kinds(template, vtx, &materialised, &value_tys, builder.function())?;
        check_operand_types(&kind, &input_kinds, &outputs)?;
        anyhow::ensure!(
            vtx != root || outputs.iter().any(|k| k.as_value().is_some()),
            "template root {kind:?} has no value output to replace the LHS with"
        );
        checked.push((vtx, kind, outputs));
    }

    for (vtx, kind, outputs) in checked {
        let inputs = collect_inputs(template, vtx, &materialised)?;
        let node = builder.create_node_attributed(kind, inputs, outputs, proof_nodes);

        let ir_outputs = builder.function().node_outputs(node);
        for out_vtx in template.graph.node_outputs(vtx).iter().copied() {
            let slot = template.graph.value_kind_ref(out_vtx).slot;
            let ir_value = *ir_outputs.get(slot).ok_or_else(|| {
                anyhow!("template output slot {slot} out of range for instantiated node")
            })?;
            materialised[idx(out_vtx)] = Some(ir_value);
        }

        if vtx == root {
            root_value = builder.function().first_value_output_of(node);
        }
    }

    root_value.ok_or_else(|| anyhow!("root template node never materialised"))
}

/// An `IntConstId` belongs to one function's interner, so template constants
/// are interned at instantiation.
fn raw_int_const() -> anyhow::Error {
    anyhow!(
        "template IntConst built from a NodeKind carries a IntConstId interned \
         elsewhere; build it with int_const or TemplateKind::FnIntConst"
    )
}

/// Every value output's type, indexed by the output vertex.
struct ValueTys {
    by_vertex: Vec<Option<ValueType>>,
    /// Shift counts whose width was inferred from their shift.
    saturated_counts: FxHashSet<TmplValueId>,
    root_ty: ValueType,
}

impl ValueTys {
    /// `vtx` must be a value-output vertex.
    fn of(&self, vtx: TmplValueId) -> ValueType {
        self.by_vertex[idx(vtx)].expect("every value-output vertex is resolved")
    }
}

/// A vertex's index into a per-vertex table.
fn idx(v: TmplValueId) -> usize {
    v.as_u32() as usize
}

fn first_value_output_vtx(template: &Template, node_vtx: NodeId) -> Option<TmplValueId> {
    template
        .graph
        .node_outputs(node_vtx)
        .iter()
        .copied()
        .find(|&v| template.graph.value_kind_ref(v).ty.is_some())
}

/// The node's `Exact` kind; a `Fn` closure's kind is unknown until instantiation.
fn exact_kind(template: &Template, node_vtx: NodeId) -> Option<NodeKind> {
    match &template.graph.node_kind(node_vtx).kind {
        TmplNodeKind::Build(TemplateKind::Exact(k)) => Some(*k),
        _ => None,
    }
}

/// Every node reachable from `root`, operands first and in slot order.
fn operand_postorder(template: &Template, root: NodeId) -> Vec<NodeId> {
    let g = &template.graph;
    let mut seen = vec![false; g.all_node_ids().count()];
    let mut order = Vec::new();
    let mut stack = vec![(root, false)];
    while let Some((node, operands_done)) = stack.pop() {
        if operands_done {
            order.push(node);
            continue;
        }
        if std::mem::replace(&mut seen[node.as_u32() as usize], true) {
            continue;
        }
        stack.push((node, true));
        let mut inputs = consumed_inputs(g, node);
        inputs.sort_by_key(|&(slot, _)| std::cmp::Reverse(slot));
        stack.extend(inputs.into_iter().map(|(_, v)| (g.producer(v), false)));
    }
    order
}

/// [`width_tie`], unioning a `Tied` head with `operand` in `groups`. A head
/// that is the operand itself reads back as `NoHead`.
fn tie(
    groups: &mut UnionFind<u32>,
    kind: &NodeKind,
    slot: usize,
    operand: TmplValueId,
    out: Option<TmplValueId>,
    first: Option<TmplValueId>,
) -> WidthTie<TmplValueId> {
    match width_tie(kind, slot, out, first) {
        WidthTie::Tied(head) if head != operand => {
            groups.union(head.as_u32(), operand.as_u32());
            WidthTie::Tied(head)
        }
        WidthTie::Tied(_) => WidthTie::NoHead,
        other @ (WidthTie::NoHead | WidthTie::Conversion(_) | WidthTie::Free) => other,
    }
}

/// Each value output's type: an [`TemplateTy::InheritRoot`] node takes its width group's type,
/// the root's group `root_ty`, an untyped shift count the shift's.
///
/// # Errors
///
/// If an output's width comes from nowhere.
fn resolve_value_tys(
    template: &Template,
    order: &[NodeId],
    bindings: &Bindings,
    function: &Function,
    root: NodeId,
    root_ty: ValueType,
) -> anyhow::Result<ValueTys> {
    let len = template.graph.all_value_ids().count();
    let mut groups = UnionFind::new(len);
    // The consumer that ties each vertex, for diagnostics.
    let mut tied_by: Vec<Option<(NodeKind, usize)>> = vec![None; len];
    // `(count, shift output)`.
    let mut counts: Vec<(TmplValueId, TmplValueId)> = Vec::new();

    for &node_vtx in order {
        let Some(kind) = exact_kind(template, node_vtx) else {
            continue;
        };
        let inputs = consumed_inputs(&template.graph, node_vtx);
        let first_operand = inputs.iter().find(|&&(slot, _)| slot == 0).map(|&(_, v)| v);
        let out_vtx = first_value_output_vtx(template, node_vtx);
        for &(slot, producer) in &inputs {
            match tie(&mut groups, &kind, slot, producer, out_vtx, first_operand) {
                WidthTie::Tied(head) => {
                    tied_by[idx(producer)].get_or_insert((kind, slot));
                    tied_by[idx(head)].get_or_insert((kind, slot));
                }
                WidthTie::NoHead => {}
                WidthTie::Conversion(_) => {
                    tied_by[idx(producer)].get_or_insert((kind, slot));
                }
                WidthTie::Free => {
                    tied_by[idx(producer)].get_or_insert((kind, slot));
                    if matches!(kind, NodeKind::IntBinaryOp(op) if op.is_shift())
                        && let Some(out) = out_vtx
                    {
                        counts.push((producer, out));
                    }
                }
            }
        }
    }

    let mut group_ty: FxHashMap<u32, ValueType> = FxHashMap::default();
    let mut anchor = |groups: &mut UnionFind<u32>, vtx: TmplValueId, ty: ValueType| {
        group_ty.entry(groups.find_mut(vtx.as_u32())).or_insert(ty);
    };
    // First wins, in operand order rather than id order (a capture can have a
    // later id than its consumer).
    for node_vtx in operand_postorder(template, root) {
        for vtx in template.graph.node_outputs(node_vtx).iter().copied() {
            let ty = match template.graph.value_kind_ref(vtx).ty {
                Some(TemplateTy::InheritBinding(cap)) => bindings.get_type(cap, function),
                Some(TemplateTy::Fixed(t)) => Some(t),
                Some(TemplateTy::InheritRoot) | None => None,
            };
            if let Some(t) = ty {
                anchor(&mut groups, vtx, t);
            }
        }
    }
    // `root_ty` overrides any other type in the root's group.
    if let Some(vtx) = first_value_output_vtx(template, root) {
        let g = groups.find_mut(vtx.as_u32());
        group_ty.insert(g, root_ty);
    }
    // To a fixpoint: a count can be another shift's output.
    let mut inferred: FxHashSet<u32> = FxHashSet::default();
    let mut changed = true;
    while changed {
        changed = false;
        for &(count, out) in &counts {
            let g = groups.find_mut(count.as_u32());
            if group_ty.contains_key(&g) {
                continue;
            }
            if let Some(&t) = group_ty.get(&groups.find_mut(out.as_u32())) {
                group_ty.insert(g, t);
                inferred.insert(g);
                changed = true;
            }
        }
    }
    let saturated_counts: FxHashSet<TmplValueId> = counts
        .iter()
        .map(|&(count, _)| count)
        .filter(|&count| inferred.contains(&groups.find_mut(count.as_u32())))
        .collect();

    let mut by_vertex: Vec<Option<ValueType>> = vec![None; len];
    // A capture's output is its binding, read by `instantiate`.
    for &node_vtx in order {
        if let TmplNodeKind::Capture = template.graph.node_kind(node_vtx).kind {
            continue;
        }
        for vtx in template.graph.node_outputs(node_vtx).iter().copied() {
            let TmplValue { slot, ty: Some(ty) } = *template.graph.value_kind_ref(vtx) else {
                continue;
            };
            let g = groups.find_mut(vtx.as_u32());
            let ty = match ty {
                TemplateTy::Fixed(t) => t,
                TemplateTy::InheritBinding(cap) => match bindings.get_type(cap, function) {
                    Some(t) => t,
                    None => anyhow::bail!(
                        "template node {} output {} takes its width from capture id={}, \
                         which bound no typed value (it is unbound, or bound to memory or \
                         control)",
                        node_name(template, node_vtx),
                        slot,
                        cap.id()
                    ),
                },
                TemplateTy::InheritRoot => match group_ty.get(&g).copied() {
                    Some(t) => t,
                    None => match tied_by[idx(vtx)] {
                        Some((kind, slot))
                            if matches!(
                                kind.operand_width(slot),
                                OperandWidth::Free | OperandWidth::Conversion(_)
                            ) =>
                        {
                            anyhow::bail!(
                                "template node feeding {kind:?} input {slot} has no width: it \
                                 is built fresh with no declared type, and that input owes \
                                 the node no width. Type it: capture_typed or set_value_ty \
                                 (strider.template.capture_typed in Python)"
                            )
                        }
                        Some((kind, slot)) => anyhow::bail!(
                            "template node feeding {kind:?} input {slot} has no width: it is \
                             built fresh with no declared type, and nothing it is evaluated \
                             against carries one either. Type it, or type an operand it is \
                             evaluated against"
                        ),
                        None => anyhow::bail!(
                            "template node {} output {} has no width: no template node \
                             consumes it and it declares no type. Type it with set_value_ty",
                            node_name(template, node_vtx),
                            slot
                        ),
                    },
                },
            };
            by_vertex[idx(vtx)] = Some(ty);
        }
    }
    Ok(ValueTys {
        by_vertex,
        saturated_counts,
        root_ty,
    })
}

/// A template node's name for diagnostics.
fn node_name(template: &Template, node_vtx: NodeId) -> String {
    match &template.graph.node_kind(node_vtx).kind {
        TmplNodeKind::Build(TemplateKind::Exact(k)) => format!("{k:?}"),
        TmplNodeKind::Build(TemplateKind::Fn(_)) => "built by a closure".to_string(),
        TmplNodeKind::Build(TemplateKind::FnIntConst(_)) => "int_const".to_string(),
        TmplNodeKind::Capture => "capture".to_string(),
    }
}

/// Interns `v` at `value_ty`, masked to its width. A float `value_ty` skips the rewrite.
fn intern_fn_int_const<B: IRBuilder>(
    builder: &mut B,
    value_ty: ValueType,
    v: strider_ir::UBig,
) -> anyhow::Result<NodeKind> {
    if !value_ty.is_integer() {
        return Err(crate::skip());
    }
    Ok(NodeKind::IntConst(builder.intern_int_const(v, value_ty)))
}

/// The node's materialised inputs in slot order. A gapped or duplicate slot errors.
fn collect_inputs(
    template: &Template,
    node_vtx: NodeId,
    materialised: &[Option<ValueId>],
) -> anyhow::Result<Vec<ValueId>> {
    let mut inputs_by_slot: BTreeMap<usize, ValueId> = BTreeMap::new();
    for (slot, producer_out_vtx) in consumed_inputs(&template.graph, node_vtx) {
        let producer_value = materialised[idx(producer_out_vtx)].ok_or_else(|| {
            anyhow!("producer output not materialised before consumer (topo order bug)")
        })?;
        if inputs_by_slot.insert(slot, producer_value).is_some() {
            return Err(anyhow!(
                "template node wires two producers into input slot {slot} \
                 (raw-builder mis-wire)"
            ));
        }
    }
    densify(inputs_by_slot, "input")
}

/// The kinds of the node's inputs in slot order: a capture's binding, else
/// the producing template output. A gapped or duplicate slot errors.
fn input_kinds(
    template: &Template,
    node_vtx: NodeId,
    materialised: &[Option<ValueId>],
    value_tys: &ValueTys,
    function: &Function,
) -> anyhow::Result<Vec<ValueKind>> {
    let mut by_slot: BTreeMap<usize, ValueKind> = BTreeMap::new();
    for (slot, out_vtx) in consumed_inputs(&template.graph, node_vtx) {
        let kind = match materialised[idx(out_vtx)] {
            Some(value) => function.value_kind(value),
            None => template
                .graph
                .value_kind_ref(out_vtx)
                .ty
                .map_or(ValueKind::Memory, |_| {
                    ValueKind::Typed(value_tys.of(out_vtx))
                }),
        };
        if by_slot.insert(slot, kind).is_some() {
            anyhow::bail!(
                "template node wires two producers into input slot {slot} \
                 (raw-builder mis-wire)"
            );
        }
    }
    densify(by_slot, "input")
}

/// The values in slot order; the keys must be exactly `0..len`.
fn densify<T>(by_slot: BTreeMap<usize, T>, what: &str) -> anyhow::Result<Vec<T>> {
    if by_slot.keys().enumerate().any(|(i, &slot)| i != slot) {
        let slots: Vec<usize> = by_slot.keys().copied().collect();
        return Err(anyhow!(
            "template node has non-contiguous {what} slots {slots:?}, \
             expected 0..{} (raw-builder mis-wire)",
            by_slot.len()
        ));
    }
    Ok(by_slot.into_values().collect())
}

/// The type of the node's first value output, else the root's.
fn node_value_ty(template: &Template, node_vtx: NodeId, value_tys: &ValueTys) -> ValueType {
    first_value_output_vtx(template, node_vtx).map_or(value_tys.root_ty, |v| value_tys.of(v))
}

/// Checks `inputs` against `kind`'s signature: count, slot kinds and operand
/// widths, as `strider_ir::validate` does.
fn check_operand_types(
    kind: &NodeKind,
    inputs: &[ValueKind],
    outputs: &[ValueKind],
) -> anyhow::Result<()> {
    let arity = kind.input_arity();
    if !arity.contains(&inputs.len()) {
        let admits = match (*arity.start(), *arity.end()) {
            (lo, usize::MAX) => format!("at least {lo}"),
            (lo, hi) if lo == hi => lo.to_string(),
            (lo, hi) => format!("{lo} to {hi}"),
        };
        anyhow::bail!(
            "template node {kind:?} has {} inputs, where its node signature admits {admits}",
            inputs.len()
        );
    }
    let out_ty = outputs.first().and_then(|k| k.as_value());
    let mut first_ty = None;
    for (slot, &got) in inputs.iter().enumerate() {
        let class = kind
            .expected_input_kind(slot)
            .expect("a slot within the arity checked above");
        if !class.admits(got) {
            anyhow::bail!(
                "template node {kind:?} takes {got} at input slot {slot}, \
                 where its node signature expects {class:?}"
            );
        }
        let Some(got) = got.as_value() else {
            continue;
        };
        if slot == 0 {
            first_ty = Some(got);
        }
        let want = match width_tie(kind, slot, out_ty, first_ty) {
            WidthTie::Tied(want) => want,
            WidthTie::Conversion(c) => {
                if let Some(out) = out_ty {
                    check_conversion(kind, c, got, out)?;
                }
                continue;
            }
            WidthTie::NoHead | WidthTie::Free => continue,
        };
        if want.bit_width() != got.bit_width() {
            return Err(crate::OperandWidthMismatch {
                kind: *kind,
                slot,
                got,
                want,
            }
            .into());
        }
    }
    Ok(())
}

fn check_conversion(
    kind: &NodeKind,
    c: Conversion,
    input: ValueType,
    output: ValueType,
) -> anyhow::Result<()> {
    match c {
        Conversion::Widen | Conversion::Narrow => anyhow::ensure!(
            c.admits(input, output),
            "template node {kind:?} has input width {} and output width {}; \
             `Extend` must strictly widen and `Truncate` must strictly narrow",
            input.bit_width(),
            output.bit_width()
        ),
        Conversion::SameSize => anyhow::ensure!(
            c.admits(input, output),
            "template node {kind:?} reinterprets {input} as {output}, \
             which is a different size"
        ),
    }
    Ok(())
}

/// The node's output kinds in slot order. A gapped or duplicate slot errors.
fn output_kinds_for(
    template: &Template,
    node_vtx: NodeId,
    value_tys: &ValueTys,
) -> anyhow::Result<Vec<ValueKind>> {
    let mut by_slot: BTreeMap<usize, ValueKind> = BTreeMap::new();
    for out_vtx in template.graph.node_outputs(node_vtx).iter().copied() {
        let TmplValue { slot, ty } = *template.graph.value_kind_ref(out_vtx);
        let kind = ty.map_or(ValueKind::Memory, |_| {
            ValueKind::Typed(value_tys.of(out_vtx))
        });
        if by_slot.insert(slot, kind).is_some() {
            anyhow::bail!(
                "template node declares two outputs at slot {slot} (raw-builder mis-wire)"
            );
        }
    }
    densify(by_slot, "output")
}
