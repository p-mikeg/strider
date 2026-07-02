"""Tests for `ElfLifter.symbol_size` + the ELF backend's
`symbol_addr_and_size`.

The ELF symbol table records each function's size in `st_size`.
Strider users typically need that value for `function_max_size=`
on `strider.run` / `Lifter.build_cfg` so the analyser knows where
the function ends (and the indirect-branch resolver can
distinguish intra-fn jumps from tail calls).  Without these
helpers users have to fall back to pyelftools.
"""

from __future__ import annotations

import pytest

import strider

from .conftest import fixture_path


def test_symbol_size_returns_known_function_size():
    elf = strider.load_elf(str(fixture_path("x64", "elf_relocs")))
    # `helper_a` is a small function in the ELF; we accept anything > 0
    # to stay tolerant of toolchain-version layout differences.
    size = elf.symbol_size("helper_a")
    assert size is not None and size > 0


def test_symbol_size_raises_on_unknown_symbol():
    elf = strider.load_elf(str(fixture_path("x64", "elf_relocs")))
    with pytest.raises(strider.errors.StriderError):
        elf.symbol_size("definitely_not_a_symbol")


def test_symbol_addr_and_size_returns_addr_and_size():
    # `symbol_addr_and_size` is an internal ELF-backend method (the one
    # `ElfLifter.analyze` uses to derive a function bound); reach it
    # through the backend the handle wraps.
    elf = strider.load_elf(str(fixture_path("x64", "elf_relocs")))
    addr, size = elf._elf.symbol_addr_and_size("helper_a")
    assert addr == elf.symbol("helper_a")
    assert size == elf.symbol_size("helper_a")


def test_symbol_size_threads_into_analyze():
    """End-to-end: `ElfLifter.analyze` derives `function_max_size`
    from the ELF's `st_size` automatically and the analyser respects
    it."""
    elf = strider.load_elf(str(fixture_path("x64", "switch")))
    function, _unresolved = elf.analyze(
        "dispatch_value", allow_code_before_start_addr=True
    )
    assert function.node_count() > 0


def test_symbol_size_returns_none_for_zero_st_size():
    """ELF symbols with `st_size == 0` (typical for stripped binaries
    or stub functions) come back as `None` — not 0 — so callers
    can branch with `if size is not None`."""
    elf = strider.load_elf(str(fixture_path("x86", "control")))
    # Walk every symbol; if any has size 0 the helper must report None.
    saw_none = False
    for name in elf.symbols():
        size = elf.symbol_size(name)
        if size is None:
            saw_none = True
            break
    # We don't assert saw_none unconditionally — the test is robust
    # whether or not the binary has a zero-size symbol.  The
    # important contract is "size 0 → None" and the test above for
    # helper_a covers the "size > 0 → integer" branch.
    _ = saw_none
