"""An object file's GOT references reach the synthetic slot holding the
symbol's address, or are holes; never another address.

The objects are `fixtures/loader/out/*`, built by `make -C fixtures/loader`.
"""

from __future__ import annotations

import pytest
import strider

from .test_loader_written_got_slots import _image


def _word(elf, addr: int) -> int:
    held = elf.read(addr, 4)
    assert held is not None and len(held) == 4, hex(addr)
    return int.from_bytes(held, "little")


def test_an_i386_base_less_got_call_reaches_the_import():
    """`-fno-pic -fno-plt`: `call *ext_fn@GOT` and the tail `jmp *ext_fn@GOT`
    address the slot by its absolute address, a ModRM disp32 with no base."""
    elf = strider.lift.load_elf(_image("x86_got_call_nopic.o"))
    f = elf.symbol("f").address
    ext_fn = elf.symbol("ext_fn").address
    for field in (0x12, 0x20):
        assert _word(elf, _word(elf, f + field)) == ext_fn, hex(field)
    result = elf.analyze("f")
    assert not result.unresolved
    assert f"iconst[{ext_fn:#x}]" in result.function.to_text()


def test_an_i386_got_call_through_a_base_register_reaches_the_import():
    """`-fPIC -fno-plt`: `call *ext_fn@GOT(%esi)` names the slot's offset from
    the GOT, which `add $_GLOBAL_OFFSET_TABLE_, %esi` puts in %esi."""
    elf = strider.lift.load_elf(_image("x86_got_call_pic.o"))
    f = elf.symbol("f").address
    got = (f + 0xA + _word(elf, f + 0xC)) & 0xFFFFFFFF
    for field in (0x1E, 0x2F):
        slot = (got + _word(elf, f + field)) & 0xFFFFFFFF
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
