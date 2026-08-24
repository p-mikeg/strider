"""CFG relational join constraints: find_all([...], constraints=[...]).

The motivating query is a call gated on the TRUE branch of a guard, not the
false branch and not after the merge, expressed region-independently via the
If's control-output values plus edge-operand dominates.
"""

import pytest

import strider
from strider import pattern as p
from strider.pattern import constraints as cons


def _diamond_with_calls():
    # A clean diamond, one call per region.  Each target is OUTSIDE the
    # function body so all three stay real Call nodes rather than collapsing
    # into intra-function branches.
    #   1000 test edi,edi
    #   1002 je  -> 100b (false arm)
    #   1004 call 0x2000 (true arm)
    #   1009 jmp -> 1010 (merge)
    #   100b call 0x3000 (false arm)
    #   1010 call 0x4000 (merge)
    #   1015 ret
    code = bytes([
        0x85, 0xff,                    # test edi, edi
        0x74, 0x07,                    # je +7 -> 100b
        0xe8, 0xf7, 0x0f, 0x00, 0x00,  # call 0x2000 (true)
        0xeb, 0x05,                    # jmp +5 -> 1010
        0xe8, 0xf0, 0x1f, 0x00, 0x00,  # call 0x3000 (false)
        0xe8, 0xeb, 0x2f, 0x00, 0x00,  # call 0x4000 (merge)
        0xc3,                          # ret
    ])
    mem = strider.reader.BufferReader(0x1000, code)
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)
    _cfg, fn, _u = lift.analyze(0x1000, strider.sleigh.CallingConvention.x86_64_systemv())
    return fn


def _call_count(fn, constraints):
    g, t, f, c = p.Capture(), p.Capture(), p.Capture(), p.Capture()
    guard = p.if_else().capture_true(t).capture_false(f).capture(g)
    call = p.call().capture(c)
    return len(fn.find_all([guard, call], constraints=constraints(t, f, c, g)))


def test_dominates_edge_isolates_true_arm_in_one_constraint():
    fn = _diamond_with_calls()
    all_calls = len(fn.find_all(p.call()))
    assert all_calls >= 3, f"expected >=3 calls, got {all_calls}"

    g, t, f, c = p.Capture(), p.Capture(), p.Capture(), p.Capture()
    guard = p.if_else().capture_true(t).capture_false(f).capture(g)
    call = p.call().capture(c)

    # Edge polarity is the IR's, not the C source's: `je 0x100b` lifts to
    # `CBRANCH 0x100b, ZF`, so the If's TRUE output is the jump-taken edge
    # (0x100b) and its FALSE output is the fallthrough (0x1004).
    #
    # One constraint means "the call in the block this edge leads into": it
    # excludes the sibling arm AND the merge tail at 0x1010, since the merge is
    # dominated by the If itself, not by either of its edges.
    hits = fn.find_all([guard, call], constraints=[cons.dominates(t, c)])
    assert len(hits) == 1, "only one call is dominated by the true edge"
    assert 0x100B in hits[0].asm_fingerprint(c), "true edge -> the jump-taken call"

    fhits = fn.find_all([guard, call], constraints=[cons.dominates(f, c)])
    assert len(fhits) == 1, "only one call is dominated by the false edge"
    assert 0x1004 in fhits[0].asm_fingerprint(c), "false edge -> the fallthrough call"


def test_find_unique_accepts_constraints():
    fn = _diamond_with_calls()
    g, t, f, c = p.Capture(), p.Capture(), p.Capture(), p.Capture()
    guard = p.if_else().capture_true(t).capture_false(f).capture(g)
    call = p.call().capture(c)
    cs = [cons.dominates(t, c)]
    m = fn.find_unique([guard, call], constraints=cs)
    assert m is not None
    assert len(fn.find_all([guard, call], constraints=cs)) == 1


def test_dominates_if_selects_every_call():
    fn = _diamond_with_calls()
    all_calls = len(fn.find_all(p.call()))
    dominated = _call_count(fn, lambda t, f, c, g: [cons.dominates(g, c)])
    # The If dominates both arms and the merge, so it dominates every call.
    assert dominated == all_calls


def test_dominates_edge_over_edge_is_exposed():
    """Two control-output captures resolve to control EDGES, so `dominates`
    is answered edge-vs-edge: an edge dominates itself but not its sibling.

    Regression: the old rule resolved both captures to their producer node
    (the same `If`), making `dominates(t, f)` self-domination, which wrongly
    held."""
    fn = _diamond_with_calls()
    t, f = p.Capture(), p.Capture()
    guard = p.if_else().capture_true(t).capture_false(f)

    self_dom = fn.find_all([guard], constraints=[cons.dominates(t, t)])
    assert len(self_dom) == 1, "the true edge dominates itself (edge->edge)"

    sibling = fn.find_all([guard], constraints=[cons.dominates(t, f)])
    assert len(sibling) == 0, "the true edge does not dominate its false sibling"


