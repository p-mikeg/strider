"""`any_int_const` / `any_bool_const` / `any_float_const` accept no capture.

Matching "any integer constant" is useful on its own (e.g. as a structural
constraint) without binding the value; the capture argument is optional.
"""

from __future__ import annotations

from strider.pattern import Capture, any_bool_const, any_float_const, any_int_const

from .conftest import built_function


def _g():
    return built_function("x86", "switch", "dispatch_value")


def test_any_int_const_without_capture_matches_same_as_with():
    g = _g()
    with_cap = g.find_all(any_int_const(Capture()))
    no_cap = g.find_all(any_int_const())
    assert len(no_cap) > 0
    assert len(no_cap) == len(with_cap)


def test_any_int_const_with_capture_still_binds():
    g = _g()
    c = Capture()
    hits = g.find_all(any_int_const(c))
    assert any(h.const_uint(c) is not None for h in hits)


def test_any_bool_and_float_const_without_capture_are_accepted():
    g = _g()
    # Match count is fixture-dependent; only "builds and runs" is pinned.
    g.find_all(any_bool_const())
    g.find_all(any_float_const())
