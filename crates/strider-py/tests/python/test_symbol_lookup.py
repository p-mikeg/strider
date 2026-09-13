"""Which symbol a name or an address resolves to."""

from __future__ import annotations

import strider

from . import _elf_builder as eb
from .conftest import fixture_path


def test_a_thumb_function_is_measured_from_its_first_instruction():
    """A Thumb function's `st_value` carries the ISA bit, one past its first
    instruction. `address` keeps it for `analyze`; lookups start at the
    instruction, so the first one is the function's own and one past its end
    is not."""
    lift = strider.lift.load_elf(str(fixture_path("arm_thumb", "arithmetic")))
    fns = [s for s in lift.functions() if s.address & 1 and s.size]
    assert fns, "the Thumb fixture has sized Thumb functions"
    for sym in fns:
        code = sym.address & ~1
        assert sym.is_thumb
        assert sym.end == code + sym.size
        assert lift.symbol_at(code).name == sym.name
        after = lift.symbol_at(code + sym.size)
        assert after is None or after.name != sym.name


def test_an_arm_function_is_not_thumb():
    lift = strider.lift.load_elf(str(fixture_path("arm", "arithmetic")))
    add = lift.symbol("add")
    assert not add.is_thumb
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
    assert lift.symbol_at(3).name == "helper"


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
    assert lift.symbol_at(0x401040).name == "libfn", "the stub is still named"


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
    assert lift.symbol_at(0x1000 + count).name == "big"
    assert lift.symbol_at(0x1000 + 7).name == "l7"
    assert lift.symbol_at(0x1000 + count + 1) is None
