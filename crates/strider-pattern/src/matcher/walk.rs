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
//! inputs to IR input slots, each input drawing from its own candidate set,
//! solved by [`match_assignments`]. A fixed operand's set is a singleton; a
//! commutative arity-2 node gives both operands `{0, 1}`, so injectivity yields
//! exactly the natural then the swapped ordering; an `any_input` existential
//! draws from every slot and lets the sub-pattern discriminate (a value sub
//! cannot match a `PhiToken`/control/memory slot). Backtracking rolls back via
//! [`Bindings::mark`] / [`Bindings::restore`].
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
//! Under a non-empty [`CastMask`](crate::matcher::CastMask), an operand that
//! is a masked cast is also tried at the cast's value input: an ordered
//! choice the caller settles outermost first, as it settles a `first_of`.
//!
//! [`PatValue`]: crate::matcher::PatValue
//! [`PatNode`]: crate::matcher::PatNode

use std::cell::{Cell, RefCell};

use either::Either;
use rustc_hash::FxHashMap;
use strider_graph::{NodeId as PatNodeId, ValueId as PatValueId};
use strider_ir::IRViewer;
use strider_ir::node::{NodeId, ValueId, ValueKind};
use strider_ir::walk::{CastMask, cast_mask_of};

use crate::bindings::{Binding, Bindings};
use crate::matcher::{InputSlot, Matcher, OutputKindSpec, PatValue, Pattern, Role, SlotMode};

struct Ctx<'a> {
    matcher: &'a Matcher<'a>,
    pat: &'a Pattern,
    /// Configurations of this attempt that reached the root continuation,
    /// i.e. produced a match; the pruned `first_of` cuts on it. Local to the
    /// attempt, so a query that caller logic (`when_match`, a
    /// `JoinPredicate`) runs on the same `Matcher` counts in its own.
    satisfied: Cell<u64>,
    /// Per [`Offer`] of a `first_of`, the lowest arm that produced a match in
    /// this attempt.
    first_match_cut: RefCell<FxHashMap<Offer, usize>>,
    /// Kept only for a pattern holding a `first_of` or a cast mask.
    trail: Option<&'a ArmTrail>,
}

/// What an [`ArmKey`] chooses between: a `first_of`'s arms, or the cast
/// levels of an operand under the cast mask, outermost first. The `first_of`s
/// settle first.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum Choice {
    FirstOf,
    Cast,
}

/// A pat node and the IR node and value it was offered.
pub(crate) type Offer = (PatNodeId, NodeId, Option<ValueId>);

/// The choice, where it was offered (an operand's outermost level for a
/// cast), and the root output the attempt anchored on.
pub(crate) type ArmKey = (Choice, Offer, Option<ValueId>);

/// An [`ArmKey`] and the arm taken there.
pub(crate) type ArmTaken = (ArmKey, usize);

/// The `first_of` arms and cast levels the configuration being matched has
/// taken.
#[derive(Default)]
pub(crate) struct ArmTrail {
    /// The root output the current attempt anchored on.
    anchor: Cell<Option<ValueId>>,
    pub(crate) arms: RefCell<Vec<ArmTaken>>,
}

impl Ctx<'_> {
    #[inline]
    fn function(&self) -> &strider_ir::Function {
        self.matcher.function()
    }
}

