"""`any_int` / `any_float` / `any_bool` constrain the output type; the matching
`int_const` / `float_const` / `bool_const` match the constant node itself, with
an optional capture in place of a value."""

from __future__ import annotations

from strider.pattern import (
    Capture,
    any_bool,
    any_float,
    any_int,
    bool_const,
    float_const,
    int_const,
)

from .conftest import built_function


def _g():
    return built_function("x86", "switch", "dispatch_value")


def test_int_const_capture_reads_back_through_uint():
    g = _g()
    c = Capture()
    hits = g.find_all(int_const(c))
    assert hits
    assert all(h.uint(c) is not None for h in hits)


def test_int_const_without_argument_matches_every_int_const():
    g = _g()
    c = Capture()
    assert len(g.find_all(int_const())) == len(g.find_all(int_const(c)))


def test_any_int_matches_a_non_constant_node():
    g = _g()
    c = Capture()
    hits = g.find_all(any_int(c))
    assert any(h.uint_opt(c) is None for h in hits)
    assert len(hits) > len(g.find_all(int_const(c)))


def test_capture_as_argument_and_as_decorator_agree():
    g = _g()
    c = Capture()
    d = Capture()
    first = g.find_all(int_const(c))
    second = g.find_all(int_const().capture(d))
    assert len(first) == len(second)
    assert [h.uint(c) for h in first] == [h.uint(d) for h in second]


def test_any_bool_matches_non_constant_i1():
    # `control::abs_val` holds one I1: the comparison feeding its branch.
    g = built_function("x86", "control", "abs_val")
    c = Capture()
    hits = g.find_all(any_bool(c))
    assert hits
    assert all(h.boolean_opt(c) is None for h in hits)
    assert not g.find_all(bool_const())


def test_float_const_is_a_subset_of_any_float():
    g = _g()
    assert len(g.find_all(float_const())) <= len(g.find_all(any_float()))
