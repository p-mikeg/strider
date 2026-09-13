"""Which symbol a name or an address resolves to."""

from __future__ import annotations

import pytest

import strider

from . import _elf_builder as eb
from .conftest import fixture_path


def _name_at(lift, address: int):
    sym = lift.symbol_at(address)
    return None if sym is None else sym.name


def test_a_thumb_function_is_measured_from_its_first_instruction():
    """A Thumb function's `st_value` carries the ISA bit, one past its first
    instruction. `address` keeps it for `analyze`; lookups start at the
    instruction, so the first one is the function's own and one past its end
    is not."""
    lift = strider.lift.load_elf(str(fixture_path("arm_thumb", "arithmetic")))
    fns = [(s, s.size) for s in lift.functions() if s.address & 1 and s.size]
    assert fns, "the Thumb fixture has sized Thumb functions"
    for sym, size in fns:
        code = sym.address & ~1
        assert sym.is_thumb
        assert sym.end == code + size
        assert _name_at(lift, code) == sym.name
        assert _name_at(lift, code + size) != sym.name


def test_an_arm_function_is_not_thumb():
    lift = strider.lift.load_elf(str(fixture_path("arm", "arithmetic")))
    add = lift.symbol("add")
    assert not add.is_thumb and add.size is not None
    assert add.end == add.address + add.size


def test_a_symbol_defined_at_address_zero_is_kept(tmp_path):
    path = tmp_path / "zero.elf"
    eb.Elf(
        loads=[(eb.PF_R | eb.PF_X, 0, 0)],
        sections=[eb.Section(".text", 0, b"\xc3" * 16)],
        symbols=[eb.Sym("helper", 0, 0, size=8), eb.Sym("_start", 8, 0, size=8)],
    ).write(path)
    lift = strider.lift.load_elf(str(path))
    assert lift.symbol("helper").address == 0
    assert [s.name for s in lift.functions()] == ["helper", "_start"]
    assert _name_at(lift, 3) == "helper"


def test_a_definition_in_a_later_image_beats_an_import_carrying_a_plt_address(tmp_path):
    """A non-PIE executable's undefined `libfn` still carries its PLT stub's
    address; the library merged afterwards is where it is defined."""
    main, lib = tmp_path / "main.elf", tmp_path / "lib.so"
    eb.Elf(
        loads=[(eb.PF_R | eb.PF_X, 0x401000, 0)],
        sections=[eb.Section(".text", 0x401000, b"\xc3" * 0x80)],
        symbols=[eb.Sym("main", 0x401000, 0, size=8), eb.Sym("libfn", 0x401040, None)],
    ).write(main)
    eb.Elf(
        loads=[(eb.PF_R | eb.PF_X, 0x7001000, 0)],
        sections=[eb.Section(".text", 0x7001000, b"\xc3" * 0x200)],
        symbols=[eb.Sym("libfn", 0x7001100, 0, size=8)],
    ).write(lib)

    lift = strider.lift.load_elf(str(main))
    assert lift.symbol("libfn").address == 0x401040, "the import names its stub"
    lift.add_elf(str(lib))
    assert lift.symbol("libfn").address == 0x7001100
    assert _name_at(lift, 0x401040) == "libfn", "the stub is still named"


def test_an_object_files_undefined_symbol_is_named_at_an_unmapped_address(tmp_path):
    path = tmp_path / "h.o"
    eb.Elf(
        e_type=eb.ET_REL,
        sections=[eb.Section(".text", 0, b"\xe8\0\0\0\0\xc3")],
        symbols=[eb.Sym("h", 0, 0, size=6), eb.Sym("ext_fn", 0, None, kind=eb.STT_NOTYPE)],
    ).write(path)
    lift = strider.lift.load_elf(str(path))
    ext = lift.symbol("ext_fn")
    assert ext.region is None
    assert lift.read(ext.address, 1) is None
    assert ext.address != lift.symbol("h").address


def test_symbol_at_inside_a_sized_function_full_of_unsized_symbols(tmp_path):
    """Mapping symbols and labels without a size sit inside a sized function.
    Each covers only its own address, and every other address still resolves
    to the function."""
    count = 2000
    path = tmp_path / "labels.elf"
    eb.Elf(
        loads=[(eb.PF_R | eb.PF_X, 0x1000, 0)],
        sections=[eb.Section(".text", 0x1000, b"\x90" * (count + 1))],
        symbols=[eb.Sym("big", 0x1000, 0, size=count + 1)]
        + [eb.Sym(f"l{i}", 0x1000 + i, 0, kind=eb.STT_NOTYPE) for i in range(1, count)],
    ).write(path)
    lift = strider.lift.load_elf(str(path))
    assert _name_at(lift, 0x1000 + count) == "big"
    assert _name_at(lift, 0x1000 + 7) == "l7"
    assert lift.symbol_at(0x1000 + count + 1) is None


def _object_calling(path, r_type: int, offset: int = 1) -> None:
    """`f: call ext_fn; ret` with a relocation of `r_type` at `offset`."""
    eb.Elf(
        e_type=eb.ET_REL,
        sections=[eb.Section(".text", 0, b"\xe8\0\0\0\0\xc3")],
        symbols=[eb.Sym("f", 0, 0, size=6), eb.Sym("ext_fn", 0, None, kind=eb.STT_NOTYPE)],
        relocs=[(0, offset, 1, r_type, -4)],
    ).write(path)


def test_an_object_files_call_to_an_undefined_function_targets_its_symbol(tmp_path):
    from strider.pattern import call

    path = tmp_path / "f.o"
    _object_calling(path, 4)  # R_X86_64_PLT32
    lift = strider.lift.load_elf(str(path))
    result = lift.analyze("f")
    assert result.function.find_all(call().target(lift.symbol("ext_fn").address))


def test_an_uncomputed_relocation_in_code_fails_the_decode(tmp_path):
    """`R_X86_64_TPOFF32` is not computed, so its field serves no bytes: a
    decode across it fails, and one starting on it names the relocation."""
    inside, start = tmp_path / "inside.o", tmp_path / "start.o"
    _object_calling(inside, 23)
    _object_calling(start, 23, offset=0)
    with pytest.raises(strider.StriderError, match="not mapped"):
        strider.lift.load_elf(str(inside)).analyze("f")
    with pytest.raises(strider.StriderError, match="relocation of type 23"):
        strider.lift.load_elf(str(start)).analyze("f")
