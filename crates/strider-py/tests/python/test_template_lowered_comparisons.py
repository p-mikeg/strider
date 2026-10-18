"""The lowered comparisons build on a rewrite RHS the shape the lifter emits,
so the matching helper finds what the building one made."""

import pytest

import strider
from strider import pattern as p
from strider import template as t

from .conftest import fixture_path


def _function(arch, case, symbol):
    return strider.lift.load_elf(str(fixture_path(arch, case))).analyze(symbol).function


@pytest.mark.parametrize(
    ("op", "source", "arch", "case", "symbol"),
    [
        ("int_ne", "int_slt", "mips32be", "complex", "complex_dispatch"),
        ("int_le", "int_slt", "mips32be", "complex", "complex_dispatch"),
        ("int_sle", "int_slt", "mips32be", "complex", "complex_dispatch"),
        ("float_ne", "float_lt", "aarch64", "floats", "f32_compare"),
    ],
)
def test_a_lowered_comparison_builds_the_shape_its_pattern_matches(op, source, arch, case, symbol):
    fn = _function(arch, case, symbol)
    find, build = getattr(p, op), getattr(t, op)
    before = len(fn.find_all(find(p.anything(), p.anything())))
    a, b = p.Capture(), p.Capture()
    fired = fn.rewrite(
        find=getattr(p, source)(p.var(a), p.var(b)), replace=build(t.var(a), t.var(b))
    )
    assert fired > 0
    assert fn.validate() is None
    assert len(fn.find_all(find(p.anything(), p.anything()))) > before
