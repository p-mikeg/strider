//! Recursive bipartite match engine with commutative-operand retry and cast
//! walk-through.
//!
//! The matcher visits the pattern graph in pull order from
//! [`Pattern::root`](crate::matcher::Pattern), kind-checking each pat node
//! against its IR node, running the node predicate, then walking inputs. An
//! input is a [`PatValue`] vertex the [`PatNode`] consumes; edges carry no
//! weight, so the consumer slot comes off the consumer's `input_slots` payload
//! and the producer is the vertex's own producer node.
//!
//! Matching a node's inputs is one problem: an injective assignment of pattern
//! inputs to IR input slots, each input drawing from its own [`Candidates`]
//! set, solved by [`match_assignments`]. A fixed operand's set is a singleton;
//! a commutative arity-2 node gives both operands `{0, 1}`, so injectivity
//! yields exactly the natural then the swapped ordering; an `any_input`
//! existential draws from every slot and lets the sub-pattern discriminate (a
//! value sub cannot match a `PhiToken`/control/memory slot). Backtracking rolls
//! back via [`Bindings::mark`] / [`Bindings::restore`].
//!
//! The engine is continuation-passing: [`try_match_at`] enumerates a pat node's
//! matches (each operand ordering by each descendant configuration) and calls
//! `k` per configuration rather than returning the first hit. A guard anywhere
//! above that rejects a configuration therefore re-drives a commutative node
//! below it, so e.g. a `when_match` on a unary parent picks which operand of a
//! commutative child binds. A node's binding walk
//! ([`BindingWalkFn`](crate::matcher::BindingWalkFn), which is how `IfPat`
//! matches its branches) is another axis of the same enumeration: it offers
//! each of its own configurations in turn. The caller supplies the root
//! continuation: a first-hit collector returns `true` to stop at the first
//! guard-satisfying configuration in DFS order, while `find_all`'s returns
//! `false` to enumerate the rest, reporting several distinct bindings per
//! root.
//!
//! On a sub-pattern mismatch at a producer output, a non-empty
//! [`CastMask`](crate::matcher::CastMask) makes the matcher unwrap a masked
//! cast and retry against the cast's value input.
//!
//! [`PatValue`]: crate::matcher::PatValue
//! [`PatNode`]: crate::matcher::PatNode

use std::cell::Cell;

use rustc_hash::FxHashMap;
use strider_graph::{NodeId as PatNodeId, ValueId as PatValueId};
use strider_ir::IRViewer;
use strider_ir::node::{NodeId, ValueId, ValueKind};

use crate::bindings::{Binding, Bindings};
use crate::matcher::{KindSpec, Matcher, OutputKindSpec, PatNode, PatValue, Pattern, cast_levels};

struct Ctx<'a> {
    matcher: &'a Matcher<'a>,
    pat: &'a Pattern,
    /// [`Matcher::satisfied`], borrowed for the walk.
    satisfied: &'a Cell<u64>,
}

impl Ctx<'_> {
    #[inline]
    fn function(&self) -> &strider_ir::Function {
        self.matcher.function()
    }
}

/// Value-rooted attempt: `pat`'s root against the IR node producing
/// `root_value`, which supplies the root output's constraints and capture.
///
/// `k` runs once per fully guard-satisfying configuration. `true` accepts and
/// stops the search; `false` drives the next one, which is how a caller
/// enumerates every distinct binding rather than only the first.
pub(crate) fn try_match(
    matcher: &Matcher,
    pat: &Pattern,
    root: PatNodeId,
    root_value: ValueId,
    bindings: &mut Bindings,
    k: &mut dyn FnMut(&mut Bindings) -> bool,
) -> bool {
    try_match_rooted(matcher, pat, root, root_value, bindings, k, true)
}

/// [`try_match`] for a sub-pattern matched INSIDE another walk, such as an
/// `IfPat` branch. `k` is the enclosing continuation, not a root, so reaching
/// it is no match of its own and must not bump `satisfied`.
pub(crate) fn try_match_nested(
    matcher: &Matcher,
    pat: &Pattern,
    root: PatNodeId,
    root_value: ValueId,
    bindings: &mut Bindings,
    k: &mut dyn FnMut(&mut Bindings) -> bool,
) -> bool {
    try_match_rooted(matcher, pat, root, root_value, bindings, k, false)
}

