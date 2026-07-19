"""`Call` / `CallOther` nested as a *value* operand of another node.

`add(x, call_other().name("f"))` follows a value out of a user-op or
call. Regression: these builders used to raise when nested as a value
operand (they were memory-producer / root only).
"""

import strider
from strider import pattern as p


def test_call_and_call_other_build_as_value_operands():
    # Regression: used to raise "cannot be nested as a value operand".
    assert p.add(p.int_const(5), p.call_other().name("f")) is not None
    assert p.add(p.anything(), p.call()) is not None


def test_call_other_value_operand_matches_real_graph():
    # `rdtsc` (0F 31) is a classified CallOther writing EDX:EAX; `add eax, 5`
    # (83 C0 05) consumes its result. x86 register aliasing (EAX/RAX) inserts
    # a truncate between the two, hence ignore_casts.
    code = b"\x0f\x31\x83\xc0\x05\xc3"  # rdtsc; add eax,5; ret
    mem = strider.reader.BufferReader(0x1000, code)
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)
    _cfg, fn, _unresolved = lift.analyze(0x1000, strider.sleigh.CallingConvention.x86_64_systemv())

    hits = fn.find_all(p.add(p.anything(), p.call_other()), ignore_casts=True)
    assert len(hits) == 1

    assert fn.find_all(p.add(p.anything(), p.call_other().name("nope")), ignore_casts=True) == []


def test_res_selector_is_chainable_and_matches_result():
    # `.res()` pins a nested value operand to the call's declared result
    # output, excluding clobbers (that exclusion is covered elsewhere).
    assert p.add(p.anything(), p.call_other().name("f").res()) is not None
    assert p.add(p.anything(), p.call().res()) is not None

    code = b"\x0f\x31\x83\xc0\x05\xc3"  # rdtsc; add eax,5; ret
    mem = strider.reader.BufferReader(0x1000, code)
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)
    _cfg, fn, _unresolved = lift.analyze(0x1000, strider.sleigh.CallingConvention.x86_64_systemv())
    assert len(fn.find_all(p.add(p.anything(), p.call_other().res()), ignore_casts=True)) == 1
