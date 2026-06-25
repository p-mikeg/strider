//! Recursive bipartite match engine with commutative-operand retry and
//! cast walk-through.
//!
//! The matcher visits the pattern graph in pull order rooted at
//! [`Pattern::root`](crate::matcher::Pattern): for each pat node it
//! kind-checks the corresponding IR node, runs the node's predicate,
//! then walks each input. An input is a `Consumes{slot}` edge whose
//! source is a [`PatValue`] vertex; that output vertex's incoming
//! `Produces` edge source is the producer [`PatNode`]. Matching one
//! input therefore checks the producer's IR output against the
//! [`PatValue`]'s declarative constraints (kind + width +
//! `value_predicate`) and recurses into the producer pat node.
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
//! [`PatValue`]: crate::matcher::PatValue
//! [`PatNode`]: crate::matcher::PatNode

use strider_graph::{NodeId as PatNodeId, ValueId as PatValueId};
use strider_ir::{
    IRViewer,
    node::{NodeId, ValueId, ValueKind, ValueType},
};

use crate::{
    bindings::{Binding, Bindings},
    graph_ext::PatGraphRead,
    matcher::{Matcher, OutputKindSpec, PatValue, Pattern, skip_casts},
};

/// Entry point for a value-rooted attempt: try `pat`'s root pat node
/// against the IR node producing `root_value`, with `root_value` available
/// for the root output's declarative constraints and the root capture.
pub(crate) fn try_match(
    matcher: &Matcher,
    pat: &Pattern,
    root: PatNodeId,
    root_value: ValueId,
    bindings: &mut Bindings,
) -> bool {
    // The root output vertex (if the root pat node declares one) carries
    // the root-level output constraints. For a value root — exactly one
    // value output vertex — that vertex's constraint applies to whichever
    // output is currently being matched (`root_value`), regardless of slot.
    let root_out_vertex = root_output_vertex_for(pat, root, matcher, root_value);
    let root_node = matcher.function().producer(root_value);
    try_match_at(
        matcher,
        pat,
        root,
        root_node,
        Some(root_value),
        root_out_vertex,
        bindings,
    )
}

/// Entry point for a zero-value-output attempt (e.g. `Return`): try
/// `pat`'s root pat node against `node` with no associated output.
///
/// A zero-output IR node produces no value, so it can only satisfy a root
/// whose output vertex imposes no value requirement (a bare `any()` /
/// `var()` wildcard, or a control/zero-output control builder). A root
/// that requires a value output — a pinned `Value` kind or any `width` —
/// is rejected here rather than silently skipping the constraint (which
/// is how `bool_value()` used to wrongly match `Return`).
pub(crate) fn try_match_node(
    matcher: &Matcher,
    pat: &Pattern,
    root: PatNodeId,
    node: NodeId,
    bindings: &mut Bindings,
) -> bool {
    if root_requires_value_output(pat, root) {
        return false;
    }
    try_match_at(matcher, pat, root, node, None, None, bindings)
}

/// Whether the root pat node declares an output vertex that demands a
/// value output (a `Value` / `AnyValue` kind, or any `width` constraint).
/// Such a root cannot match a zero-output IR node.
fn root_requires_value_output(pat: &Pattern, root: PatNodeId) -> bool {
    pat.graph.produced_outputs(root).into_iter().any(|ov| {
        let o = pat.graph.output_weight(ov);
        o.width.is_some() || matches!(o.kind, OutputKindSpec::Value(_) | OutputKindSpec::AnyValue)
    })
}

