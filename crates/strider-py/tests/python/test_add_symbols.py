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
