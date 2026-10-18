"""The lowered comparisons build on a rewrite RHS the shape the lifter emits,
with each operand in the role the matching helper and the spelled-out shape
expect."""

import pytest

import strider
from strider import pattern as p
from strider import template as t

from .conftest import fixture_path


def _function(arch, case, symbol):
    return strider.lift.load_elf(str(fixture_path(arch, case))).analyze(symbol).function


def _negated(cmp):
    return p.bool_xor(cmp, p.bool_const(True))


def _pairs(fn, pat, l, r):
    return {(m.node(l).id, m.node(r).id) for m in fn.find_all(pat)}


@pytest.mark.parametrize(
    ("op", "spelled", "source", "arch", "case", "symbol"),
    [
        ("int_ne", lambda x, y: _negated(p.int_eq(x, y)),
         "int_slt", "mips32be", "complex", "complex_dispatch"),
        ("int_le", lambda x, y: _negated(p.int_lt(y, x)),
         "int_slt", "mips32be", "complex", "complex_dispatch"),
        ("int_sle", lambda x, y: _negated(p.int_slt(y, x)),
         "int_slt", "mips32be", "complex", "complex_dispatch"),
        ("float_ne", lambda x, y: _negated(p.float_eq(x, y)),
         "float_lt", "aarch64", "floats", "f32_compare"),
    ],
)
def test_a_lowered_comparison_builds_the_shape_its_pattern_matches(
    op, spelled, source, arch, case, symbol
):
    fn = _function(arch, case, symbol)
    a, b = p.Capture(), p.Capture()
    sources = _pairs(fn, getattr(p, source)(p.var(a), p.var(b)), a, b)
    assert sources
    fired = fn.rewrite(
        find=getattr(p, source)(p.var(a), p.var(b)),
        replace=getattr(t, op)(t.var(a), t.var(b)),
    )
    assert fired > 0
    assert fn.validate() is None
    x, y = p.Capture(), p.Capture()
    assert sources <= _pairs(fn, getattr(p, op)(p.var(x), p.var(y)), x, y)
    assert sources <= _pairs(fn, spelled(p.var(x), p.var(y)), x, y)
