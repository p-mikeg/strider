"""`load_elf(relocated_got_is_rom=True)`: the opt-in that serves a relocated
`.got` / `.data.rel.ro` as read-only memory.

It is OFF by default because "the loader will not rewrite these slots again"
is a claim about the runtime that nothing in the image proves.  What it buys
is the table base: a PIC image loads one through the GOT, so with the slots
unmapped the load stays opaque and every address derived from it is unknown.
"""

from __future__ import annotations

import strider
import strider._strider as ext

from . import _elf_builder as eb
from .conftest import fixture_path


def _elf_relocs():
    return str(fixture_path("x64", "elf_relocs"))


def test_the_default_rom_still_refuses_data_rel_ro():
    es = strider.lift.load_elf(_elf_relocs())
    table = es.symbol("dispatch_table").address
    assert es._elf.ro_reader().read(table, 8) is None


def test_the_opt_in_serves_data_rel_ro_at_its_relocated_value():
    es = strider.lift.load_elf(_elf_relocs(), relocated_got_is_rom=True)
    table = es.symbol("dispatch_table").address
    helper = es.symbol("helper_a").address

    slot = es._elf.ro_reader().read(table, 8)
    assert slot is not None, "the opt-in must serve the table"
    assert int.from_bytes(slot, "little") == helper, (
        "slot 0 must read the RELOCATED pointer to helper_a"
    )


def test_the_opt_in_folds_the_got_load_of_a_table_base():
    """`dispatch_via_table` loads `&dispatch_table` out of the GOT, then
    indexes it.  Only with the GOT served does that load fold, which is what
    turns the access into `table_base + 8 * index`."""
    table = strider.lift.load_elf(_elf_relocs()).symbol("dispatch_table").address

    opaque = strider.lift.load_elf(_elf_relocs()).analyze("dispatch_via_table")
    assert f"iconst[{table:#x}]" not in opaque.function.to_text(), (
        "the default view cannot know the table base"
    )

    folded = strider.lift.load_elf(
        _elf_relocs(), relocated_got_is_rom=True
    ).analyze("dispatch_via_table")
    assert f"iconst[{table:#x}]" in folded.function.to_text(), (
        "the opt-in must fold the GOT load to the table's address"
    )


def test_the_opt_in_resolves_a_dispatch_through_a_data_rel_ro_table():
    """The ppc32le computed goto reads its label table out of an eight-byte
    `.data.rel.ro`, so the site resolves only once that is served."""
    path = str(fixture_path("ppc32le", "indirect_branch"))

    unresolved = strider.lift.load_elf(path).analyze("indirect_branch_resolved")
    assert unresolved.unresolved, "without the GOT the table base is unmapped"

    resolved = strider.lift.load_elf(
        path, relocated_got_is_rom=True
    ).analyze("indirect_branch_resolved")
    assert not resolved.unresolved, (
        f"the opt-in must resolve the dispatch, left {resolved.unresolved}"
    )


def _image_with_a_got(path, base: int, slot: bytes) -> None:
    """A linked image with a `.text` page and a writable page holding `.got`
    and `.data`, at `base`."""
    eb.Elf(
        loads=[(eb.PF_R | eb.PF_X, base, 0), (eb.PF_R | eb.PF_W, base + 0x1000, 1)],
        sections=[
            eb.Section(".text", base, b"\xc3" * 8),
            eb.Section(
                ".got", base + 0x1000, slot, eb.SHF_ALLOC | eb.SHF_WRITE
            ),
        ],
    ).write(path)


def test_add_elf_makes_the_claim_per_image(tmp_path):
    """The claim is about one image's link, so a merged ELF makes its own: a
    library added without the flag keeps its GOT out of the read-only view,
    whatever the main image claimed."""
    first, second = tmp_path / "first.elf", tmp_path / "second.elf"
    _image_with_a_got(first, 0x400000, b"\x11" * 8)
    _image_with_a_got(second, 0x500000, b"\x22" * 8)

    elf = ext._load_elf_from_segments(str(first), False, True)
    assert elf.ro_reader().read(0x401000, 8) == b"\x11" * 8

    elf.add_elf(str(second), False, False)
    assert elf.ro_reader().read(0x401000, 8) == b"\x11" * 8, (
        "a merge must not drop what the first image's claim serves"
    )
    assert elf.ro_reader().read(0x501000, 8) is None, (
        "the merged image made no claim, so its GOT stays out"
    )

    third = tmp_path / "third.elf"
    _image_with_a_got(third, 0x600000, b"\x33" * 8)
    elf.add_elf(str(third), False, True)
    assert elf.ro_reader().read(0x601000, 8) == b"\x33" * 8, (
        "a merge that does make the claim serves its own GOT"
    )
