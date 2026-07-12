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
//! order first, then the swapped order (a node may opt out via
//! `force_ordered`), rolling back via [`Bindings::mark`] /
//! [`Bindings::restore`] between attempts.
//!
//! The engine is continuation-passing: [`try_match_at`] ENUMERATES a pat
//! node's matches (each operand ordering × every descendant configuration)
//! and calls a continuation `k` per configuration instead of returning the
//! first hit. So when a guard ANYWHERE above rejects a configuration (its
//! `k` returns `false`), a commutative node BELOW it re-drives its operand
//! order to find one the ancestor accepts — e.g. a `when_match` guard on a
//! unary parent picks which operand of a commutative child binds. The root
//! continuation is `|_| true`, so the first fully guard-satisfying
//! configuration in DFS order wins.
//!
//! On a sub-pattern mismatch at a producer output, if the pattern's
//! [`CastMask`](crate::matcher::CastMask) is non-empty the matcher
//! transparently unwraps any cast in the mask and re-attempts the
//! sub-pattern against the cast's value input.
//!
//! [`PatValue`]: crate::matcher::PatValue
//! [`PatNode`]: crate::matcher::PatNode

use strider_graph::{NodeId as PatNodeId, ValueId as PatValueId};
use strider_ir::IRViewer;
use strider_ir::node::{NodeId, ValueId, ValueKind, ValueType};

use crate::bindings::{Binding, Bindings};
use crate::graph_ext::PatGraphRead;
use crate::matcher::{Matcher, OutputKindSpec, PatValue, Pattern, skip_casts};

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
    // The root accepts the first full solution whose every guard (root +
    // descendants) passed — `try_match_at` enumerates configurations and the
    // `|_| true` continuation stops at the first that reaches the root.
    try_match_at(
        matcher,
        pat,
        root,
        root_node,
        Some(root_value),
        root_out_vertex,
        bindings,
        &mut |_| true,
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
    try_match_at(matcher, pat, root, node, None, None, bindings, &mut |_| {
        true
    })
}

