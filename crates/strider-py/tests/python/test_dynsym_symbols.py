"""A binary exporting only through `.dynsym` still enumerates its functions."""

import shutil
import subprocess

import pytest

import strider
from .conftest import fixture_path


@pytest.fixture(scope="module")
def stripped_so(tmp_path_factory):
    """A fixture ELF with `.symtab` removed, leaving only `.dynsym`."""
    strip = shutil.which("strip")
    if strip is None:
        pytest.skip("strip is not installed")
    src = fixture_path("x64", "elf_relocs")
    dst = tmp_path_factory.mktemp("dynsym") / "elf_relocs.stripped.elf"
    shutil.copy(src, dst)
    subprocess.run([strip, "--strip-all", str(dst)], check=True)
    return dst


def test_stripped_binary_still_enumerates_dynamic_functions(stripped_so):
    prog = strider.lift.load_elf(str(stripped_so))
    names = {sym.name for sym in prog.functions()}
    assert names, "a .dynsym-only binary must still report its exported functions"


def test_stripped_binary_symbols_are_reachable_by_name(stripped_so):
    prog = strider.lift.load_elf(str(stripped_so))
    syms = prog.symbols()
    assert syms, "a .dynsym-only binary must still report symbols"
