//! Recursive bipartite match engine with commutative-operand retry and cast
//! walk-through.
//!
//! A pat node's inputs are matched as an injective assignment of pattern inputs to IR input
//! slots ([`match_assignments`]): a fixed operand draws from its own slot, a commutative pair from
//! `{0, 1}`, an `any_input` from every slot no fixed operand pins (every slot on a commutative
//! node). Backtracking rolls back via [`Bindings::mark`] / [`Bindings::restore`].
//!
//! The engine is continuation-passing: [`try_match_at`] calls `k` per configuration, so a guard
//! that rejects one re-drives the commutative orderings below it. `k` returning `true` stops the
//! search; `false` (as in `find_all`) enumerates the rest.
//!
//! Under a non-empty [`CastMask`](crate::matcher::CastMask), an operand that is a masked cast is
//! also tried at the cast's value input, outermost level first.

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
    /// Matches this attempt has produced; the pruned `first_of` cuts on it.
    satisfied: Cell<u64>,
    /// Per [`Offer`] of a `first_of`, the lowest arm that produced a match in this attempt.
    first_match_cut: RefCell<FxHashMap<Offer, usize>>,
    /// Present only for a pattern holding a `first_of` or a cast mask.
    trail: Option<&'a ArmTrail>,
}

/// What an [`ArmKey`] chooses between: a `first_of`'s arms, or an operand's cast levels. Ordered
/// so the `first_of`s settle first.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum Choice {
    FirstOf,
    Cast,
}

/// A pat node and the IR node and value it was offered.
pub(crate) type Offer = (PatNodeId, NodeId, Option<ValueId>);

/// A visit's preorder rank in the pattern unfolded through every slot and arm. See
/// [`InputEdge::step`].
pub(crate) type Read = u64;

/// The choice, where it was offered (a cast's outermost level), the anchoring root output, and
/// the read.
pub(crate) type ArmKey = (Choice, Offer, Option<ValueId>, Read);

/// An [`ArmKey`] and the arm taken there.
pub(crate) type ArmTaken = (ArmKey, usize);

/// The `first_of` arms and cast levels the current configuration has taken.
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

/// Attempts `pat`'s root at `node`, once per output of any kind (less [`anchor_repeats`]), or
/// node-rooted when `node` has no outputs.
///
/// `k` runs per guard-satisfying configuration, `trail` holding the arms it took; `true` stops,
/// `false` drives the next. Each output gets its own [`Ctx`], so a `first_of` cut does not carry
/// over.
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
            read: 0,
        };
        try_match_at(&ctx, site, bindings, &mut counted)
    };
    let outputs = matcher.function().node_outputs(node);
    if outputs.is_empty() {
        // A `var()` root passes this test; `bind_sibling_outputs` refuses its capture.
        let demands_value = pat.graph.node_outputs(root).iter().any(|&ov| {
            let o = pat.graph.value_kind_ref(ov);
            o.width.is_some()
                || matches!(o.kind, OutputKindSpec::Value(_) | OutputKindSpec::AnyValue)
        });
        return !demands_value && attempt(None, bindings);
    }
    // A repeated anchor re-derives bindings already reported (an `If`'s second output, say).
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

/// A necessary condition for `pat` to match at `node`: the root's kind admits it and each fixed
/// operand's producer passes its pat node's kind and node predicates (at either slot of an
/// unordered pair). Operands go unchecked under a cast mask; an alternation root admits all.
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

/// The root's output vertex to check against `root_value`: the sole vertex whatever its slot (a
/// [`SlotMode::Pinned`] one is enforced by [`output_ok`]), else the one at `root_value`'s slot.
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

/// Whether anchoring `pat`'s root at `out_id` re-runs the attempt made at `anchored`.
///
/// Distinct per-slot vertices impose one constraint set whichever anchors, since
/// [`bind_sibling_outputs`] checks the rest at their slots. A shared vertex, a
/// [`SlotMode::Enumerated`] one, or a differing anchor value type (read by post-match guards)
/// tells the attempts apart.
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

/// A pat node offered an IR node and, at a value position, the IR output and its pat vertex.
#[derive(Clone, Copy)]
struct Site {
    pat_node: PatNodeId,
    ir_node: NodeId,
    root_value: Option<ValueId>,
    out_vertex: Option<PatValueId>,
    read: Read,
}

