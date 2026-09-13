"""Bytes that hold no instruction are reported, never decoded or raised.

A branch to a word Sleigh rejects keeps the rest of the function, and a
literal pool after a call to an unmarked no-return function is data the ELF's
mapping symbols mark, so `analyze` must not decode it as code.
"""

from __future__ import annotations

import shutil
import subprocess
from pathlib import Path

import pytest

import strider

BASE = 0x1000
#: beq 0x1008 ; blr ; .long 0 (glibc's PowerPC abort word)
PPC_BRANCH_TO_ABORT_WORD = b"".join(
    w.to_bytes(4, "big") for w in (0x4182_0008, 0x4E80_0020, 0)
)


def test_analyze_reports_a_branch_to_an_undecodable_word() -> None:
    mem = strider.reader.BufferReader(BASE, PPC_BRANCH_TO_ABORT_WORD)
    lifter = strider.lift.lifter(strider.sleigh.SleighArch.ppc32be(), mem)
    opts = strider.lift.LifterOptions(
        cfg=strider.cfg.CfgOptions(function_max_size=len(PPC_BRANCH_TO_ABORT_WORD))
    )
    result = lifter.analyze(BASE, strider.sleigh.CallingConvention.powerpc_sysv32(), opts)
    assert result.cfg.undecodable_branch_targets() == [BASE + 8]
    assert not result.cfg.is_complete()


def test_data_ranges_round_trip_sorted_and_merged() -> None:
    opts = strider.cfg.CfgOptions(data_ranges=[(0x20, 0x28), (0x10, 0x18), (0x14, 0x1C)])
    assert opts.data_ranges == [(0x10, 0x1C), (0x20, 0x28)]
    assert opts.with_data_ranges([]).data_ranges == []


MID_POOL = """
    .syntax unified
    .text
    .arm
    .global _start
_start: b _start
    .global abort
    .type abort, %function
abort:
    b abort
    .size abort, .-abort
    .global mid_pool
    .type mid_pool, %function
mid_pool:
    push {r4, lr}
    cmp r0, #0
    bne 2f
    bl abort
.Lpool:
    .word 0x00012345
    .word 0x00010064
2:  ldr r1, .Lpool
    add r0, r0, r1
    pop {r4, pc}
    .size mid_pool, .-mid_pool
"""


@pytest.mark.skipif(
    shutil.which("arm-linux-gnueabihf-gcc") is None, reason="needs an ARM cross gcc"
)
def test_elf_mapping_symbols_keep_a_literal_pool_from_being_decoded(tmp_path: Path) -> None:
    src = tmp_path / "midpool.S"
    src.write_text(MID_POOL)
    elf = tmp_path / "midpool.elf"
    subprocess.run(
        ["arm-linux-gnueabihf-gcc", "-nostdlib", "-static", "-o", str(elf), str(src)],
        check=True,
    )
    lifter = strider.lift.load_elf(elf)
    sym = lifter.symbol("mid_pool")
    pool = sym.address + 0x10
    result = lifter.analyze("mid_pool")
    assert result.cfg.undecodable_branch_targets() == [pool]
    assert result.cfg.region_at(pool) is None
