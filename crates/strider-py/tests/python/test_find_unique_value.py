"""Dedup is by VALUE, not by node, so structurally distinct matches that agree
on the value collapse where `find_unique` over-fails."""

import pytest

import strider
from strider import pattern as p
from strider.pattern import constraints as cons


class OffEquals(cons.JoinPredicate):
    def __init__(self, cap: p.Capture, value: int):
        super().__init__()
        self.cap, self.value = cap, value

    def captures(self):
        return [self.cap]

    def constraint(self, m):
        return m.uint(self.cap) == self.value


def _lift(code: bytes):
    mem = strider.reader.BufferReader(0x1000, code)
    _cfg, fn, _u = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem).analyze(
        0x1000, strider.sleigh.CallingConvention.x86_64_systemv()
    )
    return fn


def test_dedups_by_value_where_find_unique_fails():
    # mov rax,[rdi+8] ; mov rcx,[rsi+8] ; add rax,rcx ; ret -> two loads, +8 each
    fn = _lift(bytes([0x48, 0x8B, 0x47, 0x08, 0x48, 0x8B, 0x4E, 0x08, 0x48, 0x01, 0xC8, 0xC3]))
    off = p.Capture("off")
    pat = p.load(addr=p.int_add(p.anything(), p.int_const(off)))

    assert len(fn.find_all(pat)) == 2  # two structurally distinct matches
    with pytest.raises(strider.StriderError):
        fn.find_unique(pat)  # over-fails: distinct nodes, same value
    assert fn.find_unique_value(pat, off) == 8  # collapsed by value
    assert fn.find_unique_value(pat, "off") == 8  # capture by name works too


def test_none_when_no_match():
    fn = _lift(bytes([0x48, 0x8B, 0x47, 0x08, 0x48, 0x8B, 0x4E, 0x08, 0x48, 0x01, 0xC8, 0xC3]))
    z = p.Capture("z")
    assert fn.find_unique_value(p.load(addr=p.int_mul(p.anything(), p.int_const(z))), z) is None


def test_raises_on_distinct_values():
    # mov rax,[rdi+8] ; mov rcx,[rsi+16] ; add rax,rcx ; ret -> offsets 8 and 16
    fn = _lift(bytes([0x48, 0x8B, 0x47, 0x08, 0x48, 0x8B, 0x4E, 0x10, 0x48, 0x01, 0xC8, 0xC3]))
    off = p.Capture("off")
    pat = p.load(addr=p.int_add(p.anything(), p.int_const(off)))
    with pytest.raises(strider.StriderError):
        fn.find_unique_value(pat, off)


def test_signed_reads_negative_offset():
    # mov rax,[rdi-8] ; ret  -> a negative displacement, as in a stack frame.
    fn = _lift(bytes([0x48, 0x8B, 0x47, 0xF8, 0xC3]))
    off = p.Capture("off")
    pat = p.load(addr=p.int_add(p.anything(), p.int_const(off)))

    assert fn.find_unique_value(pat, off, signed=True) == -8
    # Default (unsigned) returns the raw two's-complement bit pattern.
    unsigned = fn.find_unique_value(pat, off)
    assert unsigned is not None and unsigned > 0 and unsigned != -8


def test_accepts_pattern_list_and_constraints():
    # Offsets 8 and 16; a list `pat` and a constraint behave as in find_all.
    fn = _lift(bytes([0x48, 0x8B, 0x47, 0x08, 0x48, 0x8B, 0x4E, 0x10, 0x48, 0x01, 0xC8, 0xC3]))
    off = p.Capture("off")
    pat = p.load(addr=p.int_add(p.anything(), p.int_const(off)))

    # List form joins on shared captures, same as a bare pattern.
    with pytest.raises(strider.StriderError):
        fn.find_unique_value([pat], off)  # still two distinct values

    # A constraint narrows the joined tuples to one value.
    assert fn.find_unique_value([pat], off, constraints=[OffEquals(off, 16)]) == 16
    assert fn.find_unique_value(pat, off, constraints=[OffEquals(off, 8)]) == 8
    assert fn.find_unique_value(pat, off, constraints=[OffEquals(off, 99)]) is None
