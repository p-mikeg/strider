"""The `_opt` capture readers paired against their raising counterparts.

Stub parity proves both spellings exist and agree on signature. The pairing
they exist for is a runtime property: `x_opt` yields `None` in exactly the
case `x` raises, and the identical value everywhere else, on `Match` and on
the `BoundCapture` behind `m[c]` alike.
"""

from __future__ import annotations

import struct

import pytest

import strider
from strider import template as tpl
from strider.pattern import (
    Capture,
    anything,
    float_add,
    float_const,
    float_round,
    initial_var,
    int_add,
    int_const,
    int_mul,
    one_of,
    ret,
    var,
)

from .conftest import built_function, built_lifter_and_function

# Each reader as four callables: raising and `_opt`, on `Match` and on
# `BoundCapture`.
SINT = (
    lambda m, c: m.sint(c),
    lambda m, c: m.sint_opt(c),
    lambda m, c: m[c].sint,
    lambda m, c: m[c].sint_opt,
)
FLOAT_BITS = (
    lambda m, c: m.float_bits(c),
    lambda m, c: m.float_bits_opt(c),
    lambda m, c: m[c].float_bits,
    lambda m, c: m[c].float_bits_opt,
)
VALUE_TYPE = (
    lambda m, c: m.value_type(c),
    lambda m, c: m.value_type_opt(c),
    lambda m, c: m[c].value_type,
    lambda m, c: m[c].value_type_opt,
)
VN = (
    lambda m, c: m.vn(c),
    lambda m, c: m.vn_opt(c),
    lambda m, c: m[c].vn,
    lambda m, c: m[c].vn_opt,
)

READERS = (SINT, FLOAT_BITS, VALUE_TYPE, VN)


def _first(function, pat, capture):
    hits = function.find_all(pat)
    assert hits, "no match to read a capture off"
    return hits[0], capture


def _integer_constant():
    g = built_function("x64", "memory", "array_sum", optimize=False)
    c = Capture()
    return _first(g, int_const(c), c)


def _integer_addition():
    g = built_function("x64", "memory", "array_sum", optimize=False)
    c = Capture()
    return _first(g, int_add(anything(), anything()).capture(c), c)


def _entry_register_read():
    g = built_function("x64", "memory", "array_sum", optimize=False)
    c = Capture()
    return _first(g, initial_var().capture(c), c)


def _return_node():
    g = built_function("x64", "memory", "array_sum", optimize=False)
    c = Capture()
    return _first(g, ret().capture(c), c)


def _float_constant():
    # No fixture lifts to a bare FloatConst, so one is grafted in.
    _lift, g = built_lifter_and_function("x64", "floats", "f64_arith", optimize=False)
    bits = struct.unpack("<Q", struct.pack("<d", 2.5))[0]
    x, y = Capture(), Capture()
    assert g.rewrite(
        find=float_add(var(x), var(y)), replace=tpl.float_round(tpl.float_const(bits))
    )
    c = Capture()
    return _first(g, float_round(float_const(c)), c)


PRESENT = [
    pytest.param(SINT, _integer_constant, id="sint-on-an-integer-constant"),
    pytest.param(FLOAT_BITS, _float_constant, id="float_bits-on-a-float-constant"),
    pytest.param(VALUE_TYPE, _integer_addition, id="value_type-on-an-addition"),
    pytest.param(VN, _entry_register_read, id="vn-on-an-entry-register-read"),
]

ABSENT = [
    pytest.param(SINT, _integer_addition, id="sint-on-an-addition"),
    pytest.param(FLOAT_BITS, _integer_constant, id="float_bits-on-an-int-constant"),
    pytest.param(VALUE_TYPE, _return_node, id="value_type-on-a-return"),
    pytest.param(VN, _integer_addition, id="vn-on-an-addition"),
]


@pytest.mark.parametrize(("reader", "make"), PRESENT)
def test_a_present_aspect_reads_the_same_through_the_opt_spelling(reader, make):
    plain, opt, bound_plain, bound_opt = reader
    m, c = make()
    value = plain(m, c)
    assert value is not None
    assert opt(m, c) == value
    assert bound_plain(m, c) == value
    assert bound_opt(m, c) == value


@pytest.mark.parametrize(("reader", "make"), ABSENT)
def test_an_aspect_the_bound_node_lacks_reads_none_where_the_plain_reader_raises(
    reader, make
):
    plain, opt, bound_plain, bound_opt = reader
    m, c = make()
    assert m.has(c)
    with pytest.raises(strider.StriderError):
        plain(m, c)
    assert opt(m, c) is None
    with pytest.raises(strider.StriderError):
        bound_plain(m, c)
    assert bound_opt(m, c) is None


def test_a_capture_under_an_alternative_that_did_not_fire_reads_none_everywhere():
    g = built_function("x64", "memory", "array_sum", optimize=False)
    taken, skipped = Capture(), Capture()
    hits = [
        m
        for m in g.find_all(
            one_of(
                [
                    int_add(anything(), anything()).capture(taken),
                    int_mul(anything(), anything()).capture(skipped),
                ]
            )
        )
        if not m.has(skipped)
    ]
    assert hits, "no match left the multiplication arm unbound"
    m = hits[0]
    for plain, opt, bound_plain, bound_opt in READERS:
        with pytest.raises(strider.StriderError):
            plain(m, skipped)
        assert opt(m, skipped) is None
        with pytest.raises(strider.StriderError):
            bound_plain(m, skipped)
        assert bound_opt(m, skipped) is None
