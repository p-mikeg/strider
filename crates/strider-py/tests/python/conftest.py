"""Shared pytest fixtures for strider-py integration tests.

The test ELFs live under `fixtures/out/<arch>/<case>.elf` after
`make` runs in `fixtures/`.  Tests skip cleanly when fixtures are
absent so a fresh checkout doesn't fail.
"""

from __future__ import annotations

import pathlib

import pytest

import strider

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


# Arch presets for `built_function`, keyed by the fixtures/out/<arch>
# directory name.
_ARCH_PRESETS = {
    "x86": (strider.SleighArch.x86, strider.CallingConvention.x86_cdecl),
    "x64": (strider.SleighArch.x86_64, strider.CallingConvention.x86_64_systemv),
}


def built_lifter_and_function(
    arch_name: str, case: str, symbol: str, *, optimize: bool = True
):
    """Lift `fixtures/out/<arch_name>/<case>.elf::<symbol>` and return the
    `(Lifter, Function)` pair — the `Lifter` is needed by callers that
    want the Sleigh-needing pretty renders (`dump_html` / `dump_dot` /
    `html_str`), which live on it rather than on the bare `Function`.
    (The p-code audit trail, `fingerprint_pcode`, lives on the `Cfg`
    `analyze` returns instead — discarded here as `_cfg`; callers that
    need it should call `lift.analyze(...)` directly.)

    The single `Lifter.analyze` handle always drives the full
    lift+optimise+resolve pipeline (there is no lower-level "lift only,
    skip the optimizer" entry point any more — `Lifter.build_cfg` stops
    at the structural CFG, one level below IR).  `optimize` is kept as a
    no-op parameter for call-site compatibility with tests that predate
    the single-`Lifter` collapse; both branches behave identically.

    Skips cleanly (via `fixture_path`) when the fixture ELF is missing.
    """
    del optimize
    arch_ctor, cc_ctor = _ARCH_PRESETS[arch_name]
    arch, cc = arch_ctor(), cc_ctor()
    loaded = strider.load_elf(str(fixture_path(arch_name, case)))
    mem = loaded.reader()
    addr = loaded.symbol(symbol)
    lift = strider.lifter(arch, mem, rom=mem)
    _cfg, function, _unresolved = lift.analyze(
        addr,
        cc,
        opts=strider.LifterOptions(
            cfg=strider.CfgOptions(allow_code_before_start_addr=True)
        ),
    )
    return lift, function


def built_function(arch_name: str, case: str, symbol: str, *, optimize: bool = True):
    """Like `built_lifter_and_function`, but returns just the `Function`
    for callers that only need Sleigh-free reads (pattern queries,
    `node_count`, `to_dot`, ...)."""
    _lift, function = built_lifter_and_function(
        arch_name, case, symbol, optimize=optimize
    )
    return function


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