/// Every attempt of `pat`'s root at `node`: once per output whatever its kind
/// (a `Store` roots through its Memory output), less the outputs
/// [`anchor_repeats`] shows impose the attempt already made, and node-rooted
/// only when the node has no outputs at all.
///
/// `k` runs once per fully guard-satisfying configuration, `trail` then holding
/// the `first_of` arms and cast levels it took. `true` accepts and stops the
/// search; `false` drives the next one, which is how a caller enumerates every
/// distinct binding rather than only the first. Each output gets its own
/// [`Ctx`], so a root `first_of` cut does not carry over.
pub(crate) fn anchor(
    matcher: &Matcher,
    pat: &Pattern,
    root: PatNodeId,
    node: NodeId,
    bindings: &mut Bindings,
    trail: Option<&ArmTrail>,
    k: &mut dyn FnMut(&mut Bindings) -> bool,
) -> bool {
    let mut attempt = |root_value: Option<ValueId>, bindings: &mut Bindings| {
        let out_vertex = root_value.and_then(|v| root_output_vertex_for(pat, root, matcher, v));
        if let Some(t) = trail {
            t.anchor.set(root_value);
        }
        let ctx = Ctx {
            matcher,
            pat,
            satisfied: Cell::new(0),
            first_match_cut: RefCell::default(),
            trail,
        };
        let mut counted = |b: &mut Bindings| {
            ctx.satisfied.set(ctx.satisfied.get() + 1);
            k(b)
        };
        let site = Site {
            pat_node: root,
            ir_node: node,
            root_value,
            out_vertex,
        };
        try_match_at(&ctx, site, bindings, &mut counted)
    };
    let outputs = matcher.function().node_outputs(node);
    if outputs.is_empty() {
        // A root demanding a value output cannot match a zero-output node. A
        // `var()` root is rejected too, but not by this test, which reads width
        // and kind: its capture is what `bind_sibling_outputs` refuses.
        let demands_value = pat.graph.node_outputs(root).iter().any(|&ov| {
            let o = pat.graph.value_kind_ref(ov);
            o.width.is_some()
                || matches!(o.kind, OutputKindSpec::Value(_) | OutputKindSpec::AnyValue)
        });
        return !demands_value && attempt(None, bindings);
    }
    // An output whose anchor repeats the one already tried imposes the same
    // constraint set, so it can only re-derive bindings already reported.
    //
    // `if_else()` declares a vertex per control slot and
    // `root_output_vertex_for` picks by slot, so an `If` root's two outputs
    // resolve to DIFFERENT vertices; what skips the second is
    // `anchor_repeats`' `as_value()` clause, not its `prev == cur` one, which
    // would answer "not a repeat" for one shared vertex anyway.
    let mut anchored: Option<ValueId> = None;
    for &out_id in outputs {
        if let Some(prev) = anchored
            && anchor_repeats(matcher, pat, root, prev, out_id)
        {
            continue;
        }
        anchored = Some(out_id);
        if attempt(Some(out_id), bindings) {
            return true;
        }
    }
    false
}

/// A necessary condition for `pat` to match at `node`: the root's kind spec
/// admits it and, one level down, each fixed operand's producer passes its pat
/// node's kind and node predicates at the operand's slot, or at the other one
/// of an unordered pair. The operands are not checked under a cast mask, and
/// an alternation root, whose inputs are arms, admits everything.
pub(crate) fn may_match_at(m: &Matcher, pat: &Pattern, root: PatNodeId, node: NodeId) -> bool {
    let nd = pat.graph.node_kind(root);
    let f = m.function();
    let unordered = nd.role == (Role::Operands { ordered: false });
    let admits = |op: &crate::matcher::PatNode, slot| {
        f.nth_input(node, slot)
            .map(|v| f.producer(v))
            .is_some_and(|p| {
                op.kind.matches(f.node_kind(p)) && op.node_predicates.iter().all(|pr| pr(m, p))
            })
    };
    let fixed_ok = |e: &InputEdge| match e.consumer_slot {
        InputSlot::At(s) => {
            let op = pat.graph.node_kind(e.producer);
            admits(op, s) || (unordered && s < COMM_ORDER.len() && admits(op, 1 - s))
        }
        InputSlot::Any => true,
    };
    nd.is_alternation()
        || (nd.kind.matches(f.node_kind(node))
            && (!pat.cast_mask.is_empty() || pat.inputs_of(root).edges.iter().all(fixed_ok)))
}