/// Calls `k` per way `site`'s pat node matches its IR node, `bindings` extended with that
/// configuration. `true` from `k` accepts, leaving `bindings` as is; if none is accepted,
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
        ..
    } = site;
    let nd = ctx.pat.graph.node_kind(pat_node);
    if !nd.kind.matches(ctx.function().node_kind(ir_node)) {
        return false;
    }

    // With no value (node-rooted), only a vertex demanding nothing is satisfied.
    if let Some(ov_idx) = out_vertex {
        let ov = ctx.pat.graph.value_kind_ref(ov_idx);
        match root_value {
            Some(value) if !output_ok(ov, ctx.function(), value) => return false,
            None if vertex_imposes_requirement(ov) => return false,
            _ => {}
        }
    }

    if !nd.node_predicates.iter().all(|p| p(ctx.matcher, ir_node)) {
        return false;
    }

    let node_inputs = ctx.pat.inputs_of(pat_node);
    let inputs = &node_inputs.edges;
    let n_fixed = node_inputs.fixed;

    if nd.is_alternation() {
        return try_alternation(ctx, site, bindings, k);
    }

    // Swap one or two fixed operands at slots {0,1} of a commutative IR kind, unless
    // `.ordered()`, the pair is interchangeable, or both IR slots hold one value. A single pinned
    // operand swaps too, so a query does not answer by which slot it names.
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
        read: site.read,
    };

    let mark = bindings.mark();
    {
        // Finishes this node once its inputs matched. A rejection leaves `b` to the resuming
        // enumeration point, which restores it.
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

/// `one_of` / `first_of`: every arm tried against the same `ir_node`, a `first_of` recording the
/// arm taken. The pattern's `pruned_first_of` skips the arms past one that matched.
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
    // Bound before the arms, so an arm rebinding this capture conflicts rather than shadows.
    if !bind_all_captures(ctx, site, bindings) {
        return false;
    }
    let trail = ctx.trail.filter(|_| nd.role == Role::FirstOf);
    // Pruning is exact only at the first `first_of` settled: the caller drops a match past its
    // lowest matching arm before any later one is settled. Elsewhere that match may be kept.
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
            read: site.read + alt.step,
            ..site
        };
        if let Some(t) = trail {
            let key = (Choice::FirstOf, offer, t.anchor.get(), site.read);
            t.arms.borrow_mut().push((key, arm));
        }
        let accepted = try_match_at(ctx, arm_site, bindings, &mut |b| {
            bind_sibling_outputs(ctx, site, 0, b, k)
        });
        if let Some(t) = trail {
            t.arms.borrow_mut().pop();
        }
        if accepted {
            return true;
        }
        // Cut on a produced match, per attempt and per offered value: a re-offer with another
        // value (commutative swap, `any_input` slot, other output) is not cut.
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

/// Finishes `site` after its inputs matched: its non-anchoring output vertices from `idx` on, its
/// post-match predicates, its footprint entry, then `k`. A [`SlotMode::Enumerated`] vertex tries
/// each output in turn; any other is checked at its own slot. A rejection leaves `b` to the
/// caller's mark.
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
    // A missing output satisfies only a vertex that constrains nothing.
    if !any_slot_present && !vertex_imposes_requirement(ov) {
        return bind_sibling_outputs(ctx, site, idx + 1, b, k);
    }
    false
}

/// Tries `edge`'s sub-pattern at `value`, then at each masked cast's value input below it,
/// outermost first; `k` continues the enclosing match.
///
/// Failure restores `bindings` to entry. A skipped cast is recorded into the footprint before
/// the retry, so it is kept only on success. The caller keeps the outermost level that matched,
/// so a cast never adds a second binding beside a direct one.
fn try_operand(
    ctx: &Ctx,
    edge: &InputEdge,
    up: Read,
    value: ValueId,
    bindings: &mut Bindings,
    k: &mut dyn FnMut(&mut Bindings) -> bool,
) -> bool {
    let mark = bindings.mark();
    let read = up + edge.step;
    // A cast mask always comes with a trail; only a cast has a level to choose.
    let choice = ctx
        .trail
        .filter(|_| cast_below(ctx.function(), value, ctx.pat.cast_mask).is_some())
        .map(|t| {
            let offer = (edge.producer, ctx.function().producer(value), Some(value));
            (t, (Choice::Cast, offer, t.anchor.get(), read))
        });
    let mut level = value;
    for depth in 0.. {
        let site = Site {
            pat_node: edge.producer,
            ir_node: ctx.function().producer(level),
            root_value: Some(level),
            out_vertex: Some(edge.out_vertex),
            read,
        };
        if let Some((t, key)) = choice {
            t.arms.borrow_mut().push((key, depth));
        }
        let accepted = try_match_at(ctx, site, bindings, k);
        if let Some((t, _)) = choice {
            t.arms.borrow_mut().pop();
        }
        if accepted {
            return true;
        }
        let Some((cast, unwrapped)) = cast_below(ctx.function(), level, ctx.pat.cast_mask) else {
            break;
        };
        bindings.record_matched(cast);
        level = unwrapped;
    }
    bindings.restore(mark);
    false
}

