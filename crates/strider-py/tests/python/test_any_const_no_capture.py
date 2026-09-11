"""The `*_const` builders accept no argument.

Without one the pattern is a purely structural constraint on "some constant of
this type here".
"""

from __future__ import annotations

from strider.pattern import Capture, bool_const, float_const, int_const

from .conftest import built_function


def _g():
    return built_function("x86", "switch", "dispatch_value")


def test_int_const_without_capture_matches_same_as_with():
    g = _g()
    with_cap = g.find_all(int_const(Capture()))
    no_cap = g.find_all(int_const())
    assert len(no_cap) > 0
    assert len(no_cap) == len(with_cap)


def test_int_const_with_capture_still_binds():
    g = _g()
    c = Capture()
    hits = g.find_all(int_const(c))
    assert any(h.uint(c) is not None for h in hits)


def test_bool_and_float_const_without_capture_are_accepted():
    g = _g()
    # Match count is fixture-dependent; only "builds and runs" is pinned.
    g.find_all(bool_const())
    g.find_all(float_const())
