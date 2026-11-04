"""`int_sub` / `float_sub` with a constant right operand match the
`Add(x, -K)` that `ConstantFold` leaves of gcc -O1's `a - K`."""

import struct

import strider
from strider import pattern as p

_CC = strider.sleigh.CallingConvention.x86_64_systemv()


def _analyze(code, data=b""):
    """`code` at 0x1000, `data` at 0x1010, both read-only."""
    image = code.ljust(0x10, b"\xcc") + data
    mem = strider.reader.BufferReader(0x1000, image)
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem, mem)
    return lift.analyze(0x1000, _CC).function


# int subc(int a) { return a - 16; }
_SUBC = bytes([0x8D, 0x47, 0xF0, 0xC3])  # lea eax, [rdi-0x10]; ret
# double fsubc(double a) { return a - 1.5; }
_FSUBC = bytes([0xF2, 0x0F, 0x5C, 0x05, 0x08, 0, 0, 0, 0xC3])  # subsd xmm0, [rip+8]
# float fsubf(float a) { return a - 1.5f; }
_FSUBF = bytes([0xF3, 0x0F, 0x5C, 0x05, 0x08, 0, 0, 0, 0xC3])  # subss xmm0, [rip+8]

_F64 = struct.unpack("<Q", struct.pack("<d", 1.5))[0]
_F32 = struct.unpack("<I", struct.pack("<f", 1.5))[0]


def _count(fn, pat):
    return len(fn.find_all(pat, ignore_casts=True))


def test_int_sub_of_a_literal_matches_the_folded_add():
    fn = _analyze(_SUBC)
    assert _count(fn, p.int_sub(p.function_arg(0), 16)) == 1
    assert _count(fn, p.int_sub(p.function_arg(0), p.int_const(16))) == 1
    assert _count(fn, p.int_sub(p.function_arg(0), 15)) == 0


def test_float_sub_of_a_constant_matches_the_folded_add():
    fn = _analyze(_FSUBC, struct.pack("<d", 1.5))
    assert _count(fn, p.float_sub(p.any_float(), p.float_const(_F64))) == 1
    assert _count(fn, p.float_sub(p.any_float(), p.float_const(_F64 ^ 1))) == 0


def test_float_sub_negates_at_the_constant_own_width():
    fn = _analyze(_FSUBF, struct.pack("<f", 1.5))
    assert _count(fn, p.float_sub(p.any_float(), p.float_const(_F32))) == 1
    assert _count(fn, p.float_sub(p.any_float(), p.float_const(_F64))) == 0


def test_int_sub_folds_every_uncaptured_constant_form():
    fn = _analyze(_SUBC)
    x = p.function_arg(0)
    assert _count(fn, p.int_sub(x, p.int_const(16).of_width(64))) == 1
    assert _count(fn, p.int_sub(x, p.int_const([15, 16]))) == 1
    assert _count(fn, p.int_sub(x, p.int_const([15, 17]))) == 0
    assert _count(fn, p.int_sub(x, p.int_const_any_width(16))) == 1
    assert _count(fn, p.int_sub(x, p.int_const_any_width([15, 16]))) == 1


def test_int_sub_of_a_captured_constant_matches_only_the_neg_shape():
    fn = _analyze(_SUBC)
    assert _count(fn, p.int_sub(p.function_arg(0), p.int_const(16).capture("k"))) == 0


def test_float_sub_folds_a_width_pinned_constant():
    fn = _analyze(_FSUBF, struct.pack("<f", 1.5))
    assert _count(fn, p.float_sub(p.any_float(), p.float_const(_F32).of_width(32))) == 1
    assert _count(fn, p.float_sub(p.any_float(), p.float_const(_F32).of_width(64))) == 0
