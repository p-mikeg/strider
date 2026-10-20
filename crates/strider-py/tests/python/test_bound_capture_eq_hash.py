"""`BoundCapture` value comparison and hashability.

`m[c] == v` means the captured constant equals `v` at its width, so it matches
both the signed and unsigned spelling of that value. Because one bound capture
then compares equal to two Python ints with different hashes, it cannot have a
consistent hash and is unhashable.
"""

from __future__ import annotations

import pytest

import strider
from strider.pattern import Capture, anything, int_add, int_const

from .conftest import built_function


def _neg8_hit(g, c):
    """The match of `int_const(c)` on `g` whose constant is -8."""
    for hit in g.find_all(int_const(c)):
        if hit.sint(c) == -8:
            return hit
    raise AssertionError("the fixture must carry a -8 constant")


def _bound_neg_const():
    """A `BoundCapture` holding the 32-bit constant -8 (0xFFFFFFF8)."""
    c = Capture()
    return _neg8_hit(built_function("x86", "memory", "array_sum"), c)[c]


def _bound_neg_const_64():
    """A `BoundCapture` holding the 64-bit constant -8: `mov rax, -8; ret`."""
    mem = strider.reader.BufferReader(0x1000, bytes.fromhex("48c7c0f8ffffff" "c3"))
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)
    cc = strider.sleigh.CallingConvention.x86_64_systemv()
    c = Capture()
    return _neg8_hit(lift.analyze(0x1000, cc).function, c)[c]


def _bound_wide_unsigned():
    """A `BoundCapture` holding the 64-bit constant 0xFFFFFFF8, positive at that
    width: `movabs rax, 0xFFFFFFF8; ret`."""
    code = bytes.fromhex("48b8f8ffffff00000000" "c3")
    lift = strider.lift.lifter(
        strider.sleigh.SleighArch.x86_64(), strider.reader.BufferReader(0x1000, code)
    )
    g = lift.analyze(0x1000, strider.sleigh.CallingConvention.x86_64_systemv()).function
    c = Capture()
    for hit in g.find_all(int_const(c)):
        if hit.sint(c) == 0xFFFFFFF8:
            return hit[c]
    raise AssertionError("the fixture must carry a 64-bit 0xFFFFFFF8")


def test_bound_capture_eq_matches_signed_and_unsigned():
    bc = _bound_neg_const()
    assert bc == -8, "the signed spelling of the value must compare equal"
    assert bc == 0xFFFFFFF8, "the unsigned spelling must also compare equal"
    assert bc != -7


def test_bound_captures_compare_by_their_constants():
    g = built_function("x86", "memory", "array_sum")
    a, b = Capture(), Capture()
    m = _neg8_hit(g, a)
    same = _neg8_hit(g, b)[b]
    other = next(h for h in g.find_all(int_const(a)) if h.sint(a) != -8)[a]
    assert m[a] == m[a] and not m[a] != m[a]
    assert m[a] == same and not m[a] != same
    assert m[a] != other and not m[a] == other


def test_bound_captures_of_two_widths_agree_through_the_signed_value():
    narrow, wide = _bound_neg_const(), _bound_neg_const_64()
    assert int(narrow) != int(wide), "the unsigned values differ"
    assert narrow == wide and not narrow != wide


def test_bound_captures_of_two_widths_agree_through_the_unsigned_value():
    narrow, wide = _bound_neg_const(), _bound_wide_unsigned()
    assert narrow == -8 and wide != -8, "the signed values differ"
    assert narrow == wide and not narrow != wide


def test_an_unbound_capture_equals_no_capture():
    g = built_function("x86", "memory", "array_sum")
    c = Capture()
    m = _neg8_hit(g, c)
    unbound = m[Capture()]
    assert m[c] != unbound and not m[c] == unbound
    assert unbound != m[c] and not unbound == m[c]
    assert unbound != m[Capture()] and not unbound == m[Capture()]


def test_bound_captures_of_nodes_compare_by_identity():
    g = built_function("x86", "memory", "array_sum")
    a, b = Capture(), Capture()
    adds = g.find_all(int_add(anything(), anything()).capture(a))
    assert len(adds) >= 2, "the fixture must carry two adds"
    first, second = adds[0], adds[1]
    assert first[a] == first[a] and not first[a] != first[a]
    assert first[a] != second[a] and not first[a] == second[a]
    again = next(
        h for h in g.find_all(int_add(anything(), anything()).capture(b)) if h.node(b) == first.node(a)
    )
    assert first[a] == again[b], "two captures bound to one node are equal"
    other = built_function("x86", "memory", "array_sum")
    twin = other.find_all(int_add(anything(), anything()).capture(a))[0]
    assert first[a] != twin[a], "a node of another function is another node"


def test_bound_capture_is_unhashable():
    bc = _bound_neg_const()
    with pytest.raises(TypeError):
        hash(bc)
