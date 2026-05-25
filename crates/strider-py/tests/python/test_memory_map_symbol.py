"""Tests for MemoryMap.symbol / .symbols / .entry_point.

These collapse the pyelftools-based symbol-lookup boilerplate that
every other test/example used to carry. The reader caches the parsed
ELF inside MemoryMap when add_region_from_elf runs, so symbol lookups
go directly through the `object` crate instead of re-parsing.
"""

from __future__ import annotations

import pytest

import strider


def test_symbol_returns_address(x86_memory_elf):
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(x86_memory_elf))
    addr = mem.symbol("array_sum")
    assert isinstance(addr, int)
    assert addr > 0


def test_symbol_unknown_raises_reader_error(x86_memory_elf):
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(x86_memory_elf))
    with pytest.raises(strider.errors.StriderError) as excinfo:
        mem.symbol("definitely_not_a_real_symbol_xyz")
    assert "not found" in str(excinfo.value).lower()


def test_symbol_without_elf_raises(x86_memory_elf):
    mem = strider.MemoryMap()
    # No ELF loaded; symbol lookup should fail cleanly.
    with pytest.raises(strider.errors.StriderError):
        mem.symbol("array_sum")


def test_symbols_returns_dict(x86_memory_elf):
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(x86_memory_elf))
    syms = mem.symbols()
    assert isinstance(syms, dict)
    assert "array_sum" in syms
    assert syms["array_sum"] == mem.symbol("array_sum")
    # Sanity: should have at least a handful of real symbols.
    assert len(syms) > 5


def test_entry_point(x86_memory_elf):
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(x86_memory_elf))
    ep = mem.entry_point()
    assert isinstance(ep, int)
    assert ep > 0


def test_entry_point_without_elf_raises():
    mem = strider.MemoryMap()
    with pytest.raises(strider.errors.StriderError):
        mem.entry_point()


def test_two_elfs_first_wins(x86_memory_elf, x86_calls_elf):
    """When two ELFs define the same symbol, the earlier-loaded one wins."""
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(x86_memory_elf))
    mem.add_region_from_elf(str(x86_calls_elf))
    # Both ELFs have a `_start` defined at slightly different addresses.
    syms = mem.symbols()
    # The earlier ELF (memory.elf) wins for shared names.
    mem_only = strider.MemoryMap()
    mem_only.add_region_from_elf(str(x86_memory_elf))
    if "_start" in syms:
        assert syms["_start"] == mem_only.symbol("_start")
