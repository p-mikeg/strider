"""`Lifter.optimize` folds constant-address loads against the handle's own rom.

Every renderer checks the handle it is given against the graph's; `optimize`
checked nothing, so a `Function` from another binary had its loads folded
against bytes that only exist in THIS one.
"""

from __future__ import annotations

import pytest

import strider
from strider import pattern as p

_CODE = bytes([0x48, 0x8B, 0x04, 0x25, 0x00, 0x20, 0x40, 0x00, 0xC3])  # mov rax,[0x402000]; ret
_BASE = 0x401000
_RO = 0x402000


def _lifter(fill: int):
    mem = strider.reader.BufferReader(_BASE, _CODE)
    rom = strider.reader.BufferReader(_RO, bytes([fill]) * 8)
    return strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem, rom=rom)


def _function(lift):
    return lift.analyze(
        _BASE, strider.sleigh.CallingConvention.x86_64_systemv()
    ).function


def test_optimize_rejects_a_function_from_another_handle():
    mine, theirs = _lifter(0x11), _lifter(0x22)
    foreign = _function(theirs)
    with pytest.raises(strider.StriderError, match="different Lifter"):
        mine.optimize(foreign)


def test_optimize_folds_its_own_functions_load_against_its_own_rom():
    """The positive case: an unoptimised `Function` from this handle has its
    constant-address load folded to the bytes THIS handle's rom serves."""
    lift = _lifter(0x11)
    fn = lift.analyze(
        _BASE,
        strider.sleigh.CallingConvention.x86_64_systemv(),
        opts=strider.lift.LifterOptions(
            pipeline=strider.opt.OptimizerPipeline.empty()
        ),
    ).function
    assert len(fn.find_all(p.load())) == 1
    lift.optimize(fn)
    assert fn.find_all(p.load()) == []
    assert len(fn.find_all(p.int_const(0x1111_1111_1111_1111))) == 1


def test_optimize_rejects_a_cross_arch_function():
    """`optimize` refuses a foreign arch for the same reason the renderers
    do: the graph's varnodes are another register table's."""
    x86 = strider.lift.lifter(
        strider.sleigh.SleighArch.x86(), strider.reader.BufferReader(_BASE, _CODE)
    )
    other = _lifter(0x11)
    with pytest.raises(strider.StriderError, match="different Lifter"):
        x86.optimize(_function(other))
