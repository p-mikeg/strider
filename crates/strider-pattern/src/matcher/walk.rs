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
//! Matching a node's inputs is ONE problem — an injective assignment of the pat
//! node's inputs to the IR node's input slots, each input drawing from its own
//! [`Candidates`] set — solved by the single enumerator [`match_assignments`].
//! A fixed operand's candidate set is the singleton `{its own slot}` (a direct
//! index); an arity-2 pat node whose IR kind is commutative (per
//! `NodeKind::is_commutative()`, unless it opted out via `force_ordered`) gives
//! BOTH operands the candidate set `{0, 1}`, so injectivity yields exactly the
//! natural ordering then the swapped one; an `any_input` existential draws from
//! the IR kind's variadic-tail value slots (those past its fixed-arity prefix).
//! Backtracking rolls back via
//! [`Bindings::mark`] / [`Bindings::restore`] between attempts.
//!
//! The engine is continuation-passing: [`try_match_at`] ENUMERATES a pat
//! node's matches (each operand ordering × every descendant configuration)
//! and calls a continuation `k` per configuration instead of returning the
//! first hit. So when a guard ANYWHERE above rejects a configuration (its
//! `k` returns `false`), a commutative node BELOW it re-drives its operand
//! order to find one the ancestor accepts — e.g. a `when_match` guard on a
//! unary parent picks which operand of a commutative child binds. The root
//! continuation is supplied by the caller ([`Matcher`]): a first-hit collector
//! returns `true` and stops at the first fully guard-satisfying configuration
//! in DFS order, while `find_all`'s collector records each and returns `false`
//! to enumerate the rest — so several distinct bindings per root are reported
//! instead of silently dropping all but the first.
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

/// Invariant context threaded by `&` through the entire recursive match.
///
/// `matcher` (IR access + the match-time predicate closures) and `pat` (the
/// pattern graph and its `cast_mask`) are FIXED for a whole `try_match*` call —
/// every recursive call and every continuation sees the same two. Bundling them
/// into one borrowed `Ctx` lets the recursion carry a single pointer instead of
/// re-threading the pair at every frame (no allocation, no boxing, no added dyn
/// dispatch beyond the one shared reference).
struct Ctx<'a> {
    matcher: &'a Matcher<'a>,
    pat: &'a Pattern,
}

impl Ctx<'_> {
    /// The IR function under match (shorthand for `self.matcher.function()`).
    #[inline]
    fn function(&self) -> &strider_ir::Function {
        self.matcher.function()
    }
}

