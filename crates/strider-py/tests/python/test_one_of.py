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


def test_one_of_nested():
    # add rax,rbx (48 01 d8); imul rax,rbx (48 0f af c3); xor rax,rbx (48 31 d8); ret
    fn = _lift(b"\x48\x01\xd8\x48\x0f\xaf\xc3\x48\x31\xd8\xc3")
    add_p = p.add(p.anything(), p.anything())
    mul_p = p.mul(p.anything(), p.anything())
    xor_p = p.int_xor(p.anything(), p.anything())
    # one_of[ add , one_of[ mul , xor ] ] — the flattened union of all three.
    pat = p.one_of([add_p, p.one_of([mul_p, xor_p])])
    total = len(fn.find_all(add_p)) + len(fn.find_all(mul_p)) + len(fn.find_all(xor_p))
    assert total >= 3
    assert len(fn.find_all(pat)) == total


def test_one_of_alternative_order_decides_which_arm_binds():
    """First-match-wins, so a permissive arm SHADOWS narrower arms after it.

    This is the documented ordering rule on `one_of`, pinned: a wildcard also
    matches the operator a later arm was meant to catch, and because it still
    *matches*, the wrong binding is returned silently rather than failing.
    """
    # mov rax,[rdi] ; mov rdx,[rsi+8] ; add rax,rdx ; ret
    fn = _lift(bytes([0x48, 0x8B, 0x07, 0x48, 0x8B, 0x56, 0x08, 0x48, 0x01, 0xD0, 0xC3]))
    base, off = p.Capture(), p.Capture()

    def offsets(pat):
        return sorted(
            (h.const_uint(off) if h.has(off) else 0) for h in fn.find_all(p.load(addr=pat))
        )

    # Specific first: the +8 load binds `off`; the bare load leaves it unbound.
    good = p.one_of([p.add(p.var(base), p.any_int_const(off)), p.var(base)])
    assert offsets(good) == [0, 8]

    # Permissive first: `var(base)` swallows the Add too, so `off` never binds
    # and the +8 load is indistinguishable from the bare one.
    bad = p.one_of([p.var(base), p.add(p.var(base), p.any_int_const(off))])
    assert offsets(bad) == [0, 0], "a leading wildcard arm shadows the specific arm"


def test_one_of_leaves_captures_of_the_unfired_arm_unbound():
    """`has()` is how a caller tells which alternative matched."""
    # mov rax,rdi ; shl rax,3 ; imul rdx,rsi,12 ; add rax,rdx ; ret
    fn = _lift(
        bytes([0x48, 0x89, 0xF8, 0x48, 0xC1, 0xE0, 0x03,
               0x48, 0x6B, 0xD6, 0x0C, 0x48, 0x01, 0xD0, 0xC3])
    )
    m, s = p.Capture(), p.Capture()
    pat = p.one_of([p.mul(p.anything(), p.any_int_const(m)),
                    p.shl(p.anything(), p.any_int_const(s))])
    seen = set()
    for h in fn.find_all(pat):
        # Exactly one arm binds per match — never both, never neither.
        assert h.has(m) != h.has(s)
        seen.add(h.const_uint(m) if h.has(m) else ("shl", h.const_uint(s)))
    assert seen == {12, ("shl", 3)}


def test_one_of_empty_raises():
    with pytest.raises(strider.errors.StriderError):
        p.one_of([])


def test_one_of_is_match_only_as_rewrite_rhs():
    # one_of can't be a rewrite replacement (it doesn't build one concrete shape).
    fn = _lift(b"\x01\xd8\xc3")
    with pytest.raises(strider.errors.StriderError):
        fn.rewrite(find=p.add(p.anything(), p.anything()),
                   replace=p.one_of([p.int_const(0), p.int_const(1)]))
