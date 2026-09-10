"""`Lifter.optimize` folds constant-address loads against the handle's own rom.

Every renderer checks the handle it is given against the graph's; `optimize`
checked nothing, so a `Function` from another binary had its loads folded
against bytes that only exist in THIS one.
"""

from __future__ import annotations

import pytest

import strider

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


def test_optimize_accepts_its_own_function():
    lift = _lifter(0x11)
    lift.optimize(_function(lift))


def test_optimize_rejects_a_cross_arch_function():
    """The renderers raise here; `optimize` used to accept it."""
    x86 = strider.lift.lifter(
        strider.sleigh.SleighArch.x86(), strider.reader.BufferReader(_BASE, _CODE)
    )
    other = _lifter(0x11)
    with pytest.raises(strider.StriderError, match="different Lifter"):
        x86.optimize(_function(other))