#[allow(clippy::too_many_arguments)]
fn try_match_rooted(
    matcher: &Matcher,
    pat: &Pattern,
    root: PatNodeId,
    root_value: ValueId,
    bindings: &mut Bindings,
    k: &mut dyn FnMut(&mut Bindings) -> bool,
    count_root: bool,
) -> bool {
    let root_out_vertex = root_output_vertex_for(pat, root, matcher, root_value);
    let root_node = matcher.function().producer(root_value);
    let ctx = Ctx {
        matcher,
        pat,
        satisfied: &matcher.satisfied,
    };
    if !count_root {
        return try_match_at(
            &ctx,
            root,
            root_node,
            Some(root_value),
            root_out_vertex,
            bindings,
            k,
        );
    }
    let mut counted = counting(&matcher.satisfied, k);
    try_match_at(
        &ctx,
        root,
        root_node,
        Some(root_value),
        root_out_vertex,
        bindings,
        &mut counted,
    )
}

/// Wraps the root continuation so every configuration reaching it bumps
/// `satisfied`.
fn counting<'a>(
    satisfied: &'a Cell<u64>,
    k: &'a mut dyn FnMut(&mut Bindings) -> bool,
) -> impl FnMut(&mut Bindings) -> bool + 'a {
    move |b| {
        satisfied.set(satisfied.get() + 1);
        k(b)
    }
}

/// Zero-value-output attempt (`Return` and friends): `pat`'s root against
/// `node`, with no associated output.
///
/// Such a node can only satisfy a root whose output vertex imposes no value
/// requirement (a bare `anything()`, or a control builder); a root demanding a
/// value output is rejected here. A `var()` root is rejected too, but not by
/// this test, which reads width and kind: its capture is what
/// `bind_sibling_outputs` refuses.
///
/// `k` is the root continuation; see [`try_match`]. `count_root` is `false`
/// for a sub-pattern matched inside another walk; see [`try_match_nested`].
pub(crate) fn try_match_node(
    matcher: &Matcher,
    pat: &Pattern,
    root: PatNodeId,
    node: NodeId,
    bindings: &mut Bindings,
    k: &mut dyn FnMut(&mut Bindings) -> bool,
    count_root: bool,
) -> bool {
    if root_requires_value_output(pat, root) {
        return false;
    }
    let ctx = Ctx {
        matcher,
        pat,
        satisfied: &matcher.satisfied,
    };
    if !count_root {
        return try_match_at(&ctx, root, node, None, None, bindings, k);
    }
    let mut counted = counting(&matcher.satisfied, k);
    try_match_at(&ctx, root, node, None, None, bindings, &mut counted)
}

/// A root demanding a value output cannot match a zero-output IR node.
fn root_requires_value_output(pat: &Pattern, root: PatNodeId) -> bool {
    pat.graph.node_outputs(root).iter().any(|&ov| {
        let o = pat.graph.value_kind_ref(ov);
        o.width.is_some() || matches!(o.kind, OutputKindSpec::Value(_) | OutputKindSpec::AnyValue)
    })
}

/// The root pat node's output vertex carrying the root-level constraints to
/// check against `root_value`; `None` if it declares no output vertex.
///
/// A sole output vertex is taken whatever slot `root_value` sits at: its
/// constraint applies to the output being matched, not to a fixed slot. A
/// vertex that does mean a fixed slot says so through
/// [`PatValue::match_slot`], which [`output_ok`] then enforces. With several
/// vertices (`if_else`, `call`, `region`, `entry`, `load`, `phi`, ...) the
/// slots pick between them.
pub(crate) fn root_output_vertex_for(
    pat: &Pattern,
    root: PatNodeId,
    matcher: &Matcher,
    root_value: ValueId,
) -> Option<PatValueId> {
    let outs = pat.graph.node_outputs(root);
    let mut iter = outs.iter().copied();
    let first = iter.next()?;
    if iter.next().is_none() {
        return Some(first);
    }

    let (_node, ir_slot) = matcher.function().value_definition(root_value);
    outs.iter()
        .copied()
        .find(|&out_vertex| pat.graph.value_kind_ref(out_vertex).slot as u32 == ir_slot)
}

/// Whether anchoring `pat`'s root at `out_id` re-runs the attempt already made
/// at `anchored`.
///
/// A root declaring a vertex per output imposes one constraint set whichever of
/// them anchors: the anchoring vertex is checked against its own output and
/// carries its captures there, and [`bind_sibling_outputs`] checks every other
/// at its own slot, so the two attempts differ only in the order they bind.
/// A vertex shared between the outputs, an `any_slot` one (pinned as an anchor,
/// enumerated as a sibling), and a differing anchor value type (which a
/// post-match guard reads) each tell them apart.
pub(crate) fn anchor_repeats(
    matcher: &Matcher,
    pat: &Pattern,
    root: PatNodeId,
    anchored: ValueId,
    out_id: ValueId,
) -> bool {
    let (Some(prev), Some(cur)) = (
        root_output_vertex_for(pat, root, matcher, anchored),
        root_output_vertex_for(pat, root, matcher, out_id),
    ) else {
        return false;
    };
    if prev == cur
        || pat.graph.value_kind_ref(prev).any_slot
        || pat.graph.value_kind_ref(cur).any_slot
    {
        return false;
    }
    let f = matcher.function();
    f.value_kind(anchored).as_value() == f.value_kind(out_id).as_value()
}

