"""Symbol sources attached after `load_elf`: a debug companion file, and
symbols supplied directly."""

import shutil
import subprocess

import pytest

import strider

from .conftest import fixture_path


def _strip_and_split(tmp_path):
    """A stripped image plus its `--only-keep-debug` companion, the shape a
    distro ships. Skips where binutils is not available."""
    if shutil.which("objcopy") is None:
        pytest.skip("objcopy not available")
    src = fixture_path("x64", "switch")
    full, dbg, stripped = (tmp_path / n for n in ("full.elf", "dbg.elf", "stripped.elf"))
    shutil.copy(src, full)
    for args in (["--only-keep-debug", str(full), str(dbg)],
                 ["--strip-all", str(full), str(stripped)]):
        if subprocess.run(["objcopy", *args], capture_output=True).returncode != 0:
            pytest.skip("objcopy could not split this fixture")
    return full, dbg, stripped


def test_a_debug_companion_restores_the_stripped_names(tmp_path):
    """The companion is linked at the SAME addresses as the image, so
    `add_elf` would refuse it as an overlap; `add_symbol_file` takes only its
    symbols and recovers exactly what stripping removed."""
    full, dbg, stripped = _strip_and_split(tmp_path)
    want = strider.lift.load_elf(str(full)).symbols()
    lift = strider.lift.load_elf(str(stripped))
    assert lift.symbols() == {} or len(lift.symbols()) < len(want)
    lift.add_symbol_file(str(dbg))
    got = lift.symbols()
    assert {n: s.address for n, s in got.items()} == {n: s.address for n, s in want.items()}


def test_add_symbol_file_leaves_the_mapped_bytes_alone(tmp_path):
    """It is a symbol source, not a second image: lifting is unchanged."""
    full, dbg, stripped = _strip_and_split(tmp_path)
    lift = strider.lift.load_elf(str(stripped))
    entry = lift.entry_point()
    before = lift.build_cfg(entry).to_dot()
    lift.add_symbol_file(str(dbg))
    assert lift.build_cfg(entry).to_dot() == before


def test_a_symbol_file_changed_after_its_table_is_built_is_not_reread(tmp_path):
    """`analyze` reads the image's bytes, re-stat on every read, and a symbol
    file only through the tables built from it. Once those are built, a
    change to that file is nothing `analyze` reads, so it does not raise."""
    full, dbg, stripped = _strip_and_split(tmp_path)
    lift = strider.lift.load_elf(str(stripped))
    lift.add_symbol_file(str(dbg))
    name = "dispatch_value"
    lift.analyze(name)
    with dbg.open("ab") as f:
        f.write(b"\0" * 64)
    assert lift.analyze(name).function.node_count() > 0
    assert lift.analyze(lift.symbol(name).address).function.node_count() > 0
    with stripped.open("ab") as f:
        f.write(b"\0" * 64)
    with pytest.raises(strider.StriderError, match="changed on disk"):
        lift.analyze(name)


def test_add_symbols_keeps_the_data_ranges_it_cannot_change(tmp_path):
    """`$d` spans come from the ELFs and symbol files, never from symbols
    supplied directly, so `add_symbols` leaves the built ones in place. A
    rebuild re-stats every symbol file, so one changed since is how a rebuild
    shows. Naming `fragment_ranges` keeps the build off the symbol table."""
    companion = tmp_path / "companion.elf"
    shutil.copy(fixture_path("arm", "calls"), companion)
    lift = strider.lift.load_elf(str(fixture_path("arm", "calls")))
    lift.add_symbol_file(str(companion))
    start = lift.symbol("_start").address
    opts = strider.cfg.CfgOptions(fragment_ranges=[(0, 1)])
    first = lift.build_cfg(start, opts).incomplete
    with companion.open("ab") as f:
        f.write(b"\0" * 64)
    lift.add_symbols({"made_up": 0x1234})
    assert lift.build_cfg(start, opts).incomplete == first


def test_add_symbols_takes_addresses_and_extents():
    lift = strider.lift.load_elf(str(fixture_path("x64", "switch")))
    lift.add_symbols({"bare": 0x40_0000, "sized": (0x50_0000, 0x40)})
    assert lift.symbol("bare").address == 0x40_0000
    assert lift.symbol("bare").size is None
    assert lift.symbol("sized").size == 0x40
    # an extent is what lets an interior address resolve
    inside = lift.symbol_at(0x50_0020)
    assert inside is not None and inside.name == "sized"
    # a bare address covers only itself, so its interior resolves to nothing
    past = lift.symbol_at(0x40_0020)
    assert past is None or past.name != "bare"


def test_an_elf_keeps_a_name_a_later_source_reuses():
    lift = strider.lift.load_elf(str(fixture_path("x64", "switch")))
    real = lift.symbol("f").address
    lift.add_symbols({"f": real + 0x1000})
    assert lift.symbol("f").address == real


def test_aliases_at_one_address_rank_by_extent_before_by_being_code():
    """`symbol_at` ranks `(has a size, is code)` in that order, so a sized DATA
    alias outranks an unsized CODE one; `functions()` filters to code instead,
    which is how the two accessors name different symbols at one address."""
    lift = strider.lift.load_elf(str(fixture_path("x64", "switch")))
    at = 0x60_0000
    lift.add_symbols({"code_alias": at}, is_function=True)
    lift.add_symbols({"data_alias": (at, 0x20)}, is_function=False)

    hit = lift.symbol_at(at)
    assert hit is not None and hit.name == "data_alias"
    assert [s.name for s in lift.functions() if s.address == at] == ["code_alias"]


def test_the_merge_methods_take_a_path_object(tmp_path, x86_memory_elf, x86_relocs_elf):
    """`load_elf` accepts any `os.PathLike`, so the two methods that attach a
    second file do too."""
    import pathlib

    lift = strider.lift.load_elf(pathlib.Path(x86_memory_elf))
    lift.add_elf(pathlib.Path(x86_relocs_elf))
    assert "helper_a" in lift.symbols()

    _full, dbg, stripped = _strip_and_split(tmp_path)
    stripped_lift = strider.lift.load_elf(pathlib.Path(stripped))
    stripped_lift.add_symbol_file(pathlib.Path(dbg))
    assert stripped_lift.symbols()


def test_a_rom_mapped_from_another_file_is_checked_on_its_own(tmp_path):
    """`analyze` stats each mapping behind `mem` and `rom` once, so a `rom`
    cut from a different file than `mem` still raises when only it changed."""
    code, data = tmp_path / "code.elf", tmp_path / "data.elf"
    shutil.copy(fixture_path("x64", "switch"), code)
    shutil.copy(fixture_path("x64", "switch"), data)
    mem = strider.lift.load_elf(str(code))
    rom = strider.lift.load_elf(str(data)).reader()
    lift = strider.lift.lifter(mem.arch, mem.reader(), rom=rom)
    cc = strider.sleigh.CallingConvention.x86_64_systemv()
    entry = mem.symbol("dispatch_value").address
    lift.analyze(entry, cc)
    with data.open("ab") as f:
        f.write(b"\0" * 64)
    with pytest.raises(strider.StriderError, match="data.elf changed on disk"):
        lift.analyze(entry, cc)
