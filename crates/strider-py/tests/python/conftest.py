"""Shared pytest fixtures for strider-py integration tests.

The test ELFs live under `fixtures/out/<arch>/<case>.elf` after
`make` runs in `fixtures/`.  Tests skip cleanly when fixtures are
absent so a fresh checkout doesn't fail.
"""

from __future__ import annotations

import pathlib

import pytest

# crates/strider-py/tests/python/conftest.py → workspace root is parents[4]:
#   /python/.. = /tests
#   /tests/..  = /strider-py
#   /strider-py/.. = /crates
#   /crates/..     = workspace root
WORKSPACE_ROOT = pathlib.Path(__file__).resolve().parents[4]
FIXTURES_DIR = WORKSPACE_ROOT / "fixtures" / "out"


def _ensure_pyelftools():
    try:
        import elftools.elf.elffile  # noqa: F401
    except ImportError:
        pytest.skip("pyelftools not installed (pip install pyelftools)")


def fixture_path(arch: str, case: str) -> pathlib.Path:
    """Returns the path to fixtures/out/<arch>/<case>.elf, skipping
    cleanly if the fixture is missing.
    """
    p = FIXTURES_DIR / arch / f"{case}.elf"
    if not p.exists():
        pytest.skip(f"fixture missing: {p} (run `make` in fixtures/)")
    return p


def symbol_addr(elf_path: pathlib.Path, name: str) -> int:
    """Resolves the address of a function symbol in an ELF, skipping
    cleanly if the symbol or pyelftools is missing.
    """
    _ensure_pyelftools()
    import elftools.elf.elffile

    with elf_path.open("rb") as f:
        ef = elftools.elf.elffile.ELFFile(f)
        symtab = ef.get_section_by_name(".symtab")
        if symtab is None:
            pytest.skip(f"{elf_path}: no .symtab section")
        for s in symtab.iter_symbols():
            if s.name == name and s["st_value"]:
                return int(s["st_value"])
    pytest.skip(f"{elf_path}: symbol {name!r} not found")


@pytest.fixture
def x86_memory_elf() -> pathlib.Path:
    """Path to fixtures/out/x86/memory.elf — a small binary with
    several exercise functions (`array_sum`, `pointer_chase`,
    `struct_field_load`, etc.).  This replaces the plan's reference
    to a non-existent `test.elf` / `struct_test` symbol.
    """
    return fixture_path("x86", "memory")


@pytest.fixture
def x86_calls_elf() -> pathlib.Path:
    return fixture_path("x86", "calls")


@pytest.fixture
def x86_patterns_elf() -> pathlib.Path:
    return fixture_path("x86", "patterns")


@pytest.fixture
def x86_indirect_branch_elf() -> pathlib.Path:
    return fixture_path("x86", "indirect_branch")


@pytest.fixture
def x86_switch_elf() -> pathlib.Path:
    return fixture_path("x86", "switch")
