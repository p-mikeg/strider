from __future__ import annotations

import pytest

import strider


def test_symbol_returns_address(x86_memory_elf):
    elf = strider.lift.load_elf(str(x86_memory_elf))
    sym = elf.symbol("array_sum")
    assert isinstance(sym.address, int)
    assert sym.address > 0


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
    assert syms["array_sum"].address == elf.symbol("array_sum").address
    assert len(syms) > 5


def test_entry_point(x86_memory_elf):
    """No `> 0` assert: `memory.elf` is a freestanding function-export ELF
    with no `_start`, so its `e_entry` is 0 by construction."""
    elf = strider.lift.load_elf(str(x86_memory_elf))
    ep = elf.entry_point()
    assert isinstance(ep, int)
    assert ep >= 0


def test_add_elf_merges_symbols_of_both(x86_memory_elf, x86_relocs_elf):
    """A valid (non-overlapping) `add_elf` unions the symbol sets, leaving the
    base ELF's own symbols at their original addresses."""
    elf = strider.lift.load_elf(str(x86_memory_elf))
    base_array_sum = elf.symbol("array_sum").address
    elf.add_elf(str(x86_relocs_elf))
    syms = elf.symbols()
    assert "array_sum" in syms  # from memory.elf
    assert "helper_a" in syms  # from elf_relocs.elf
    assert syms["array_sum"].address == base_array_sum  # base symbol unchanged


def test_add_elf_invalidates_a_populated_symbol_lookup(x86_memory_elf, x86_relocs_elf):
    """A by-name lookup before `add_elf` must not hide the added ELF's symbols
    from a by-name lookup after it."""
    elf = strider.lift.load_elf(str(x86_memory_elf))
    base = elf.symbol("array_sum")  # populates any name index
    assert elf.symbol_opt("helper_a") is None

    elf.add_elf(str(x86_relocs_elf))

    assert elf.symbol("helper_a") is not None  # only in elf_relocs.elf
    after = elf.symbol("array_sum")  # base ELF still wins
    assert (after.address, after.size) == (base.address, base.size)


def test_add_elf_rejects_differing_overlap(x86_memory_elf, x86_calls_elf):
    """`add_elf` is for shared objects at DISTINCT addresses.  memory.elf and
    calls.elf are both linked at ~0x401000, so merging them would map two
    unrelated functions onto one address and splice their bytes together in
    the decode.  Reject the overlap loudly instead of producing garbage."""
    elf = strider.lift.load_elf(str(x86_memory_elf))
    with pytest.raises(strider.StriderError) as excinfo:
        elf.add_elf(str(x86_calls_elf))
    msg = str(excinfo.value).lower()
    assert "differ" in msg and "distinct addresses" in msg


def test_add_elf_then_analyze_sees_merged_regions(x86_memory_elf, x86_relocs_elf):
    """`analyze` must see regions merged by `add_elf`.

    The lifter snapshots the memory map when it is built, so `add_elf`
    rebuilds that snapshot.  Without the rebuild a function living only in
    a merged-in ELF is invisible: the lift has no bytes to read there.

    Uses `elf_relocs.elf` (an ET_DYN at 0x12b0) so the merge does not collide
    with memory.elf's 0x401000 range, the case `add_elf` actually supports.
    """
    elf = strider.lift.load_elf(str(x86_memory_elf))
    # `helper_a` is defined only in elf_relocs.elf, not in memory.elf.
    assert "helper_a" not in strider.lift.load_elf(str(x86_memory_elf)).symbols()
    elf.add_elf(str(x86_relocs_elf))
    _cfg, function, _unresolved = elf.analyze("helper_a")
    assert function.node_count() > 0
