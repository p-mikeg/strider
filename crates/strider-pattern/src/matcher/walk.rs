//! Recursive bipartite match engine with commutative-operand retry and cast
//! walk-through.
//!
//! The matcher visits the pattern graph in pull order from
//! [`Pattern::root`](crate::matcher::Pattern), kind-checking each pat node
//! against its IR node, running the node predicate, then walking inputs. An
//! input is a `Consumes{slot}` edge from a [`PatValue`] vertex, whose incoming
//! `Produces` edge names the producer [`PatNode`].
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
//! commutative child binds. The caller supplies the root continuation: a
//! first-hit collector returns `true` to stop at the first guard-satisfying
//! configuration in DFS order, while `find_all`'s returns `false` to enumerate
//! the rest, reporting several distinct bindings per root.
//!
//! On a sub-pattern mismatch at a producer output, a non-empty
//! [`CastMask`](crate::matcher::CastMask) makes the matcher unwrap a masked
//! cast and retry against the cast's value input.
//!
//! [`PatValue`]: crate::matcher::PatValue
//! [`PatNode`]: crate::matcher::PatNode

use strider_graph::{NodeId as PatNodeId, ValueId as PatValueId};
use strider_ir::IRViewer;
use strider_ir::node::{NodeId, ValueId, ValueKind, ValueType};

use crate::bindings::{Binding, Bindings};
use crate::graph_ext::PatGraphRead;
use crate::matcher::{Matcher, OutputKindSpec, PatValue, Pattern, skip_casts};

/// Both fields are fixed for a whole `try_match*` call, so bundling them lets
/// the recursion carry one pointer instead of re-threading a pair per frame.
struct Ctx<'a> {
    matcher: &'a Matcher<'a>,
    pat: &'a Pattern,
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
    let root_out_vertex = root_output_vertex_for(pat, root, matcher, root_value);
    let root_node = matcher.function().producer(root_value);
    let ctx = Ctx { matcher, pat };
    try_match_at(
        &ctx,
        root,
        root_node,
        Some(root_value),
        root_out_vertex,
        bindings,
        k,
    )
}

/// Zero-value-output attempt (`Return` and friends): `pat`'s root against
/// `node`, with no associated output.
///
/// Such a node can only satisfy a root whose output vertex imposes no value
/// requirement (a bare `any()` / `var()`, or a control builder). A root
/// demanding a value output is rejected here rather than having its constraint
/// silently skipped, which would let `bool_value()` match a `Return`.
///
/// `k` is the root continuation; see [`try_match`].
pub(crate) fn try_match_node(
    matcher: &Matcher,
    pat: &Pattern,
    root: PatNodeId,
    node: NodeId,
    bindings: &mut Bindings,
    k: &mut dyn FnMut(&mut Bindings) -> bool,
) -> bool {
    if root_requires_value_output(pat, root) {
        return false;
    }
    let ctx = Ctx { matcher, pat };
    try_match_at(&ctx, root, node, None, None, bindings, k)
}

/// A root demanding a value output cannot match a zero-output IR node.
fn root_requires_value_output(pat: &Pattern, root: PatNodeId) -> bool {
    pat.graph.produced_outputs(root).iter().any(|&ov| {
        let o = pat.graph.output_weight(ov);
        o.width.is_some() || matches!(o.kind, OutputKindSpec::Value(_) | OutputKindSpec::AnyValue)
    })
}