/// Whether the root pat node declares an output vertex that demands a
/// value output (a `Value` / `AnyValue` kind, or any `width` constraint).
/// Such a root cannot match a zero-output IR node.
fn root_requires_value_output(pat: &Pattern, root: PatNodeId) -> bool {
    pat.graph.produced_outputs(root).iter().any(|&ov| {
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
    outs.iter()
        .copied()
        .find(|&out_vertex| pat.graph.output_weight(out_vertex).slot as u32 == ir_slot)
}

/// Recursive worker. `pat_node` is the current pattern node index;
/// `ir_node` is the IR node being matched; `root_value` / `out_vertex` are
/// the IR output and its pat-output vertex when this pat node sits at a
/// value-producing position (used for the output constraints + capture).
/// Recursive worker, continuation-passing so a guard failure ANYWHERE above can
/// re-drive a commutative operand order BELOW it.
///
/// Rather than return the first sub-match, `try_match_at` ENUMERATES every way
/// `pat_node` matches `ir_node` (each operand ordering × every descendant
/// configuration); for each it extends `bindings` with that configuration's
/// captures + footprint and calls `k`. When `k` returns `true` the whole match
/// is accepted and `bindings` keeps the accepted state; when `k` returns
/// `false` (an ancestor guard rejected this configuration) the next one is
/// tried. With no configuration accepted, `bindings` is restored to entry and
/// `false` is returned. The root passes `k = |_| true`, so the FIRST fully
/// guard-satisfying configuration in DFS order wins.
#[allow(clippy::too_many_arguments)]
fn try_match_at(
    matcher: &Matcher,
    pat: &Pattern,
    pat_node: PatNodeId,
    ir_node: NodeId,
    root_value: Option<ValueId>,
    out_vertex: Option<PatValueId>,
    bindings: &mut Bindings,
    k: &mut dyn FnMut(&mut Bindings) -> bool,
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

    // Capture to bind for THIS node, independent of operand ordering: an
    // output-vertex capture (value positions) binds the matched VALUE; a
    // node-declared capture (control nodes like `If`) binds the matched NODE.
    // Picking the kind from `root_value` alone used to mis-bind a node capture
    // on `If` as `Binding::Value`.
    // Both may be present and are independent captures: e.g. an `If` with
    // `capture_true(t)` (an output-vertex value capture) AND `capture(g)` (the
    // node capture). Binding only one — the old either/or — silently dropped the
    // other. `[anchor_output_value_capture, node_capture]`.
    let cap_bindings = [
        out_vertex
            .and_then(|ov| pat.graph.output_weight(ov).capture)
            .and_then(|cap| root_value.map(|value| (cap, Binding::Value(value)))),
        nd.capture.map(|cap| (cap, Binding::Node(ir_node))),
    ];

    // Alternation (`one_of`): `inputs` are independent alternative sub-patterns,
    // not operands. Bind this node's own capture (the matched value) once, then
    // try each alternative against the SAME `ir_node`, accepting the first whose
    // whole configuration (its own guards + the ancestor continuation `k`)
    // succeeds. Each alternative's `try_match_at` handles its captures / guard /
    // footprint, so the alternation node records nothing itself.
    if nd.alternation {
        let mark = bindings.mark();
        for &(cap, binding) in cap_bindings.iter().flatten() {
            if !bindings.bind_capture(cap, binding) {
                bindings.restore(mark);
                return false;
            }
        }
        for alt in &inputs {
            let inner = bindings.mark();
            if try_match_at(
                matcher,
                pat,
                alt.producer,
                ir_node,
                root_value,
                Some(alt.out_vertex),
                bindings,
                k,
            ) {
                return true;
            }
            bindings.restore(inner);
        }
        bindings.restore(mark);
        return false;
    }

    // Partition inputs into fixed-slot operands and existential (`any_input`)
    // sub-patterns. A fixed input matches its own IR slot; an existential input
    // matches SOME value input of `ir_node` (see `match_existential`).
    let (existential, fixed): (Vec<InputEdge>, Vec<InputEdge>) = inputs
        .into_iter()
        .partition(|e| e.consumer_slot == crate::matcher::ANY_INPUT_SLOT);

    // Commutativity: exactly two fixed operands, IR kind commutative, not
    // force_ordered. (Existential inputs never participate in reordering.)
    let commutative = !nd.force_ordered
        && fixed.len() == 2
        && matcher.function().node_kind(ir_node).is_commutative();

    // `ir_node`'s inputs are invariant across the whole existential search, so
    // collect them once here (empty in the common no-`any_input` case) rather
    // than re-collecting at every `match_existential` recursion level.
    let ext_values: Vec<ValueId> = if existential.is_empty() {
        Vec::new()
    } else {
        matcher.function().node_inputs(ir_node).into_iter().collect()
    };

    let mark = bindings.mark();
    let orders: &[bool] = if commutative {
        &[false, true]
    } else {
        &[false]
    };

    for &swap in orders {
        // `finalize` finishes THIS node once every input (fixed + existential)
        // has matched: bind the node's capture (capture-equality is enforced
        // here against deeper bindings), run its guard, record it into the
        // footprint, then hand off to the parent continuation `k`. Returning
        // `false` makes the input enumeration try its next configuration (a
        // deeper commutative swap or existential slot), and exhausting those
        // makes the ordering loop try the swap — so an ANCESTOR guard failure
        // surfaced through `k` re-drives this node and everything beneath it.
        let mut finalize = |b: &mut Bindings| -> bool {
            let inner = b.mark();
            for &(cap, binding) in cap_bindings.iter().flatten() {
                if !b.bind_capture(cap, binding) {
                    b.restore(inner);
                    return false;
                }
            }
            // Bind captures on this node's SECONDARY (non-anchor) output
            // vertices to the matched IR node's output value at each vertex's
            // slot — how `If::capture_true` / `capture_false` bind their
            // control-output values (the matcher never descends into a node's
            // outputs otherwise). The anchor output's capture (and the node
            // capture) are handled by the `cap_bindings` loop above.
            for &ov_idx in pat.graph.produced_outputs(pat_node).iter() {
                if Some(ov_idx) == out_vertex {
                    continue;
                }
                let ov = pat.graph.output_weight(ov_idx);
                if let Some(cap) = ov.capture {
                    let Some(&val) = matcher.function().node_outputs(ir_node).get(ov.slot) else {
                        b.restore(inner);
                        return false;
                    };
                    if !b.bind_capture(cap, Binding::Value(val)) {
                        b.restore(inner);
                        return false;
                    }
                }
            }
            if let Some(pm) = &nd.post_match {
                let ty = root_value
                    .and_then(|value| matcher.function().value_kind(value).as_value())
                    .unwrap_or(ValueType::I1);
                if !pm(matcher, ir_node, ty, b) {
                    b.restore(inner);
                    return false;
                }
            }
            b.record_matched(ir_node);
            if k(b) {
                return true;
            }
            b.restore(inner);
            false
        };
        // After the fixed slots match, satisfy each existential input against
        // some value input of `ir_node` — each against a DISTINCT slot — then
        // finalize.
        let mut done = |b: &mut Bindings| -> bool {
            match_existential(matcher, pat, &existential, 0, &ext_values, &[], b, &mut finalize)
        };
        if match_inputs(matcher, pat, &fixed, swap, ir_node, 0, bindings, &mut done) {
            return true;
        }
        bindings.restore(mark);
    }
    false
}

/// Continuation-passing match of a node's **existential** (`any_input`) inputs:
/// each `exts[j]` must match SOME value input of `ir_node`, and each on a
/// DISTINCT input slot (`used` holds the slot indices already claimed by
/// earlier existentials). For input `ei` it tries the sub-pattern against every
/// not-yet-used value input in turn (backtracking via [`Bindings::mark`] /
/// [`Bindings::restore`]), and on a hit recurses into the next existential;
/// with all satisfied it invokes `done`. Non-value inputs (a `Phi`'s `PhiToken`
/// slot, control / memory edges) are rejected by the sub-pattern's own
/// output-kind check, so no explicit kind filtering is needed here. Distinctness
/// is by slot index, not value, so `any_input(1).any_input(1)` still matches a
/// phi with two separate `1` predecessors.
fn match_existential(
    matcher: &Matcher,
    pat: &Pattern,
    exts: &[InputEdge],
    ei: usize,
    values: &[ValueId],
    used: &[usize],
    bindings: &mut Bindings,
    done: &mut dyn FnMut(&mut Bindings) -> bool,
) -> bool {
    let Some(edge) = exts.get(ei) else {
        return done(bindings);
    };
    for (idx, &producer_value) in values.iter().enumerate() {
        if used.contains(&idx) {
            continue;
        }
        let mark = bindings.mark();
        let producer_ir = matcher.function().producer(producer_value);
        // Small arity (a phi's predecessor count); a fresh Vec per candidate is
        // cheap and sidesteps threading a &mut set through the continuation.
        let mut next_used = used.to_vec();
        next_used.push(idx);
        if try_match_at(
            matcher,
            pat,
            edge.producer,
            producer_ir,
            Some(producer_value),
            Some(edge.out_vertex),
            bindings,
            &mut |b| match_existential(matcher, pat, exts, ei + 1, values, &next_used, b, done),
        ) {
            return true;
        }
        bindings.restore(mark);
    }
    false
}

/// One incoming input of a consumer pat node.
struct InputEdge {
    consumer_slot: usize,
    out_vertex: PatValueId,
    producer: PatNodeId,
}

/// Continuation-passing match of a consumer's inputs: match `inputs[i..]` (in
/// operand order `swap`) against `ir_node`'s slots, then invoke `done`. Each
/// input's sub-match recurses with a continuation that matches the REMAINING
/// inputs, so when a later stage rejects (`done`, or a deeper continuation,
/// returns `false`) an earlier input's sub-match is asked for its next
/// configuration before this call fails — the backtracking that lets a parent
/// guard re-drive a child's commutative ordering.
#[allow(clippy::too_many_arguments)]
fn match_inputs(
    matcher: &Matcher,
    pat: &Pattern,
    inputs: &[InputEdge],
    swap: bool,
    ir_node: NodeId,
    i: usize,
    bindings: &mut Bindings,
    done: &mut dyn FnMut(&mut Bindings) -> bool,
) -> bool {
    let Some(edge) = inputs.get(i) else {
        return done(bindings);
    };
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

    let mark = bindings.mark();
    // Direct sub-match, continuing into the remaining inputs.
    let producer_ir = matcher.function().producer(producer_value);
    if try_match_at(
        matcher,
        pat,
        edge.producer,
        producer_ir,
        Some(producer_value),
        Some(edge.out_vertex),
        bindings,
        &mut |b| match_inputs(matcher, pat, inputs, swap, ir_node, i + 1, b, done),
    ) {
        return true;
    }
    bindings.restore(mark);

    // Cast walk-through fallback.
    let cast_mask = pat.cast_mask;
    if cast_mask.is_empty() {
        return false;
    }
    let mut skipped = Vec::new();
    let unwrapped = skip_casts(matcher, producer_value, cast_mask, &mut skipped);
    if unwrapped == producer_value {
        // Producer wasn't a registered cast — no further fallback.
        return false;
    }
    // Record the skipped casts into the footprint BEFORE the sub-match: they are
    // journaled like every other footprint entry, so a failure here is rolled
    // back by the `restore(mark)` below. On success they stay, keeping the
    // asm-fingerprint superset contract intact.
    for &cast in &skipped {
        bindings.record_matched(cast);
    }
    let unwrapped_ir = matcher.function().producer(unwrapped);
    if try_match_at(
        matcher,
        pat,
        edge.producer,
        unwrapped_ir,
        Some(unwrapped),
        Some(edge.out_vertex),
        bindings,
        &mut |b| match_inputs(matcher, pat, inputs, swap, ir_node, i + 1, b, done),
    ) {
        return true;
    }
    bindings.restore(mark);
    false
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
        && o.match_slot
            .is_none_or(|s| f.value_definition(value).1 as usize == s)
}
