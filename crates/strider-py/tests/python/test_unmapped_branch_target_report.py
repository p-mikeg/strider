"""A direct branch out of the mapped image is a result, not an error.

The regions that did decode survive and the target is reported, so Python has
to read the channel to learn the CFG stops at the edge of the buffer.
"""

from __future__ import annotations

import strider

BASE = 0x1000
TARGET = 0x4000_1005
#: jmp 0x40001005, then `ret` filler.
PROGRAM = bytes([0xE9, 0x00, 0x00, 0x00, 0x40]) + b"\xc3" * 8


def _lifter() -> strider.lift.Lifter:
    mem = strider.reader.BufferReader(BASE, PROGRAM)
    return strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)


def test_analyze_reports_the_unmapped_target() -> None:
    result = _lifter().analyze(
        BASE, strider.sleigh.CallingConvention.x86_64_systemv()
    )
    assert result.cfg.unmapped_branch_targets() == [TARGET]
    assert not result.cfg.is_complete()


def test_build_cfg_reports_the_unmapped_target() -> None:
    """The only non-empty channel here, so `is_complete` answers on it alone."""
    cfg = _lifter().build_cfg(BASE)
    assert cfg.unmapped_branch_targets() == [TARGET]
    assert cfg.unverified_seeded_sites() == []
    assert cfg.isa_mode_conflicts() == []
    assert cfg.interior_branch_targets() == []
    assert not cfg.is_complete()