/// The root pat node's output vertex carrying the root-level constraints to
/// check against `root_value`; `None` if it declares no output vertex.
///
/// A value root declares exactly one output vertex, and its constraint applies
/// to whichever output is being matched: [`Matcher::matches`] roots an attempt
/// at every IR output of a node, so matching by slot instead would silently
/// skip the constraint whenever a multi-output `Region` / `Call` is rooted at a
/// non-slot-0 output.
///
/// The one multi-output-vertex root is the `If` control builder (two `Control`
/// vertices), which keeps the per-slot lookup.
///
/// [`Matcher::matches`]: crate::Matcher::matches
fn root_output_vertex_for(
    pat: &Pattern,
    root: PatNodeId,
    matcher: &Matcher,
    root_value: ValueId,
) -> Option<PatValueId> {
    // Single output vertex: applies regardless of slot.
    let outs = pat.graph.produced_outputs(root);
    let mut iter = outs.iter().copied();
    let first = iter.next()?;
    if iter.next().is_none() {
        return Some(first);
    }

    // The `If` control root: per-slot lookup, so each control output's
    // constraints land on the right slot.
    let (_node, ir_slot) = matcher.function().value_definition(root_value);
    outs.iter()
        .copied()
        .find(|&out_vertex| pat.graph.output_weight(out_vertex).slot as u32 == ir_slot)
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
    let nd = ctx.pat.graph.node_weight(pat_node);
    if !nd.kind.matches(ctx.function().node_kind(ir_node)) {
        return false;
    }

    // Root-output kind / width constraints (`bool_*` pins `Value(I1)`,
    // `value_of_width` pins width).
    if let Some(ov_idx) = out_vertex
        && let Some(value) = root_value
    {
        let ov = ctx.pat.graph.output_weight(ov_idx);
        if !output_ok(ov, ctx.function(), value) {
            return false;
        }
    }

    // Fires after kind + output constraints and before descending into inputs,
    // so node-only predicates short-circuit here.
    if let Some(predicate) = &nd.node_predicate
        && !predicate(ctx.matcher, ir_node)
    {
        return false;
    }

    let mut inputs: Vec<InputEdge> = ctx
        .pat
        .graph
        .consumed_inputs(pat_node)
        .into_iter()
        .map(|(slot, out_vertex)| {
            let producer = ctx.pat.graph.producer_of(out_vertex);
            InputEdge {
                consumer_slot: slot,
                out_vertex,
                producer,
            }
        })
        .collect();

    // Captures for THIS node, independent of operand ordering. An
    // output-vertex capture binds the matched VALUE; a node-declared capture
    // (control nodes like `If`) binds the matched NODE. Both can be present and
    // are independent: an `If` may carry `capture_true(t)` and `capture(g)`, so
    // binding only one would silently drop the other.
    let cap_bindings = [
        out_vertex
            .and_then(|ov| ctx.pat.graph.output_weight(ov).capture)
            .and_then(|cap| root_value.map(|value| (cap, Binding::Value(value)))),
        nd.capture.map(|cap| (cap, Binding::Node(ir_node))),
    ];

    // Alternation (`one_of`): `inputs` are independent alternative
    // sub-patterns, not operands, all tried against the SAME `ir_node`. Each
    // alternative's own `try_match_at` handles its captures / guard / footprint,
    // so the alternation node records nothing itself.
    //
    // This is an ordered choice, not a union: once an arm matches, later arms
    // are shadowed by design. So enumeration runs WITHIN the winning arm, never
    // across arms; otherwise a trailing wildcard arm would add a spurious
    // second binding to every hit of a specific earlier arm. An enumerating `k`
    // makes the return value useless for "did this arm match", hence `reached`.
    if nd.alternation {
        let mark = bindings.mark();
        if !bind_all_captures(bindings, &cap_bindings) {
            return false;
        }
        for alt in &inputs {
            let inner = bindings.mark();
            let mut reached = false;
            {
                let mut k_alt = |b: &mut Bindings| -> bool {
                    reached = true;
                    k(b)
                };
                if try_match_at(
                    ctx,
                    alt.producer,
                    ir_node,
                    root_value,
                    Some(alt.out_vertex),
                    bindings,
                    &mut k_alt,
                ) {
                    return true;
                }
            }
            bindings.restore(inner);
            if reached {
                // This arm matched and was fully enumerated; the rest are
                // shadowed.
                break;
            }
        }
        bindings.restore(mark);
        return false;
    }

    // Fixed-slot operands are assigned before the existential (`any_input`)
    // ones. Stable, so each group keeps its relative order.
    inputs.sort_by_key(|e| e.consumer_slot == crate::matcher::ANY_INPUT_SLOT);
    let n_fixed = inputs
        .iter()
        .filter(|e| e.consumer_slot != crate::matcher::ANY_INPUT_SLOT)
        .count();

    // Commutativity needs exactly two fixed operands at slots {0,1} (the shape
    // of every commutative IR kind's signature), a commutative IR kind, and no
    // `.ordered()`. Otherwise both operands stay on `Candidates::Only`, i.e.
    // one ordering.
    let commutative = !nd.force_ordered
        && n_fixed == 2
        && inputs[..2]
            .iter()
            .all(|e| e.consumer_slot < COMM_ORDER.len())
        && ctx.function().node_kind(ir_node).is_commutative();

    // The existential candidate set: every input slot of `ir_node`. Invariant
    // across the search, so collected once here rather than per recursion level
    // (and empty in the common no-`any_input` case).
    //
    // No value-kind filter: the sub-pattern discriminates. A typed sub carries
    // a value output-kind and so skips control/memory/PhiToken inputs, while a
    // bare wildcard (`OutputKindSpec::Any`) binds any input including a
    // `PhiToken`. That one mechanism covers a phi's value predecessors, a
    // region's control predecessors, a mem_phi's memory predecessors, and a
    // call's args.
    let ext_slots: Vec<usize> = if n_fixed == inputs.len() {
        Vec::new()
    } else {
        (0..ctx.function().node_inputs(ir_node).len()).collect()
    };

    let assigns: Vec<Assign> = inputs
        .into_iter()
        .map(|edge| {
            let cands = if edge.consumer_slot == crate::matcher::ANY_INPUT_SLOT {
                Candidates::OneOf(&ext_slots)
            } else if commutative {
                Candidates::OneOf(&COMM_ORDER[edge.consumer_slot])
            } else {
                Candidates::Only(edge.consumer_slot)
            };
            Assign { edge, cands }
        })
        .collect();

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
            if !bind_all_captures(b, &cap_bindings) {
                return false;
            }
            // Secondary (non-anchor) output vertices, checked and bound at
            // each vertex's slot. This is how `If::capture_true` /
            // `capture_false` and the generic `output(j)` builder reach a
            // sibling output; the matcher never descends into a node's outputs
            // otherwise. The anchor output and node captures are handled above.
            for &ov_idx in ctx.pat.graph.produced_outputs(pat_node).iter() {
                if Some(ov_idx) == out_vertex {
                    continue;
                }
                let ov = ctx.pat.graph.output_weight(ov_idx);
                let Some(&val) = ctx.function().node_outputs(ir_node).get(ov.slot) else {
                    // No IR output at this slot. A vertex imposing nothing (a
                    // bare node-rooted `any()`) is vacuously satisfied, which
                    // is what lets `any()` match a value-less `Return`. One
                    // carrying a capture or any constraint cannot be.
                    if vertex_imposes_requirement(ov) {
                        b.restore(inner);
                        return false;
                    }
                    continue;
                };
                if !output_ok(ov, ctx.function(), val) {
                    b.restore(inner);
                    return false;
                }
                if let Some(cap) = ov.capture
                    && !b.bind_capture(cap, Binding::Value(val))
                {
                    b.restore(inner);
                    return false;
                }
            }
            if let Some(pm) = &nd.post_match {
                let ty = root_value
                    .and_then(|value| ctx.function().value_kind(value).as_value())
                    .unwrap_or(ValueType::I1);
                if !pm(ctx.matcher, ir_node, ty, b) {
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
        if match_assignments(ctx, &assigns, 0, ir_node, None, bindings, &mut finalize) {
            return true;
        }
    }
    bindings.restore(mark);
    false
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
/// when the direct sub-pattern yields no configuration at all, never to offer a
/// second binding beside a direct one. Under an enumerating `k` the return
/// value can't tell those apart (it is always `false`), hence the `reached`
/// flag. Fallback-only also bounds enumeration by the PATTERN's commutative
/// nodes; offering the cast-peeled value as an extra binding would multiply
/// matches by the GRAPH's cast-chain depth.
fn try_operand(
    ctx: &Ctx,
    edge: &InputEdge,
    value: ValueId,
    bindings: &mut Bindings,
    k: &mut dyn FnMut(&mut Bindings) -> bool,
) -> bool {
    let mark = bindings.mark();
    let producer_ir = ctx.function().producer(value);
    let mut reached = false;
    {
        let mut k_direct = |b: &mut Bindings| -> bool {
            reached = true;
            k(b)
        };
        if try_match_at(
            ctx,
            edge.producer,
            producer_ir,
            Some(value),
            Some(edge.out_vertex),
            bindings,
            &mut k_direct,
        ) {
            return true;
        }
    }
    bindings.restore(mark);
    if reached {
        // Direct producer matched and was fully enumerated; the fallback is
        // only for a mismatch.
        return false;
    }

    // Cast walk-through fallback.
    if ctx.pat.cast_mask.is_empty() {
        return false;
    }
    let mut skipped = Vec::new();
    let unwrapped = skip_casts(ctx.matcher, value, ctx.pat.cast_mask, &mut skipped);
    if unwrapped == value {
        return false;
    }
    for &cast in &skipped {
        bindings.record_matched(cast);
    }
    let unwrapped_ir = ctx.function().producer(unwrapped);
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
    false
}

struct InputEdge {
    consumer_slot: usize,
    out_vertex: PatValueId,
    producer: PatNodeId,
}

/// The IR input slots one pattern input may occupy. The matcher's three
/// disciplines differ only in this set:
///
/// | pattern input | candidate set |
/// |---|---|
/// | fixed, non-commutative | `Only(its own consumer slot)` |
/// | fixed, commutative pair | `OneOf([own slot, the other])`, injectivity yielding exactly the 2 orderings |
/// | existential (`any_input`) | `OneOf(every input slot)` |
#[derive(Clone, Copy)]
enum Candidates<'a> {
    /// The common case, and why this is an enum rather than a uniform slice:
    /// [`match_assignments`] serves it with one direct `node_input_id_at`
    /// index, no candidate loop, no injectivity check, no allocation. A
    /// one-element `OneOf` would be correct but trades an indexed lookup for a
    /// scan, so keep the singleton arm.
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

struct Assign<'a> {
    edge: InputEdge,
    cands: Candidates<'a>,
}

/// Slots already claimed by enclosing [`Candidates::OneOf`] assignments, as a
/// borrowed cons list living in the recursion's own stack frames: the chain is
/// bounded by the input count, so injectivity costs no allocation and nothing
/// needs threading back out through the continuation closures.
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

/// Continuation-passing injective assignment of `assigns[i..]` to `ir_node`'s
/// input slots, invoking `done` once every input is placed and matched.
///
/// Each input's sub-match recurses with a continuation assigning the remaining
/// inputs, so a later rejection asks an earlier input for its next candidate
/// before this call fails. That is the backtracking that lets a parent guard
/// re-drive a child's commutative ordering. Every candidate goes through
/// [`try_operand`], so an existential honours `ignore_casts` like a fixed slot.
fn match_assignments(
    ctx: &Ctx,
    assigns: &[Assign],
    i: usize,
    ir_node: NodeId,
    claimed: Option<&Claimed>,
    bindings: &mut Bindings,
    done: &mut dyn FnMut(&mut Bindings) -> bool,
) -> bool {
    let Some(a) = assigns.get(i) else {
        return done(bindings);
    };
    match a.cands {
        // Direct index, no search and no injectivity bookkeeping. The slot is
        // pinned, so a failed operand fails it outright.
        Candidates::Only(slot) => {
            let Some(value) = input_at(ctx, ir_node, slot) else {
                return false;
            };
            try_operand(ctx, &a.edge, value, bindings, &mut |b| {
                match_assignments(ctx, assigns, i + 1, ir_node, claimed, b, done)
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
                if try_operand(ctx, &a.edge, value, bindings, &mut |b| {
                    match_assignments(ctx, assigns, i + 1, ir_node, Some(&next), b, done)
                }) {
                    return true;
                }
                // Bindings already restored; try the next slot.
            }
            false
        }
    }
}

/// Bind every present capture all-or-nothing: on the first rebind conflict,
/// roll back what THIS call bound and return `false`. It takes its own rollback
/// mark, which restores to the same point a caller's would, since no mutation
/// happens between the caller's mark and this call.
fn bind_all_captures(b: &mut Bindings, caps: &[Option<(crate::Capture, Binding)>]) -> bool {
    let mark = b.mark();
    for &(cap, binding) in caps.iter().flatten() {
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

/// Whether the vertex constrains anything at all: a capture to bind, or a kind
/// / width / slot filter. A vertex imposing nothing is the bare `any()`
/// wildcard, vacuously satisfied node-rooted against a value-less node, which
/// is what lets `any()` match a `Return`.
fn vertex_imposes_requirement(o: &PatValue) -> bool {
    o.capture.is_some()
        || o.width.is_some()
        || o.match_slot.is_some()
        || !matches!(o.kind, OutputKindSpec::Any)
}

fn output_ok(o: &PatValue, f: &strider_ir::Function, value: ValueId) -> bool {
    let val = f.value_kind(value).as_value();
    let kind_ok = match &o.kind {
        // A `width` constraint below can still narrow this to a value.
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
