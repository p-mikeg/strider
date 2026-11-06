"""Merging a second image into a loaded `ElfLifter`: what `add_elf` unions,
what it refuses, and what the lift sees afterwards."""

from __future__ import annotations

import pytest

import strider

from .conftest import fixture_path


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


@pytest.mark.parametrize(
    ("loaded", "added", "differs"),
    [
        ("x64", "aarch64", "Aarch64"),
        ("aarch64", "aarch64be", "big-endian"),
        ("arm_be", "arm_be8", "BE8"),
    ],
)
def test_add_elf_refuses_an_image_another_decoder_reads(loaded, added, differs):
    """One handle decodes with one arch, so an image of another architecture,
    class or byte order, or a BE8 image beside a BE32 one (`EI_DATA` marks
    both alike), would decode as garbage. Refused before any byte merges."""
    elf = strider.lift.load_elf(str(fixture_path(loaded, "memory")))
    with pytest.raises(strider.StriderError, match="one handle decodes one target") as e:
        elf.add_elf(str(fixture_path(added, "calls")))
    assert differs in str(e.value)


@pytest.mark.parametrize("method", ["add_elf", "add_symbol_file"])
def test_a_missing_or_non_elf_path_raises_as_load_elf_does(method, x86_memory_elf, tmp_path):
    elf = strider.lift.load_elf(str(x86_memory_elf))
    with pytest.raises(FileNotFoundError):
        getattr(elf, method)(str(tmp_path / "missing.elf"))
    text = tmp_path / "text.txt"
    text.write_bytes(b"not an ELF at all")
    with pytest.raises(ValueError, match="not an ELF"):
        getattr(elf, method)(str(text))