/// Entry point for a value-rooted attempt: try `pat`'s root pat node
/// against the IR node producing `root_value`, with `root_value` available
/// for the root output's declarative constraints and the root capture.
///
/// `k` is the root continuation, invoked once per fully guard-satisfying
/// configuration (each operand ordering × every descendant configuration).
/// Returning `true` accepts that configuration and stops the search (the
/// first-hit collector); returning `false` drives the next one, which is how a
/// caller ENUMERATES every distinct binding rather than only the first.
pub(crate) fn try_match(
    matcher: &Matcher,
    pat: &Pattern,
    root: PatNodeId,
    root_value: ValueId,
    bindings: &mut Bindings,
    k: &mut dyn FnMut(&mut Bindings) -> bool,
) -> bool {
    // The root output vertex (if the root pat node declares one) carries
    // the root-level output constraints. For a value root — exactly one
    // value output vertex — that vertex's constraint applies to whichever
    // output is currently being matched (`root_value`), regardless of slot.
    let root_out_vertex = root_output_vertex_for(pat, root, matcher, root_value);
    let root_node = matcher.function().producer(root_value);
    let ctx = Ctx { matcher, pat };
    // `try_match_at` enumerates every configuration whose every guard (root +
    // descendants) passed and hands each to `k`, which decides whether to
    // accept-and-stop or record-and-continue.
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

/// Entry point for a zero-value-output attempt (e.g. `Return`): try
/// `pat`'s root pat node against `node` with no associated output.
///
/// A zero-output IR node produces no value, so it can only satisfy a root
/// whose output vertex imposes no value requirement (a bare `any()` /
/// `var()` wildcard, or a control/zero-output control builder). A root
/// that requires a value output — a pinned `Value` kind or any `width` —
/// is rejected here rather than silently skipping the constraint (which
/// is how `bool_value()` used to wrongly match `Return`).
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
/// *whichever* output is being matched — [`Matcher::matches`] iterates
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
/// [`Matcher::matches`]: crate::Matcher::matches
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

    // Root-output constraints (kind / width). The output vertex carries the
    // declarative shape constraints (e.g. `bool_*` builders pin `Value(I1)`;
    // `value_of_width` pins width).
    if let Some(ov_idx) = out_vertex
        && let Some(value) = root_value
    {
        let ov = ctx.pat.graph.output_weight(ov_idx);
        if !output_ok(ov, ctx.function(), value) {
            return false;
        }
    }

    // Node predicate. Fires after kind + output constraints and BEFORE
    // descending into inputs — node-only predicates short-circuit here.
    if let Some(predicate) = &nd.node_predicate
        && !predicate(ctx.matcher, ir_node)
    {
        return false;
    }

    // Collect this pat node's inputs: each incoming `Consumes{slot}` edge
    // source is a PatValue vertex; that vertex's incoming `Produces`
    // edge source is the producer pat node.
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
            .and_then(|ov| ctx.pat.graph.output_weight(ov).capture)
            .and_then(|cap| root_value.map(|value| (cap, Binding::Value(value)))),
        nd.capture.map(|cap| (cap, Binding::Node(ir_node))),
    ];

    // Alternation (`one_of`): `inputs` are independent alternative sub-patterns,
    // not operands. Bind this node's own capture (the matched value) once, then
    // try each alternative against the SAME `ir_node`, accepting the first whose
    // whole configuration (its own guards + the ancestor continuation `k`)
    // succeeds. Each alternative's `try_match_at` handles its captures / guard /
    // footprint, so the alternation node records nothing itself.
    //
    // `one_of` is an ORDERED CHOICE (first-match-wins), not a union: once an arm
    // matches, the later arms are shadowed by design — a permissive leading arm
    // deliberately swallows what a narrower later arm would have caught. So the
    // enumeration runs WITHIN the winning arm (every operand ordering it
    // admits), never ACROSS arms; otherwise a trailing wildcard arm would add a
    // spurious second binding to every hit of a specific earlier arm. As in
    // `try_operand`, an enumerating `k` makes `try_match_at`'s return value
    // useless for "did this arm match", hence the explicit `reached` flag.
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
                // This arm matched and every configuration of it went through
                // `k`; first-match-wins means the remaining arms are shadowed.
                break;
            }
        }
        bindings.restore(mark);
        return false;
    }

    // Order the fixed-slot operands ahead of the existential (`any_input`) ones
    // — a stable sort, so each group keeps its relative order — and count them:
    // the fixed operands are assigned their slots first, as before.
    inputs.sort_by_key(|e| e.consumer_slot == crate::matcher::ANY_INPUT_SLOT);
    let n_fixed = inputs
        .iter()
        .filter(|e| e.consumer_slot != crate::matcher::ANY_INPUT_SLOT)
        .count();

    // Commutativity: exactly two fixed operands, at slots {0,1} (the shape of
    // every commutative IR kind's signature), IR kind commutative, not
    // force_ordered. `.ordered()` sets `force_ordered`, which leaves both
    // operands on `Candidates::Only` — one ordering, exactly as before.
    let commutative = !nd.force_ordered
        && n_fixed == 2
        && inputs[..2]
            .iter()
            .all(|e| e.consumer_slot < COMM_ORDER.len())
        && ctx.function().node_kind(ir_node).is_commutative();

    // The existential candidate set: `ir_node`'s value input slots. Invariant
    // across the whole search, so collect it once here (empty in the common
    // no-`any_input` case) rather than at every recursion level.
    //
    // `any_input` ranges over the IR kind's variadic TAIL — the value inputs
    // past its fixed-arity prefix (per `node_signature::expected_signature`).
    // The prefix carries a kind's bookkeeping / structural operands (a
    // `Phi`/`MemPhi`'s slot-0 `PhiToken`; a `Call`'s `[ctrl, mem, target, sp]`),
    // which are NOT existential-input candidates. This must be dropped here
    // rather than left to the sub-pattern's own output-kind check, because the
    // kind-unconstrained wildcards (`any()` / `var(c)`) carry
    // `OutputKindSpec::Any` and would bind a prefix operand — and being at a low
    // slot, it would shadow the real tail inputs. (The wildcards' `Any` output
    // kind is deliberate and load-bearing elsewhere — `with_true(any())` matches
    // a `Region`; a bare `any()` root matches zero-output nodes — so the guard
    // belongs here, not on them.)
    //
    // For `Phi` the prefix is exactly `[PhiToken]` (length 1), so this reduces
    // to the historical "skip the slot-0 phi-token" rule. But the prefix isn't
    // the whole story: `Region`'s variadic TAIL is itself `Control` (its
    // predecessor edges) and `MemPhi`'s is `Memory` (its per-predecessor memory
    // defs) — neither has a fixed-arity prefix to exclude, so a slot-based
    // filter alone would leak a Control/Memory token into a value existential.
    // The correct filter is therefore positive: keep only REAL VALUE kinds
    // (`ValueKind::Typed(_)` — integers incl. I1, floats), which as a
    // consequence also excludes `PhiToken` (there is no variadic tail slot
    // that is a phi-token, so this subsumes the old exclusion-only guard).
    let ext_slots: Vec<usize> = if n_fixed == inputs.len() {
        Vec::new()
    } else {
        let prefix = strider_ir::fixed_input_prefix_len(ctx.function().node_kind(ir_node));
        ctx.function()
            .node_inputs(ir_node)
            .into_iter()
            .enumerate()
            .filter(|&(slot, _)| slot >= prefix)
            .filter(|&(_, v)| matches!(ctx.function().value_kind(v), ValueKind::Typed(_)))
            .map(|(slot, _)| slot)
            .collect()
    };

    // The one enumeration problem: assign each pattern input an IR input slot
    // drawn from its own candidate set, injectively.
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
        // `finalize` finishes THIS node once every input has been assigned a
        // slot and matched there: bind the node's capture (capture-equality is
        // enforced here against deeper bindings), run its guard, record it into
        // the footprint, then hand off to the parent continuation `k`. Returning
        // `false` makes the assignment enumeration try its next configuration (a
        // deeper commutative swap, this node's swap, or an existential's next
        // slot) — so an ANCESTOR guard failure surfaced through `k` re-drives
        // this node and everything beneath it.
        let mut finalize = |b: &mut Bindings| -> bool {
            let inner = b.mark();
            if !bind_all_captures(b, &cap_bindings) {
                return false;
            }
            // Bind captures on this node's SECONDARY (non-anchor) output
            // vertices to the matched IR node's output value at each vertex's
            // slot — how `If::capture_true` / `capture_false` bind their
            // control-output values (the matcher never descends into a node's
            // outputs otherwise). The anchor output's capture (and the node
            // capture) are handled by the `cap_bindings` loop above.
            for &ov_idx in ctx.pat.graph.produced_outputs(pat_node).iter() {
                if Some(ov_idx) == out_vertex {
                    continue;
                }
                let ov = ctx.pat.graph.output_weight(ov_idx);
                if let Some(cap) = ov.capture {
                    let Some(&val) = ctx.function().node_outputs(ir_node).get(ov.slot) else {
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

/// Try `edge`'s sub-pattern against the operand `value`, then — if the pattern's
/// cast mask permits and `value` is a registered cast — against the unwrapped
/// value. `k` continues the enclosing match (the remaining input
/// assignments). Shared by both [`Candidates`] arms, which differ only
/// in what they do when this returns `false`: a singleton fails its pinned slot,
/// a multi-candidate set tries its next slot.
///
/// On every failure path `bindings` is restored to its state at entry, so a
/// caller may retry without cleaning up. The skipped casts are journaled BEFORE
/// the retry, so a failure rolls them back while a success keeps them — which is
/// what holds the asm-fingerprint superset contract.
///
/// The cast walk-through is a **fallback**, not an alternative: it engages only
/// when the direct sub-pattern yields NO configuration at all, never to offer a
/// second binding alongside a direct one that already matched. `try_match_at`'s
/// return value can't decide that under an enumerating `k` (which returns
/// `false` for every configuration, so the call always reports `false`), hence
/// the explicit `reached` flag — "did any configuration reach the
/// continuation", which is what "mismatch" has always meant here. Keeping this a
/// fallback also keeps enumeration bounded by the PATTERN's commutative nodes:
/// offering the cast-peeled value as an extra binding would instead multiply
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
        // The direct producer matched (and every configuration was enumerated
        // through `k`); the fallback exists for a mismatch, so stop here.
        return false;
    }

    // Cast walk-through fallback.
    if ctx.pat.cast_mask.is_empty() {
        return false;
    }
    let mut skipped = Vec::new();
    let unwrapped = skip_casts(ctx.matcher, value, ctx.pat.cast_mask, &mut skipped);
    if unwrapped == value {
        // Not a registered cast — no further fallback.
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

/// One incoming input of a consumer pat node.
struct InputEdge {
    consumer_slot: usize,
    out_vertex: PatValueId,
    producer: PatNodeId,
}

/// The IR input slots ONE pattern input is allowed to occupy.
///
/// This is the whole abstraction: matching a consumer's inputs is an injective
/// assignment of its pattern inputs to `ir_node`'s input slots, and the three
/// disciplines the matcher needs differ only in this candidate set —
///
/// | pattern input | candidate set |
/// |---|---|
/// | fixed, non-commutative | `Only(its own consumer slot)` |
/// | fixed, commutative pair | `OneOf([own slot, the other])` — injectivity yields exactly the 2 orderings |
/// | existential (`any_input`) | `OneOf(every non-`PhiToken` value slot)` |
#[derive(Clone, Copy)]
enum Candidates<'a> {
    /// Exactly one slot. This is the COMMON case (most patterns are all
    /// fixed-slot), and it is why this is an enum rather than a uniform slice:
    /// [`match_assignments`] serves it with a single direct `node_input_id_at`
    /// index — no candidate loop, no injectivity check, no allocation. Modelling
    /// it as a one-element `OneOf` would be correct but would trade an indexed
    /// lookup for a scan, so a singleton MUST stay its own arm.
    Only(usize),
    /// An ordered candidate list; the input takes the first slot that is both
    /// unclaimed by an enclosing assignment and matches the sub-pattern.
    OneOf(&'a [usize]),
}

/// Candidate slots for a commutative operand pair, indexed by the pattern
/// input's OWN consumer slot: each operand tries its own slot first, so
/// injectivity enumerates the natural ordering before the swapped one no matter
/// which of the two operands the assignment visits first.
const COMM_ORDER: [[usize; 2]; 2] = [[0, 1], [1, 0]];

/// One pattern input plus the slots it may be assigned to.
struct Assign<'a> {
    edge: InputEdge,
    cands: Candidates<'a>,
}

/// The slots already claimed by the enclosing [`Candidates::OneOf`]
/// assignments, as a borrowed cons list rooted in the recursion's own stack
/// frames: the chain is bounded by the input count and each link lives in the
/// frame that claimed it, so injectivity costs no allocation and nothing has to
/// be threaded back out through the continuation closures.
///
/// Distinctness is a property of the ASSIGNMENT — by slot index, never by value
/// — so `any_input(1).any_input(1)` still matches a phi with two separate `1`
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

/// The one enumerator: continuation-passing injective assignment of
/// `assigns[i..]` to `ir_node`'s input slots, invoking `done` once every input
/// is placed and matched.
///
/// Each input's sub-match recurses with a continuation that assigns the
/// REMAINING inputs, so when a later stage rejects (`done`, or a deeper
/// continuation, returns `false`) an earlier input is asked for its next
/// candidate before this call fails — the backtracking that lets a parent guard
/// re-drive a child's commutative ordering. Every candidate goes through the
/// shared [`try_operand`], so an existential honours `ignore_casts` exactly like
/// a fixed slot.
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
        // Singleton: a direct index, no search and no injectivity bookkeeping.
        // The slot is pinned, so a failed operand fails the whole slot.
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
                // This candidate didn't work out (bindings already restored) —
                // try the next slot.
            }
            false
        }
    }
}

/// Bind every present capture in `caps` all-or-nothing: on the first rebind
/// conflict, roll back the captures already bound by THIS call and return
/// `false`. Allocation-free — it takes its own rollback mark, which (because no
/// mutation happens between a caller's own mark and this call) restores to the
/// same point the caller would. The two backtracking sites — the alternation
/// branch and the `finalize` continuation — bind the same `cap_bindings` pair
/// this way.
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

/// The value feeding `ir_node`'s input slot `slot`, or `None` when it has no
/// such slot.
fn input_at(ctx: &Ctx, ir_node: NodeId, slot: usize) -> Option<ValueId> {
    let use_id = ctx.function().node_input_id_at(ir_node, slot).ok()?;
    Some(ctx.function().graph().value_of_use(use_id))
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
