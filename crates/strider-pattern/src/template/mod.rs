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
use strider_ir::node::{ExpectedValueKind, NodeId, NodeKind, ValueId, ValueKind, ValueType};
use strider_ir::{Function, IRBuilder, IRViewer, IntBinaryOp};

use crate::bindings::Bindings;
use crate::graph_ext::{consumed_inputs, reachable_topo};
use crate::matcher::OutputKindSpec;

pub type TemplateKindFn = Box<dyn Fn(&TemplateCtx<'_>) -> anyhow::Result<NodeKind> + Send>;

/// The `u128` return caps the expressible range: an output wider than `I128`
/// skips the rewrite rather than interning a truncated constant.
pub type TemplateKindFnIntConst = Box<dyn Fn(&TemplateCtx<'_>) -> anyhow::Result<u128> + Send>;

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
    /// No width of its own: the one the node is evaluated at, which is the
    /// rewrite root's output type unless an operand group pins another.
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
/// [`TemplateTy::InheritRoot`] and whose width no operand group pins.
/// `lhs_root` reaches [`TemplateKind::Fn`] closures as [`TemplateCtx::root`];
/// a pure-`Exact` template ignores it.
///
/// `proof_nodes` is unioned into the asm-fingerprint of every node created
/// here.
///
/// Interior nodes may be multi-output; the returned value is the root node's
/// first value output.
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
/// input slots, has a node no width reaches, or if node creation yields no
/// value output.
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

    for vtx in order {
        let nd = template.graph.node_kind(vtx);

        // A `Capture` leaf has no build kind: it IS the materialisation. Its
        // `ValueCapture` output resolves to the LHS binding, reusing that
        // value verbatim as in `int_add(x, 0) -> x`, and is never synthesised.
        // The capture id lives on the output, not the marker node.
        let kind = match &nd.kind {
            TmplNodeKind::Capture => {
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
                f(&ctx)?
            }
            TmplNodeKind::Build(TemplateKind::FnIntConst(f)) => {
                let value_ty = node_value_ty(template, vtx, &value_tys);
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
            if !output_kind_admissible(want, *got) {
                anyhow::bail!(
                    "template node {kind:?} declares {got:?} at output slot {slot}, \
                     where its node signature expects {want:?}"
                );
            }
        }

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

/// Every value output's type, keyed on the output vertex.
struct ValueTys {
    by_vertex: FxHashMap<TmplValueId, ValueType>,
    root_ty: ValueType,
}

impl ValueTys {
    /// A vertex outside the root's cone never reaches the build loop, so the
    /// root's type stands in for it.
    fn of(&self, vtx: TmplValueId) -> ValueType {
        self.by_vertex.get(&vtx).copied().unwrap_or(self.root_ty)
    }
}

/// How input `slot` of `kind` relates to the widths around it, mirroring the
/// arithmetic-width agreement `strider_ir::validate` enforces.
#[derive(Clone, Copy)]
enum WidthRelation {
    /// Evaluated at the node's own output width.
    OfOutput,
    /// Evaluated at input slot 0's width, the output being the `I1` verdict.
    OfFirstOperand,
    /// A width the node pins apart from its output, so neither the output nor
    /// a sibling operand carries it.
    Foreign,
    /// Tied to no other slot.
    Free,
}

fn input_width_relation(kind: &NodeKind, slot: usize) -> WidthRelation {
    match kind {
        // p-code leaves a shift COUNT any width; only the shifted operand
        // carries the output's.
        NodeKind::IntBinaryOp(
            IntBinaryOp::ShiftLeft | IntBinaryOp::ShiftRight | IntBinaryOp::SShiftRight,
        ) if slot > 0 => WidthRelation::Free,
        NodeKind::IntBinaryOp(_)
        | NodeKind::IntUnaryOp(_)
        | NodeKind::FloatBinaryOp(_)
        | NodeKind::FloatUnaryOp(_) => WidthRelation::OfOutput,
        NodeKind::IntCmpOp(_) | NodeKind::FloatCmpOp(_) => WidthRelation::OfFirstOperand,
        // `Extend` / `Truncate` must strictly widen / narrow, and a bitcast
        // reinterprets the same bits as the other kind of value.
        NodeKind::Extend(_)
        | NodeKind::Truncate
        | NodeKind::IntBitsToFloat
        | NodeKind::FloatBitsToInt => WidthRelation::Foreign,
        _ => WidthRelation::Free,
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
/// replaces the matched value, so its group takes `root_ty`.
///
/// # Errors
///
/// If a group a node's width is tied to carries no width at all: every member
/// is built fresh with no declared type, so any width picked here is arbitrary
/// and the pick shows up as IR `strider_ir::validate` rejects.
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

    for &node_vtx in order {
        let Some(kind) = exact_kind(template, node_vtx) else {
            continue;
        };
        let inputs = consumed_inputs(&template.graph, node_vtx);
        let first_operand = inputs.iter().find(|&&(slot, _)| slot == 0).map(|&(_, v)| v);
        for &(slot, producer) in &inputs {
            let head = match input_width_relation(&kind, slot) {
                WidthRelation::OfOutput => first_value_output_vtx(template, node_vtx),
                WidthRelation::OfFirstOperand => first_operand,
                WidthRelation::Foreign => {
                    tied_by.entry(producer).or_insert((kind, slot));
                    continue;
                }
                WidthRelation::Free => continue,
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
                TemplateTy::InheritBinding(cap) => {
                    binding_tys.get(&cap).copied().unwrap_or(root_ty)
                }
                TemplateTy::InheritRoot => match group_ty.get(&groups.find(vtx)).copied() {
                    Some(t) => t,
                    None => match tied_by.get(&vtx) {
                        Some((kind, slot)) => anyhow::bail!(
                            "template node feeding {kind:?} input {slot} has no width: it is \
                             built fresh with no declared type, and nothing it is evaluated \
                             against carries one either. Type it, or type an operand it is \
                             evaluated against"
                        ),
                        None => root_ty,
                    },
                },
            };
            by_vertex.insert(vtx, ty);
        }
    }
    Ok(ValueTys { by_vertex, root_ty })
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

/// Whether `got` is admissible where the node signature expects `want`.
///
/// `expected_output_kind` is `strider-ir`'s single source of truth; without
/// this the author's declaration stands unchecked, so a `Store` built with no
/// explicit output vertex silently takes a value output where `[Memory]` is
/// required and the malformed node reaches the graph.
fn output_kind_admissible(want: ExpectedValueKind, got: ValueKind) -> bool {
    match (want, got) {
        (ExpectedValueKind::Control, ValueKind::Control)
        | (ExpectedValueKind::Memory, ValueKind::Memory)
        | (ExpectedValueKind::PhiToken, ValueKind::PhiToken)
        | (ExpectedValueKind::AnyValue, ValueKind::Typed(_)) => true,
        (ExpectedValueKind::Bool, ValueKind::Typed(t)) => t == ValueType::I1,
        (ExpectedValueKind::AnyInt, ValueKind::Typed(t)) => t.is_integer(),
        (ExpectedValueKind::AnyFloat, ValueKind::Typed(t)) => t.is_float(),
        _ => false,
    }
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
        (root, hits[0].bindings_clone(), root_ty)
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
        EditFunction::new(fx).replace_all_uses(old_value, new_value)?;
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
            let a = b.build_int_const(5u64, T::I64)?;
            let k = b.build_int_const(1u64, T::I64)?;
            b.build_int_cmp_operation(a, k, IntCmpOp::Equal, T::I64)
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
        assert_eq!(const_operand_ty(&fx, node), T::I64);
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
        assert_eq!(const_operand_ty(&fx, node), T::I64);
    }

    /// A float comparison is the same shape: the fresh `FloatConst` is `F64`,
    /// which is also what makes its bits mask at the right width.
    #[test]
    fn a_fresh_float_const_at_a_comparison_operand_takes_the_operand_width() {
        let one = f64::to_bits(1.0);
        let two = f64::to_bits(2.0);
        let f = Capture::new();
        let mut fx = make_empty_fn(|b| {
            let a = b.build_float_const(one, T::F64);
            let c = b.build_float_const(two, T::F64);
            b.build_float_cmp_op(a, c, FloatCmpOp::Equal)
        })
        .unwrap();
        let lhs = crate::float_eq(var(f), crate::float_const(two)).into_pattern();
        let rhs = super::float_lt(var(f), crate::float_const(two)).into_template();

        let node = rewrite(&mut fx, &lhs, &rhs).unwrap();
        assert_eq!(const_operand_ty(&fx, node), T::F64);
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
            let a = b.build_int_const(5u64, T::I64)?;
            let k = b.build_int_const(1u64, T::I64)?;
            b.build_int_binary_operation(a, k, IntBinaryOp::Add, T::I64)
        })
        .unwrap();
        let lhs = crate::int_add(var(x), crate::int_const(1u128)).into_pattern();
        let rhs = super::int_add(var(x), crate::int_const(2u128)).into_template();

        let node = rewrite(&mut fx, &lhs, &rhs).unwrap();
        assert_eq!(const_operand_ty(&fx, node), T::I64);
        assert!(matches!(
            fx.node_kind(node),
            NodeKind::IntBinaryOp(IntBinaryOp::Add)
        ));
    }
}