/// Recursive worker, continuation-passing so a guard failure anywhere above can
/// re-drive a commutative operand order below it. `root_value` / `out_vertex`
/// are the IR output and its pat-output vertex when this pat node sits at a
/// value-producing position.
///
/// Enumerates every way `pat_node` matches `ir_node`; for each it extends
/// `bindings` with that configuration's captures and footprint and calls `k`.
/// `k` returning `true` accepts and leaves `bindings` in the accepted state;
/// `false` tries the next configuration. If none is accepted, `bindings` is
/// restored to entry.
fn try_match_at(
    ctx: &Ctx,
    pat_node: PatNodeId,
    ir_node: NodeId,
    root_value: Option<ValueId>,
    out_vertex: Option<PatValueId>,
    bindings: &mut Bindings,
    k: &mut dyn FnMut(&mut Bindings) -> bool,
) -> bool {
    let nd = ctx.pat.graph.node_kind(pat_node);
    if !nd.kind.matches(ctx.function().node_kind(ir_node)) {
        return false;
    }

    // Root-output kind / width constraints (`bool_*` pins `Value(I1)`,
    // `value_of_width` pins width). With no value to check against (a
    // node-rooted match at a zero-output kind, reached through an alternation
    // whose own synthesized vertex imposes nothing), a vertex that demands
    // anything cannot be satisfied, mirroring `bind_sibling_outputs`'s
    // no-output-at-the-slot arm. A bare `anything()` demands nothing and still
    // matches a `Return`.
    if let Some(ov_idx) = out_vertex {
        let ov = ctx.pat.graph.value_kind_ref(ov_idx);
        match root_value {
            Some(value) if !output_ok(ov, ctx.function(), value) => return false,
            None if vertex_imposes_requirement(ov) => return false,
            _ => {}
        }
    }

    // Fires after kind + output constraints and before descending into inputs,
    // so node-only predicates short-circuit here.
    if let Some(predicate) = &nd.node_predicate
        && !predicate(ctx.matcher, ir_node)
    {
        return false;
    }

    // Resolved once at seal: neither the edge list, its ordering nor the fixed
    // count depends on the IR node, and a match attempt re-enters this function
    // once per operand ordering.
    let node_inputs = ctx.pat.inputs_of(pat_node);
    let inputs = &node_inputs.edges;
    let n_fixed = node_inputs.fixed;

    // Carries this node's own captures; see `bind_all_captures`.
    let out_weight = out_vertex.map(|ov| ctx.pat.graph.value_kind_ref(ov));

    // Alternation (`one_of` / `first_of`): `inputs` are independent alternative
    // sub-patterns, not operands, all tried against the SAME `ir_node`. `one_of`
    // enumerates every matching arm; `first_of` cuts to the first.
    if nd.alternation {
        let mark = bindings.mark();
        // Bound before any arm runs, where every other kind binds in
        // `finalize` once its inputs have matched, so an arm rebinding this
        // node's capture conflicts rather than shadows.
        if !bind_all_captures(bindings, nd, out_weight, ir_node, root_value) {
            return false;
        }
        for alt in inputs {
            let before = ctx.satisfied.get();
            let accepted = try_match_at(
                ctx,
                alt.producer,
                ir_node,
                root_value,
                Some(alt.out_vertex),
                bindings,
                // The arm's own `try_match_at` restores on every `false`
                // return, so a rejected configuration needs no cleanup here.
                &mut |b| {
                    finish_node(
                        ctx,
                        nd,
                        pat_node,
                        ir_node,
                        root_value,
                        out_vertex,
                        b,
                        &mut |b| continue_node(ctx, nd, ir_node, b, k),
                    )
                },
            );
            if accepted {
                return true;
            }
            // The cut is on a produced match, not on structural arm shape: an
            // arm reaching `k` but rejected by a guard above falls through to
            // the next arm.
            if nd.first_match && ctx.satisfied.get() > before {
                break;
            }
        }
        bindings.restore(mark);
        return false;
    }

    // Commutativity needs one or two fixed operands at slots {0,1} (the shape
    // of every commutative IR kind's signature), a commutative IR kind, and no
    // `.ordered()`. A single pinned operand reaches both slots too, or the same
    // query answers differently by which slot it names. Otherwise operands stay
    // on `Candidates::Only`, i.e. one ordering.
    //
    // Both orderings are re-driven under an enumerating continuation, so a
    // commutative node whose two operands EACH satisfy both sub-patterns
    // doubles, and nesting multiplies: a wildcard operand satisfies either
    // side, so an ordinary wildcard-leaved add tree costs 2^k for k nodes under
    // `find_all`, whose continuation returns `false` and drives every
    // configuration. `NodeInputs::interchangeable` cuts the symmetric half of
    // that; `.ordered()` pins an operand pair for the rest.
    let commutative = !nd.force_ordered
        && (1..=COMM_ORDER.len()).contains(&n_fixed)
        && !node_inputs.interchangeable
        && inputs[..n_fixed]
            .iter()
            .all(|e| e.consumer_slot < COMM_ORDER.len())
        && ctx.function().node_kind(ir_node).is_commutative();

    // The existential candidate set: every input slot of `ir_node`, less the
    // ones a fixed operand pinned, which on a commutative node is none, its
    // fixed operands reaching both slots. `k` existentials enumerate every
    // injective assignment over it, which is what `any_input`'s cost is.
    // Invariant across the search, so collected once here rather than per
    // recursion level (and empty in the common no-`any_input` case).
    // `Candidates::Only` never extends the `Claimed` chain, so a pinned slot is
    // excluded here or it is offered twice; the commutative pair is `OneOf` and
    // `Claimed` handles it.
    //
    // No kind filter here: the sub-pattern discriminates. A sub carries an
    // output-kind and reaches only inputs of that kind (a value sub skips
    // control/memory/PhiToken, a memory sub binds a memory input), while a bare
    // wildcard (`OutputKindSpec::Any`) binds any input including a `PhiToken`.
    let ext_slots: Vec<usize> = if n_fixed == inputs.len() {
        Vec::new()
    } else {
        let pinned: Vec<usize> = if commutative {
            Vec::new()
        } else {
            inputs[..n_fixed].iter().map(|e| e.consumer_slot).collect()
        };
        (0..ctx.function().node_inputs(ir_node).len())
            .filter(|s| !pinned.contains(s))
            .collect()
    };

    let assignment = Assignment {
        edges: inputs,
        commutative,
        ext_slots: &ext_slots,
    };

    let mark = bindings.mark();
    {
        // Finishes THIS node once every input is assigned and matched: bind
        // its capture (capture-equality is enforced here against deeper
        // bindings), run its guard, record it into the footprint, hand off to
        // `k`. Returning `false` makes the enumeration try its next
        // configuration, so an ancestor guard failure surfaced through `k`
        // re-drives this node and everything beneath it.
        let mut finalize = |b: &mut Bindings| -> bool {
            let inner = b.mark();
            if !bind_all_captures(b, nd, out_weight, ir_node, root_value) {
                return false;
            }
            if finish_node(
                ctx,
                nd,
                pat_node,
                ir_node,
                root_value,
                out_vertex,
                b,
                &mut |b| continue_node(ctx, nd, ir_node, b, k),
            ) {
                return true;
            }
            b.restore(inner);
            false
        };
        if match_assignments(ctx, &assignment, 0, ir_node, None, bindings, &mut finalize) {
            return true;
        }
    }
    bindings.restore(mark);
    false
}

