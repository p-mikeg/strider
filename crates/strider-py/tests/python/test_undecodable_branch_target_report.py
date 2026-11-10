"""Bytes that hold no instruction are reported incomplete, never decoded.

A branch to a word Sleigh rejects keeps the rest of the function, and a
literal pool after a call to an unmarked no-return function is data the ELF's
mapping symbols mark, so `analyze` must not decode it as code.
"""

from __future__ import annotations

import pytest

import strider

from .conftest import WORKSPACE_ROOT

BASE = 0x1000
#: beq 0x1008 ; blr ; .long 0 (glibc's PowerPC abort word)
PPC_BRANCH_TO_ABORT_WORD = b"".join(
    w.to_bytes(4, "big") for w in (0x4182_0008, 0x4E80_0020, 0)
)


def test_analyze_reports_a_branch_to_an_undecodable_word() -> None:
    mem = strider.reader.BufferReader(BASE, PPC_BRANCH_TO_ABORT_WORD)
    lifter = strider.lift.lifter(strider.sleigh.SleighArch.ppc32be(), mem)
    opts = strider.lift.LifterOptions(
        cfg=strider.cfg.CfgOptions(function_max_size=len(PPC_BRANCH_TO_ABORT_WORD)),
    )
    result = lifter.analyze(BASE, strider.sleigh.CallingConvention.powerpc_sysv32(), opts)
    assert [a for a, r in result.cfg.incomplete if r == "undecodable_target"] == [BASE + 8]


def test_data_ranges_round_trip_sorted_and_merged() -> None:
    opts = strider.cfg.CfgOptions(data_ranges=[(0x20, 0x28), (0x10, 0x18), (0x14, 0x1C)])
    assert opts.data_ranges == [(0x10, 0x1C), (0x20, 0x28)]


def test_elf_mapping_symbols_keep_a_literal_pool_from_being_decoded() -> None:
    """`fixtures/loader/out/arm_mid_pool.elf`, from `src/arm_mid_pool.S`."""
    elf = WORKSPACE_ROOT / "fixtures" / "loader" / "out" / "arm_mid_pool.elf"
    with elf.open("rb") as f:
        if f.read(42) == b"version https://git-lfs.github.com/spec/v1":
            pytest.fail(f"{elf} is a Git LFS pointer; run `git lfs pull`")
    lifter = strider.lift.load_elf(elf)
    sym = lifter.symbol("mid_pool")
    pool = sym.address + 0x10
    result = lifter.analyze(
        "mid_pool",
    )
    assert [a for a, r in result.cfg.incomplete if r == "undecodable_target"] == [pool]
    assert result.cfg.region_at(pool) is None