/// The cast producing `value` and its value input, when `mask` selects that cast's kind.
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
    /// The producer's [`Read`] less its consumer's: one plus the unfolded size of earlier inputs.
    step: Read,
}

/// A pat node's consumed inputs, existentials (`any_input`) last.
pub(crate) struct NodeInputs {
    pub(crate) edges: Vec<InputEdge>,
    pub(crate) fixed: usize,
    /// The swapped ordering of the two fixed operands is redundant. See [`interchangeable`].
    pub(crate) interchangeable: bool,
}

/// One entry per pat node, indexed by [`PatNodeId::as_u32`], edges in [`Read`] order.
///
/// `compare_operands` is off for a refused pattern, too deep for [`interchangeable`] to recurse.
pub(crate) fn collect_node_inputs(
    graph: &crate::matcher::graph::PatGraph,
    size: &[(usize, Option<Read>)],
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
                    step: 0,
                })
                .collect();
            edges.sort_by_key(|e| e.consumer_slot == InputSlot::Any);
            let mut below: Read = 1;
            for e in &mut edges {
                e.step = below;
                below =
                    below.saturating_add(size[e.producer.as_u32() as usize].1.unwrap_or(Read::MAX));
            }
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

/// Whether two operand sub-patterns accept the same operand pairs with the same bindings under
/// either ordering. One shared vertex always does; distinct vertices only while nothing under
/// either binds or carries a predicate.
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
    // Placeholder: a shared DAG pair is not re-walked mid-flight.
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
    // Every output vertex, not only the consumed one: a sibling's capture swaps too.
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

/// Candidate slots for a commutative operand, indexed by its own slot: natural ordering first.
const COMM_ORDER: [[usize; 2]; 2] = [[0, 1], [1, 0]];

/// One node's assignment problem: which IR input slots each pattern input may occupy.
struct Assignment<'a> {
    edges: &'a [InputEdge],
    fixed: usize,
    commutative: bool,
    /// The node's [`Read`].
    read: Read,
}

impl Assignment<'_> {
    /// Whether an existential may take `slot`: any slot no fixed operand pins (a pinned slot is
    /// not in the `Claimed` chain). On a commutative node, every slot. No kind filter: the
    /// sub-pattern discriminates.
    fn open_to_existential(&self, slot: usize) -> bool {
        self.commutative
            || !self.edges[..self.fixed]
                .iter()
                .any(|e| e.consumer_slot == InputSlot::At(slot))
    }
}

/// Slots claimed by enclosing multi-slot assignments. Distinct by slot index, not by value.
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

/// Injectively assigns `assignment.edges[i..]` to `ir_node`'s input slots, calling `done` once
/// every input is placed and matched. A later rejection asks an earlier input for its next
/// candidate.
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
    let slots = match edge.consumer_slot {
        InputSlot::At(slot) if !assignment.commutative => {
            let Some(value) = ctx.function().nth_input(ir_node, slot) else {
                return false;
            };
            return try_operand(ctx, edge, assignment.read, value, bindings, &mut |b| {
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
        if try_operand(ctx, edge, assignment.read, value, bindings, &mut |b| {
            match_assignments(ctx, assignment, i + 1, ir_node, Some(&next), b, done)
        }) {
            return true;
        }
    }
    false
}

/// Binds every capture this pat node declares, all or nothing. Vertex captures and the identity
/// pin bind the value (skipped when node-rooted); node captures bind the node.
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

/// Whether the vertex binds a capture or identity pin, or filters kind, width or slot.
fn vertex_imposes_requirement(o: &PatValue) -> bool {
    !o.captures.is_empty()
        || o.identity.is_some()
        || o.width.is_some()
        || o.slot_mode == SlotMode::Pinned
        || o.kind != OutputKindSpec::Any
}

/// `value` is a memory token whose producer has no value output.
fn is_sole_token_of(f: &strider_ir::Function, value: ValueId) -> bool {
    matches!(f.value_kind(value), ValueKind::Memory)
        && f.node_outputs(f.producer(value))
            .iter()
            .all(|&o| f.value_kind(o).as_value().is_none())
}

fn output_ok(o: &PatValue, f: &strider_ir::Function, value: ValueId) -> bool {
    let val = f.value_kind(value).as_value();
    let kind_ok = match &o.kind {
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