/// The constraints a pat node owes once its operands, or its alternation arm,
/// have matched: its sibling output vertices and its guard. Restores `b` to
/// entry on rejection; [`continue_node`] carries on from here.
#[allow(clippy::too_many_arguments)]
fn finish_node(
    ctx: &Ctx,
    nd: &PatNode,
    pat_node: PatNodeId,
    ir_node: NodeId,
    root_value: Option<ValueId>,
    out_vertex: Option<PatValueId>,
    b: &mut Bindings,
    k: &mut dyn FnMut(&mut Bindings) -> bool,
) -> bool {
    let outs = ctx.pat.graph.node_outputs(pat_node);
    let inner = b.mark();
    if bind_sibling_outputs(ctx, nd, outs, out_vertex, 0, ir_node, root_value, b, k) {
        return true;
    }
    b.restore(inner);
    false
}

/// The output vertices from `idx` on other than `anchor`, then `nd`'s
/// post-match predicate, then `k`. An `any_slot` vertex enumerates the node's
/// outputs, so a capture on it binds each in turn and a rejection anywhere
/// above re-drives it; every other vertex is checked at its own slot.
#[allow(clippy::too_many_arguments)]
fn bind_sibling_outputs(
    ctx: &Ctx,
    nd: &PatNode,
    sibs: &[PatValueId],
    anchor: Option<PatValueId>,
    idx: usize,
    ir_node: NodeId,
    root_value: Option<ValueId>,
    b: &mut Bindings,
    k: &mut dyn FnMut(&mut Bindings) -> bool,
) -> bool {
    let Some(&ov_idx) = sibs.get(idx) else {
        if let Some(pm) = &nd.post_match {
            let ty = root_value.and_then(|value| ctx.function().value_kind(value).as_value());
            if !pm(ctx.matcher, ir_node, ty, b) {
                return false;
            }
        }
        return k(b);
    };
    if Some(ov_idx) == anchor {
        return bind_sibling_outputs(ctx, nd, sibs, anchor, idx + 1, ir_node, root_value, b, k);
    }
    let ov = ctx.pat.graph.value_kind_ref(ov_idx);
    let outs = ctx.function().node_outputs(ir_node);
    let pinned = [ov.slot];
    let enumerated: Vec<usize>;
    let slots: &[usize] = if ov.any_slot {
        enumerated = (0..outs.len()).collect();
        &enumerated
    } else {
        &pinned
    };
    let mut any_slot_present = false;
    for &slot in slots {
        let Some(&val) = outs.get(slot) else { continue };
        any_slot_present = true;
        if !output_ok(ov, ctx.function(), val) {
            continue;
        }
        let here = b.mark();
        if ov
            .captures
            .iter()
            .copied()
            .chain(ov.identity)
            .any(|cap| !b.bind_capture(cap, Binding::Value(val)))
        {
            b.restore(here);
            continue;
        }
        if bind_sibling_outputs(ctx, nd, sibs, anchor, idx + 1, ir_node, root_value, b, k) {
            return true;
        }
        b.restore(here);
    }
    // No output at the slot at all is vacuously satisfied only while the vertex
    // constrains nothing, which is what lets a bare `anything()` match a
    // value-less `Return`; one carrying a capture or a filter rejects instead of
    // leaving that capture unbound.
    if !any_slot_present && !vertex_imposes_requirement(ov) {
        return bind_sibling_outputs(ctx, nd, sibs, anchor, idx + 1, ir_node, root_value, b, k);
    }
    false
}

