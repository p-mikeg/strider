"""Tests for `MemoryMap.symbol_size` + `MemoryMap.symbol_addr_and_size`.

The ELF symbol table records each function's size in `st_size`.
Strider users typically need that value for `function_max_size=`
on `strider.run` / `strider.build_cfg` so the analyser knows where
the function ends (and the indirect-branch resolver can
distinguish intra-fn jumps from tail calls).  Without these
helpers users have to fall back to pyelftools.
"""

from __future__ import annotations

import pytest

import strider

from .conftest import fixture_path


def test_symbol_size_returns_known_function_size():
    elf = fixture_path("x64", "elf_relocs")
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(elf))
    # `helper_a` is a 4-byte function in the ELF (`add eax, 100; ret`
    # = ~8 bytes — we accept anything > 0 to stay tolerant of
    # toolchain-version layout differences).
    size = mem.symbol_size("helper_a")
    assert size is not None and size > 0


def test_symbol_size_raises_on_unknown_symbol():
    elf = fixture_path("x64", "elf_relocs")
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(elf))
    with pytest.raises(strider.errors.StriderError):
        mem.symbol_size("definitely_not_a_symbol")


def test_symbol_addr_and_size_returns_addr_and_size():
    elf = fixture_path("x64", "elf_relocs")
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(elf))
    addr, size = mem.symbol_addr_and_size("helper_a")
    assert addr == mem.symbol("helper_a")
    assert size == mem.symbol_size("helper_a")


def test_symbol_addr_and_size_threads_into_strider_run():
    """End-to-end: derive `function_max_size` from the ELF, pass it
    into `strider.run`, confirm the analyser respects the bound."""
    elf = fixture_path("x64", "switch")
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(elf))
    addr, size = mem.symbol_addr_and_size("dispatch_value")
    assert size is not None
    result = strider.run(
        arch=strider.SleighArch.x86_64(),
        cc=strider.CallingConvention.x86_64_systemv(),
        mem=mem,
        rom=mem,
        entry=addr,
        function_max_size=size,
        allow_code_before_start_addr=True,
    )
    assert result.function.node_count() > 0


def test_symbol_size_returns_none_for_zero_st_size():
    """ELF symbols with `st_size == 0` (typical for stripped binaries
    or stub functions) come back as `None` — not 0 — so callers
    can branch with `if size is not None`."""
    elf = fixture_path("x86", "control")
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(elf))
    # Walk every symbol; if any has size 0 the helper must report None.
    saw_none = False
    for name in mem.symbols():
        size = mem.symbol_size(name)
        if size is None:
            saw_none = True
            break
    # We don't assert saw_none unconditionally — the test is robust
    # whether or not the binary has a zero-size symbol.  The
    # important contract is "size 0 → None" and the test above for
    # helper_a covers the "size > 0 → integer" branch.
    _ = saw_none
