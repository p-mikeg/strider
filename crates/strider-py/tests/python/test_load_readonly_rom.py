"""The `LoadReadOnly` rom must be runtime-immutable.

`LoadReadOnly` folds a constant-address `Load(RAM)` to the byte value the
rom resolves WITHOUT consulting the load's memory-token chain — it trusts
that everything the rom can resolve is runtime-immutable.  If the rom
included writable sections (`.data`, `.got`, `.data.rel.ro`), a function
that stores to a global and later reloads it would fold to the
FILE-INITIAL value, discarding the store: a wrong analysis result.

These tests pin the contract at the wiring level: the immutable rom
reader the ELF analysis path uses must NOT resolve a writable-section
address, while the code/instruction-fetch `mem` reader still must.  The
`elf_relocs` fixture's `dispatch_table` lives in `.data.rel.ro`
(SHF_WRITE) and `helper_a` lives in `.text` — so they straddle the
immutable boundary exactly.
"""

from __future__ import annotations

import strider

from .conftest import fixture_path


X64_RELOCS = lambda: fixture_path("x64", "elf_relocs")  # noqa: E731


def test_rom_reader_excludes_writable_sections():
    # `dispatch_table` (0x3ea0) is in `.data.rel.ro` — SHF_WRITE, hence
    # runtime-mutable and forbidden from the rom.  `helper_a` (0x1030)
    # is in `.text` — immutable, allowed.
    es = strider.load_elf(str(X64_RELOCS()))
    elf = es._elf
    table = es.symbol("dispatch_table")
    helper = es.symbol("helper_a")

    rom = elf.ro_reader()

    # rom: the writable global must NOT resolve (so LoadReadOnly can't
    # fold a load from it to the file-initial value).
    assert rom.read(table, 8) is None, (
        "rom must exclude writable .data.rel.ro — folding trusts it "
        "unconditionally"
    )
    # rom: an immutable code/rodata address still resolves.
    assert rom.read(helper, 8) is not None, (
        "rom must still resolve immutable code/rodata addresses"
    )


def test_mem_reader_still_includes_writable_sections():
    # The instruction-fetch / raw-read `mem` reader is unchanged: it
    # still includes writable sections (relocations are applied there)
    # so reading the (relocated) dispatch table by hand keeps working.
    es = strider.load_elf(str(X64_RELOCS()))
    elf = es._elf
    table = es.symbol("dispatch_table")
    assert elf.read(table, 8) is not None, (
        "mem (code reader) must still resolve writable sections"
    )
