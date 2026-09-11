"""Pins "Python lifts this arch at all", not IR shape: each test only
requires the `add` fixture to yield at least one Add node.
"""

from __future__ import annotations

import pytest
import strider
import strider.pattern as pat


def _load_fixture(arch_dir: str, case: str = "arithmetic"):
    from .conftest import fixture_path, symbol_addr  # type: ignore

    return fixture_path(arch_dir, case)


def _lift_add(arch: strider.sleigh.SleighArch, cc: strider.sleigh.CallingConvention, elf_path):
    from .conftest import symbol_addr  # type: ignore

    mem = strider.lift.load_elf(str(elf_path)).reader()
    entry = symbol_addr(elf_path, "add")
    lift = strider.lift.lifter(arch, mem)
    _cfg, function, _unresolved = lift.analyze(entry, cc)
    return function


def test_aarch64_arithmetic_add_lifts_cleanly():
    elf = _load_fixture("aarch64")
    g = _lift_add(strider.sleigh.SleighArch.aarch64(), strider.sleigh.CallingConvention.aarch64_aapcs64(), elf)
    matches = g.find_all(pat.int_add(pat.anything(), pat.anything()))
    assert matches, "AArch64 int_add() must lift to at least one Add node"


def test_mips32le_arithmetic_add_lifts_cleanly():
    elf = _load_fixture("mips32le")
    g = _lift_add(strider.sleigh.SleighArch.mipsle32(), strider.sleigh.CallingConvention.mips_o32(), elf)
    matches = g.find_all(pat.int_add(pat.anything(), pat.anything()))
    assert matches, "MIPS32LE int_add() must lift to at least one Add node"


def test_mips32be_arithmetic_add_lifts_cleanly():
    elf = _load_fixture("mips32be")
    g = _lift_add(strider.sleigh.SleighArch.mipsbe32(), strider.sleigh.CallingConvention.mips_o32(), elf)
    matches = g.find_all(pat.int_add(pat.anything(), pat.anything()))
    assert matches, "MIPS32BE int_add() must lift to at least one Add node"