/// A matched pat node's footprint entry and hand-off to `k`, through its
/// binding walk where it has one. A walk is an enumeration like any operand's,
/// so `k` runs once per configuration it reaches rather than once per node,
/// and a rejection above re-drives it.
///
/// A declined `k` leaves `b` for the caller to roll back, which every call site
/// does to a mark taken earlier.
///
/// Inlined on measured grounds: this runs once per matched node per
/// configuration, and left to the compiler's judgement the extra call shows up
/// in `benches/matcher.rs`'s `matcher_find_all_add_const`.
#[inline(always)]
fn continue_node(
    ctx: &Ctx,
    nd: &PatNode,
    ir_node: NodeId,
    b: &mut Bindings,
    k: &mut dyn FnMut(&mut Bindings) -> bool,
) -> bool {
    let Some(bw) = &nd.binding_walk else {
        b.record_matched(ir_node);
        return k(b);
    };
    // The footprint entry is scoped to one continuation call, not to this
    // frame, so the walk's next configuration starts from a clean footprint.
    bw(ctx.matcher, ir_node, b, &mut |b| {
        let per_config = b.mark();
        b.record_matched(ir_node);
        if k(b) {
            return true;
        }
        b.restore(per_config);
        false
    })
}

/// Try `edge`'s sub-pattern against the operand `value`, then, if the cast mask
/// permits and `value` is a registered cast, against the unwrapped value. `k`
/// continues the enclosing match. Both [`Candidates`] arms share this and
/// differ only in their reaction to `false`: a singleton fails its pinned slot,
/// a multi-candidate set tries the next.
///
/// Every failure path restores `bindings` to entry, so a caller may retry
/// without cleanup. Skipped casts are journaled BEFORE the retry, so a failure
/// rolls them back and a success keeps them, which is what holds the
/// asm-fingerprint superset contract.
///
/// The cast walk-through is a fallback, not an alternative: it engages only
/// when the level above PRODUCED no match, never to offer a second binding
/// beside a direct one. Reaching the continuation is not the same test (a
/// guard above can reject a configuration, and the ladder must still run),
/// so the cut counts full matches through `ctx.satisfied`, which is also what
/// `first_of` cuts on. Under an enumerating `k` the return value cannot tell
/// the two apart: it is always `false`.
fn try_operand(
    ctx: &Ctx,
    edge: &InputEdge,
    value: ValueId,
    bindings: &mut Bindings,
    k: &mut dyn FnMut(&mut Bindings) -> bool,
) -> bool {
    let mark = bindings.mark();
    let producer_ir = ctx.function().producer(value);
    let before = ctx.satisfied.get();
    if try_match_at(
        ctx,
        edge.producer,
        producer_ir,
        Some(value),
        Some(edge.out_vertex),
        bindings,
        k,
    ) {
        return true;
    }
    bindings.restore(mark);
    if ctx.satisfied.get() > before {
        return false;
    }

    if ctx.pat.cast_mask.is_empty() {
        return false;
    }
    // Each level in turn, outermost first: a pattern naming one cast kind under
    // a mask that also selects another matches at an INTERMEDIATE value, which
    // jumping straight to the bottom would skip.
    //
    // The chain is a fallback ladder, not an alternation: the FIRST level that
    // produces a match wins, exactly as a direct hit above wins over the whole
    // chain. Letting every level report would make the answer depend on an
    // incidental cast sitting on top of the operand.
    let levels = cast_levels(ctx.matcher, value, ctx.pat.cast_mask);
    for (depth, &(_, unwrapped)) in levels.iter().enumerate() {
        for &(cast, _) in levels.iter().take(depth + 1) {
            bindings.record_matched(cast);
        }
        let unwrapped_ir = ctx.function().producer(unwrapped);
        let before = ctx.satisfied.get();
        if try_match_at(
            ctx,
            edge.producer,
            unwrapped_ir,
            Some(unwrapped),
            Some(edge.out_vertex),
            bindings,
            k,
        ) {
            return true;
        }
        bindings.restore(mark);
        if ctx.satisfied.get() > before {
            return false;
        }
    }
    false
}

