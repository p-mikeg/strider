"""`one_of([...])` alternation: match a value against several shapes.

The motivating case is a value that may or may not be wrapped (an address that
may or may not be masked), matched by one pattern instead of two.
"""

import pytest

import strider
from strider import pattern as p


def _lift(code: bytes):
    mem = strider.reader.BufferReader(0x1000, code)
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)
    _cfg, fn, _unresolved = lift.analyze(0x1000, strider.sleigh.CallingConvention.x86_64_systemv())
    return fn


def test_one_of_is_the_union_of_its_alternatives():
    # add eax,ebx (01 d8); imul eax,ebx (0f af c3); ret (c3)
    fn = _lift(b"\x01\xd8\x0f\xaf\xc3\xc3")
    adds = len(fn.find_all(p.int_add(p.anything(), p.anything())))
    muls = len(fn.find_all(p.int_mul(p.anything(), p.anything())))
    union = len(fn.find_all(p.one_of([
        p.int_add(p.anything(), p.anything()),
        p.int_mul(p.anything(), p.anything()),
    ])))
    assert adds >= 1 and muls >= 1
    assert union == adds + muls  # a node is an add xor a mul; no overlap


def test_one_of_nested_as_an_operand():
    # add rax,rbx (48 01 d8); ret (c3).  Full registers, so the operands are
    # direct InitialVar reads with no sub-register truncation.
    fn = _lift(b"\x48\x01\xd8\xc3")
    pat = p.int_add(p.one_of([p.int_const(0), p.initial_var()]), p.anything())
    assert len(fn.find_all(pat)) == 1
    # When no alternative matches, the whole enclosing add is rejected.
    assert fn.find_all(p.int_add(p.one_of([p.int_const(0), p.int_const(7)]), p.anything())) == []


def test_one_of_nested():
    # add rax,rbx (48 01 d8); imul rax,rbx (48 0f af c3); xor rax,rbx (48 31 d8); ret
    fn = _lift(b"\x48\x01\xd8\x48\x0f\xaf\xc3\x48\x31\xd8\xc3")
    add_p = p.int_add(p.anything(), p.anything())
    mul_p = p.int_mul(p.anything(), p.anything())
    xor_p = p.int_xor(p.anything(), p.anything())
    # A nested one_of flattens: the union of all three.
    pat = p.one_of([add_p, p.one_of([mul_p, xor_p])])
    total = len(fn.find_all(add_p)) + len(fn.find_all(mul_p)) + len(fn.find_all(xor_p))
    assert total >= 3
    assert len(fn.find_all(pat)) == total


def test_one_of_is_order_independent():
    """Union semantics: every matching arm is enumerated with its own bindings,
    so alternative order does not change the result. A wildcard arm contributes
    its own offset-less match instead of shadowing the specific arm.
    """
    # mov rax,[rdi] ; mov rdx,[rsi+8] ; add rax,rdx ; ret
    fn = _lift(bytes([0x48, 0x8B, 0x07, 0x48, 0x8B, 0x56, 0x08, 0x48, 0x01, 0xD0, 0xC3]))
    base, off = p.Capture(), p.Capture()

    def offsets(pat):
        return sorted(
            (h.uint(off) if h.has(off) else 0) for h in fn.find_all(p.load(addr=pat))
        )

    specific_first = p.one_of([p.int_add(p.var(base), p.int_const(off)), p.var(base)])
    wildcard_first = p.one_of([p.var(base), p.int_add(p.var(base), p.int_const(off))])

    # [rdi] matches only the bare arm (off unbound -> 0). [rsi+8] matches both
    # the specific arm (off=8) and the wildcard arm (off unbound -> 0).
    assert offsets(specific_first) == [0, 0, 8]
    assert offsets(wildcard_first) == [0, 0, 8], "order does not change the union"


def test_one_of_leaves_captures_of_the_unfired_arm_unbound():
    """`has()` is how a caller tells which alternative matched."""
    # mov rax,rdi ; shl rax,3 ; imul rdx,rsi,12 ; add rax,rdx ; ret
    fn = _lift(
        bytes([0x48, 0x89, 0xF8, 0x48, 0xC1, 0xE0, 0x03,
               0x48, 0x6B, 0xD6, 0x0C, 0x48, 0x01, 0xD0, 0xC3])
    )
    m, s = p.Capture(), p.Capture()
    pat = p.one_of([p.int_mul(p.anything(), p.int_const(m)),
                    p.int_shl(p.anything(), p.int_const(s))])
    seen = set()
    for h in fn.find_all(pat):
        # Exactly one arm binds per match: never both, never neither.
        assert h.has(m) != h.has(s)
        seen.add(h.uint(m) if h.has(m) else ("shl", h.uint(s)))
    assert seen == {12, ("shl", 3)}


def test_empty_alternation_matches_nothing():
    # No alternatives means nothing qualifies, so a caller assembling the
    # arms programmatically does not have to guard the empty case.
    fn = _lift(b"\x01\xd8\xc3")
    assert fn.find_all(p.one_of([])) == []
    assert fn.find_all(p.first_of([])) == []
    # Still an alternation once nested, so the enclosing pattern fails too.
    assert fn.find_all(p.int_add(p.one_of([]), p.anything())) == []


def test_one_of_is_match_only_as_rewrite_rhs():
    # one_of names no single concrete shape, so it can't be a replacement.
    fn = _lift(b"\x01\xd8\xc3")
    with pytest.raises(strider.StriderError):
        fn.rewrite(find=p.int_add(p.anything(), p.anything()),
                   replace=p.one_of([p.int_const(0), p.int_const(1)]))
