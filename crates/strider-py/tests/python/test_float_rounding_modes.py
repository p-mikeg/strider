"""The three float rounding ops, matched and evaluated.

`FloatUnaryOp::Round` is p-code `opRound`: nearest, ties AWAY FROM ZERO, not
IEEE `roundToIntegralTiesToEven`. `round(2.5)` is therefore 3 and
`round(-2.5)` is -3, where ties-to-even would answer 2 and -2. Constant
folding is the only place that distinction is observable from Python, so the
value is read back off a folded constant rather than asserted about the node
kind alone.
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
    float_ceil,
    float_const,
    float_floor,
    float_round,
    var,
)

from .conftest import built_lifter_and_function, fixture_path


def _bits(value: float) -> int:
    return struct.unpack("<Q", struct.pack("<d", value))[0]


def _value(bits: int) -> float:
    return struct.unpack("<d", struct.pack("<Q", bits))[0]


def _folded_constants(build, operand: float):
    """Graft `build(operand)` over every float add in a real graph, optimize,
    and report the float constants the pipeline is left holding."""
    lift, g = built_lifter_and_function("x64", "floats", "f64_arith", optimize=False)
    x, y = Capture(), Capture()
    assert g.rewrite(
        find=float_add(var(x), var(y)), replace=build(tpl.float_const(_bits(operand)))
    )
    assert g.validate() is None
    lift.optimize(g)
    c = Capture()
    return {_value(m.float_bits(c)) for m in g.find_all(float_const(c))}


ROUNDING = [
    pytest.param(tpl.float_round, 2.5, 3.0, 2.0, id="round-2.5-away-from-zero"),
    pytest.param(tpl.float_round, -2.5, -3.0, -2.0, id="round-minus-2.5-away-from-zero"),
    pytest.param(tpl.float_ceil, 2.1, 3.0, 2.0, id="ceil-2.1-upward"),
    pytest.param(tpl.float_ceil, -2.1, -2.0, -3.0, id="ceil-minus-2.1-upward"),
    pytest.param(tpl.float_floor, 2.1, 2.0, 3.0, id="floor-2.1-downward"),
]


@pytest.mark.parametrize(("build", "operand", "expected", "rejected"), ROUNDING)
def test_a_rounding_op_on_a_constant_folds_to_the_value_its_name_promises(
    build, operand, expected, rejected
):
    folded = _folded_constants(build, operand)
    assert expected in folded
    assert rejected not in folded


def test_the_ceiling_and_round_patterns_do_not_match_each_other():
    _lift, g = built_lifter_and_function("x64", "floats", "f64_arith", optimize=False)
    x, y = Capture(), Capture()
    assert g.rewrite(
        find=float_add(var(x), var(y)),
        replace=tpl.float_round(tpl.float_const(_bits(2.5))),
    )
    c = Capture()
    assert len(g.find_all(float_round(float_const(c)))) == 1
    assert not g.find_all(float_ceil(anything()))
    assert not g.find_all(float_floor(anything()))


def test_the_round_pattern_matches_the_x87_lift_that_emits_it():
    """`fistp` lowers to FLOAT_ROUND then a float-to-int conversion, the one
    place in the fixture corpus a rounding op arrives from real bytes."""
    prog = strider.lift.load_elf(str(fixture_path("x86", "floats")))
    g = prog.analyze(
        "float_to_int",
        opts=strider.lift.LifterOptions(pipeline=strider.opt.OptimizerPipeline.empty()),
    ).function
    assert len(g.find_all(float_round(anything()))) == 1
    assert not g.find_all(float_ceil(anything()))
