"""A call that raises must not leave half its work behind.

A multi-pattern query leaves its patterns usable after a bad one, and
`add_symbols` commits none of the entries it had already extracted.
"""

from __future__ import annotations

import pytest

import strider
from strider import pattern as p
from .conftest import fixture_path


def _function():
    code = bytes([0x48, 0x31, 0xC0, 0xC3])  # xor rax, rax ; ret
    mem = strider.reader.BufferReader(0x1000, code)
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)
    return lift.analyze(
        0x1000, strider.sleigh.CallingConvention.x86_64_systemv()
    ).function


def test_a_failed_join_query_leaves_its_patterns_usable():
    fn = _function()
    good = p.ret().into_pat()
    with pytest.raises(strider.StriderError):
        fn.find_all([good, p.int_add(p.store(), 1)])  # type: ignore[arg-type]  # the refusal under test
    assert fn.find_all(good)
    assert fn.find_all(good)


def test_a_failed_add_symbols_adds_nothing():
    elf = strider.lift.load_elf(str(fixture_path("x64", "arithmetic")))
    with pytest.raises((TypeError, ValueError)):
        elf.add_symbols({"ghost": 0x2000, "bad": "not an address"})  # type: ignore[dict-item]
    assert elf.symbol_opt("ghost") is None
    # A stranded entry published by a later mutation would leave the
    # corrected retry with two `ghost` rows, `by_name` keeping the first.
    elf.add_symbols({"ghost": 0x3000})
    ghost = elf.symbol_opt("ghost")
    assert ghost is not None and ghost.address == 0x3000
