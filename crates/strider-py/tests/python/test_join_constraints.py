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
    guard = p.if_else().capture(g).capture_true(t).capture_false(f)
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


def test_dominates_if_selects_every_call():
    fn = _diamond_with_calls()
    all_calls = len(fn.find_all(p.call()))
    dominated = _call_count(fn, lambda t, f, c, g: [p.dominates(g, c)])
    # The If dominates both arms and the merge — every call is dominated by it.
    assert dominated == all_calls
