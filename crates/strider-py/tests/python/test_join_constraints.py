"""find_all([...], constraints=[...]) — CFG relational join constraints.

The motivating query: a call gated on the TRUE branch of a guard, not the false
branch and not after the merge — expressed region-independently via the If's
control-output values plus reaches / not_reaches.
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


def test_reaches_and_not_reaches_isolate_the_true_arm():
    fn = _diamond_with_calls()
    all_calls = len(fn.find_all(p.call()))
    assert all_calls >= 3, f"expected >=3 calls, got {all_calls}"

    # reaches(true) alone: true arm + merge.
    reach_true = _call_count(fn, lambda t, f, c, g: [p.reaches(t, c)])
    # + not_reaches(false): drops the merge (reachable from both) -> true only.
    true_only = _call_count(fn, lambda t, f, c, g: [p.reaches(t, c), p.not_reaches(f, c)])

    assert reach_true > true_only, "adding not_reaches(false) must drop the merge tail"
    assert true_only >= 1, "the true-arm call survives"


def test_dominated_by_branch_isolates_true_arm_in_one_constraint():
    fn = _diamond_with_calls()
    g, t, f, c = p.Capture(), p.Capture(), p.Capture(), p.Capture()
    guard = p.if_else().capture_true(t).capture_false(f).capture(g)
    call = p.call().capture(c)
    # One constraint = "call in the true block" (excludes false arm AND merge).
    one = len(fn.find_all([guard, call], constraints=[p.dominated_by_branch(t, c)]))
    two = len(fn.find_all([guard, call], constraints=[p.reaches(t, c), p.not_reaches(f, c)]))
    assert one == two >= 1, "one dominated_by_branch equals reaches+not_reaches"


def test_find_one_and_find_unique_accept_constraints():
    fn = _diamond_with_calls()
    g, t, f, c = p.Capture(), p.Capture(), p.Capture(), p.Capture()
    guard = p.if_else().capture_true(t).capture_false(f).capture(g)
    call = p.call().capture(c)
    cons = [p.dominated_by_branch(t, c)]
    # find_one returns the (only) true-arm hit; find_unique agrees it's unique.
    assert fn.find_one([guard, call], constraints=cons) is not None
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