pub(crate) struct InputEdge {
    consumer_slot: usize,
    out_vertex: PatValueId,
    producer: PatNodeId,
}

/// A pat node's consumed inputs, with the existential (`any_input`) ones sorted
/// last and the fixed ones counted.
pub(crate) struct NodeInputs {
    pub(crate) edges: Vec<InputEdge>,
    pub(crate) fixed: usize,
    /// The two fixed operands bind identically whichever slot each takes, so
    /// the swapped ordering is redundant. See [`interchangeable`].
    pub(crate) interchangeable: bool,
}

/// One entry per pat node, indexed by [`PatNodeId::as_u32`].
///
/// `compare_operands` is off for a pattern already refused, whose cone may be
/// deeper than [`interchangeable`] can recurse through.
pub(crate) fn collect_node_inputs(
    graph: &crate::matcher::graph::PatGraph,
    compare_operands: bool,
) -> Vec<NodeInputs> {
    let mut memo = FxHashMap::default();
    graph
        .all_node_ids()
        .map(|node| {
            let mut edges: Vec<InputEdge> = crate::graph_ext::consumed_inputs(graph, node)
                .into_iter()
                .map(|(slot, out_vertex)| InputEdge {
                    consumer_slot: slot,
                    out_vertex,
                    producer: graph.producer(out_vertex),
                })
                .collect();
            // Stable, so each group keeps its relative order.
            edges.sort_by_key(|e| e.consumer_slot == crate::matcher::ANY_INPUT_SLOT);
            let fixed = edges
                .iter()
                .filter(|e| e.consumer_slot != crate::matcher::ANY_INPUT_SLOT)
                .count();
            let interchangeable = compare_operands
                && fixed == 2
                && !graph.node_kind(node).alternation
                && interchangeable(graph, edges[0].out_vertex, edges[1].out_vertex, &mut memo);
            NodeInputs {
                edges,
                fixed,
                interchangeable,
            }
        })
        .collect()
}

/// Whether the two operand sub-patterns accept exactly the same operand pairs
/// with exactly the same bindings under either ordering, so enumerating the
/// swap adds nothing.
///
/// One vertex feeding both slots qualifies whatever it binds: both slots then
/// carry the SAME captures and identity pin, and that constraint is symmetric.
/// Two distinct vertices qualify only while nothing under either binds (a
/// capture would land on the other operand when swapped) and nothing is opaque
/// (a boxed predicate cannot be compared to another).
fn interchangeable(
    graph: &crate::matcher::graph::PatGraph,
    a: PatValueId,
    b: PatValueId,
    memo: &mut FxHashMap<(PatValueId, PatValueId), bool>,
) -> bool {
    if a == b {
        return true;
    }
    if let Some(&hit) = memo.get(&(a, b)) {
        return hit;
    }
    // A cycle would have been refused at seal; the placeholder guards the
    // sharing-heavy DAG against re-walking a pair mid-flight.
    memo.insert((a, b), false);
    let verdict = value_interchangeable(graph.value_kind_ref(a), graph.value_kind_ref(b))
        && node_interchangeable(graph, graph.producer(a), graph.producer(b), memo);
    memo.insert((a, b), verdict);
    verdict
}

