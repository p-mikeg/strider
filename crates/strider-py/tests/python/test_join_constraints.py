"""find_all([...], constraints=[...]) — CFG relational join constraints.

The motivating query: a call gated on the TRUE branch of a guard, not the false
branch and not after the merge — expressed region-independently via the If's
control-output values plus dominated_by_branch.
"""

import strider
from strider import pattern as p


def _diamond_with_calls():
    # A clean diamond, one call per region, each targeting a distinct address
    # OUTSIDE the function body (so all three stay real Call nodes rather than
    # collapsing to intra-function branches):
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
    mem = strider.BufferReader(0x1000, code)
    lift = strider.lifter(strider.SleighArch.x86_64(), mem)
    _cfg, fn, _u = lift.analyze(0x1000, strider.CallingConvention.x86_64_systemv())
    return fn


def _call_count(fn, constraints):
    g, t, f, c = p.Capture(), p.Capture(), p.Capture(), p.Capture()
    guard = p.if_else().capture_true(t).capture_false(f).capture(g)
    call = p.call().capture(c)
    return len(fn.find_all([guard, call], constraints=constraints(t, f, c, g)))


def test_dominated_by_branch_isolates_true_arm_in_one_constraint():
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
    # One constraint = "the call in the block this edge leads into": it excludes
    # the sibling arm AND the merge tail at 0x1010 (the merge is dominated by
    # the If itself, not by either of its edges).
    hits = fn.find_all([guard, call], constraints=[p.dominated_by_branch(t, c)])
    assert len(hits) == 1, "only one call is dominated by the true edge"
    assert 0x100B in hits[0].asm_fingerprint(c), "true edge -> the jump-taken call"

    fhits = fn.find_all([guard, call], constraints=[p.dominated_by_branch(f, c)])
    assert len(fhits) == 1, "only one call is dominated by the false edge"
    assert 0x1004 in fhits[0].asm_fingerprint(c), "false edge -> the fallthrough call"


def test_find_unique_accepts_constraints():
    fn = _diamond_with_calls()
    g, t, f, c = p.Capture(), p.Capture(), p.Capture(), p.Capture()
    guard = p.if_else().capture_true(t).capture_false(f).capture(g)
    call = p.call().capture(c)
    cons = [p.dominated_by_branch(t, c)]
    # find_unique agrees the constrained true-arm hit is unique.
    m = fn.find_unique([guard, call], constraints=cons)
    assert m is not None
    # Same target as find_all under the same constraint.
    assert len(fn.find_all([guard, call], constraints=cons)) == 1


def test_dominates_if_selects_every_call():
    fn = _diamond_with_calls()
    all_calls = len(fn.find_all(p.call()))
    dominated = _call_count(fn, lambda t, f, c, g: [p.dominates(g, c)])
    # The If dominates both arms and the merge — every call is dominated by it.
    assert dominated == all_calls


def test_phi_input_from_edge_ties_value_to_its_branch():
    """`phi_input_from_edge(phi, edge, value)` — the phi's data input on the
    predecessor fed by `edge` equals `value`.  A distinct-const diamond:
    the two branches assign different constants, so each If edge selects its
    own branch's merged value.
    """
    # test edi,edi; je (eax=2); mov eax,1; jmp; mov eax,2; ret
    code = bytes([0x85, 0xFF, 0x74, 0x07, 0xB8, 0x01, 0, 0, 0,
                  0xEB, 0x05, 0xB8, 0x02, 0, 0, 0, 0xC3])
    mem = strider.BufferReader(0x1000, code)
    _cfg, fn, _u = strider.lifter(
        strider.SleighArch.x86_64(), mem
    ).analyze(0x1000, strider.CallingConvention.x86_64_systemv())

    t, f, ph, v = p.Capture(), p.Capture(), p.Capture(), p.Capture()
    guard = p.if_else().capture_true(t).capture_false(f)
    phi = p.phi().capture(ph)
    val = p.any_int_const(v)

    th = fn.find_all([guard, phi, val], constraints=[p.phi_input_from_edge(ph, t, v)])
    fh = fn.find_all([guard, phi, val], constraints=[p.phi_input_from_edge(ph, f, v)])

    assert len(th) == 1 and len(fh) == 1
    true_val, false_val = th[0].uint(v), fh[0].uint(v)
    # Each edge picks its OWN branch's merged constant, and they differ.
    assert {true_val, false_val} == {1, 2}
    assert true_val != false_val