/// Resolve the root pat node's output vertex carrying the root-level
/// output constraints to check against the IR `root_value`.
///
/// A value root declares exactly one output vertex (the value / memory /
/// wildcard it produces). Its `kind` / `width` constraint applies to
/// *whichever* output is being matched — [`Matcher::find_all`] iterates
/// every IR output of a node and roots an attempt at each — so it is
/// checked against `root_value` directly, with no slot matching. (Matching
/// by slot would silently skip the constraint whenever a multi-output
/// node such as `Region` / `Call` is rooted at a non-slot-0 output: that
/// is the bug this resolves.)
///
/// The only multi-output-vertex root is the `If` control builder (two
/// `Control` vertices); for it the per-slot lookup is kept, anchoring the
/// branch node-limit on the slot-0 control output. Returns `None` when
/// the root pat node declares no output vertex (no constraint).
///
/// [`Matcher::find_all`]: crate::Matcher::find_all
fn root_output_vertex_for(
    pat: &Pattern,
    root: PatNodeId,
    matcher: &Matcher,
    root_value: ValueId,
) -> Option<PatValueId> {
    // Single output vertex: its constraint applies to the matched output
    // regardless of slot.
    let outs = pat.graph.produced_outputs(root);
    let mut iter = outs.iter().copied();
    let first = iter.next()?;
    if iter.next().is_none() {
        return Some(first);
    }

    // Multiple output vertices (the `If` control root): keep the per-slot
    // lookup so each control output's constraints land on the right slot.
    let (_node, ir_slot) = matcher.function().value_definition(root_value);
    outs.into_iter()
        .find(|&out_vertex| pat.graph.output_weight(out_vertex).slot as u32 == ir_slot)
}

