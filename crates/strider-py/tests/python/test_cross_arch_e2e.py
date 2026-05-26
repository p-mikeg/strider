"""Cross-arch end-to-end smoke tests via the Python API.

Pin that AArch64 and MIPS32 fixtures lift cleanly through the full
strider pipeline.  Mirrors the rust-side `arithmetic.rs` matrix but
exercises the Python boundary: ELF load → MemoryMap →
SleighArch + CallingConvention → run → graph.

Each test asserts the lift produces a non-empty IR graph
(specifically, at least one Add node from the `add` function).
Ignores fingerprint absorption details — the goal is "Python lifts
this arch at all", not "the IR is byte-identical".
"""

from __future__ import annotations

import pytest
import strider
import strider.pattern as pat


def _load_fixture(arch_dir: str, case: str = "arithmetic"):
    from .conftest import fixture_path, symbol_addr  # type: ignore

    return fixture_path(arch_dir, case)


def _lift_add(arch: strider.SleighArch, cc: strider.CallingConvention, elf_path):
    from .conftest import symbol_addr  # type: ignore

    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(elf_path))
    entry = symbol_addr(elf_path, "add")
    res = strider.run(arch=arch, cc=cc, mem=mem, entry=entry)
    return res.function


def test_aarch64_arithmetic_add_lifts_cleanly():
    elf = _load_fixture("aarch64")
    g = _lift_add(strider.SleighArch.aarch64(), strider.CallingConvention.aarch64_aapcs64(), elf)
    matches = g.find_all(pat.add(pat.any_(), pat.any_()))
    assert matches, "AArch64 add() must lift to at least one Add node"


def test_mips32le_arithmetic_add_lifts_cleanly():
    elf = _load_fixture("mips32le")
    g = _lift_add(strider.SleighArch.mipsle32(), strider.CallingConvention.mips_o32(), elf)
    matches = g.find_all(pat.add(pat.any_(), pat.any_()))
    assert matches, "MIPS32LE add() must lift to at least one Add node"


def test_mips32be_arithmetic_add_lifts_cleanly():
    elf = _load_fixture("mips32be")
    g = _lift_add(strider.SleighArch.mipsbe32(), strider.CallingConvention.mips_o32(), elf)
    matches = g.find_all(pat.add(pat.any_(), pat.any_()))
    assert matches, "MIPS32BE add() must lift to at least one Add node"
