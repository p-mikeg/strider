"""`ElfLifter` follows a GCC hot/cold split into `<name>.cold`.

GCC -O2 moves a function's unlikely blocks into a `<name>.cold` symbol that
the body branches to and that jumps back into the body. `ElfLifter.analyze`
and `build_cfg` add each such symbol to the function's extent, so its code
is decoded rather than modelled as a tail call.
"""

from __future__ import annotations

import pytest

import strider
from strider.cfg import CfgOptions

from . import _elf_builder as eb
from .conftest import WORKSPACE_ROOT

LOADER_OUT = WORKSPACE_ROOT / "fixtures" / "loader" / "out"

#: 0x0ff0: add eax, 1 ; jmp 0x1005 ; nop x11
#: 0x1000: test edi, edi ; js 0x0ff0 ; nop ; ret
_CODE = bytes([0x83, 0xC0, 0x01, 0xEB, 0x10]) + b"\x90" * 11 + bytes(
    [0x85, 0xFF, 0x78, 0xEC, 0x90, 0xC3]
)
_COLD, _COLD_JMP, _FOO = 0x0FF0, 0x0FF3, 0x1000


def _split(tmp_path, fragment_name: str):
    path = tmp_path / "split.elf"
    eb.Elf(
        loads=[(eb.PF_R | eb.PF_X, _COLD, 0)],
        sections=[eb.Section(".text", _COLD, _CODE)],
        symbols=[
            eb.Sym(fragment_name, _COLD, 0, size=5),
            eb.Sym("foo", _FOO, 0, size=6),
        ],
    ).write(path)
    return strider.lift.load_elf(str(path))


@pytest.mark.parametrize("name", ["foo.cold", "foo.cold.3"])
def test_a_cold_fragment_is_part_of_its_function(tmp_path, name):
    lift = _split(tmp_path, name)
    result = lift.analyze("foo")
    assert result.cfg.pcode_at(_COLD_JMP) is not None
    assert result.cfg.incomplete == []
    assert lift.build_cfg(_FOO).pcode_at(_COLD_JMP) is not None


@pytest.mark.parametrize(
    "name",
    ["foo.part.0", "foo.constprop.0", "foo.isra.0", "foo.part.0.cold", "bar.cold", "foo.coldx"],
)
def test_a_clone_or_another_functions_fragment_stays_a_tail_call(tmp_path, name):
    lift = _split(tmp_path, name)
    assert lift.analyze("foo").cfg.pcode_at(_COLD_JMP) is None
    assert lift.build_cfg(_FOO).pcode_at(_COLD_JMP) is None


def _per_file(tmp_path, cold_file: str, foo_bind: int):
    """`foo` at `_FOO` in `a.c` (a static, or a global after the locals), a
    static `foo` in `b.c`, and `foo.cold` at `_COLD` local to `cold_file`."""
    local = eb.STB_LOCAL
    files = {
        name: [eb.Sym(name, 0, None, kind=eb.STT_FILE, bind=local)] for name in ("a.c", "b.c")
    }
    files["b.c"].append(eb.Sym("foo", _COLD + 5, 0, size=1, bind=local))
    files[cold_file].append(eb.Sym("foo.cold", _COLD, 0, size=5, bind=local))
    foo = eb.Sym("foo", _FOO, 0, size=6, bind=foo_bind)
    symbols = files["a.c"] + ([foo] if foo_bind == local else []) + files["b.c"]
    path = tmp_path / "per_file.elf"
    eb.Elf(
        loads=[(eb.PF_R | eb.PF_X, _COLD, 0)],
        sections=[eb.Section(".text", _COLD, _CODE)],
        symbols=symbols + ([] if foo_bind == local else [foo]),
    ).write(path)
    return strider.lift.load_elf(str(path))