def test_phi_input_from_edge_ties_value_to_its_branch():
    """`phi_input_from_edge(phi, edge, value)` pins the phi's data input on
    the predecessor fed by `edge`.  The two arms assign different constants,
    so each edge must select its own branch's value.
    """
    # test edi,edi; je (eax=2); mov eax,1; jmp; mov eax,2; ret
    code = bytes([0x85, 0xFF, 0x74, 0x07, 0xB8, 0x01, 0, 0, 0,
                  0xEB, 0x05, 0xB8, 0x02, 0, 0, 0, 0xC3])
    mem = strider.reader.BufferReader(0x1000, code)
    _cfg, fn, _u = strider.lift.lifter(
        strider.sleigh.SleighArch.x86_64(), mem
    ).analyze(0x1000, strider.sleigh.CallingConvention.x86_64_systemv())

    t, f, ph, v = p.Capture(), p.Capture(), p.Capture(), p.Capture()
    guard = p.if_else().capture_true(t).capture_false(f)
    phi = p.phi().capture(ph)
    val = p.int_const(v)

    th = fn.find_all([guard, phi, val], constraints=[cons.phi_input_from_edge(ph, t, v)])
    fh = fn.find_all([guard, phi, val], constraints=[cons.phi_input_from_edge(ph, f, v)])

    assert len(th) == 1 and len(fh) == 1
    true_val, false_val = th[0].uint(v), fh[0].uint(v)
    assert {true_val, false_val} == {1, 2}
    assert true_val != false_val


def _const_diamond():
    """`if (edi==0) eax=2 else eax=1`; the two arms merge distinct constants."""
    code = bytes([0x85, 0xFF, 0x74, 0x07, 0xB8, 0x01, 0, 0, 0,
                  0xEB, 0x05, 0xB8, 0x02, 0, 0, 0, 0xC3])
    mem = strider.reader.BufferReader(0x1000, code)
    _cfg, fn, _u = strider.lift.lifter(
        strider.sleigh.SleighArch.x86_64(), mem
    ).analyze(0x1000, strider.sleigh.CallingConvention.x86_64_systemv())
    return fn


def test_phi_input_from_edge_with_any_input_bound_value():
    """The arm value is bound by `any_input` on the phi, not a floating root."""
    fn = _const_diamond()
    t, f, ph, v = p.Capture(), p.Capture(), p.Capture(), p.Capture()
    guard = p.if_else().capture_true(t).capture_false(f)

    def hits(edge, k):
        phi = p.phi().any_input(p.int_const(k).capture(v)).capture(ph)
        return len(fn.find_all(
            [guard, phi],
            constraints=[cons.phi_input_from_edge(ph, edge, v)],
        ))

    # Each edge merges exactly one of {1, 2}, and the two edges disagree.
    assert hits(t, 1) + hits(t, 2) == 1
    assert hits(f, 1) + hits(f, 2) == 1
    assert hits(t, 1) != hits(f, 1)


def test_any_input_capture_is_readable():
    """A capture bound by `any_input` reads back off the Match."""
    fn = _const_diamond()
    t, f, ph, v = p.Capture(), p.Capture(), p.Capture(), p.Capture()
    guard = p.if_else().capture_true(t).capture_false(f)
    phi = p.phi().any_input(p.int_const(v)).capture(ph)

    def read(edge):
        hits = fn.find_all(
            [guard, phi],
            constraints=[cons.phi_input_from_edge(ph, edge, v)],
        )
        assert len(hits) == 1, f"expected one arm value, got {len(hits)}"
        return hits[0].uint(v)

    true_val, false_val = read(t), read(f)
    assert {true_val, false_val} == {1, 2}
    assert true_val != false_val


def test_any_input_negative_when_arm_differs():
    fn = _const_diamond()
    t, ph, v = p.Capture(), p.Capture(), p.Capture()
    guard = p.if_else().capture_true(t)
    phi = p.phi().any_input(p.int_const(0xDEAD).capture(v)).capture(ph)
    hits = fn.find_all(
        [guard, phi],
        constraints=[cons.phi_input_from_edge(ph, t, v)],
    )
    assert hits == [], "no arm merges 0xDEAD"


