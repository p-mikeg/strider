"""Tests for `strider.lift.load_elf(path, apply_relocations=True)`.

The `elf_relocs.c` fixture is an ET_DYN shared library whose
`dispatch_table` is a function-pointer array in `.data.rel.ro`; every
slot is a relocation against `helper_a..helper_d`.  Unrelocated, the
analyser reads zeros there and follows an indirect call into address 0.
"""

from __future__ import annotations

import struct

import strider

from .conftest import fixture_path


X64_RELOCS = lambda: fixture_path("x64", "elf_relocs")  # noqa: E731


def _read_u64_le(elf, addr):
    """None if the region isn't mapped at `addr`."""
    bs = elf.read(addr, 8)
    if bs is None:
        return None
    return struct.unpack("<Q", bs)[0]


def test_default_load_applies_relocations():
    elf = strider.lift.load_elf(str(X64_RELOCS()))  # apply_relocations=True (default)
    table_addr = elf.symbol("dispatch_table").address
    helper_a = elf.symbol("helper_a").address
    assert _read_u64_le(elf, table_addr) == helper_a


def test_apply_relocations_true_populates_dispatch_table():
    elf = strider.lift.load_elf(str(X64_RELOCS()), apply_relocations=True)
    table_addr = elf.symbol("dispatch_table").address
    helper_a = elf.symbol("helper_a").address
    helper_b = elf.symbol("helper_b").address
    helper_c = elf.symbol("helper_c").address
    helper_d = elf.symbol("helper_d").address
    assert _read_u64_le(elf, table_addr) == helper_a
    assert _read_u64_le(elf, table_addr + 8) == helper_b
    assert _read_u64_le(elf, table_addr + 16) == helper_c
    assert _read_u64_le(elf, table_addr + 24) == helper_d


def test_apply_relocations_idempotent():
    elf = strider.lift.load_elf(str(X64_RELOCS()), apply_relocations=True)
    helper_a = elf.symbol("helper_a").address
    table_addr = elf.symbol("dispatch_table").address
    first = _read_u64_le(elf, table_addr)
    # Merging duplicates the regions and the newer copy wins the lookup,
    # so both copies have to be patched identically.
    elf.add_elf(str(X64_RELOCS()), apply_relocations=True)
    second = _read_u64_le(elf, table_addr)
    assert first == second == helper_a


def test_apply_relocations_default_argument_is_true():
    """`load_elf` relocates unless the caller opts out; flipping that
    default would silently change every existing caller's results."""
    import inspect
    sig = inspect.signature(strider.lift.load_elf)
    p = sig.parameters.get("apply_relocations")
    assert p is not None, "load_elf must accept apply_relocations"
    assert p.default is True, "load_elf default must be apply_relocations=True"
    elf = strider.lift.load_elf(str(X64_RELOCS()))
    table_addr = elf.symbol("dispatch_table").address
    assert _read_u64_le(elf, table_addr) == elf.symbol("helper_a").address