@pytest.mark.parametrize(
    ("cold_file", "foo_bind", "followed"),
    [
        ("a.c", eb.STB_LOCAL, True),
        ("b.c", eb.STB_LOCAL, False),
        ("a.c", eb.STB_GLOBAL, True),
        ("b.c", eb.STB_GLOBAL, False),
    ],
)
def test_a_cold_fragment_belongs_to_the_function_of_its_own_file(
    tmp_path, cold_file, foo_bind, followed
):
    """GCC emits `foo.cold` local to the object of its `foo`, and the linker
    gathers every object's `.text.unlikely` apart from the bodies, so the
    file a static belongs to, not the address, tells two `foo`s apart. A
    global `foo` owns a fragment whose file defines no `foo` of its own."""
    lift = _per_file(tmp_path, cold_file, foo_bind)
    assert (lift.build_cfg(_FOO).pcode_at(_COLD_JMP) is not None) == followed


@pytest.mark.parametrize("name", ["foo", "foo.lto_priv.0"])
def test_a_function_the_linker_made_local_keeps_its_cold_fragment(tmp_path, name):
    """GNU ld makes a hidden function, or one LTO promoted from a static,
    local in its own trailing group behind an empty-name `FILE`, and leaves
    `<f>.cold` in the object's group (gcc -O2 / -flto, x86-64 PIE and .so)."""
    local = eb.STB_LOCAL
    path = tmp_path / "linker_local.elf"
    eb.Elf(
        loads=[(eb.PF_R | eb.PF_X, _COLD, 0)],
        sections=[eb.Section(".text", _COLD, _CODE)],
        symbols=[
            eb.Sym("a.c", 0, None, kind=eb.STT_FILE, bind=local),
            eb.Sym(f"{name}.cold", _COLD, 0, size=5, bind=local),
            eb.Sym("", 0, None, kind=eb.STT_FILE, bind=local),
            eb.Sym(name, _FOO, 0, size=6, bind=local),
        ],
    ).write(path)
    lift = strider.lift.load_elf(str(path))
    assert lift.build_cfg(_FOO).pcode_at(_COLD_JMP) is not None


def test_a_cold_symbol_added_after_an_analysis_is_followed(tmp_path):
    lift = _split(tmp_path, "bar")
    assert lift.build_cfg(_FOO).pcode_at(_COLD_JMP) is None
    lift.add_symbols({"foo.cold": (_COLD, 5)})
    assert lift.build_cfg(_FOO).pcode_at(_COLD_JMP) is not None


def test_explicit_fragment_ranges_win_over_the_elfs(tmp_path):
    lift = _split(tmp_path, "foo.cold")
    elsewhere = CfgOptions(fragment_ranges=[(0x9000, 0x9010)])
    assert lift.build_cfg(_FOO, elsewhere).pcode_at(_COLD_JMP) is None
    opts = strider.lift.LifterOptions(cfg=elsewhere)
    assert lift.analyze("foo", opts=opts).cfg.pcode_at(_COLD_JMP) is None


def test_a_plain_lifter_takes_fragment_ranges_from_its_options():
    mem = strider.reader.BufferReader(_COLD, _CODE)
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)
    bound = CfgOptions(function_max_size=6)
    assert lift.build_cfg(_FOO, bound).pcode_at(_COLD_JMP) is None
    opts = CfgOptions(function_max_size=6, fragment_ranges=[(_COLD, _COLD + 5)])
    assert opts.fragment_ranges == [(_COLD, _COLD + 5)]
    assert lift.build_cfg(_FOO, opts).pcode_at(_COLD_JMP) is not None


def test_an_empty_fragment_range_raises():
    with pytest.raises(ValueError, match="fragment_ranges"):
        CfgOptions(fragment_ranges=[(0x10, 0x10)])


@pytest.mark.parametrize(("arch", "jmp_len"), [("x64", 5), ("aarch64", 4)])
def test_gcc_split_is_followed_into_the_cold_partition(arch, jmp_len):
    """`fixtures/loader/src/cold.c`: `foo.cold` calls `log_err`, adds to `g`
    and jumps back into the loop."""
    lift = strider.lift.load_elf(str(LOADER_OUT / f"{arch}_cold.elf"))
    cold = lift.symbol("foo.cold")
    assert cold.size is not None
    last = cold.address + cold.size - jmp_len  # the `jmp` / `b` back
    result = lift.analyze("foo")
    assert result.cfg.pcode_at(cold.address) is not None
    assert result.cfg.pcode_at(last) is not None
    assert result.cfg.incomplete == []
    assert result.function.validate() is None
