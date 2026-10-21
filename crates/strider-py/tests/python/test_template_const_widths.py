"""A fresh template constant is built at the width it is evaluated at, never at
the `I1` a comparison root carries, and is refused where nothing gives it one.

`br_neg` is `a < 0` on an i32 `a`, so `int_slt(var(a), int_const(0))` matches
once with an `I1` root.
"""

import gc

import pytest

import strider
from strider import pattern as p
from strider import template as t

from .conftest import fixture_path


def _sign_test():
    elf = strider.lift.load_elf(str(fixture_path("x64", "cmp_branches")))
    return elf.analyze("br_neg").function


def _rewrite(fn, make):
    a = p.Capture()
    return fn.rewrite(find=p.int_slt(p.var(a), p.int_const(0)), replace=make(a))


def _consts(fn, shape):
    """`(type, value)` of the constant `shape(k)` captures as `k`."""
    k = p.Capture()
    return {(m.value_type(k), m.uint(k)) for m in fn.find_all(shape(p.int_const().capture(k)))}


def _shift_counts(fn):
    return _consts(fn, lambda k: p.int_shr(p.anything(), k))


def test_a_shift_count_under_an_i1_root_keeps_its_value():
    fn = _sign_test()
    assert _rewrite(fn, lambda a: t.int_truncate(t.int_shr(t.var(a), 31))) == 1
    assert fn.validate() is None
    assert _shift_counts(fn) == {("I32", 31)}


def test_a_shift_count_in_a_bit_test_keeps_its_value():
    fn = _sign_test()
    assert _rewrite(fn, lambda a: t.int_eq(t.int_and(t.int_shr(t.var(a), 5), 1), 0)) == 1
    assert fn.validate() is None
    assert _shift_counts(fn) == {("I32", 5)}


def test_an_untyped_popcount_operand_is_refused():
    fn = _sign_test()
    with pytest.raises(strider.StriderError, match="has no width"):
        _rewrite(fn, lambda a: t.int_eq(t.var(a), t.int_popcount(t.int_const(0xFF))))


def test_capture_typed_types_a_constant_at_the_capture_width():
    fn = _sign_test()

    def replace(a):
        return t.int_eq(t.var(a), t.int_popcount(t.capture_typed(a, t.int_const(0xFF))))

    assert _rewrite(fn, replace) == 1
    assert fn.validate() is None
    assert _consts(fn, p.int_popcount) == {("I32", 0xFF)}


def test_capture_typed_reports_its_operand_to_the_collector():
    inner = t.int_const(1)
    assert inner in gc.get_referents(t.capture_typed(p.Capture(), inner))


@pytest.mark.parametrize("inner", [lambda c: c, t.var], ids=["capture", "var"])
def test_capture_typed_over_a_capture_is_refused(inner):
    c = p.Capture()
    with pytest.raises(strider.StriderError, match="over a capture"):
        t.capture_typed(c, inner(c))
