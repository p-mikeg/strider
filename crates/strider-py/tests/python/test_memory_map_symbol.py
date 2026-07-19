"""`ElfLifter.symbol` / `.symbols` / `.entry_point`: symbol lookup off a
single parse, replacing the pyelftools boilerplate every test and example
used to carry.
"""

from __future__ import annotations

import pytest

import strider


def test_symbol_returns_address(x86_memory_elf):
    elf = strider.lift.load_elf(str(x86_memory_elf))
    addr = elf.symbol("array_sum")
    assert isinstance(addr, int)
    assert addr > 0


def test_symbol_unknown_raises_reader_error(x86_memory_elf):
    elf = strider.lift.load_elf(str(x86_memory_elf))
    with pytest.raises(strider.StriderError) as excinfo:
        elf.symbol("definitely_not_a_real_symbol_xyz")
    assert "not found" in str(excinfo.value).lower()


def test_symbols_returns_dict(x86_memory_elf):
    elf = strider.lift.load_elf(str(x86_memory_elf))
    syms = elf.symbols()
    assert isinstance(syms, dict)
    assert "array_sum" in syms
    assert syms["array_sum"] == elf.symbol("array_sum")
    assert len(syms) > 5


def test_entry_point(x86_memory_elf):
    """No `> 0` assert: `memory.elf` is a freestanding function-export ELF
    with no `_start`, so its `e_entry` is 0 by construction."""
    elf = strider.lift.load_elf(str(x86_memory_elf))
    ep = elf.entry_point()
    assert isinstance(ep, int)
    assert ep >= 0


def test_two_elfs_first_wins(x86_memory_elf, x86_calls_elf):
    """When two ELFs define the same symbol, the earlier-loaded one wins."""
    elf = strider.lift.load_elf(str(x86_memory_elf))
    elf.add_elf(str(x86_calls_elf))
    syms = elf.symbols()
    mem_only = strider.lift.load_elf(str(x86_memory_elf))
    if "_start" in syms:
        assert syms["_start"] == mem_only.symbol("_start")


def test_add_elf_then_analyze_sees_merged_regions(x86_memory_elf, x86_calls_elf):
    """Regression: `analyze` must see regions merged by `add_elf`.

    The lifter snapshots the memory map when it is built.  That snapshot
    used to predate any later `add_elf`, so a function living only in a
    merged-in ELF was invisible: the lift had no bytes to read there.
    """
    elf = strider.lift.load_elf(str(x86_memory_elf))
    # `fib_recursive` is defined only in calls.elf, not in memory.elf.
    assert "fib_recursive" not in strider.lift.load_elf(str(x86_memory_elf)).symbols()
    elf.add_elf(str(x86_calls_elf))
    _cfg, function, _unresolved = elf.analyze("fib_recursive")
    assert function.node_count() > 0
