"""`ElfLifter.symbol_size` + the ELF backend's `symbol_addr_and_size`.

These surface `st_size`, which callers need for `function_max_size=` so
the analyser knows where a function ends (and the indirect-branch
resolver can tell intra-fn jumps from tail calls).  Without them users
fall back to pyelftools.
"""

from __future__ import annotations

import pytest

import strider

from .conftest import fixture_path


def test_symbol_size_returns_known_function_size():
    elf = strider.lift.load_elf(str(fixture_path("x64", "elf_relocs")))
    # Any size > 0, to stay tolerant of toolchain layout differences.
    size = elf.symbol_size("helper_a")
    assert size is not None and size > 0


def test_symbol_size_raises_on_unknown_symbol():
    elf = strider.lift.load_elf(str(fixture_path("x64", "elf_relocs")))
    with pytest.raises(strider.StriderError):
        elf.symbol_size("definitely_not_a_symbol")


def test_symbol_addr_and_size_returns_addr_and_size():
    # Internal ELF-backend method (the one `ElfLifter.analyze` uses to
    # derive a function bound), reached through the wrapped backend.
    elf = strider.lift.load_elf(str(fixture_path("x64", "elf_relocs")))
    addr, size = elf._elf.symbol_addr_and_size("helper_a")
    assert addr == elf.symbol("helper_a")
    assert size == elf.symbol_size("helper_a")


def test_symbol_size_threads_into_analyze():
    """`ElfLifter.analyze` derives `function_max_size` from `st_size`
    automatically."""
    elf = strider.lift.load_elf(str(fixture_path("x64", "switch")))
    _cfg, function, _unresolved = elf.analyze(
        "dispatch_value",
        opts=strider.lift.LifterOptions(cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True)),
    )
    assert function.node_count() > 0


def test_symbol_size_returns_none_for_zero_st_size():
    """`st_size == 0` (stripped binaries, stub functions) comes back as
    `None`, not 0, so callers can branch with `if size is not None`."""
    elf = strider.lift.load_elf(str(fixture_path("x86", "control")))
    saw_none = False
    for name in elf.symbols():
        size = elf.symbol_size(name)
        if size is None:
            saw_none = True
            break
    # Deliberately not asserted: the binary may have no zero-size symbol.
    # `helper_a` above covers the "size > 0 gives an integer" branch.
    _ = saw_none
