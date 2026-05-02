"""Tests for `MemoryMap.add_region_from_elf(apply_relocations=True)`.

The `fixtures/cases/elf_relocs.c` shared library is built with
`-shared -fPIC`, producing an ET_DYN ELF whose `dispatch_table` is a
function-pointer array in `.data.rel.ro`.  Each slot has a
`R_X86_64_64` (or arch-equivalent) relocation pointing at one of
`helper_a..helper_d`.

Without `apply_relocations=True`, the analyser sees zeros where the
function addresses should be (and `dispatch_via_table` follows an
indirect call into address 0).  With the flag, the slots read the
real helper addresses and pattern queries against the call site
work the same way they do on a normally-linked ET_EXEC binary.

Mirrors the FreeBSD-kernel scenario the strider user reported (see
`crates/reader/src/elf.rs::apply_elf_relocations` for the kernel-
specific motivation).
"""

from __future__ import annotations

import struct

import pytest

import strider

from .conftest import fixture_path


X64_RELOCS = lambda: fixture_path("x64", "elf_relocs")  # noqa: E731


def _read_u64_le(mem, addr):
    """Helper: read 8 bytes from MemoryMap and decode as LE u64.
    Returns None if the region isn't mapped at `addr`."""
    bs = mem.read(addr, 8)
    if bs is None:
        return None
    return struct.unpack("<Q", bs)[0]


def test_default_load_does_not_apply_relocations():
    elf = X64_RELOCS()
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(elf))  # apply_relocations=False (default)
    table_addr = mem.symbol("dispatch_table")
    # The default path doesn't even load `.data.rel.ro` — read returns
    # None.  This is the back-compat behaviour: existing strider-py
    # callers see no change.
    assert mem.read(table_addr, 8) is None


def test_apply_relocations_true_populates_dispatch_table():
    elf = X64_RELOCS()
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(elf), apply_relocations=True)
    table_addr = mem.symbol("dispatch_table")
    helper_a = mem.symbol("helper_a")
    helper_b = mem.symbol("helper_b")
    helper_c = mem.symbol("helper_c")
    helper_d = mem.symbol("helper_d")
    # Each 8-byte slot now reads its helper's address.  The whole
    # point of the fix: the analyser can read function-pointer
    # tables in ET_DYN binaries without first having to apply
    # relocations by hand.
    assert _read_u64_le(mem, table_addr) == helper_a
    assert _read_u64_le(mem, table_addr + 8) == helper_b
    assert _read_u64_le(mem, table_addr + 16) == helper_c
    assert _read_u64_le(mem, table_addr + 24) == helper_d


def test_apply_relocations_idempotent():
    """Loading the same ELF twice with apply_relocations=True must
    leave the second load's slots reading the same value as the
    first — relocation application is a deterministic write."""
    elf = X64_RELOCS()
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(elf), apply_relocations=True)
    helper_a = mem.symbol("helper_a")
    table_addr = mem.symbol("dispatch_table")
    first = _read_u64_le(mem, table_addr)
    # Re-loading would produce duplicate regions in the MemoryMap
    # (the lookup table's "last one inserted wins" rule keeps the
    # newer copy); both copies must be patched identically.
    mem.add_region_from_elf(str(elf), apply_relocations=True)
    second = _read_u64_le(mem, table_addr)
    assert first == second == helper_a


def test_apply_relocations_default_argument_is_false():
    """Pin the default-False contract — third-party code that
    doesn't pass the flag must continue to see only code-and-
    readonly sections."""
    import inspect
    sig = inspect.signature(strider.MemoryMap.add_region_from_elf)
    p = sig.parameters.get("apply_relocations")
    assert p is not None, "MemoryMap.add_region_from_elf must accept apply_relocations"
    # We can't introspect the *value* of the default through the
    # PyO3-generated signature object reliably, but the absence-of-
    # mapping check below confirms the runtime default is False.
    elf = X64_RELOCS()
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(elf))
    table_addr = mem.symbol("dispatch_table")
    assert mem.read(table_addr, 8) is None  # no .data.rel.ro coverage


def test_apply_elf_relocations_method_returns_stats():
    """Standalone form: load some way (here via the wider apply=True
    path), then call apply_elf_relocations(path) to get the stats."""
    elf = X64_RELOCS()
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(elf), apply_relocations=True)
    stats = mem.apply_elf_relocations(str(elf))
    # Every reloc in elf_relocs.elf is one we model
    # (R_X86_64_64 / R_X86_64_GLOB_DAT / R_X86_64_JUMP_SLOT) and the
    # widened load covered every site, so applied should equal seen.
    assert stats.seen >= 6, f"seen too low: {stats!r}"
    assert stats.applied == stats.seen, (
        f"some relocs unexpectedly skipped: {stats!r}"
    )
    assert stats.skipped_unsupported_kind == 0
    assert stats.skipped_no_region == 0


def test_apply_elf_relocations_reports_skipped_no_region():
    """When the load step omits `.data.rel.ro` (default behaviour),
    the standalone applier counts the relocs as `skipped_no_region`
    rather than silently dropping them."""
    elf = X64_RELOCS()
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(elf))  # default: no `.data.rel.ro`
    stats = mem.apply_elf_relocations(str(elf))
    # Sites in `.data.rel.ro` aren't in any region → skipped_no_region.
    assert stats.skipped_no_region >= 4, (
        f"expected ≥4 dispatch_table sites skipped: {stats!r}"
    )
    assert stats.applied == 0


def test_relocation_stats_repr_round_trips():
    """`RelocationStats` repr is stable enough to use in test
    diagnostics; the field accessors match the doc-comment."""
    elf = X64_RELOCS()
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(elf), apply_relocations=True)
    stats = mem.apply_elf_relocations(str(elf))
    s = repr(stats)
    for field in (
        "seen",
        "applied",
        "skipped_unresolved_target",
        "skipped_unsupported_kind",
        "skipped_no_region",
        "unsupported_r_types",
    ):
        assert field in s, f"repr missing {field!r}: {s}"


def test_unsupported_r_types_is_empty_when_all_applied():
    """When every relocation in the ELF is one we model, the
    diagnostic list is empty.  This pins the absence of
    false-positives in the unsupported-kind tracker."""
    elf = X64_RELOCS()
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(elf), apply_relocations=True)
    stats = mem.apply_elf_relocations(str(elf))
    assert stats.skipped_unsupported_kind == 0
    assert stats.unsupported_r_types == []


def test_unsupported_r_types_field_exists_on_default_load():
    """The list is present whether or not it has entries — pin the
    accessor's existence so future API removals can't go quiet."""
    elf = X64_RELOCS()
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(elf))
    stats = mem.apply_elf_relocations(str(elf))
    # Default load doesn't include `.data.rel.ro`, so every reloc is
    # skipped_no_region — but it's not unsupported_kind, so the
    # diagnostic list stays empty.  The point of this test is to pin
    # the accessor itself, not the contents.
    assert isinstance(stats.unsupported_r_types, list)