/// The root pat node's output vertex carrying the root-level constraints to
/// check against `root_value`; `None` if it declares no output vertex.
///
/// A sole output vertex is taken whatever slot `root_value` sits at: its
/// constraint applies to the output being matched, not to a fixed slot. A
/// vertex that does mean a fixed slot is [`SlotMode::Pinned`], which
/// [`output_ok`] then enforces. With several vertices (`if_else`, `call`,
/// `region`, `entry`, `load`, `phi`, ...) the slots pick between them.
pub(crate) fn root_output_vertex_for(
    pat: &Pattern,
    root: PatNodeId,
    matcher: &Matcher,
    root_value: ValueId,
) -> Option<PatValueId> {
    let outs = pat.graph.node_outputs(root);
    if let [only] = *outs {
        return Some(only);
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
/// A vertex shared between the outputs, a [`SlotMode::Enumerated`] one
/// (fixed to the anchored output as an anchor, enumerated as a sibling), and a
/// differing anchor value type (which a post-match guard reads) each tell them
/// apart.
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
        || pat.graph.value_kind_ref(prev).slot_mode == SlotMode::Enumerated
        || pat.graph.value_kind_ref(cur).slot_mode == SlotMode::Enumerated
    {
        return false;
    }
    let f = matcher.function();
    f.value_kind(anchored).as_value() == f.value_kind(out_id).as_value()
}

/// A pat node offered an IR node and, at a value-producing position, the IR
/// output and the pat-output vertex it is matched as.
#[derive(Clone, Copy)]
struct Site {
    pat_node: PatNodeId,
    ir_node: NodeId,
    root_value: Option<ValueId>,
    out_vertex: Option<PatValueId>,
}

/// Recursive worker, continuation-passing so a guard failure anywhere above can
/// re-drive a commutative operand order below it.
///
/// Enumerates every way `site`'s pat node matches its IR node; for each it
/// extends `bindings` with that configuration's captures and footprint and
/// calls `k`. `k` returning `true` accepts and leaves `bindings` in the
/// accepted state; `false` tries the next configuration. If none is accepted,
/// `bindings` is restored to entry.
fn try_match_at(
    ctx: &Ctx,
    site: Site,
    bindings: &mut Bindings,
    k: &mut dyn FnMut(&mut Bindings) -> bool,
) -> bool {
    let Site {
        pat_node,
        ir_node,
        root_value,
        out_vertex,
    } = site;
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
    if !nd.node_predicates.iter().all(|p| p(ctx.matcher, ir_node)) {
        return false;
    }

    // Resolved once at seal: neither the edge list, its ordering nor the fixed
    // count depends on the IR node, and a match attempt re-enters this function
    // once per operand ordering.
    let node_inputs = ctx.pat.inputs_of(pat_node);
    let inputs = &node_inputs.edges;
    let n_fixed = node_inputs.fixed;

    if nd.is_alternation() {
        return try_alternation(ctx, site, bindings, k);
    }

    // Commutativity needs one or two fixed operands at slots {0,1} (the shape
    // of every commutative IR kind's signature), a commutative IR kind, and no
    // `.ordered()`. A single pinned operand reaches both slots too, or the same
    // query answers differently by which slot it names. Otherwise operands stay
    // on their pinned slot, i.e. one ordering.
    //
    // Both orderings are re-driven under an enumerating continuation, so a
    // commutative node whose two operands EACH satisfy both sub-patterns
    // doubles, and nesting multiplies: 2^k for k such nodes under `find_all`,
    // whose continuation returns `false` and drives every configuration.
    // `NodeInputs::interchangeable` cuts the pairs that cannot tell the two
    // orderings apart, which is every bare-wildcard-leaved tree, so what is
    // left to pay for is operands differing by capture, predicate or kind;
    // `.ordered()` pins a pair for the rest. Both IR slots holding one value
    // makes the swap assign every operand what the natural order did, so it is
    // not tried.
    let commutative = nd.role == (Role::Operands { ordered: false })
        && (1..=COMM_ORDER.len()).contains(&n_fixed)
        && !node_inputs.interchangeable
        && inputs[..n_fixed]
            .iter()
            .all(|e| matches!(e.consumer_slot, InputSlot::At(s) if s < COMM_ORDER.len()))
        && ctx.function().node_kind(ir_node).is_commutative()
        && ctx.function().nth_input(ir_node, 0) != ctx.function().nth_input(ir_node, 1);

    let assignment = Assignment {
        edges: inputs,
        fixed: n_fixed,
        commutative,
    };

    let mark = bindings.mark();
    {
        // Finishes THIS node once every input is assigned and matched: bind
        // its capture (capture-equality is enforced here against deeper
        // bindings), run its guard, record it into the footprint, hand off to
        // `k`. Returning `false` makes the enumeration try its next
        // configuration, so an ancestor guard failure surfaced through `k`
        // re-drives this node and everything beneath it.
        // A rejection leaves `b` to the enumeration point that resumes, which
        // restores to a mark taken before this node's operands.
        let mut finalize = |b: &mut Bindings| {
            bind_all_captures(ctx, site, b) && bind_sibling_outputs(ctx, site, 0, b, k)
        };
        if match_assignments(ctx, &assignment, 0, ir_node, None, bindings, &mut finalize) {
            return true;
        }
    }
    bindings.restore(mark);
    false
}

/// `one_of` / `first_of`: the inputs are independent alternative sub-patterns,
/// not operands, all tried against the SAME `ir_node`. Both enumerate every
/// matching arm, `first_of` recording the arm it took. The pattern's
/// `pruned_first_of` also skips the arms past one that matched.
fn try_alternation(
    ctx: &Ctx,
    site: Site,
    bindings: &mut Bindings,
    k: &mut dyn FnMut(&mut Bindings) -> bool,
) -> bool {
    let Site {
        pat_node, ir_node, ..
    } = site;
    let nd = ctx.pat.graph.node_kind(pat_node);
    let mark = bindings.mark();
    // Bound before any arm runs, where every other kind binds in `finalize`
    // once its inputs have matched, so an arm rebinding this node's capture
    // conflicts rather than shadows.
    if !bind_all_captures(ctx, site, bindings) {
        return false;
    }
    let trail = ctx.trail.filter(|_| nd.role == Role::FirstOf);
    // The caller keeps, per `first_of` and value offered, the matches through
    // the lowest arm taken there, settling the `first_of`s in build order. At
    // the first one settled, reached once per configuration, a match through
    // an arm past one that matched is dropped before it can give a later one
    // its lowest arm, so skipping that arm is exact. Anywhere else such a
    // match may be the one an earlier `first_of` keeps.
    let prunes = ctx.pat.pruned_first_of == Some(pat_node);
    let offer = (pat_node, ir_node, site.root_value);
    let cut = prunes
        .then(|| ctx.first_match_cut.borrow().get(&offer).copied())
        .flatten();
    for (arm, alt) in ctx.pat.inputs_of(pat_node).edges.iter().enumerate() {
        if prunes && cut.is_some_and(|c| arm > c) {
            break;
        }
        let before = ctx.satisfied.get();
        let arm_site = Site {
            pat_node: alt.producer,
            out_vertex: Some(alt.out_vertex),
            ..site
        };
        if let Some(t) = trail {
            t.arms
                .borrow_mut()
                .push(((Choice::FirstOf, offer, t.anchor.get()), arm));
        }
        // The arm's own `try_match_at` restores on every `false` return, so a
        // rejected configuration needs no cleanup here.
        let accepted = try_match_at(ctx, arm_site, bindings, &mut |b| {
            bind_sibling_outputs(ctx, site, 0, b, k)
        });
        if let Some(t) = trail {
            t.arms.borrow_mut().pop();
        }
        if accepted {
            return true;
        }
        // The cut is on a produced match, not on structural arm shape: an arm
        // reaching `k` but rejected by a guard above falls through to the next
        // arm.
        //
        // The cut is scoped to the attempt rather than to one invocation of
        // the alternation, so a sibling operand enumerated around the
        // alternation does not re-offer the arms past it. It can still bring
        // an EARLIER arm a match, which the caller then prefers. The value
        // offered is part of the key because a commutative ordering, an
        // `any_input` slot or another output of one node re-offers the same
        // pat node a DIFFERENT value, for which an earlier arm's match proves
        // nothing.
        if prunes && ctx.satisfied.get() > before {
            ctx.first_match_cut
                .borrow_mut()
                .entry(offer)
                .and_modify(|c| *c = (*c).min(arm))
                .or_insert(arm);
            break;
        }
    }
    bindings.restore(mark);
    false
}

/// What `site` owes once its operands, or its alternation arm, have matched:
/// its output vertices from `idx` on other than the anchoring one, then its
/// post-match predicates, then its footprint entry and `k`. A
/// [`SlotMode::Enumerated`] vertex enumerates the node's outputs, so a capture
/// on it binds each in turn and a rejection anywhere above re-drives it; every
/// other vertex is checked at its own slot. A rejection leaves `b` to the mark
/// its caller took.
fn bind_sibling_outputs(
    ctx: &Ctx,
    site: Site,
    idx: usize,
    b: &mut Bindings,
    k: &mut dyn FnMut(&mut Bindings) -> bool,
) -> bool {
    let Some(&ov_idx) = ctx.pat.graph.node_outputs(site.pat_node).get(idx) else {
        let nd = ctx.pat.graph.node_kind(site.pat_node);
        if !nd.post_matches.is_empty() {
            let ty = site
                .root_value
                .and_then(|value| ctx.function().value_kind(value).as_value());
            if !nd
                .post_matches
                .iter()
                .all(|pm| pm(ctx.matcher, site.ir_node, ty, b))
            {
                return false;
            }
        }
        b.record_matched(site.ir_node);
        return k(b);
    };
    if Some(ov_idx) == site.out_vertex {
        return bind_sibling_outputs(ctx, site, idx + 1, b, k);
    }
    let ov = ctx.pat.graph.value_kind_ref(ov_idx);
    let outs = ctx.function().node_outputs(site.ir_node);
    let slots = if ov.slot_mode == SlotMode::Enumerated {
        0..outs.len()
    } else {
        ov.slot..ov.slot.saturating_add(1)
    };
    let mut any_slot_present = false;
    for slot in slots {
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
        if bind_sibling_outputs(ctx, site, idx + 1, b, k) {
            return true;
        }
        b.restore(here);
    }
    // No output at the slot at all is vacuously satisfied only while the vertex
    // constrains nothing, which is what lets a bare `anything()` match a
    // value-less `Return`; one carrying a capture or a filter rejects instead of
    // leaving that capture unbound.
    if !any_slot_present && !vertex_imposes_requirement(ov) {
        return bind_sibling_outputs(ctx, site, idx + 1, b, k);
    }
    false
}

/// Try `edge`'s sub-pattern against the operand `value`, then, if the cast mask
/// permits and `value` is a registered cast, against the unwrapped value. `k`
/// continues the enclosing match. The candidate sets share this and differ
/// only in their reaction to `false`: a singleton fails its pinned slot, a
/// multi-candidate set tries the next.
///
/// Every failure path restores `bindings` to entry, so a caller may retry
/// without cleanup. Skipped casts are journaled BEFORE the retry, so a failure
/// rolls them back and a success keeps them, which is what holds the
/// asm-fingerprint superset contract.
///
/// The cast walk-through is an ordered choice over the levels, outermost
/// first, like a `first_of` over them: every level runs, and the caller keeps,
/// per operand and among the matches the earlier keys kept, those at the
/// outermost level any of them reached, so a cast sitting on top of an
/// operand never adds a second binding beside a direct one.
fn try_operand(
    ctx: &Ctx,
    edge: &InputEdge,
    value: ValueId,
    bindings: &mut Bindings,
    k: &mut dyn FnMut(&mut Bindings) -> bool,
) -> bool {
    let mark = bindings.mark();
    // A cast mask always comes with a trail; only a cast has a level to choose.
    let choice = ctx
        .trail
        .filter(|_| cast_below(ctx.function(), value, ctx.pat.cast_mask).is_some())
        .map(|t| {
            (
                t,
                (edge.producer, ctx.function().producer(value), Some(value)),
            )
        });
    let mut level = value;
    for depth in 0.. {
        let site = Site {
            pat_node: edge.producer,
            ir_node: ctx.function().producer(level),
            root_value: Some(level),
            out_vertex: Some(edge.out_vertex),
        };
        if let Some((t, offer)) = choice {
            t.arms
                .borrow_mut()
                .push(((Choice::Cast, offer, t.anchor.get()), depth));
        }
        let accepted = try_match_at(ctx, site, bindings, k);
        if let Some((t, _)) = choice {
            t.arms.borrow_mut().pop();
        }
        if accepted {
            return true;
        }
        // Outermost first, so a pattern naming one cast kind under a mask also
        // selecting another matches at the intermediate value.
        let Some((cast, unwrapped)) = cast_below(ctx.function(), level, ctx.pat.cast_mask) else {
            break;
        };
        bindings.record_matched(cast);
        level = unwrapped;
    }
    bindings.restore(mark);
    false
}

/// The cast producing `value` and its value input, when `mask` selects that
/// cast's kind. Casts are the only kind walked through: a pattern crossing a
/// region boundary spells the `Region` node out.
fn cast_below(
    f: &strider_ir::Function,
    value: ValueId,
    mask: CastMask,
) -> Option<(NodeId, ValueId)> {
    if mask.is_empty() {
        return None;
    }
    let cast = f.producer(value);
    let bit = cast_mask_of(f.node_kind(cast));
    if bit.is_empty() || !mask.contains(bit) {
        return None;
    }
    Some((cast, f.node_inputs(cast).into_iter().next()?))
}

pub(crate) struct InputEdge {
    consumer_slot: InputSlot,
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
            edges.sort_by_key(|e| e.consumer_slot == InputSlot::Any);
            let fixed = edges
                .iter()
                .filter(|e| e.consumer_slot != InputSlot::Any)
                .count();
            let interchangeable = compare_operands
                && fixed == 2
                && !graph.node_kind(node).is_alternation()
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
    // A cycle is refused at wiring; the placeholder guards the
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
        && na.node_predicates.is_empty()
        && nb.node_predicates.is_empty()
        && na.post_matches.is_empty()
        && nb.post_matches.is_empty()
        && na.role == nb.role
        && na.input_slots == nb.input_slots
        && na.kind == nb.kind)
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
        && a.slot_mode == b.slot_mode
        && a.token_fallback == b.token_fallback
        && a.kind == b.kind
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
    fixed: usize,
    commutative: bool,
}

