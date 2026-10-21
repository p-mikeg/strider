"""An object file's GOT references reach the synthetic slot holding the
symbol's address, or are holes; never another address.

The objects are `fixtures/loader/out/*`, built by `make -C fixtures/loader`.
"""

from __future__ import annotations

import pytest
import strider

from .conftest import _ensure_pyelftools
from .test_loader_written_got_slots import _image

R_386_GOT32X = 43
R_386_GOTPC = 10


def _word(elf, addr: int) -> int:
    held = elf.read(addr, 4)
    assert held is not None and len(held) == 4, hex(addr)
    return int.from_bytes(held, "little")


def _text_relocations(name: str, elf) -> list[tuple[int, int, int]]:
    """`.rel.text`'s `(field address, type, implicit addend)` entries, so
    the tests do not hard-code where the compiler put each instruction."""
    _ensure_pyelftools()
    from elftools.elf.elffile import ELFFile
    from elftools.elf.relocation import RelocationSection

    with open(_image(name), "rb") as f:
        ef = ELFFile(f)
        text = ef.get_section_by_name(".text")
        rel = ef.get_section_by_name(".rel.text")
        assert text is not None and isinstance(rel, RelocationSection)
        data = text.data()
        base = elf.symbol("f").address - _symbol_value(ef, "f")
        return [
            (
                base + r["r_offset"],
                r["r_info_type"],
                int.from_bytes(data[r["r_offset"] : r["r_offset"] + 4], "little"),
            )
            for r in rel.iter_relocations()
        ]


def _symbol_value(ef, name: str) -> int:
    symtab = ef.get_section_by_name(".symtab")
    return next(int(s["st_value"]) for s in symtab.iter_symbols() if s.name == name)


def test_an_i386_base_less_got_call_reaches_the_import():
    """`-fno-pic -fno-plt`: `call *ext_fn@GOT` and the tail `jmp *ext_fn@GOT`
    address the slot by its absolute address, a ModRM disp32 with no base."""
    name = "x86_got_call_nopic.o"
    elf = strider.lift.load_elf(_image(name))
    ext_fn = elf.symbol("ext_fn").address
    fields = [p for p, t, _ in _text_relocations(name, elf) if t == R_386_GOT32X]
    assert len(fields) == 2, "the call and the tail jump"
    for field in fields:
        assert _word(elf, _word(elf, field)) == ext_fn, hex(field)
    result = elf.analyze("f")
    assert not result.unresolved
    assert f"iconst[{ext_fn:#x}]" in result.function.to_text()


def test_an_i386_got_call_through_a_base_register_reaches_the_import():
    """`-fPIC -fno-plt`: `call *ext_fn@GOT(%esi)` names the slot's offset from
    the GOT, which `add $_GLOBAL_OFFSET_TABLE_, %esi` puts in %esi."""
    name = "x86_got_call_pic.o"
    elf = strider.lift.load_elf(_image(name))
    relocs = _text_relocations(name, elf)
    # GOTPC is `GOT + A - P`.
    ((p, _, a),) = [(p, t, a) for p, t, a in relocs if t == R_386_GOTPC]
    got = (_word(elf, p) + p - a) & 0xFFFFFFFF
    fields = [p for p, t, _ in relocs if t == R_386_GOT32X]
    assert len(fields) == 2, "the call and the tail jump"
    for field in fields:
        slot = (got + _word(elf, field)) & 0xFFFFFFFF
        assert _word(elf, slot) == elf.symbol("ext_fn").address, hex(field)


@pytest.mark.parametrize(
    "fn, r_type",
    [("f", 309), ("g", 311)],
    ids=["ld_prel19", "adr_got_page"],
)
def test_an_aarch64_got_reference_with_an_addend_is_a_hole(fn, r_type):
    """`:got:foo+16` asks for the slot holding foo + 16, which strider does
    not make; `G + A` is bar's slot, so the load folded to bar."""
    elf = strider.lift.load_elf(_image("aarch64_got_addend.o"))
    with pytest.raises(strider.StriderError, match=f"relocation of type {r_type} "):
        elf.analyze(fn)


def test_an_aarch64_got_reference_without_an_addend_still_folds():
    elf = strider.lift.load_elf(_image("aarch64_got_addend.o"))
    text = elf.analyze("h").function.to_text()
    for sym in ("bar", "baz"):
        assert f"iconst[{elf.symbol(sym).address:#x}]" in text, sym
