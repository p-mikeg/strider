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

import pytest
import strider
import strider._strider as ext

from . import _elf_builder as eb
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


def _image_with_data(path, base: int, past_eof: bool) -> None:
    """A `.text` page and a writable `.data` page. `past_eof` points the
    `.data` PT_LOAD at file bytes from the end of the file on, so reading
    that mapping fails."""
    eb.Elf(
        loads=[(eb.PF_R | eb.PF_X, base, 0), (eb.PF_R | eb.PF_W, base + 0x1000, 1)],
        sections=[
            eb.Section(".text", base, b"\xc3" * 8),
            eb.Section(".data", base + 0x1000, b"\x11" * 8, eb.SHF_ALLOC | eb.SHF_WRITE),
        ],
    ).write(path)
    if past_eof:
        eb.point_load_past_eof(path, 1)


def _image_with_rodata(path, base: int, rodata: int) -> None:
    """A `.text` page at `base` and a read-only `.rodata` at `rodata`."""
    eb.Elf(
        loads=[(eb.PF_R | eb.PF_X, base, 0), (eb.PF_R, rodata, 1)],
        sections=[
            eb.Section(".text", base, b"\xc3" * 8),
            eb.Section(".rodata", rodata, b"\x22" * 8, eb.SHF_ALLOC),
        ],
    ).write(path)


def test_an_unrelocated_load_never_reads_the_writable_mappings_it_drops(tmp_path):
    """Unrelocated, writable non-executable mappings are not loaded, so bytes
    they cannot supply fail nothing; their addresses still bar the ROM."""
    path, rodata = tmp_path / "data_past_eof.elf", tmp_path / "rodata.elf"
    _image_with_data(path, 0x400000, past_eof=True)
    _image_with_rodata(rodata, 0x600000, 0x401000)

    elf = ext._load_elf_from_segments(str(path), False, False)
    assert elf.read(0x400000, 1) == b"\xc3"
    assert elf.read(0x401000, 1) is None
    elf.add_elf(str(rodata), False, False)
    assert elf.read(0x401000, 1) == b"\x22"
    assert elf.ro_reader().read(0x401000, 1) is None, (
        "the dropped writable mapping still bars its addresses from the ROM"
    )

    with pytest.raises(strider.StriderError, match="failed to parse ELF"):
        ext._load_elf_from_segments(str(path), True, False)


def test_an_unrelocated_merge_never_reads_the_writable_mappings_it_drops(tmp_path):
    first, second = tmp_path / "first.elf", tmp_path / "second.elf"
    rodata = tmp_path / "rodata.elf"
    _image_with_data(first, 0x400000, past_eof=False)
    _image_with_data(second, 0x500000, past_eof=True)
    _image_with_rodata(rodata, 0x600000, 0x501000)
    elf = ext._load_elf_from_segments(str(first), False, False)

    elf.add_elf(str(second), False, False)
    assert elf.read(0x500000, 1) == b"\xc3"
    elf.add_elf(str(rodata), False, False)
    assert elf.read(0x501000, 1) == b"\x22"
    assert elf.ro_reader().read(0x501000, 1) is None, (
        "the dropped writable mapping still bars its addresses from the ROM"
    )

    with pytest.raises(strider.StriderError, match="failed to parse ELF"):
        elf.add_elf(str(second), True, False)
