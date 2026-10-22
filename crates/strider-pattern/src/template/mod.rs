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
use rustc_hash::{FxHashMap, FxHashSet};
use strider_graph::ValueId as TmplValueId;
use strider_ir::node::{Conversion, NodeId, NodeKind, OperandWidth, ValueId, ValueKind, ValueType};
use strider_ir::{Function, IRBuilder, IRViewer};

use crate::bindings::Bindings;
use crate::graph_ext::{consumed_inputs, topo_order};
use crate::matcher::OutputKindSpec;

pub type TemplateKindFn = Box<dyn Fn(&TemplateCtx<'_>) -> anyhow::Result<NodeKind> + Send>;

/// The `u128` return caps the expressible range: an output wider than `I128`
/// skips the rewrite rather than interning a truncated constant.
pub type TemplateKindFnIntConst = Box<dyn Fn(&TemplateCtx<'_>) -> anyhow::Result<u128> + Send>;

/// How a template node materialises into fresh IR.
pub enum TemplateKind {
    Exact(NodeKind),
    /// The closure returns the `NodeKind` to materialise, given a
    /// [`TemplateCtx`]; an `IntConst` is refused, as under `Exact`.
    Fn(TemplateKindFn),
    /// The closure computes a `u128` at rewrite time, interned at the
    /// resolved output width.
    FnIntConst(TemplateKindFnIntConst),
}

/// The output type a template node declares for its value output. All three
/// resolve at instantiation time.
#[derive(Clone, Copy)]
pub enum TemplateTy {
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
/// bound no typed value), gives a node an input count, an output or an operand its node
/// signature refuses, or if node creation yields no value output. Closure,
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
    let order = topo_order(&template.graph)?;

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
    // `Store`'s memory output and a sibling value output must resolve to
    // distinct IR outputs.
    let mut materialised: FxHashMap<TmplValueId, ValueId> = FxHashMap::default();
    let mut root_value: Option<ValueId> = None;

    // Every kind first, so a closure's skip declines the template before any
    // node exists. `None` is a capture leaf.
    let mut kinds: Vec<(NodeId, Option<NodeKind>)> = Vec::with_capacity(order.len());
    for vtx in order {
        let nd = template.graph.node_kind(vtx);
        let kind = match &nd.kind {
            TmplNodeKind::Capture => {
                kinds.push((vtx, None));
                continue;
            }
            TmplNodeKind::Build(TemplateKind::Exact(NodeKind::IntConst(_))) => {
                return Err(raw_int_const());
            }
            TmplNodeKind::Build(TemplateKind::Exact(k)) => *k,
            TmplNodeKind::Build(TemplateKind::Fn(f)) => {
                // The closure computes its constant against this node's own
                // declared output type, not the rewrite root's.
                let value_ty = node_value_ty(template, vtx, &value_tys);
                let ctx = TemplateCtx {
                    function: builder.function(),
                    bindings,
                    root: lhs_root,
                    root_ty: value_ty,
                };
                match f(&ctx)? {
                    NodeKind::IntConst(_) => return Err(raw_int_const()),
                    kind => kind,
                }
            }
            TmplNodeKind::Build(TemplateKind::FnIntConst(f)) => {
                let value_ty = node_value_ty(template, vtx, &value_tys);
                let ctx = TemplateCtx {
                    function: builder.function(),
                    bindings,
                    root: lhs_root,
                    root_ty: value_ty,
                };
                let mut v = f(&ctx)?;
                // P-code tests the whole count against the width, so one that
                // does not fit keeps every bit of the width set rather than
                // wrapping back into range, as the lifter's count does.
                if first_value_output_vtx(template, vtx)
                    .is_some_and(|o| value_tys.saturated_counts.contains(&o))
                    && value_ty.wrap_u128(v) != v
                {
                    let bits = u128::try_from(value_ty.bit_width()).expect("bit width fits u128");
                    v = value_ty.wrap_u128(v) | bits;
                }
                intern_fn_int_const(builder, value_ty, v)?
            }
        };

        // A template `FloatConst` carries raw IEEE bits with no width; the
        // width is resolved here, so bits above it are dropped here.
        let kind = match kind {
            NodeKind::FloatConst(bits) => {
                let ty = node_value_ty(template, vtx, &value_tys);
                // The payload is a `u64`, so a wider float has no encoding here
                // and the mask would silently keep 64 of its bits.
                anyhow::ensure!(
                    !(ty.is_float() && ty.byte_size() > 8),
                    "template float const at {ty}: the payload is a u64"
                );
                NodeKind::FloatConst(if ty.is_float() {
                    ty.mask_float_bits(bits)
                } else {
                    bits
                })
            }
            other => other,
        };
        kinds.push((vtx, Some(kind)));
    }

    if kinds.iter().any(|(_, k)| k.is_some()) {
        before_build(builder.function())?;
    }

    for (vtx, kind) in kinds {
        // A `Capture` leaf has no build kind: it IS the materialisation. Its
        // `ValueCapture` output resolves to the LHS binding, reusing that
        // value verbatim as in `int_add(x, 0) -> x`, and is never synthesised.
        // The capture id lives on the output, not the marker node.
        let Some(kind) = kind else {
            let outs = template.graph.node_outputs(vtx);
            let out_vtx = *outs
                .first()
                .ok_or_else(|| anyhow!("capture leaf has no value-capture output"))?;
            let TmplValue::ValueCapture(cap) = template.graph.value_kind_ref(out_vtx) else {
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
        };

        let inputs = collect_inputs(template, vtx, &materialised)?;

        // Usually a single value output; a multi-output node such as a
        // `Store` declares its memory output here too.
        let outputs = output_kinds_for(template, vtx, &value_tys)?;
        for (slot, got) in outputs.iter().enumerate() {
            let want = kind.expected_output_kind(slot).ok_or_else(|| {
                anyhow::anyhow!(
                    "template node {kind:?} declares output slot {slot}, past what \
                     its node signature admits"
                )
            })?;
            // A `Store` with no explicit output vertex gets a value output here.
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
            // A `ValueCapture` never hangs off a `Build` node.
            let TmplValue::TmplOutput(o) = template.graph.value_kind_ref(out_vtx) else {
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

/// A `ConstId` indexes one function's interner at one width, so a template
/// constant is interned at instantiation, at the node's resolved width.
fn raw_int_const() -> anyhow::Error {
    anyhow!(
        "template IntConst built from a NodeKind carries a ConstId interned \
         elsewhere; build it with int_const or TemplateKind::FnIntConst"
    )
}

/// Every value output's type, keyed on the output vertex.
struct ValueTys {
    by_vertex: FxHashMap<TmplValueId, ValueType>,
    /// Shift counts whose width is their shift's, inferred: a constant there
    /// that does not fit saturates rather than wraps.
    saturated_counts: FxHashSet<TmplValueId>,
    root_ty: ValueType,
}

impl ValueTys {
    /// Every value-output vertex is resolved, so `vtx` must be one.
    fn of(&self, vtx: TmplValueId) -> ValueType {
        self.by_vertex[&vtx]
    }
}

/// Union-find over value vertices, grouping the ones a node evaluates at one
/// width.
struct WidthGroups {
    parent: Vec<u32>,
}

impl WidthGroups {
    fn new(len: usize) -> Self {
        Self {
            parent: (0..len as u32).collect(),
        }
    }

    fn find(&mut self, v: TmplValueId) -> usize {
        let mut i = v.as_u32() as usize;
        while self.parent[i] as usize != i {
            let grandparent = self.parent[self.parent[i] as usize];
            self.parent[i] = grandparent;
            i = grandparent as usize;
        }
        i
    }

    fn union(&mut self, a: TmplValueId, b: TmplValueId) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.parent[rb] = ra as u32;
        }
    }
}

/// Whether the slot carries a value, and so a width.
fn is_value_output(kind: &OutputKindSpec) -> bool {
    matches!(
        kind,
        OutputKindSpec::Value(_) | OutputKindSpec::AnyValue | OutputKindSpec::Any
    )
}

/// The node's first value-output vertex.
fn first_value_output_vtx(template: &Template, node_vtx: NodeId) -> Option<TmplValueId> {
    template
        .graph
        .node_outputs(node_vtx)
        .iter()
        .copied()
        .find(|&v| match template.graph.value_kind_ref(v) {
            TmplValue::TmplOutput(o) => is_value_output(&o.kind),
            TmplValue::ValueCapture(_) => true,
        })
}

/// The build kind a node declares up front. A `Fn` closure picks its kind at
/// instantiation, so its slots relate no widths here.
fn exact_kind(template: &Template, node_vtx: NodeId) -> Option<NodeKind> {
    match &template.graph.node_kind(node_vtx).kind {
        TmplNodeKind::Build(TemplateKind::Exact(k)) => Some(*k),
        _ => None,
    }
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
/// If a group a node's width is tied to carries no width at all: every member
/// is built fresh with no declared type, so any width picked here is arbitrary.
/// At a slot [`NodeKind::operand_width`] leaves free, such as a `Popcount`
/// operand, `strider_ir::validate` would accept the pick and the constant
/// would hold a different value.
fn resolve_value_tys(
    template: &Template,
    order: &[NodeId],
    bindings: &Bindings,
    function: &Function,
    root: NodeId,
    root_ty: ValueType,
) -> anyhow::Result<ValueTys> {
    let binding_tys = resolve_binding_tys(template, bindings, function);
    let len = template
        .graph
        .all_value_ids()
        .map(|v| v.as_u32() as usize + 1)
        .max()
        .unwrap_or(0);
    let mut groups = WidthGroups::new(len);
    // The consumer that ties each vertex to a group, for the error naming it.
    let mut tied_by: FxHashMap<TmplValueId, (NodeKind, usize)> = FxHashMap::default();
    // `(count, shift output)`: a count with no width of its own takes the
    // shift's, as the lifter emits it. Never the other way round.
    let mut counts: Vec<(TmplValueId, TmplValueId)> = Vec::new();

    for &node_vtx in order {
        let Some(kind) = exact_kind(template, node_vtx) else {
            continue;
        };
        let inputs = consumed_inputs(&template.graph, node_vtx);
        let first_operand = inputs.iter().find(|&&(slot, _)| slot == 0).map(|&(_, v)| v);
        for &(slot, producer) in &inputs {
            let head = match kind.operand_width(slot) {
                OperandWidth::OfOutput => first_value_output_vtx(template, node_vtx),
                OperandWidth::OfFirstOperand => first_operand,
                // A conversion pins its input apart from its output.
                OperandWidth::Conversion(_) => {
                    tied_by.entry(producer).or_insert((kind, slot));
                    continue;
                }
                // A free slot pins it to nothing.
                OperandWidth::Free => {
                    tied_by.entry(producer).or_insert((kind, slot));
                    if matches!(kind, NodeKind::IntBinaryOp(op) if op.is_shift())
                        && let Some(out) = first_value_output_vtx(template, node_vtx)
                    {
                        counts.push((producer, out));
                    }
                    continue;
                }
            };
            let Some(head) = head.filter(|&h| h != producer) else {
                continue;
            };
            groups.union(head, producer);
            tied_by.entry(producer).or_insert((kind, slot));
            tied_by.entry(head).or_insert((kind, slot));
        }
    }

    let mut group_ty: FxHashMap<usize, ValueType> = FxHashMap::default();
    let mut anchor = |groups: &mut WidthGroups, vtx: TmplValueId, ty: ValueType| {
        group_ty.entry(groups.find(vtx)).or_insert(ty);
    };
    for &node_vtx in order {
        for vtx in template.graph.node_outputs(node_vtx).iter().copied() {
            match template.graph.value_kind_ref(vtx) {
                TmplValue::ValueCapture(cap) => {
                    if let Some(v) = bindings.get_value(*cap)
                        && let ValueKind::Typed(t) = function.value_kind(v)
                    {
                        anchor(&mut groups, vtx, t);
                    }
                }
                TmplValue::TmplOutput(o) if is_value_output(&o.kind) => match o.ty {
                    TemplateTy::Fixed(t) => anchor(&mut groups, vtx, t),
                    TemplateTy::InheritBinding(cap) => {
                        if let Some(&t) = binding_tys.get(&cap) {
                            anchor(&mut groups, vtx, t);
                        }
                    }
                    TemplateTy::InheritRoot => {}
                },
                TmplValue::TmplOutput(_) => {}
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

    let mut by_vertex: FxHashMap<TmplValueId, ValueType> = FxHashMap::default();
    for &node_vtx in order {
        for vtx in template.graph.node_outputs(node_vtx).iter().copied() {
            let TmplValue::TmplOutput(o) = template.graph.value_kind_ref(vtx) else {
                continue;
            };
            if !is_value_output(&o.kind) {
                continue;
            }
            let ty = match o.ty {
                TemplateTy::Fixed(t) => t,
                TemplateTy::InheritBinding(cap) => match binding_tys.get(&cap) {
                    Some(&t) => t,
                    None => anyhow::bail!(
                        "template node {} output {} takes its width from capture id={}, \
                         which bound no typed value (it is unbound, or bound to memory or \
                         control)",
                        node_name(template, node_vtx),
                        o.slot,
                        cap.id()
                    ),
                },
                TemplateTy::InheritRoot => match group_ty.get(&groups.find(vtx)).copied() {
                    Some(t) => t,
                    None => match tied_by.get(&vtx) {
                        Some((kind, slot))
                            if matches!(
                                kind.operand_width(*slot),
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
                            o.slot
                        ),
                    },
                },
            };
            by_vertex.insert(vtx, ty);
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

/// Resolves every `InheritBinding(cap)` width, from the value each capture
/// matched. An unbound or non-typed capture is omitted.
fn resolve_binding_tys(
    template: &Template,
    bindings: &Bindings,
    function: &Function,
) -> FxHashMap<crate::Capture, ValueType> {
    let mut out: FxHashMap<crate::Capture, ValueType> = FxHashMap::default();
    for vtx in template.graph.all_node_ids() {
        for out_vtx in template.graph.node_outputs(vtx).iter().copied() {
            let TmplValue::TmplOutput(o) = template.graph.value_kind_ref(out_vtx) else {
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
/// rewrite instead of interning a truncated constant. A float `value_ty` has no
/// integer literal to intern at all, and skips for the same reason: the caller
/// gave a float-typed root an integer constant, which is the rule's mistake,
/// not a graph error to surface downstream.
fn intern_fn_int_const<B: IRBuilder>(
    builder: &mut B,
    value_ty: ValueType,
    v: u128,
) -> anyhow::Result<NodeKind> {
    if value_ty.bit_width() > 128 || !value_ty.is_integer() {
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
    for (slot, producer_out_vtx) in consumed_inputs(&template.graph, node_vtx) {
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
fn resolved_output_kind(o: &TmplOutput, vtx: TmplValueId, value_tys: &ValueTys) -> ValueKind {
    match o.kind {
        OutputKindSpec::Memory => ValueKind::Memory,
        OutputKindSpec::Control => ValueKind::Control,
        OutputKindSpec::PhiToken => ValueKind::PhiToken,
        // Every value shape uses this output's own resolved type. `Any` is
        // match-only and no template builder emits it; resolved defensively.
        OutputKindSpec::Value(_) | OutputKindSpec::AnyValue | OutputKindSpec::Any => {
            ValueKind::Typed(value_tys.of(vtx))
        }
    }
}

/// The resolved type of the node's first value output vertex. A node with no
/// value output vertex falls back to the root's type.
fn node_value_ty(template: &Template, node_vtx: NodeId, value_tys: &ValueTys) -> ValueType {
    template
        .graph
        .node_outputs(node_vtx)
        .iter()
        .copied()
        .find_map(|out_vtx| {
            let TmplValue::TmplOutput(o) = template.graph.value_kind_ref(out_vtx) else {
                return None;
            };
            match resolved_output_kind(o, out_vtx, value_tys) {
                ValueKind::Typed(t) => Some(t),
                _ => None,
            }
        })
        .unwrap_or(value_tys.root_ty)
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
        let want = match kind.operand_width(slot) {
            OperandWidth::OfOutput => out_ty,
            OperandWidth::OfFirstOperand => first_ty,
            OperandWidth::Conversion(c) => {
                if let Some(out) = out_ty {
                    check_conversion(kind, c, got, out)?;
                }
                continue;
            }
            OperandWidth::Free => continue,
        };
        if let Some(want) = want
            && want.bit_width() != got.bit_width()
        {
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

/// Each value output takes its own resolved type. A node with no explicit
/// output vertex falls back to a single value output of the root's type.
fn output_kinds_for(
    template: &Template,
    node_vtx: NodeId,
    value_tys: &ValueTys,
) -> anyhow::Result<Vec<ValueKind>> {
    let mut by_slot: BTreeMap<usize, ValueKind> = BTreeMap::new();
    for out_vtx in template.graph.node_outputs(node_vtx).iter().copied() {
        if let TmplValue::TmplOutput(o) = template.graph.value_kind_ref(out_vtx) {
            by_slot.insert(o.slot, resolved_output_kind(o, out_vtx, value_tys));
        }
    }
    if by_slot.is_empty() {
        return Ok(vec![ValueKind::Typed(value_tys.root_ty)]);
    }
    // As in `collect_inputs`: the dense `into_values()` would shift a later
    // slot down onto the wrong IR output index, and the signature check above
    // would then validate the wrong slot.
    if by_slot.keys().enumerate().any(|(i, &slot)| i != slot) {
        let slots: Vec<usize> = by_slot.keys().copied().collect();
        return Err(anyhow!(
            "template node has non-contiguous output slots {slots:?}, \
             expected 0..{} (raw-builder mis-wire)",
            by_slot.len()
        ));
    }
    Ok(by_slot.into_values().collect())
}

#[cfg(test)]
mod tests {
    use strider_ir::node::{NodeId, NodeKind, ValueType as T, ValueType};
    use strider_ir::{
        EditFunction, FloatCmpOp, Function, IRBuilderExt, IRViewer, IntBinaryOp, IntCmpOp,
    };
    use strider_ir_test_utils::make_empty_fn;

    use crate::matcher::Pattern;
    use crate::{Bindings, Capture, MatchPat, Matcher, Template, TemplatePat, var};

    /// The one match of `lhs`: its root node, bindings, and root value type.
    #[track_caller]
    fn match_once(fx: &Function, lhs: &Pattern) -> (NodeId, Bindings, ValueType) {
        let hits = Matcher::new(fx).find_all(lhs).unwrap();
        assert_eq!(hits.len(), 1, "the LHS must match exactly once");
        let root = hits[0].root();
        let [root_value] = fx.node_outputs_exact::<1>(root).unwrap();
        let root_ty = fx.value_kind(root_value).as_value().unwrap();
        (root, hits[0].bindings().clone(), root_ty)
    }

    /// Instantiates `rhs` over the one match of `lhs` and splices it in, so
    /// `validate` sees the fresh nodes.
    #[track_caller]
    fn rewrite(fx: &mut Function, lhs: &Pattern, rhs: &Template) -> anyhow::Result<NodeId> {
        let (root, bindings, root_ty) = match_once(fx, lhs);
        let new_value = {
            let mut ef = EditFunction::new(fx);
            super::instantiate(rhs, &mut ef, &bindings, root, &[root], root_ty)?
        };
        let [old_value] = fx.node_outputs_exact::<1>(root).unwrap();
        EditFunction::new(fx).replace_value(old_value, new_value);
        strider_ir::validate::validate(fx).expect("a rewritten function must be valid IR");
        Ok(fx.producer(new_value))
    }

    /// The type of the one constant operand `node` consumes.
    #[track_caller]
    fn const_operand_ty(fx: &Function, node: NodeId) -> ValueType {
        fx.node_inputs(node)
            .into_iter()
            .find(|&v| fx.node_kind(fx.producer(v)).is_const())
            .map(|v| fx.value_type(v).unwrap())
            .expect("the RHS mints one constant operand")
    }

    /// `Equal(5:I64, 1:I64):I1` returned; `int_eq(var(x), int_const(1))` binds
    /// `x` to the `I64` `5`.
    fn cmp_over_i64() -> Function {
        make_empty_fn(|b| {
            let a = b.build_int_const(5u64, T::int(64))?;
            let k = b.build_int_const(1u64, T::int(64))?;
            b.build_int_cmp_operation(a, k, IntCmpOp::Equal, T::int(64))
        })
        .unwrap()
    }

    /// The reported shape: a fresh constant at a comparison's operand 0, whose
    /// width is the operand's and not the `I1` the root carries.
    #[test]
    fn a_fresh_const_at_a_comparison_operand_takes_the_operand_width() {
        let x = Capture::new();
        let mut fx = cmp_over_i64();
        let lhs = crate::int_eq(var(x), crate::int_const(1u128)).into_pattern();
        let rhs = super::int_sborrow(crate::int_const(7u128), var(x)).into_template();

        let node = rewrite(&mut fx, &lhs, &rhs).unwrap();
        assert_eq!(const_operand_ty(&fx, node), T::int(64));
    }

    /// The same at operand 1, where the width comes from the operand the
    /// comparison heads with.
    #[test]
    fn a_fresh_const_at_the_second_comparison_operand_takes_the_first_ones_width() {
        let x = Capture::new();
        let mut fx = cmp_over_i64();
        let lhs = crate::int_eq(var(x), crate::int_const(1u128)).into_pattern();
        let rhs = super::int_lt(var(x), crate::int_const(7u128)).into_template();

        let node = rewrite(&mut fx, &lhs, &rhs).unwrap();
        assert_eq!(const_operand_ty(&fx, node), T::int(64));
    }

    /// A float comparison is the same shape: the fresh `FloatConst` is `F64`,
    /// which is also what makes its bits mask at the right width.
    #[test]
    fn a_fresh_float_const_at_a_comparison_operand_takes_the_operand_width() {
        let one = f64::to_bits(1.0);
        let two = f64::to_bits(2.0);
        let f = Capture::new();
        let mut fx = make_empty_fn(|b| {
            let a = b.build_float_const(one, T::float(64));
            let c = b.build_float_const(two, T::float(64));
            b.build_float_cmp_op(a, c, FloatCmpOp::Equal)
        })
        .unwrap();
        let lhs = crate::float_eq(var(f), crate::float_const(two)).into_pattern();
        let rhs = super::float_lt(var(f), crate::float_const(two)).into_template();

        let node = rewrite(&mut fx, &lhs, &rhs).unwrap();
        assert_eq!(const_operand_ty(&fx, node), T::float(64));
    }

    /// `Extend` pins its input APART from its output, so the root's width is
    /// the one width the operand cannot have. Nothing else offers one.
    #[test]
    fn a_fresh_const_under_an_extend_is_refused() {
        let x = Capture::new();
        let mut fx = cmp_over_i64();
        let lhs = crate::int_eq(var(x), crate::int_const(1u128)).into_pattern();
        // `var(x)` types the comparison, so the extend's operand is the one
        // unanchored node left.
        let rhs =
            super::int_eq(super::int_zero_extend(crate::int_const(7u128)), var(x)).into_template();

        let err = rewrite(&mut fx, &lhs, &rhs).unwrap_err().to_string();
        assert!(err.contains("Extend"), "got: {err}");
        assert!(err.contains("has no width"), "got: {err}");
        assert!(err.contains("capture_typed"), "got: {err}");
    }

    /// Two fresh constants compared against each other anchor nothing, so the
    /// width would be a guess.
    #[test]
    fn a_comparison_of_two_fresh_consts_is_refused() {
        let x = Capture::new();
        let mut fx = cmp_over_i64();
        let lhs = crate::int_eq(var(x), crate::int_const(1u128)).into_pattern();
        let rhs = super::int_eq(crate::int_const(1u128), crate::int_const(2u128)).into_template();

        let err = rewrite(&mut fx, &lhs, &rhs).unwrap_err().to_string();
        assert!(err.contains("has no width"), "got: {err}");
    }

    /// An arithmetic root is evaluated at its own output width, so a fresh
    /// operand there still inherits the rewrite root's type.
    #[test]
    fn a_fresh_const_under_an_arithmetic_root_still_inherits_the_root() {
        let x = Capture::new();
        let mut fx = make_empty_fn(|b| {
            let a = b.build_int_const(5u64, T::int(64))?;
            let k = b.build_int_const(1u64, T::int(64))?;
            b.build_int_binary_operation(a, k, IntBinaryOp::Add, T::int(64))
        })
        .unwrap();
        let lhs = crate::int_add(var(x), crate::int_const(1u128)).into_pattern();
        let rhs = super::int_add(var(x), crate::int_const(2u128)).into_template();

        let node = rewrite(&mut fx, &lhs, &rhs).unwrap();
        assert_eq!(const_operand_ty(&fx, node), T::int(64));
        assert!(matches!(
            fx.node_kind(node),
            NodeKind::IntBinaryOp(IntBinaryOp::Add)
        ));
    }

    /// `Sless(5:I32, 0:I32):I1`; `int_slt(var(a), int_const(0))` binds `a` to
    /// the `I32` `5`.
    fn sign_test_over_i32() -> Function {
        make_empty_fn(|b| {
            let a = b.build_int_const(5u64, T::int(32))?;
            let z = b.build_int_const(0u64, T::int(32))?;
            b.build_int_cmp_operation(a, z, IntCmpOp::Sless, T::int(32))
        })
        .unwrap()
    }

    /// The type and value of the count of the one shift at or below `node`.
    #[track_caller]
    fn shift_count(fx: &Function, node: NodeId) -> (ValueType, u128) {
        let mut stack = vec![node];
        while let Some(n) = stack.pop() {
            if let NodeKind::IntBinaryOp(op) = fx.node_kind(n)
                && op.is_shift()
            {
                let count = fx.node_inputs(n)[1];
                return (
                    fx.value_type(count).unwrap(),
                    fx.int_const_u128(count).unwrap(),
                );
            }
            stack.extend(fx.node_inputs(n).into_iter().map(|v| fx.producer(v)));
        }
        panic!("the RHS builds a shift");
    }

    /// A shift count is evaluated at the shift's width, not at the `I1` the
    /// root carries: masked to `I1`, 31 would test bit 1.
    #[test]
    fn a_fresh_shift_count_under_an_i1_root_keeps_its_value() {
        let a = Capture::new();
        let mut fx = sign_test_over_i32();
        let lhs = crate::int_slt(var(a), crate::int_const(0u128)).into_pattern();
        let rhs =
            super::int_truncate(super::int_shr(var(a), crate::int_const(31u128))).into_template();

        let node = rewrite(&mut fx, &lhs, &rhs).unwrap();
        assert_eq!(shift_count(&fx, node), (T::int(32), 31));
    }

    /// The same count inside a bit test, where the shift is two operands
    /// below the `I1` root.
    #[test]
    fn a_fresh_shift_count_in_a_bit_test_keeps_its_value() {
        let a = Capture::new();
        let mut fx = sign_test_over_i32();
        let lhs = crate::int_slt(var(a), crate::int_const(0u128)).into_pattern();
        let rhs = super::int_eq(
            super::int_and(
                super::int_shr(var(a), crate::int_const(5u128)),
                crate::int_const(1u128),
            ),
            crate::int_const(0u128),
        )
        .into_template();

        let node = rewrite(&mut fx, &lhs, &rhs).unwrap();
        assert_eq!(shift_count(&fx, node), (T::int(32), 5));
    }

    /// `shl` and `sshr` count the same way as `shr`.
    #[test]
    fn a_fresh_shift_count_keeps_its_value_under_every_shift() {
        let a = Capture::new();
        let lhs = crate::int_slt(var(a), crate::int_const(0u128)).into_pattern();
        for rhs in [
            super::int_truncate(super::int_shl(var(a), crate::int_const(31u128))).into_template(),
            super::int_truncate(super::int_sshr(var(a), crate::int_const(31u128))).into_template(),
        ] {
            let mut fx = sign_test_over_i32();
            let node = rewrite(&mut fx, &lhs, &rhs).unwrap();
            assert_eq!(shift_count(&fx, node), (T::int(32), 31));
        }
    }

    /// A count that is itself a shift: the outer shift types the inner one,
    /// whose own count then takes that width.
    #[test]
    fn a_count_built_from_a_shift_takes_the_outer_shifts_width() {
        let a = Capture::new();
        let mut fx = sign_test_over_i32();
        let lhs = crate::int_slt(var(a), crate::int_const(0u128)).into_pattern();
        let rhs = super::int_truncate(super::int_shr(
            var(a),
            super::int_shl(crate::int_const(1u128), crate::int_const(2u128)),
        ))
        .into_template();

        let node = rewrite(&mut fx, &lhs, &rhs).unwrap();
        let shr = fx.producer(fx.node_inputs(node)[0]);
        let shl_out = fx.node_inputs(shr)[1];
        assert_eq!(fx.value_type(shl_out).unwrap(), T::int(32));
        let shl = fx.producer(shl_out);
        assert_eq!(shift_count(&fx, shl), (T::int(32), 2));
    }

    /// A count that does not fit the shift's width saturates, as p-code tests
    /// the whole count: masked, `2^32 + 3` would shift an `I32` by 3.
    #[test]
    fn a_fresh_shift_count_too_wide_for_the_shift_saturates() {
        let a = Capture::new();
        let mut fx = sign_test_over_i32();
        let lhs = crate::int_slt(var(a), crate::int_const(0u128)).into_pattern();
        let rhs = super::int_truncate(super::int_shr(var(a), crate::int_const(0x1_0000_0003u128)))
            .into_template();

        let node = rewrite(&mut fx, &lhs, &rhs).unwrap();
        assert_eq!(shift_count(&fx, node), (T::int(32), 3 | 32));
    }

    /// `capture_typed` over a capture is refused, not a panic: a capture keeps
    /// the type of the value it bound.
    #[test]
    fn capture_typed_over_a_capture_is_refused() {
        let x = Capture::new();
        let mut fx = cmp_over_i64();
        let lhs = crate::int_eq(var(x), crate::int_const(1u128)).into_pattern();
        let rhs =
            super::int_eq(crate::capture_typed(x, var(x)), crate::int_const(1u128)).into_template();

        let err = rewrite(&mut fx, &lhs, &rhs).unwrap_err().to_string();
        assert!(err.contains("targets a capture"), "got: {err}");
    }

    /// An output no template node consumes has nothing to take a width from.
    #[test]
    fn an_untyped_output_no_node_consumes_is_refused() {
        let x = Capture::new();
        let mut fx = make_empty_fn(|b| {
            let a = b.build_int_const(5u64, T::int(64))?;
            let k = b.build_int_const(1u64, T::int(64))?;
            b.build_int_binary_operation(a, k, IntBinaryOp::Add, T::int(64))
        })
        .unwrap();
        let lhs = crate::int_add(var(x), crate::int_const(1u128)).into_pattern();
        let mut b = super::TemplateBuilder::new();
        let l = b.capture(x);
        let r = b.dynamic_leaf(super::TemplateKind::FnIntConst(Box::new(|_| Ok(2))));
        let add = b.node(NodeKind::IntBinaryOp(IntBinaryOp::Add));
        b.input(add, 0, l);
        b.input(add, 1, r);
        let _consumed = b.value_output(add, 0);
        let _unconsumed = b.value_output(add, 0);
        let rhs = b.finish();

        let err = rewrite(&mut fx, &lhs, &rhs).unwrap_err().to_string();
        assert!(err.contains("no template node consumes it"), "got: {err}");
    }

    /// A width taken from a capture bound to memory has no width to take.
    #[test]
    fn a_width_from_a_capture_bound_to_memory_is_refused() {
        let m = Capture::new();
        let mut fx = make_empty_fn(|b| {
            let a = b.build_int_const(0x10u64, T::int(64))?;
            b.build_load(a, rsleigh::VnSpace::RAM, T::int(32))
        })
        .unwrap();
        let lhs = crate::load().mem(var(m)).build();
        let rhs = super::int_add(
            crate::capture_typed(m, crate::int_const(1u128)),
            crate::int_const(2u128),
        )
        .into_template();

        let err = rewrite(&mut fx, &lhs, &rhs).unwrap_err().to_string();
        assert!(err.contains("bound no typed value"), "got: {err}");
    }

    /// A `Popcount` operand owes its output no width, so a fresh constant
    /// there has none to take.
    #[test]
    fn a_fresh_popcount_operand_is_refused() {
        let x = Capture::new();
        let mut fx = cmp_over_i64();
        let lhs = crate::int_eq(var(x), crate::int_const(1u128)).into_pattern();
        let rhs =
            super::int_eq(var(x), super::int_popcount(crate::int_const(0xffu128))).into_template();

        let err = rewrite(&mut fx, &lhs, &rhs).unwrap_err().to_string();
        assert!(err.contains("Popcount input 0"), "got: {err}");
        assert!(err.contains("has no width"), "got: {err}");
        assert!(err.contains("capture_typed"), "got: {err}");
    }

    /// Typed through a capture, the same operand builds at that width.
    #[test]
    fn a_capture_typed_popcount_operand_takes_the_capture_width() {
        let x = Capture::new();
        let mut fx = cmp_over_i64();
        let lhs = crate::int_eq(var(x), crate::int_const(1u128)).into_pattern();
        let rhs = super::int_eq(
            var(x),
            super::int_popcount(crate::capture_typed(x, crate::int_const(0xffu128))),
        )
        .into_template();

        let node = rewrite(&mut fx, &lhs, &rhs).unwrap();
        let popcount = fx.producer(fx.node_inputs(node)[1]);
        let operand = fx.node_inputs(popcount)[0];
        assert_eq!(fx.value_type(operand).unwrap(), T::int(64));
        assert_eq!(fx.int_const_u128(operand), Some(0xff));
    }
}