fn node_interchangeable(
    graph: &crate::matcher::graph::PatGraph,
    a: PatNodeId,
    b: PatNodeId,
    memo: &mut FxHashMap<(PatValueId, PatValueId), bool>,
) -> bool {
    if a == b {
        return true;
    }
    let (na, nb) = (graph.node_kind(a), graph.node_kind(b));
    if !(na.captures.is_empty()
        && nb.captures.is_empty()
        && na.node_predicate.is_none()
        && nb.node_predicate.is_none()
        && na.post_match.is_none()
        && nb.post_match.is_none()
        && na.binding_walk.is_none()
        && nb.binding_walk.is_none()
        && na.force_ordered == nb.force_ordered
        && na.alternation == nb.alternation
        && na.first_match == nb.first_match
        && na.input_slots == nb.input_slots
        && kind_spec_interchangeable(&na.kind, &nb.kind))
    {
        return false;
    }
    // Every output vertex, not just the consumed anchor: a capture or filter
    // `.output(slot)` / `.any_output()` put on a sibling vertex still lands on
    // the other operand when swapped. Compared as values only, since their
    // producers are the pair under test.
    let (oa, ob) = (graph.node_outputs(a), graph.node_outputs(b));
    if oa.len() != ob.len()
        || !oa.iter().zip(ob).all(|(&va, &vb)| {
            value_interchangeable(graph.value_kind_ref(va), graph.value_kind_ref(vb))
        })
    {
        return false;
    }
    let (ia, ib) = (
        crate::graph_ext::consumed_inputs(graph, a),
        crate::graph_ext::consumed_inputs(graph, b),
    );
    ia.len() == ib.len()
        && ia
            .iter()
            .zip(&ib)
            .all(|(&(sa, va), &(sb, vb))| sa == sb && interchangeable(graph, va, vb, memo))
}

fn value_interchangeable(a: &PatValue, b: &PatValue) -> bool {
    a.captures.is_empty()
        && b.captures.is_empty()
        && a.identity.is_none()
        && b.identity.is_none()
        && a.slot == b.slot
        && a.width == b.width
        && a.match_slot == b.match_slot
        && a.any_slot == b.any_slot
        && a.token_fallback == b.token_fallback
        && output_kind_eq(&a.kind, &b.kind)
}

fn output_kind_eq(a: &OutputKindSpec, b: &OutputKindSpec) -> bool {
    match (a, b) {
        (OutputKindSpec::Any, OutputKindSpec::Any)
        | (OutputKindSpec::AnyValue, OutputKindSpec::AnyValue)
        | (OutputKindSpec::Control, OutputKindSpec::Control)
        | (OutputKindSpec::Memory, OutputKindSpec::Memory)
        | (OutputKindSpec::PhiToken, OutputKindSpec::PhiToken) => true,
        (OutputKindSpec::Value(x), OutputKindSpec::Value(y)) => x == y,
        _ => false,
    }
}

/// [`KindSpec::VariantWith`] carries a closure, so two of them are never known
/// to agree.
fn kind_spec_interchangeable(a: &KindSpec, b: &KindSpec) -> bool {
    match (a, b) {
        (KindSpec::Any, KindSpec::Any) => true,
        (KindSpec::Variant(x), KindSpec::Variant(y)) => x == y,
        (KindSpec::Exact(x), KindSpec::Exact(y)) => x == y,
        _ => false,
    }
}

/// The IR input slots one pattern input may occupy: the whole difference
/// between a fixed operand, a commutative pair and an existential.
#[derive(Clone, Copy)]
enum Candidates<'a> {
    /// A pinned slot: exactly one candidate.
    Only(usize),
    /// Ordered; the input takes the first slot both unclaimed by an enclosing
    /// assignment and matching the sub-pattern.
    OneOf(&'a [usize]),
}

/// Candidate slots for a commutative operand pair, indexed by the pattern
/// input's own consumer slot. Each operand tries its own slot first, so
/// injectivity enumerates the natural ordering before the swapped one whichever
/// operand the assignment visits first.
const COMM_ORDER: [[usize; 2]; 2] = [[0, 1], [1, 0]];

/// One node's assignment problem: which IR input slots each pattern input may
/// occupy. Everything here is invariant across the search below it.
struct Assignment<'a> {
    edges: &'a [InputEdge],
    commutative: bool,
    ext_slots: &'a [usize],
}

impl Assignment<'_> {
    fn candidates(&self, edge: &InputEdge) -> Candidates<'_> {
        if edge.consumer_slot == crate::matcher::ANY_INPUT_SLOT {
            Candidates::OneOf(self.ext_slots)
        } else if self.commutative {
            Candidates::OneOf(&COMM_ORDER[edge.consumer_slot])
        } else {
            Candidates::Only(edge.consumer_slot)
        }
    }
}

/// Slots already claimed by enclosing [`Candidates::OneOf`] assignments.
///
/// Distinctness is by slot index, never by value, so
/// `any_input(1).any_input(1)` still matches a phi with two separate `1`
/// predecessors.
struct Claimed<'a> {
    slot: usize,
    prev: Option<&'a Claimed<'a>>,
}

impl Claimed<'_> {
    fn contains(chain: Option<&Claimed>, slot: usize) -> bool {
        let mut cur = chain;
        while let Some(c) = cur {
            if c.slot == slot {
                return true;
            }
            cur = c.prev;
        }
        false
    }
}

