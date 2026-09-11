"""`function_arg_float(i)` reaches an incoming float parameter.

`floats.c::f64_arith(double, double)` takes both parameters in XMM0/XMM1 on
x86-64 SysV and never touches an integer argument register, so before float
carriers existed no argument query reached either parameter.
"""

from __future__ import annotations

import strider
from strider.pattern import (
    Capture,
    Pat,
    function_arg,
    any_function_arg,
    function_arg_float,
)

from .conftest import built_function, built_lifter_and_function

CARRIER = Capture("carrier")


def _bound_ids(function, pat) -> list[int]:
    return [m.node(CARRIER).id for m in function.find_all(pat.capture(CARRIER))]


def test_function_arg_float_binds_the_xmm_registers():
    lift, function = built_lifter_and_function("x64", "floats", "f64_arith")
    for i, name in enumerate(("XMM0", "XMM1")):
        hits = function.find_all(function_arg_float(i).capture(CARRIER))
        assert len(hits) == 1, f"function_arg_float({i}) must bind one carrier"
        assert hits[0].vn(CARRIER) == lift.reg(name)


def test_integer_and_float_indices_are_separate():
    function = built_function("x64", "floats", "f64_arith")
    float0 = _bound_ids(function, function_arg_float(0))
    assert len(float0) == 1
    for i in range(8):
        assert float0[0] not in _bound_ids(function, function_arg(i)), (
            f"function_arg({i}) must not reach the float carrier"
        )
    any_carriers = _bound_ids(function, any_function_arg())
    assert float0[0] in any_carriers, "any_function_arg() spans both classes"
    assert len(any_carriers) > 1


def test_function_arg_float_builds_a_pat():
    assert isinstance(function_arg_float(0).into_pat(), Pat)
    assert isinstance(strider.pattern.function_arg_float(1), object)