def test_any_input_equivalent_to_floating_root_spelling():
    """`any_input` binding agrees with the floating-root spelling, where the
    value ranges over the whole function as its own root."""
    fn = _const_diamond()

    def via_floating_root(pick_true):
        t, f, ph, v = p.Capture(), p.Capture(), p.Capture(), p.Capture()
        guard = p.if_else().capture_true(t).capture_false(f)
        edge = t if pick_true else f
        hits = fn.find_all(
            [guard, p.phi().capture(ph), p.int_const(v)],
            constraints=[cons.phi_input_from_edge(ph, edge, v)],
        )
        assert len(hits) == 1
        return hits[0].uint(v)

    def via_any_input(pick_true):
        t, f, ph, v = p.Capture(), p.Capture(), p.Capture(), p.Capture()
        guard = p.if_else().capture_true(t).capture_false(f)
        edge = t if pick_true else f
        hits = fn.find_all(
            [guard, p.phi().any_input(p.int_const(v)).capture(ph)],
            constraints=[cons.phi_input_from_edge(ph, edge, v)],
        )
        assert len(hits) == 1
        return hits[0].uint(v)

    assert via_floating_root(True) == via_any_input(True)
    assert via_floating_root(False) == via_any_input(False)


def test_any_input_capture_unifies_with_tuple_binding():
    """A capture bound BOTH by a floating root and by `any_input` must unify,
    not yield two independent bindings."""
    fn = _const_diamond()
    t, f, ph, v = p.Capture(), p.Capture(), p.Capture(), p.Capture()
    guard = p.if_else().capture_true(t).capture_false(f)
    phi = p.phi().any_input(p.int_const(v)).capture(ph)
    val_root = p.int_const(v)

    def count(edge):
        return len(fn.find_all(
            [guard, phi, val_root],
            constraints=[cons.phi_input_from_edge(ph, edge, v)],
        ))

    # The root ranges over every int const, so unification must pin `v` to the
    # arm rather than letting the root widen the result.
    assert count(t) == 1
    assert count(f) == 1


def _across_call_diamond():
    """`if (edi==0) { f(); eax=2 } else { f(); eax=1 }`.  The call splits each
    branch's block, so the If's edges are NOT the merge region's direct
    predecessors.  This is the shape real kernel code merges across.
    """
    code = bytes([
        0x85, 0xFF,                    # 1000: test edi,edi
        0x74, 0x0C,                    # 1002: je 1010
        0xE8, 0x17, 0, 0, 0,           # 1004: call 1020
        0xB8, 0x01, 0, 0, 0,           # 1009: mov eax,1
        0xEB, 0x0C,                    # 100e: jmp 101c
        0xE8, 0x0B, 0, 0, 0,           # 1010: call 1020
        0xB8, 0x02, 0, 0, 0,           # 1015: mov eax,2
        0xEB, 0x00,                    # 101a: jmp 101c
        0xC3,                          # 101c: ret
        0x90, 0x90, 0x90,              # padding
        0xC3,                          # 1020: callee
    ])
    mem = strider.reader.BufferReader(0x1000, code)
    _cfg, fn, _u = strider.lift.lifter(
        strider.sleigh.SleighArch.x86_64(), mem
    ).analyze(0x1000, strider.sleigh.CallingConvention.x86_64_systemv())
    return fn


def test_phi_input_from_edge_reaches_through_intervening_call():
    """The arms merge across a call, so neither edge is a literal predecessor
    of the join.  Each arm must still pin to its own branch.
    """
    fn = _across_call_diamond()
    t, f, ph, v = p.Capture(), p.Capture(), p.Capture(), p.Capture()
    guard = p.if_else().capture_true(t).capture_false(f)
    phi = p.phi().capture(ph)
    val = p.int_const(v)

    th = fn.find_all([guard, phi, val], constraints=[cons.phi_input_from_edge(ph, t, v)])
    fh = fn.find_all([guard, phi, val], constraints=[cons.phi_input_from_edge(ph, f, v)])

    assert len(th) == 1 and len(fh) == 1
    assert {th[0].uint(v), fh[0].uint(v)} == {1, 2}


def test_phi_input_from_edge_wildcard_probe_discriminates_blind_from_mismatch():
    """A `[]` result is ambiguous: either the edge reaches no arm of this phi,
    or it does and the arm merges a different value.  Re-probing with
    `anything()` discriminates, since a wildcard cannot fail on value grounds,
    so `[]` from it proves the edge is invisible.  No dedicated API needed.
    """
    fn = _across_call_diamond()
    t, ph, v = p.Capture(), p.Capture(), p.Capture()
    guard = p.if_else().capture_true(t)

    # The edge is visible even across the call, so the wildcard hits.
    probe = p.phi().any_input(p.anything().capture(v)).capture(ph)
    visible = fn.find_all([guard, probe], constraints=[cons.phi_input_from_edge(ph, t, v)])
    assert len(visible) >= 1

    # No arm merges 0xDEAD.  Same `[]`, but the probe above proves this one is
    # a real value mismatch, not blindness.
    dead = p.phi().any_input(p.int_const(0xDEAD).capture(v)).capture(ph)
    mismatch = fn.find_all([guard, dead], constraints=[cons.phi_input_from_edge(ph, t, v)])
    assert mismatch == []


