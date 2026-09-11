from __future__ import annotations

import pathlib

import pytest

import strider

# python/ -> tests/ -> strider-py/ -> crates/ -> workspace root.
WORKSPACE_ROOT = pathlib.Path(__file__).resolve().parents[4]
FIXTURES_DIR = WORKSPACE_ROOT / "fixtures" / "out"


def _ensure_pyelftools():
    try:
        import elftools.elf.elffile  # noqa: F401
    except ImportError:
        pytest.skip("pyelftools not installed (pip install pyelftools)")


def fixture_path(arch: str, case: str, ext: str = ".elf") -> pathlib.Path:
    """fixtures/out/<arch>/<case><ext>; skips if the fixture is missing."""
    p = FIXTURES_DIR / arch / f"{case}{ext}"
    if not p.exists():
        pytest.skip(f"fixture missing: {p} (run `make` in fixtures/)")
    return p


def symbol_addr(elf_path: pathlib.Path, name: str) -> int:
    """Address of a function symbol; skips if the symbol or pyelftools is
    missing."""
    _ensure_pyelftools()
    import elftools.elf.elffile
    from elftools.elf.sections import SymbolTableSection

    with elf_path.open("rb") as f:
        ef = elftools.elf.elffile.ELFFile(f)
        symtab = ef.get_section_by_name(".symtab")
        if not isinstance(symtab, SymbolTableSection):
            pytest.skip(f"{elf_path}: no .symtab section")
        for s in symtab.iter_symbols():
            if s.name == name and s["st_value"]:
                return int(s["st_value"])
    pytest.skip(f"{elf_path}: symbol {name!r} not found")


# Keyed by the fixtures/out/<arch> directory name.
_ARCH_PRESETS = {
    "x86": (strider.sleigh.SleighArch.x86, strider.sleigh.CallingConvention.x86_cdecl),
    "x64": (strider.sleigh.SleighArch.x86_64, strider.sleigh.CallingConvention.x86_64_systemv),
}


def lift_bytes(code: bytes, base: int = 0x1000):
    """Lift raw `code` mapped at `base` as an x86-64 SysV function."""
    mem = strider.reader.BufferReader(base, code)
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)
    _cfg, function, _unresolved = lift.analyze(
        base, strider.sleigh.CallingConvention.x86_64_systemv()
    )
    return function


def built_lifter_and_function(
    arch_name: str, case: str, symbol: str, *, optimize: bool = True
):
    """Lift `fixtures/out/<arch_name>/<case>.elf::<symbol>` to a
    `(Lifter, Function)` pair.  Callers wanting the `Cfg` (for
    `fingerprint_pcode`) must call `lift.analyze(...)` themselves.

    `optimize=False` runs an empty optimizer pipeline, so the lifted IR
    reaches the caller uncanonicalised.
    """
    arch_ctor, cc_ctor = _ARCH_PRESETS[arch_name]
    arch, cc = arch_ctor(), cc_ctor()
    loaded = strider.lift.load_elf(str(fixture_path(arch_name, case)))
    mem = loaded.reader()
    addr = loaded.symbol(symbol).address
    lift = strider.lift.lifter(arch, mem, rom=mem)
    _cfg, function, _unresolved = lift.analyze(
        addr,
        cc,
        opts=strider.lift.LifterOptions(
            cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True),
            pipeline=None if optimize else strider.opt.OptimizerPipeline.empty(),
        ),
    )
    return lift, function


def built_function(arch_name: str, case: str, symbol: str, *, optimize: bool = True):
    """`built_lifter_and_function` without the `Lifter`, for callers that
    only need Sleigh-free reads (pattern queries, `node_count`, ...)."""
    _lift, function = built_lifter_and_function(
        arch_name, case, symbol, optimize=optimize
    )
    return function


@pytest.fixture
def x86_memory_elf() -> pathlib.Path:
    """fixtures/out/x86/memory.elf: `array_sum`, `pointer_chase`,
    `struct_field_load`, etc."""
    return fixture_path("x86", "memory")


@pytest.fixture
def x86_calls_elf() -> pathlib.Path:
    return fixture_path("x86", "calls")


@pytest.fixture
def x86_relocs_elf() -> pathlib.Path:
    """fixtures/out/x86/elf_relocs.elf: an ET_DYN shared object based at
    ~0x12b0 (`helper_a`..`helper_d`, `compute_via_helper`), so it does NOT
    overlap the 0x401000-based executable fixtures."""
    return fixture_path("x86", "elf_relocs")



@pytest.fixture
def x86_indirect_branch_elf() -> pathlib.Path:
    return fixture_path("x86", "indirect_branch")


@pytest.fixture
def x86_switch_elf() -> pathlib.Path:
    return fixture_path("x86", "switch")
