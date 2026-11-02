//! [`instantiate`] synthesises every `Build` node through the
//! [`strider_ir::IRBuilder::create_node_attributed`] seam, so each implementor
//! keeps its own attribution and liveness policy.

mod builder;
mod ctx;
mod graph;
pub(crate) mod template_pat;
#[cfg(test)]
mod tests;

pub use builder::{TemplateBuilder, TmplNodeRef, TmplValueRef};
pub use ctx::TemplateCtx;
pub use graph::Template;
/// The vertex payloads behind a [`Template`]'s graph. Not the seam: a template
/// is described through [`TemplateBuilder`], never by handing it a vertex.
pub(crate) use graph::{TmplNodeKind, TmplValue};

pub use crate::typed::value_ops::ops::*;

use std::collections::BTreeMap;

use anyhow::anyhow;
use rustc_hash::{FxHashMap, FxHashSet};
use strider_graph::ValueId as TmplValueId;
use strider_ir::node::{
    Conversion, NodeId, NodeKind, OperandWidth, ValueId, ValueKind, ValueType, WidthGroups,
    WidthTie, width_tie,
};
use strider_ir::{Function, IRBuilder, IRViewer};

use crate::bindings::Bindings;
use crate::graph_ext::consumed_inputs;

pub type TemplateKindFn = Box<dyn Fn(&TemplateCtx<'_>) -> anyhow::Result<NodeKind> + Send>;

/// Unbounded: the value is interned at the resolved output width, whatever
/// that width is.
pub type TemplateKindFnIntConst =
    Box<dyn Fn(&TemplateCtx<'_>) -> anyhow::Result<strider_ir::UBig> + Send>;

/// How a template node materialises into fresh IR.
pub enum TemplateKind {
    Exact(NodeKind),
    /// The closure returns the `NodeKind` to materialise, given a
    /// [`TemplateCtx`]; an `IntConst` is refused, as under `Exact`.
    Fn(TemplateKindFn),
    /// The closure computes a [`strider_ir::UBig`] at rewrite time, interned
    /// at the resolved output width.
    FnIntConst(TemplateKindFnIntConst),
}

/// The output type a template node declares for its value output. All three
/// resolve at instantiation time.
#[derive(Clone, Copy)]
pub(crate) enum TemplateTy {
    /// No width of its own: the one the node is evaluated at, which is the
    /// rewrite root's output type unless an operand group pins another. A
    /// shift count takes the shift's width, a constant that does not fit it
    /// saturating; an operand at another slot that owes its node no width,
    /// or an output no node consumes, must be typed.
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
/// `root_ty` types the root's operand group: the value it replaces.
/// `lhs_root` reaches [`TemplateKind::Fn`] closures as [`TemplateCtx::root`];
/// a pure-`Exact` template ignores it.
///
/// `proof_nodes` is unioned into the asm-fingerprint of every node created
/// here.
///
/// Interior nodes may be multi-output; the returned value is the root node's
/// first value output.
///
/// # Signature checking
///
/// [`strider_ir::validate`] is not run, so each node is checked here against
/// its `NodeKind`'s `expected_signature`: the declared output kinds, the input
/// count, each input slot's value kind and the operand widths. Input-slot
/// wiring is checked too: a gap or a duplicate errors out. What the signature
/// leaves open, such as which address a `Load` reads, is the author's.
///
/// # Errors
///
/// If the template is rootless, references an unbound capture, has a
/// [`TemplateKind::Fn`] closure that itself errors, has gapped or duplicate
/// input slots, has an `IntConst` built from a `NodeKind`, has a node no width
/// reaches (an untyped fresh operand at an input that owes its node no width,
/// an untyped output no node consumes, an output typed from a capture that
/// bound no typed value, a node that declares no output), gives a node an
/// input count, an output or an operand its node signature refuses, or if
/// node creation yields no value output. Closure, unbound-capture, width,
/// `IntConst`-from-`NodeKind` and wide-float refusals come before any node is
/// built; a signature or wiring refusal is found mid-build, so the nodes
/// created before it stay in `builder`'s function, dead.
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

/// [`instantiate`], calling `before_build` once every node's kind is resolved
/// and before the first node is created, only when the template builds one.
/// A closure's skip comes first, so a declined template never reaches it.
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
    // Ids run producers first but for a capture, which materialises before
    // any node is built.
    let order: Vec<NodeId> = template.graph.all_node_ids().collect();

    // Scanned before the mutating build loop, which is what the immutable
    // `function` borrow requires.
    let value_tys = resolve_value_tys(
        template,
        &order,
        bindings,
        builder.function(),
        root,
        root_ty,
    )?;

    // Keyed on the output VERTEX rather than the producer node, so a
    // multi-output interior node feeds the right slot to each consumer: a
    // `Call`'s memory output and its value outputs must resolve to distinct
    // IR outputs.
    let mut materialised: Vec<Option<ValueId>> = vec![None; value_tys.by_vertex.len()];
    let mut root_value: Option<ValueId> = None;

    // Every step first, so a closure's skip or an unbound capture declines
    // the template before any node exists.
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
            // A capture leaf IS its materialisation: its one output resolves
            // to the LHS binding, reused verbatim as in `int_add(x, 0) -> x`.
            let out_vtx = template.graph.node_outputs(vtx)[0];
            let Some(TemplateTy::InheritBinding(cap)) = template.graph.value_kind_ref(out_vtx).ty
            else {
                unreachable!("a capture leaf's one output is typed from its binding")
            };
            match bindings.get_value(cap) {
                Some(value) => {
                    materialised[idx(out_vtx)] = Some(value);
                    if vtx == root {
                        root_value = Some(value);
                    }
                }
                None => unbound = unbound.or(Some(cap)),
            }
            continue;
        };
        // A closure computes against this node's own declared output type,
        // not the rewrite root's.
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
                // P-code tests the whole count against the width, so one that
                // does not fit keeps every bit of the width set rather than
                // wrapping back into range, as the lifter's count does.
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

        // A template `FloatConst` carries raw IEEE bits with no width; the
        // width is resolved here, and bits above it are another constant.
        if let NodeKind::FloatConst(bits) = kind {
            let ty = value_ty;
            // The payload is a `u64`, so a wider float has no encoding here.
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
    // After every closure, so a skip declines the template ahead of this.
    if let Some(cap) = unbound {
        anyhow::bail!("capture {cap:?} referenced in template but unbound by LHS");
    }

    if !plan.is_empty() {
        before_build(builder.function())?;
    }

    for (vtx, kind) in plan {
        let inputs = collect_inputs(template, vtx, &materialised)?;

        // Usually a single value output; a multi-output node such as a
        // `Call` declares its memory output here too.
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

        check_operand_types(&kind, &inputs, &outputs, builder.function())?;

        let node = builder.create_node_attributed(kind, inputs, outputs, proof_nodes);

        // Map each template output vertex onto the IR output at the same
        // slot, so multi-output consumers wire the right edge.
        let ir_outputs = builder.function().node_outputs(node);
        for out_vtx in template.graph.node_outputs(vtx).iter().copied() {
            let slot = template.graph.value_kind_ref(out_vtx).slot;
            let ir_value = *ir_outputs.get(slot).ok_or_else(|| {
                anyhow!("template output slot {slot} out of range for instantiated node")
            })?;
            materialised[idx(out_vtx)] = Some(ir_value);
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

/// A `IntConstId` indexes one function's interner at one width, so a template
/// constant is interned at instantiation, at the node's resolved width.
fn raw_int_const() -> anyhow::Error {
    anyhow!(
        "template IntConst built from a NodeKind carries a IntConstId interned \
         elsewhere; build it with int_const or TemplateKind::FnIntConst"
    )
}

/// Every value output's type, indexed by the output vertex.
struct ValueTys {
    by_vertex: Vec<Option<ValueType>>,
    /// Shift counts whose width is their shift's, inferred: a constant there
    /// that does not fit saturates rather than wraps.
    saturated_counts: FxHashSet<TmplValueId>,
    root_ty: ValueType,
}

impl ValueTys {
    /// Every value-output vertex is resolved, so `vtx` must be one.
    fn of(&self, vtx: TmplValueId) -> ValueType {
        self.by_vertex[idx(vtx)].expect("every value-output vertex is resolved")
    }
}

/// A vertex's position in a table spanning every vertex of the template.
fn idx(v: TmplValueId) -> usize {
    v.as_u32() as usize
}

/// The node's first value-output vertex.
fn first_value_output_vtx(template: &Template, node_vtx: NodeId) -> Option<TmplValueId> {
    template
        .graph
        .node_outputs(node_vtx)
        .iter()
        .copied()
        .find(|&v| template.graph.value_kind_ref(v).ty.is_some())
}

/// The build kind a node declares up front. A `Fn` closure picks its kind at
/// instantiation, so its slots relate no widths here.
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

/// Each value output's type. A node built with no declared type
/// ([`TemplateTy::InheritRoot`]) takes the width of the group it is evaluated
/// in, so a fresh constant in a comparison operand gets the OPERAND width and
/// not the `I1` verdict the root carries. The rewrite root itself is what
/// replaces the matched value, so its group takes `root_ty`. A shift count
/// with no width of its own takes the shift's.
///
/// # Errors
///
/// If an output's width comes from nowhere, since any width picked here would
/// be arbitrary: a group every member of which is built fresh with no
/// declared type (at a slot [`NodeKind::operand_width`] leaves free, such as
/// a `Popcount` operand, `strider_ir::validate` would accept the pick and the
/// constant would hold a different value), an untyped output no template node
/// consumes, or an output typed from a capture that bound no typed value.
fn resolve_value_tys(
    template: &Template,
    order: &[NodeId],
    bindings: &Bindings,
    function: &Function,
    root: NodeId,
    root_ty: ValueType,
) -> anyhow::Result<ValueTys> {
    let len = template.graph.all_value_ids().count();
    let mut groups = WidthGroups::new(len);
    // The consumer that ties each vertex to a group, for the error naming it.
    let mut tied_by: Vec<Option<(NodeKind, usize)>> = vec![None; len];
    // `(count, shift output)`: a count with no width of its own takes the
    // shift's, as the lifter emits it. Never the other way round.
    let mut counts: Vec<(TmplValueId, TmplValueId)> = Vec::new();

    for &node_vtx in order {
        let Some(kind) = exact_kind(template, node_vtx) else {
            continue;
        };
        let inputs = consumed_inputs(&template.graph, node_vtx);
        let first_operand = inputs.iter().find(|&&(slot, _)| slot == 0).map(|&(_, v)| v);
        let out_vtx = first_value_output_vtx(template, node_vtx);
        for &(slot, producer) in &inputs {
            match groups.tie(&kind, slot, producer, out_vtx, first_operand) {
                WidthTie::Tied(head) => {
                    tied_by[idx(producer)].get_or_insert((kind, slot));
                    tied_by[idx(head)].get_or_insert((kind, slot));
                }
                WidthTie::NoHead => {}
                // A conversion pins its input apart from its output.
                WidthTie::Conversion(_) => {
                    tied_by[idx(producer)].get_or_insert((kind, slot));
                }
                // A free slot pins it to nothing.
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

    let mut group_ty: FxHashMap<usize, ValueType> = FxHashMap::default();
    let mut anchor = |groups: &mut WidthGroups, vtx: TmplValueId, ty: ValueType| {
        group_ty.entry(groups.find(vtx)).or_insert(ty);
    };
    // First wins, so the order is the graph's own: a capture can be built
    // after its consumer, and its id would otherwise decide the anchor.
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
    // Overrides whatever else the root's group holds, so a template that
    // already types its nodes builds exactly what it built before.
    if let Some(vtx) = first_value_output_vtx(template, root) {
        let g = groups.find(vtx);
        group_ty.insert(g, root_ty);
    }
    // To a fixpoint: a count can be the output of another shift whose own
    // width comes from a count.
    let mut inferred: FxHashSet<usize> = FxHashSet::default();
    let mut changed = true;
    while changed {
        changed = false;
        for &(count, out) in &counts {
            let g = groups.find(count);
            if group_ty.contains_key(&g) {
                continue;
            }
            if let Some(&t) = group_ty.get(&groups.find(out)) {
                group_ty.insert(g, t);
                inferred.insert(g);
                changed = true;
            }
        }
    }
    let saturated_counts: FxHashSet<TmplValueId> = counts
        .iter()
        .map(|&(count, _)| count)
        .filter(|&count| inferred.contains(&groups.find(count)))
        .collect();

    let mut by_vertex: Vec<Option<ValueType>> = vec![None; len];
    // A capture's output is its binding, which `instantiate` reads instead.
    for &node_vtx in order {
        if let TmplNodeKind::Capture = template.graph.node_kind(node_vtx).kind {
            continue;
        }
        for vtx in template.graph.node_outputs(node_vtx).iter().copied() {
            let TmplValue { slot, ty: Some(ty) } = *template.graph.value_kind_ref(vtx) else {
                continue;
            };
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
                TemplateTy::InheritRoot => match group_ty.get(&groups.find(vtx)).copied() {
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

/// A template node as an error names it.
fn node_name(template: &Template, node_vtx: NodeId) -> String {
    match &template.graph.node_kind(node_vtx).kind {
        TmplNodeKind::Build(TemplateKind::Exact(k)) => format!("{k:?}"),
        TmplNodeKind::Build(TemplateKind::Fn(_)) => "built by a closure".to_string(),
        TmplNodeKind::Build(TemplateKind::FnIntConst(_)) => "int_const".to_string(),
        TmplNodeKind::Capture => "capture".to_string(),
    }
}

/// Masks `v` to `value_ty`'s width and stores it.
/// A float `value_ty` has no integer literal to intern: the caller gave a
/// float-typed root an integer constant, which is the rule's mistake, not a
/// graph error to surface downstream, so it skips the rewrite.
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

/// In slot order, reading each producer's already-materialised IR output from
/// `materialised`. A gapped or duplicate input slot is rejected here rather
/// than silently closed or overwritten.
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

/// The keys must be exactly `0..len`, or the dense `into_values()` shifts
/// later slots down onto the wrong IR index, and the signature check then
/// validates the wrong slot. `what` names the side for the diagnostic.
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

/// The resolved type of the node's first value output vertex. A node with no
/// value output vertex falls back to the root's type.
fn node_value_ty(template: &Template, node_vtx: NodeId, value_tys: &ValueTys) -> ValueType {
    first_value_output_vtx(template, node_vtx).map_or(value_tys.root_ty, |v| value_tys.of(v))
}

/// Checks `inputs` against `kind`'s node signature: the input count, each
/// slot's value kind, and the width an operand owes the output or operand 0,
/// as `strider_ir::validate` enforces them.
fn check_operand_types(
    kind: &NodeKind,
    inputs: &[ValueId],
    outputs: &[ValueKind],
    function: &Function,
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
    for (slot, &value) in inputs.iter().enumerate() {
        let got = function.value_kind(value);
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
            anyhow::bail!(
                "template node {kind:?} takes a {got} operand at input slot {slot}, \
                 where it is evaluated at {want}"
            );
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

/// Each output takes its own resolved type. A gapped or duplicate output slot
/// is rejected, as on the input side.
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
