"""`one_of([...])` alternation — match a value against several shapes.

The motivating case: a value that may or may not be wrapped (e.g. an address
that may or may not be masked), matched by one pattern instead of two.
"""

import pytest

import strider
from strider import pattern as p


def _lift(code: bytes):
    mem = strider.BufferReader(0x1000, code)
    lift = strider.lifter(strider.SleighArch.x86_64(), mem)
    _cfg, fn, _unresolved = lift.analyze(0x1000, strider.CallingConvention.x86_64_systemv())
    return fn


def test_one_of_is_the_union_of_its_alternatives():
    # add eax,ebx (01 d8); imul eax,ebx (0f af c3); ret (c3)
    fn = _lift(b"\x01\xd8\x0f\xaf\xc3\xc3")
    adds = len(fn.find_all(p.add(p.anything(), p.anything())))
    muls = len(fn.find_all(p.mul(p.anything(), p.anything())))
    union = len(fn.find_all(p.one_of([
        p.add(p.anything(), p.anything()),
        p.mul(p.anything(), p.anything()),
    ])))
    assert adds >= 1 and muls >= 1
    assert union == adds + muls  # a node is an add xor a mul; no overlap


def test_one_of_nested_as_an_operand():
    # add rax,rbx (48 01 d8); ret (c3) -> add(<reg>, <reg>) on full regs, so the
    # operands are direct InitialVar reads (no sub-register truncation).
    fn = _lift(b"\x48\x01\xd8\xc3")
    # match the add whose first operand is one_of a constant or an initial-var
    # read — the alternation sits at a nested operand position.
    pat = p.add(p.one_of([p.int_const(0), p.initial_var()]), p.anything())
    assert len(fn.find_all(pat)) == 1
    # the wrong alternatives don't match, so the whole add is rejected.
    assert fn.find_all(p.add(p.one_of([p.int_const(0), p.int_const(7)]), p.anything())) == []


def test_one_of_empty_raises():
    with pytest.raises(strider.errors.StriderError):
        p.one_of([])


def test_one_of_is_match_only_as_rewrite_rhs():
    # one_of can't be a rewrite replacement (it doesn't build one concrete shape).
    fn = _lift(b"\x01\xd8\xc3")
    with pytest.raises(strider.errors.StriderError):
        fn.rewrite(find=p.add(p.anything(), p.anything()),
                   replace=p.one_of([p.int_const(0), p.int_const(1)]))
