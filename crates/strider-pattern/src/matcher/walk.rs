//! Recursive bipartite match engine with commutative-operand retry and
//! cast walk-through.
//!
//! The matcher visits the pattern graph in pull order rooted at
//! [`Pattern::root`](crate::pattern::Pattern): for each pat node it
//! kind-checks the corresponding IR node, runs the node's local limit,
//! then walks each input. An input is a `Consumes{slot}` edge whose
//! source is a [`PatOutput`] vertex; that output vertex's incoming
//! `Produces` edge source is the producer [`PatNode`]. Matching one
//! input therefore checks the producer's IR output against the
//! [`PatOutput`]'s declarative constraints (kind + width +
//! `output_limit`) and recurses into the producer pat node.
//!
//! For arity-2 pat nodes whose IR kind is commutative (per
//! `NodeKind::is_commutative()`) the matcher tries the natural operand
//! order first; on failure it rolls back via [`Bindings::mark`] /
//! [`Bindings::restore`] and retries with the operand slots swapped (a
//! node may opt out via `force_ordered`).
//!
//! On a sub-pattern mismatch at a producer output, if the pattern's
//! [`CastMask`](crate::matcher::CastMask) is non-empty the matcher
//! transparently unwraps any cast in the mask and re-attempts the
//! sub-pattern against the cast's value input.
//!
//! [`PatOutput`]: crate::pattern::PatOutput
//! [`PatNode`]: crate::pattern::PatNode

use petgraph::stable_graph::NodeIndex;
use petgraph::visit::EdgeRef;
use strider_ir::node::{NodeId, NodeOutputId, NodeOutputKind, NodeOutputType};

use crate::bindings::{Binding, Bindings};
use crate::matcher::{Matcher, skip_casts};
use crate::pattern::{OutputKindSpec, PatEdge, PatOutput, PatVertex, Pattern};

/// Entry point for a value-rooted attempt: try `pat`'s root pat node
/// against the IR node producing `root_out`, with `root_out` available
/// for the root output's declarative constraints and the root capture.
pub(crate) fn try_match(
    matcher: &Matcher,
    pat: &Pattern,
    root_out: NodeOutputId,
    bindings: &mut Bindings,
) -> bool {
    let Some(root) = pat.root else {
        return false;
    };
    // The root output vertex (if the root pat node declares one) carries
    // the root-level output constraints. Find it by walking the root pat
    // node's outgoing `Produces` edges for the matching slot. Most value
    // roots have exactly one value output at slot 0.
    let root_out_vertex = root_output_vertex_for(pat, root, matcher, root_out);
    let root_node = matcher.function().node_for_output(root_out);
    try_match_at(
        matcher,
        pat,
        root,
        root_node,
        Some(root_out),
        root_out_vertex,
        bindings,
    )
}

/// Entry point for a zero-value-output attempt (e.g. `Return`): try
/// `pat`'s root pat node against `node` with no associated output.
pub(crate) fn try_match_node(
    matcher: &Matcher,
    pat: &Pattern,
    node: NodeId,
    bindings: &mut Bindings,
) -> bool {
    let Some(root) = pat.root else {
        return false;
    };
    try_match_at(matcher, pat, root, node, None, None, bindings)
}

/// Resolve the root pat node's output vertex whose slot matches the IR
/// `root_out`'s slot (so the root output's declarative constraints are
/// checked against the right IR output). Returns `None` when the root
/// pat node declares no matching output vertex.
fn root_output_vertex_for(
    pat: &Pattern,
    root: NodeIndex,
    matcher: &Matcher,
    root_out: NodeOutputId,
) -> Option<NodeIndex> {
    let (_node, ir_slot) = matcher.function().output_definition(root_out);
    pat.inner
        .edges_directed(root, petgraph::Outgoing)
        .filter(|e| matches!(e.weight(), PatEdge::Produces))
        .map(|e| e.target())
        .find(|&out_vertex| match pat.inner.node_weight(out_vertex) {
            Some(PatVertex::Output(o)) => o.slot as u32 == ir_slot,
            _ => false,
        })
}

