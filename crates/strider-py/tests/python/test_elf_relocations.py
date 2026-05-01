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