/// Recursive worker. `pat_node` is the current pattern node index;
/// `ir_node` is the IR node being matched; `root_value` / `out_vertex` are
/// the IR output and its pat-output vertex when this pat node sits at a
/// value-producing position (used for the output constraints + capture).
#[allow(clippy::too_many_arguments)]
fn try_match_at(
    matcher: &Matcher,
    pat: &Pattern,
    pat_node: PatNodeId,
    ir_node: NodeId,
    root_value: Option<ValueId>,
    out_vertex: Option<PatValueId>,
    bindings: &mut Bindings,
) -> bool {
    let nd = pat.graph.node_weight(pat_node);
    if !nd.kind.matches(matcher.function().node_kind(ir_node)) {
        return false;
    }

    // Root-output constraints (kind / width). The output vertex carries the
    // declarative shape constraints (e.g. `bool_*` builders pin `Value(I1)`;
    // `value_of_width` pins width).
    if let Some(ov_idx) = out_vertex
        && let Some(value) = root_value
    {
        let ov = pat.graph.output_weight(ov_idx);
        if !output_ok(ov, matcher.function(), value) {
            return false;
        }
    }

    // Node predicate. Fires after kind + output constraints and BEFORE
    // descending into inputs — node-only predicates short-circuit here.
    if let Some(predicate) = &nd.node_predicate
        && !predicate(matcher, ir_node)
    {
        return false;
    }

    // Collect this pat node's inputs: each incoming `Consumes{slot}` edge
    // source is a PatValue vertex; that vertex's incoming `Produces`
    // edge source is the producer pat node.
    let inputs: Vec<InputEdge> = pat
        .graph
        .consumed_inputs(pat_node)
        .into_iter()
        .map(|(slot, out_vertex)| {
            let producer = pat.graph.producer_of(out_vertex);
            InputEdge {
                consumer_slot: slot,
                out_vertex,
                producer,
            }
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
            let Ok(use_id) = matcher
                .function()
                .graph()
                .node_input_id_at(ir_node, ir_slot)
            else {
                return false;
            };
            let producer_value = matcher.function().graph().value_of_use(use_id);
            let sub_mark = b.mark();
            if match_subpattern(matcher, pat, edge, producer_value, b) {
                continue;
            }
            // Cast walk-through fallback.
            b.restore(sub_mark);
            if cast_mask.is_empty() {
                return false;
            }
            let mut skipped = Vec::new();
            let unwrapped = skip_casts(matcher, producer_value, cast_mask, &mut skipped);
            if unwrapped == producer_value {
                // Producer wasn't a registered cast — no further fallback.
                return false;
            }
            // Record the skipped casts into the footprint BEFORE the
            // sub-match: they are journaled like every other footprint
            // entry, so a subsequent failure here is rolled back by the
            // caller's `restore(mark)`. On success they stay, keeping the
            // asm-fingerprint superset contract intact.
            for &cast in &skipped {
                b.record_matched(cast);
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
    //
    // Choose the binding KIND by where the capture was DECLARED, not by
    // whether the matched node happens to have a value output.  An
    // output-vertex capture (`capture_output`, used for value positions) binds
    // the matched VALUE; a node-declared capture (`capture_node`, used for
    // control nodes like `If` via `if_node().capture(c)`) binds the matched
    // NODE — even though `If` carries Control value outputs that would make
    // `root_value` `Some`.  Picking the kind from `root_value` alone used to
    // mis-bind a node capture on `If` as `Binding::Value(control_value)`,
    // contradicting the `Binding::Node` contract.
    let ov_capture = out_vertex
        .map(|ov| pat.graph.output_weight(ov))
        .and_then(|ov| ov.capture);
    let cap_binding = match ov_capture {
        Some(cap) => root_value.map(|value| (cap, Binding::Value(value))),
        None => nd.capture.map(|cap| (cap, Binding::Node(ir_node))),
    };
    if let Some((cap, binding)) = cap_binding
        && !bindings.bind_capture(cap, binding)
    {
        bindings.restore(mark);
        return false;
    }

    // Post-match hook: runs after all inputs are already resolved.
    // Returning `false` here unwinds the entire match attempt (restores
    // bindings and fails); it does not re-drive the swapped-operand order.
    if let Some(pm) = &nd.post_match {
        let ty = root_value
            .and_then(|value| matcher.function().value_kind(value).as_value())
            .unwrap_or(ValueType::I1);
        if !pm(matcher, ir_node, ty, bindings) {
            bindings.restore(mark);
            return false;
        }
    }
    // This pat node fully matched `ir_node` — commit it to the match
    // footprint.  Recorded only here, after every failure path above has
    // returned, so a partially-matched-then-failed attempt records nothing
    // (and any footprint entries from its sub-matches were rolled back by the
    // `restore(mark)` failure paths).
    bindings.record_matched(ir_node);
    true
}

/// One incoming input of a consumer pat node.
struct InputEdge {
    consumer_slot: usize,
    out_vertex: PatValueId,
    producer: PatNodeId,
}

/// Attempt the sub-pattern feeding one input against the IR producer
/// output `producer_value`: check the pat output's constraints, then
/// recurse into the producer pat node at the IR node producing
/// `producer_value`.
fn match_subpattern(
    matcher: &Matcher,
    pat: &Pattern,
    edge: &InputEdge,
    producer_value: ValueId,
    bindings: &mut Bindings,
) -> bool {
    let producer_ir = matcher.function().producer(producer_value);
    try_match_at(
        matcher,
        pat,
        edge.producer,
        producer_ir,
        Some(producer_value),
        Some(edge.out_vertex),
        bindings,
    )
}

/// Whether the IR output `value` satisfies the pat output's declarative
/// kind + width constraints.
fn output_ok(o: &PatValue, f: &strider_ir::Function, value: ValueId) -> bool {
    let val = f.value_kind(value).as_value();
    let kind_ok = match &o.kind {
        // Unconstrained wildcard: any output kind matches. A `width`
        // constraint (checked below) can still narrow it to a value.
        OutputKindSpec::Any => true,
        OutputKindSpec::Value(ty) => val == Some(*ty),
        OutputKindSpec::AnyValue => val.is_some(),
        OutputKindSpec::Control => matches!(f.value_kind(value), ValueKind::Control),
        OutputKindSpec::Memory => matches!(f.value_kind(value), ValueKind::Memory),
        OutputKindSpec::PhiToken => matches!(f.value_kind(value), ValueKind::PhiToken),
    };
    kind_ok
        && o.width
            .is_none_or(|w| val.is_some_and(|t| t.bit_width() == w as usize))
}