/// Recursive worker. `pat_node` is the current pattern node index;
/// `ir_node` is the IR node being matched; `root_out` / `out_vertex` are
/// the IR output and its pat-output vertex when this pat node sits at a
/// value-producing position (used for the output constraints + capture).
#[allow(clippy::too_many_arguments)]
fn try_match_at(
    matcher: &Matcher,
    pat: &Pattern,
    pat_node: NodeIndex,
    ir_node: NodeId,
    root_out: Option<NodeOutputId>,
    out_vertex: Option<NodeIndex>,
    bindings: &mut Bindings,
) -> bool {
    // `pat_node` is read from `pat.inner`'s own index space; a missing or
    // non-Node weight would mean the pattern is malformed — treat as a
    // non-match defensively rather than panicking.
    let nd = match pat.inner.node_weight(pat_node) {
        Some(PatVertex::Node(n)) => n,
        _ => return false,
    };
    if !nd.kind.matches(matcher.function().node_kind(ir_node)) {
        return false;
    }

    // Root-output constraints (kind / width) + output_limit. The output
    // vertex carries the declarative shape constraints (e.g. `bool_*`
    // builders pin `Value(Some(I1))`; `value_of_width` pins width).
    if let Some(ov_idx) = out_vertex
        && let (Some(PatVertex::Output(ov)), Some(out)) =
            (pat.inner.node_weight(ov_idx), root_out)
    {
        if !output_ok(ov, matcher.function(), out) {
            return false;
        }
        if let Some(lim) = &ov.output_limit {
            let ty = matcher
                .function()
                .output_kind(out)
                .as_value()
                .unwrap_or(NodeOutputType::I1);
            if !lim(matcher, ir_node, ty) {
                return false;
            }
        }
    }

    // Node-local limit. Fires after kind + output constraints and BEFORE
    // descending into inputs — node-only predicates short-circuit here.
    // Zero-output kinds fall back to `I1` as a placeholder type.
    if let Some(limit) = &nd.node_limit {
        let ty = root_out
            .and_then(|out| matcher.function().output_kind(out).as_value())
            .unwrap_or(NodeOutputType::I1);
        if !limit(matcher, ir_node, ty) {
            return false;
        }
    }

    // Collect this pat node's inputs: each incoming `Consumes{slot}` edge
    // source is a PatOutput vertex; that vertex's incoming `Produces`
    // edge source is the producer pat node.
    let inputs: Vec<InputEdge> = pat
        .inner
        .edges_directed(pat_node, petgraph::Incoming)
        .filter_map(|e| match e.weight() {
            PatEdge::Consumes { slot } => {
                let out_vertex = e.source();
                let producer = producer_of(pat, out_vertex)?;
                Some(InputEdge {
                    consumer_slot: *slot,
                    out_vertex,
                    producer,
                })
            }
            PatEdge::Produces => None,
        })
        .collect();

    // Commutativity: arity-2, IR kind commutative, not force_ordered.
    let commutative = !nd.force_ordered
        && inputs.len() == 2
        && matcher.function().node_kind(ir_node).is_commutative();

    let mark = bindings.mark();
    let cast_mask = pat.cast_mask;

    let attempt = |swap: bool, b: &mut Bindings| -> bool {
        for edge in &inputs {
            let ir_slot = if swap {
                match edge.consumer_slot {
                    0 => 1,
                    1 => 0,
                    other => other,
                }
            } else {
                edge.consumer_slot
            };
            let Ok(input_id) = matcher.function().node_input_id_at(ir_node, ir_slot) else {
                return false;
            };
            let producer_out = matcher.function().input_output_id(input_id);
            let sub_mark = b.mark();
            if match_subpattern(matcher, pat, edge, producer_out, b) {
                continue;
            }
            // Cast walk-through fallback.
            b.restore(sub_mark);
            if cast_mask.is_empty() {
                return false;
            }
            let unwrapped = skip_casts(matcher, producer_out, cast_mask);
            if unwrapped == producer_out {
                // Producer wasn't a registered cast — no further fallback.
                return false;
            }
            if !match_subpattern(matcher, pat, edge, unwrapped, b) {
                return false;
            }
        }
        true
    };

    let inputs_ok = if attempt(false, bindings) {
        true
    } else if commutative {
        bindings.restore(mark);
        attempt(true, bindings)
    } else {
        false
    };
    if !inputs_ok {
        bindings.restore(mark);
        return false;
    }

    // Capture: bound after children matched, so shared captures bound
    // deeper are already recorded — `bind_capture` rejects a re-bind to a
    // different binding here, enforcing capture-equality.
    if let Some(cap) = nd.capture {
        let binding = root_out.map_or(Binding::Node(ir_node), Binding::Output);
        if !bindings.bind_capture(cap, binding) {
            bindings.restore(mark);
            return false;
        }
    }

    // Post-match hook: runs after all inputs are already resolved.
    // Returning `false` here unwinds the entire match attempt (restores
    // bindings and fails); it does not re-drive the swapped-operand order.
    if let Some(pm) = &nd.post_match {
        let ty = root_out
            .and_then(|out| matcher.function().output_kind(out).as_value())
            .unwrap_or(NodeOutputType::I1);
        if !pm(matcher, ir_node, ty, bindings) {
            bindings.restore(mark);
            return false;
        }
    }
    true
}

/// One incoming input of a consumer pat node.
struct InputEdge {
    consumer_slot: usize,
    out_vertex: NodeIndex,
    producer: NodeIndex,
}

/// Attempt the sub-pattern feeding one input against the IR producer
/// output `producer_out`: check the pat output's constraints, then
/// recurse into the producer pat node at the IR node producing
/// `producer_out`.
fn match_subpattern(
    matcher: &Matcher,
    pat: &Pattern,
    edge: &InputEdge,
    producer_out: NodeOutputId,
    bindings: &mut Bindings,
) -> bool {
    let producer_ir = matcher.function().node_for_output(producer_out);
    try_match_at(
        matcher,
        pat,
        edge.producer,
        producer_ir,
        Some(producer_out),
        Some(edge.out_vertex),
        bindings,
    )
}

/// The producer pat node of an output vertex (source of its incoming
/// `Produces` edge).
fn producer_of(pat: &Pattern, out_vertex: NodeIndex) -> Option<NodeIndex> {
    pat.inner
        .edges_directed(out_vertex, petgraph::Incoming)
        .find(|e| matches!(e.weight(), PatEdge::Produces))
        .map(|e| e.source())
}

/// Whether the IR output `out` satisfies the pat output's declarative
/// kind + width constraints.
fn output_ok(o: &PatOutput, f: &strider_ir::Function, out: NodeOutputId) -> bool {
    let val = f.output_kind(out).as_value();
    let kind_ok = match &o.kind {
        OutputKindSpec::Value(Some(ty)) => val == Some(*ty),
        OutputKindSpec::AnyValue | OutputKindSpec::Value(None) => val.is_some(),
        OutputKindSpec::Control => matches!(f.output_kind(out), NodeOutputKind::Control),
        OutputKindSpec::Memory => matches!(f.output_kind(out), NodeOutputKind::Memory),
        OutputKindSpec::PhiToken => matches!(f.output_kind(out), NodeOutputKind::PhiToken),
    };
    kind_ok
        && o.width
            .is_none_or(|w| val.is_some_and(|t| t.bit_width() == w as usize))
}