impl Assignment<'_> {
    /// Whether an existential may take `slot`: any slot but one a fixed
    /// operand pinned, which on a commutative node is none, its fixed operands
    /// reaching both slots. A pinned slot never extends the `Claimed` chain, so
    /// it is excluded here or it is offered twice.
    ///
    /// No kind filter: the sub-pattern discriminates. A sub carries an
    /// output-kind and reaches only inputs of that kind (a value sub skips
    /// control/memory/PhiToken, a memory sub binds a memory input), while a
    /// bare wildcard (`OutputKindSpec::Any`) binds any input including a
    /// `PhiToken`. `k` existentials enumerate every injective assignment over
    /// these slots, which is what `any_input`'s cost is.
    fn open_to_existential(&self, slot: usize) -> bool {
        // Commutative only on a raw `MatcherBuilder` node whose spec admits a
        // commutative kind: no typed builder puts an existential on one.
        self.commutative
            || !self.edges[..self.fixed]
                .iter()
                .any(|e| e.consumer_slot == InputSlot::At(slot))
    }
}

/// Slots already claimed by enclosing multi-slot assignments.
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
    // The IR input slots the input may occupy, the whole difference between a
    // fixed operand, a commutative pair and an existential. A multi-slot set
    // is ordered: the input takes the first slot both unclaimed by an
    // enclosing assignment and matching the sub-pattern.
    let slots = match edge.consumer_slot {
        // Direct index, no search and no injectivity bookkeeping. The slot is
        // pinned, so a failed operand fails it outright.
        InputSlot::At(slot) if !assignment.commutative => {
            let Some(value) = ctx.function().nth_input(ir_node, slot) else {
                return false;
            };
            return try_operand(ctx, edge, value, bindings, &mut |b| {
                match_assignments(ctx, assignment, i + 1, ir_node, claimed, b, done)
            });
        }
        InputSlot::At(slot) => Either::Left(COMM_ORDER[slot].iter().copied()),
        InputSlot::Any => Either::Right(
            (0..ctx.function().node_inputs(ir_node).len())
                .filter(|&s| assignment.open_to_existential(s)),
        ),
    };
    for slot in slots {
        if Claimed::contains(claimed, slot) {
            continue;
        }
        let Some(value) = ctx.function().nth_input(ir_node, slot) else {
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

/// Bind every capture this pat node declares, all-or-nothing: on the first
/// rebind conflict, roll back what THIS call bound and return `false`.
///
/// An output-vertex capture binds the matched VALUE and needs one, so it is
/// skipped at a node-rooted position; a node-declared capture binds the matched
/// NODE. The vertex's identity pin binds the value too, so a second slot
/// consuming the same vertex conflicts unless it lands on the same value.
fn bind_all_captures(ctx: &Ctx, site: Site, b: &mut Bindings) -> bool {
    let nd = ctx.pat.graph.node_kind(site.pat_node);
    let out_weight = site.out_vertex.map(|ov| ctx.pat.graph.value_kind_ref(ov));
    let vertex_caps: &[crate::Capture] = out_weight.map_or(&[], |ov| ov.captures.as_slice());
    let identity = out_weight.and_then(|ov| ov.identity);
    let value_caps = site.root_value.into_iter().flat_map(move |value| {
        vertex_caps
            .iter()
            .copied()
            .chain(identity)
            .map(move |cap| (cap, Binding::Value(value)))
    });
    let node_caps = nd
        .captures
        .iter()
        .map(|&cap| (cap, Binding::Node(site.ir_node)));
    let mark = b.mark();
    for (cap, binding) in value_caps.chain(node_caps) {
        if !b.bind_capture(cap, binding) {
            b.restore(mark);
            return false;
        }
    }
    true
}

/// Whether the vertex constrains anything at all: a capture or identity pin to
/// bind, or a kind / width / slot filter.
fn vertex_imposes_requirement(o: &PatValue) -> bool {
    !o.captures.is_empty()
        || o.identity.is_some()
        || o.width.is_some()
        || o.slot_mode == SlotMode::Pinned
        || o.kind != OutputKindSpec::Any
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
    };
    kind_ok
        && o.width
            .is_none_or(|w| val.is_some_and(|t| t.bit_width() == w as usize))
        && (o.slot_mode != SlotMode::Pinned || f.value_definition(value).1 as usize == o.slot)
}
