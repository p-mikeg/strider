"""`Call` / `CallOther` nested as a *value* operand of another node.

`add(x, call_other().name("f"))` matches an arithmetic node one of whose
operands is a value output of the call — so a pattern can follow a value
through a user-op / call. Previously these builders raised when used as a
value operand (they were memory-producer / root only).
"""

import strider
from strider import pattern as p


def test_call_and_call_other_build_as_value_operands():
    # Previously raised "cannot be nested as a value operand"; now builds.
    assert p.add(p.int_const(5), p.call_other().name("f")) is not None
    assert p.add(p.anything(), p.call()) is not None


def test_call_other_value_operand_matches_real_graph():
    # `rdtsc` (0F 31) is a classified CallOther writing EDX:EAX; `add eax, 5`
    # (83 C0 05) consumes its result. x86 register aliasing (EAX↔RAX) inserts a
    # truncate between the two, so match through casts.
    code = b"\x0f\x31\x83\xc0\x05\xc3"  # rdtsc; add eax,5; ret
    mem = strider.BufferReader(0x1000, code)
    lift = strider.lifter(strider.SleighArch.x86_64(), mem)
    _cfg, fn, _unresolved = lift.analyze(0x1000, strider.CallingConvention.x86_64_systemv())

    # add whose operand is a value output of the rdtsc CallOther.
    hits = fn.find_all(p.add(p.anything(), p.call_other()), ignore_casts=True)
    assert len(hits) == 1

    # A wrong user-op name must not match.
    assert fn.find_all(p.add(p.anything(), p.call_other().name("nope")), ignore_casts=True) == []
