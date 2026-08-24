"""The `LoadReadOnly` rom must be runtime-immutable.

`LoadReadOnly` folds a constant-address `Load(RAM)` without consulting
the load's memory-token chain, trusting that anything the rom resolves is
immutable.  A rom including writable sections (`.data`, `.got`,
`.data.rel.ro`) would fold a store-then-reload of a global to the
FILE-INITIAL value, silently discarding the store.

Pinned at the wiring level: the rom reader must NOT resolve a
writable-section address; the code-fetch `mem` reader still must.  The
`elf_relocs` fixture straddles the boundary exactly (`dispatch_table` in
`.data.rel.ro`, `helper_a` in `.text`).
"""

from __future__ import annotations

import strider

from .conftest import fixture_path


X64_RELOCS = lambda: fixture_path("x64", "elf_relocs")  # noqa: E731


def test_rom_reader_excludes_writable_sections():
    # `dispatch_table` (0x3ea0) is SHF_WRITE, hence forbidden from the
    # rom; `helper_a` (0x1030) is `.text`, hence allowed.
    es = strider.lift.load_elf(str(X64_RELOCS()))
    elf = es._elf
    table = es.symbol("dispatch_table").address
    helper = es.symbol("helper_a").address

    rom = elf.ro_reader()

    assert rom.read(table, 8) is None, (
        "rom must exclude writable .data.rel.ro; folding trusts it "
        "unconditionally"
    )
    assert rom.read(helper, 8) is not None, (
        "rom must still resolve immutable code/rodata addresses"
    )


def test_mem_reader_still_includes_writable_sections():
    # The code reader still includes writable sections (relocations are
    # applied there), so reading the relocated table by hand keeps working.
    es = strider.lift.load_elf(str(X64_RELOCS()))
    elf = es._elf
    table = es.symbol("dispatch_table").address
    assert elf.read(table, 8) is not None, (
        "mem (code reader) must still resolve writable sections"
    )
