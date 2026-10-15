"""Each integer-op builder finds the one op of its name in the x64
`arithmetic` fixture. Which op every arch lowers each function to is pinned
by the Rust goldens, fixtures/golden/<arch>/arithmetic.txt.
"""

from __future__ import annotations

import pytest

from strider import pattern as pat
from strider.pattern import anything

from .conftest import built_function

_BINARY = [
    ("add", pat.int_add),
    ("sub", pat.int_sub),
    ("mul", pat.int_mul),
    ("udiv", pat.int_div),
    ("umod", pat.int_rem),
    ("sdiv", pat.int_sdiv),
    ("smod", pat.int_srem),
    ("bit_and", pat.int_and),
    ("bit_or", pat.int_or),
    ("bit_xor", pat.int_xor),
    ("shl", pat.int_shl),
    ("lshr", pat.int_shr),
    ("ashr", pat.int_sshr),
]


@pytest.mark.parametrize(("symbol", "builder"), _BINARY)
def test_binary_builder_finds_its_op(symbol, builder):
    g = built_function("x64", "arithmetic", symbol)
    assert len(g.find_all(builder(anything(), anything()))) == 1


@pytest.mark.parametrize(
    ("symbol", "builder"), [("bit_not", pat.int_not), ("negate", pat.int_neg)]
)
def test_unary_builder_finds_its_op(symbol, builder):
    g = built_function("x64", "arithmetic", symbol)
    assert len(g.find_all(builder(anything()))) == 1


def test_ret_finds_the_return():
    g = built_function("x64", "arithmetic", "add")
    assert len(g.find_all(pat.ret())) == 1
