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


def test_add_elf_then_analyze_sees_merged_regions(x86_memory_elf, x86_calls_elf):
    """Regression: `analyze` must see regions merged by `add_elf`.

    The `ElfStrider`'s inner `Strider` run handle snapshots the memory
    map when it is built.  Before the fix it was built once at
    construction, so a function whose code lives ONLY in a later
    `add_elf`-merged ELF was invisible to `analyze` — the lift had no
    bytes to read.  `add_elf` now rebuilds the inner handle from the
    merged regions, so analysing a calls.elf-only function succeeds.
    """
    elf = strider.load_elf(str(x86_memory_elf))
    # `fib_recursive` is defined only in calls.elf, not in memory.elf.
    assert "fib_recursive" not in strider.load_elf(str(x86_memory_elf)).symbols()
    elf.add_elf(str(x86_calls_elf))
    analysis = elf.analyze("fib_recursive")
    # A real lift of `fib_recursive` produces a non-trivial IR graph;
    # before the fix this raised because the inner run handle's memory
    # snapshot predated the merge, so the lifter had no bytes to read at
    # the calls.elf-only address.
    assert analysis.function.node_count() > 0
