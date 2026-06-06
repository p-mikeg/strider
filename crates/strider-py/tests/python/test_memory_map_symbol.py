"""Tests for `ElfStrider.symbol` / `.symbols` / `.entry_point`.

These collapse the pyelftools-based symbol-lookup boilerplate that
every other test/example used to carry.  `strider.load_elf(path)`
parses the ELF once and answers symbol lookups directly through the
`object` crate instead of re-parsing the file.
"""

from __future__ import annotations

import pytest

import strider


def test_symbol_returns_address(x86_memory_elf):
    elf = strider.load_elf(str(x86_memory_elf))
    addr = elf.symbol("array_sum")
    assert isinstance(addr, int)
    assert addr > 0


def test_symbol_unknown_raises_reader_error(x86_memory_elf):
    elf = strider.load_elf(str(x86_memory_elf))
    with pytest.raises(strider.errors.StriderError) as excinfo:
        elf.symbol("definitely_not_a_real_symbol_xyz")
    assert "not found" in str(excinfo.value).lower()


def test_symbols_returns_dict(x86_memory_elf):
    elf = strider.load_elf(str(x86_memory_elf))
    syms = elf.symbols()
    assert isinstance(syms, dict)
    assert "array_sum" in syms
    assert syms["array_sum"] == elf.symbol("array_sum")
    # Sanity: should have at least a handful of real symbols.
    assert len(syms) > 5


def test_entry_point(x86_memory_elf):
    """Pin the API shape (returns an int, doesn't raise).  We don't
    assert `> 0` here because `fixtures/out/x86/memory.elf` is a
    freestanding function-export ELF whose `e_entry` is 0 by
    construction — there is no `_start`."""
    elf = strider.load_elf(str(x86_memory_elf))
    ep = elf.entry_point()
    assert isinstance(ep, int)
    assert ep >= 0


def test_two_elfs_first_wins(x86_memory_elf, x86_calls_elf):
    """When two ELFs define the same symbol, the earlier-loaded one wins."""
    elf = strider.load_elf(str(x86_memory_elf))
    elf.add_elf(str(x86_calls_elf))
    syms = elf.symbols()
    # The earlier ELF (memory.elf) wins for shared names.
    mem_only = strider.load_elf(str(x86_memory_elf))
    if "_start" in syms:
        assert syms["_start"] == mem_only.symbol("_start")