def _const_diamond():
    """`if (edi==0) eax=2 else eax=1` — the two arms merge distinct constants."""
    code = bytes([0x85, 0xFF, 0x74, 0x07, 0xB8, 0x01, 0, 0, 0,
                  0xEB, 0x05, 0xB8, 0x02, 0, 0, 0, 0xC3])
    mem = strider.BufferReader(0x1000, code)
    _cfg, fn, _u = strider.lifter(
        strider.SleighArch.x86_64(), mem
    ).analyze(0x1000, strider.CallingConvention.x86_64_systemv())
    return fn


def test_phi_input_from_edge_accepts_inline_value_pattern():
    """`value` may be a PATTERN matched inline at the arm — no floating root."""
    fn = _const_diamond()
    t, f, ph = p.Capture(), p.Capture(), p.Capture()
    guard = p.if_else().capture_true(t).capture_false(f)
    phi = p.phi().capture(ph)

    def hits(edge, k):
        return len(fn.find_all(
            [guard, phi],
            constraints=[p.phi_input_from_edge(ph, edge, p.int_const(k))],
        ))

    # Each edge merges exactly one of {1, 2}, and the two edges disagree.
    assert hits(t, 1) + hits(t, 2) == 1
    assert hits(f, 1) + hits(f, 2) == 1
    assert hits(t, 1) != hits(f, 1)


def test_inline_pattern_capture_is_readable():
    """A capture INSIDE the inline pattern binds and reads back off the Match."""
    fn = _const_diamond()
    t, f, ph, v = p.Capture(), p.Capture(), p.Capture(), p.Capture()
    guard = p.if_else().capture_true(t).capture_false(f)
    phi = p.phi().capture(ph)

    def read(edge):
        hits = fn.find_all(
            [guard, phi],
            constraints=[p.phi_input_from_edge(ph, edge, p.any_int_const(v))],
        )
        assert len(hits) == 1, f"expected one arm value, got {len(hits)}"
        return hits[0].uint(v)

    true_val, false_val = read(t), read(f)
    assert {true_val, false_val} == {1, 2}
    assert true_val != false_val


def test_inline_pattern_negative_when_arm_differs():
    fn = _const_diamond()
    t, ph = p.Capture(), p.Capture()
    guard = p.if_else().capture_true(t)
    phi = p.phi().capture(ph)
    hits = fn.find_all(
        [guard, phi],
        constraints=[p.phi_input_from_edge(ph, t, p.int_const(0xDEAD))],
    )
    assert hits == [], "no arm merges 0xDEAD"


def test_inline_equivalent_to_capture_spelling():
    """The inline form and today's two-root capture form agree on the arm."""
    fn = _const_diamond()

    def via_capture(pick_true):
        t, f, ph, v = p.Capture(), p.Capture(), p.Capture(), p.Capture()
        guard = p.if_else().capture_true(t).capture_false(f)
        edge = t if pick_true else f
        hits = fn.find_all(
            [guard, p.phi().capture(ph), p.any_int_const(v)],
            constraints=[p.phi_input_from_edge(ph, edge, v)],
        )
        assert len(hits) == 1
        return hits[0].uint(v)

    def via_inline(pick_true):
        t, f, ph, v = p.Capture(), p.Capture(), p.Capture(), p.Capture()
        guard = p.if_else().capture_true(t).capture_false(f)
        edge = t if pick_true else f
        hits = fn.find_all(
            [guard, p.phi().capture(ph)],
            constraints=[p.phi_input_from_edge(ph, edge, p.any_int_const(v))],
        )
        assert len(hits) == 1
        return hits[0].uint(v)

    assert via_capture(True) == via_inline(True)
    assert via_capture(False) == via_inline(False)


def test_inline_capture_unifies_with_tuple_binding():
    """`v` bound BOTH by a real root and inside the inline pattern must AGREE."""
    fn = _const_diamond()
    t, f, ph, v = p.Capture(), p.Capture(), p.Capture(), p.Capture()
    guard = p.if_else().capture_true(t).capture_false(f)
    phi = p.phi().capture(ph)
    val_root = p.any_int_const(v)

    def count(edge):
        return len(fn.find_all(
            [guard, phi, val_root],
            constraints=[p.phi_input_from_edge(ph, edge, p.any_int_const(v))],
        ))

    # The root ranges over every int const; unification must pin `v` to the arm.
    assert count(t) == 1
    assert count(f) == 1