def test_negate_is_the_exact_complement_of_dominates_edge():
    """`negate` holds exactly where the positive constraint does not, checked
    on a diamond where both cases actually occur."""
    fn = _diamond_with_calls()
    t, f, c = p.Capture(), p.Capture(), p.Capture()
    guard = p.if_else().capture_true(t).capture_false(f)
    call = p.call().capture(c)

    pos = fn.find_all([guard, call], constraints=[cons.dominates(t, c)])
    neg = fn.find_all([guard, call], constraints=[cons.negate(cons.dominates(t, c))])
    # One call per region (true arm, false arm, merge); the positive picks the
    # true-arm one, so the negation must pick the other two.
    assert len(pos) == 1
    assert len(neg) == 2
    pos_nodes = {h.node(c).id for h in pos}
    neg_nodes = {h.node(c).id for h in neg}
    assert not (pos_nodes & neg_nodes)
    assert len(pos_nodes | neg_nodes) == 3


def test_double_negate_is_the_identity():
    fn = _diamond_with_calls()
    t, c = p.Capture(), p.Capture()
    guard = p.if_else().capture_true(t)
    call = p.call().capture(c)

    once = fn.find_all([guard, call], constraints=[cons.dominates(t, c)])
    twice = fn.find_all(
        [guard, call],
        constraints=[cons.negate(cons.negate(cons.dominates(t, c)))],
    )
    assert len(twice) == len(once) == 1
    assert {h.node(c).id for h in twice} == {h.node(c).id for h in once}


def test_negate_with_an_unbound_capture_does_not_vacuously_match():
    """Range restriction.  `unbound` is bound by no pattern, so the inner
    constraint fails for want of a binding; under naive negation-as-failure
    that flips to a vacuous true and matches every tuple.  It must be rejected
    loudly instead."""
    fn = _diamond_with_calls()
    t, c, unbound = p.Capture(), p.Capture(), p.Capture()
    guard = p.if_else().capture_true(t)
    call = p.call().capture(c)

    with pytest.raises(strider.StriderError) as ei:
        fn.find_all(
            [guard, call],
            constraints=[cons.negate(cons.dominates(t, unbound))],
        )
    assert "negate" in str(ei.value)


def test_negate_of_phi_input_from_edge_with_a_capture_value_is_allowed():
    ph, e, v = p.Capture(), p.Capture(), p.Capture()
    assert cons.negate(cons.phi_input_from_edge(ph, e, v)) is not None


def test_negate_over_an_unbound_capture_drops_every_row():
    """Three-valued (Kleene) evaluation.  `ph` is captured only in the phi arm
    of a `one_of`, so it is absent in the bare-const rows.  Where `ph` is bound
    `dominates(ph, ph)` is true, so its negation is false; where `ph` is absent
    the relation is unanswerable (None) and `Not(None) == None`.  Every row
    therefore drops.

    Regression: the old two-valued code read the absent capture as false,
    flipped it to a vacuous true, and kept exactly those rows."""
    fn = _const_diamond()
    ph, v = p.Capture(), p.Capture()
    operand = p.one_of(
        [p.phi().any_input(p.int_const(v)).capture(ph), p.int_const(v)]
    )
    unconstrained = fn.find_all([operand])
    assert len(unconstrained) > 0, "some rows exist, including ph-unbound ones"

    negated = fn.find_all([operand], constraints=[cons.negate(cons.dominates(ph, ph))])
    assert negated == [], (
        "negate(dominates(ph, ph)) drops the ph-bound rows (false) AND the "
        "ph-unbound rows (None); the bug kept the latter vacuously"
    )


def test_empty_any_of_passes_nothing_empty_all_of_passes_everything():
    fn = _const_diamond()
    t, ph, v = p.Capture(), p.Capture(), p.Capture()
    guard = p.if_else().capture_true(t)
    phi = p.phi().any_input(p.int_const(v)).capture(ph)
    # A capture-free connective correlates nothing, so pair it with a real
    # relation to keep the two patterns joined.
    linked = cons.phi_input_from_edge(ph, t, v)
    kept = fn.find_all([guard, phi], constraints=[cons.all_of([]), linked])
    dropped = fn.find_all([guard, phi], constraints=[cons.any_of([]), linked])
    assert len(kept) >= 1
    assert dropped == []