/// Continuation-passing injective assignment of `assignment.edges[i..]` to
/// `ir_node`'s input slots, invoking `done` once every input is placed and
/// matched.
///
/// Each input's sub-match recurses with a continuation assigning the remaining
/// inputs, so a later rejection asks an earlier input for its next candidate
/// before this call fails. That is the backtracking that lets a parent guard
/// re-drive a child's commutative ordering. Every candidate goes through
/// [`try_operand`], so an existential honours `ignore_casts` like a fixed slot.
fn match_assignments(
    ctx: &Ctx,
    assignment: &Assignment,
    i: usize,
    ir_node: NodeId,
    claimed: Option<&Claimed>,
    bindings: &mut Bindings,
    done: &mut dyn FnMut(&mut Bindings) -> bool,
) -> bool {
    let Some(edge) = assignment.edges.get(i) else {
        return done(bindings);
    };
    match assignment.candidates(edge) {
        // Direct index, no search and no injectivity bookkeeping. The slot is
        // pinned, so a failed operand fails it outright.
        Candidates::Only(slot) => {
            let Some(value) = input_at(ctx, ir_node, slot) else {
                return false;
            };
            try_operand(ctx, edge, value, bindings, &mut |b| {
                match_assignments(ctx, assignment, i + 1, ir_node, claimed, b, done)
            })
        }
        Candidates::OneOf(slots) => {
            for &slot in slots {
                if Claimed::contains(claimed, slot) {
                    continue;
                }
                let Some(value) = input_at(ctx, ir_node, slot) else {
                    continue;
                };
                let next = Claimed {
                    slot,
                    prev: claimed,
                };
                if try_operand(ctx, edge, value, bindings, &mut |b| {
                    match_assignments(ctx, assignment, i + 1, ir_node, Some(&next), b, done)
                }) {
                    return true;
                }
                // Bindings already restored; try the next slot.
            }
            false
        }
    }
}

/// Bind every capture this pat node declares, all-or-nothing: on the first
/// rebind conflict, roll back what THIS call bound and return `false`.
///
/// An output-vertex capture binds the matched VALUE and needs one, so it is
/// skipped at a node-rooted position; a node-declared capture binds the matched
/// NODE. The vertex's identity pin binds the value too, so a second slot
/// consuming the same vertex conflicts unless it lands on the same value.
fn bind_all_captures(
    b: &mut Bindings,
    nd: &PatNode,
    out_weight: Option<&PatValue>,
    ir_node: NodeId,
    root_value: Option<ValueId>,
) -> bool {
    let vertex_caps: &[crate::Capture] = out_weight.map_or(&[], |ov| ov.captures.as_slice());
    let identity = out_weight.and_then(|ov| ov.identity);
    let value_caps = root_value.into_iter().flat_map(move |value| {
        vertex_caps
            .iter()
            .copied()
            .chain(identity)
            .map(move |cap| (cap, Binding::Value(value)))
    });
    let node_caps = nd.captures.iter().map(|&cap| (cap, Binding::Node(ir_node)));
    let mark = b.mark();
    for (cap, binding) in value_caps.chain(node_caps) {
        if !b.bind_capture(cap, binding) {
            b.restore(mark);
            return false;
        }
    }
    true
}

fn input_at(ctx: &Ctx, ir_node: NodeId, slot: usize) -> Option<ValueId> {
    let use_id = ctx.function().node_input_id_at(ir_node, slot).ok()?;
    Some(ctx.function().graph().value_of_use(use_id))
}

/// Whether the vertex constrains anything at all: a capture or identity pin to
/// bind, or a kind / width / slot filter.
fn vertex_imposes_requirement(o: &PatValue) -> bool {
    !o.captures.is_empty()
        || o.identity.is_some()
        || o.width.is_some()
        || o.match_slot.is_some()
        || !matches!(o.kind, OutputKindSpec::Any)
}

/// `value` is a memory token whose producer offers no value output, so a
/// value anchor on that node has nothing else to bind.
fn is_sole_token_of(f: &strider_ir::Function, value: ValueId) -> bool {
    matches!(f.value_kind(value), ValueKind::Memory)
        && f.node_outputs(f.producer(value))
            .iter()
            .all(|&o| f.value_kind(o).as_value().is_none())
}

fn output_ok(o: &PatValue, f: &strider_ir::Function, value: ValueId) -> bool {
    let val = f.value_kind(value).as_value();
    let kind_ok = match &o.kind {
        // A `width` constraint below can still narrow this to a value.
        OutputKindSpec::Any => true,
        OutputKindSpec::Value(ty) => val == Some(*ty),
        OutputKindSpec::AnyValue => {
            val.is_some() || (o.token_fallback && is_sole_token_of(f, value))
        }
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
