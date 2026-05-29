"""Tests for the high-level Python API facade.

Covers `strider.load(path)`, `Program.analyze(name)`,
`Analysis.find(pat)`, `Analysis.fingerprint(node)`, and the
`Program.functions()` iterator.  Each test skips cleanly when the
required fixture isn't built so a fresh checkout doesn't fail.
"""

from __future__ import annotations

import pytest

import strider
from strider import _api

from .conftest import fixture_path


# ── ARM Thumb interworking arch selection (pure unit, no lifting) ──────


def test_effective_arch_arm_odd_addr_picks_thumb():
    """An ARM (non-Thumb) arch with an odd entry address is a Thumb
    function (interworking convention): pick `arm_thumb` and strip
    the low bit before lifting."""
    arch, addr = _api._effective_arch_and_addr(strider.SleighArch.arm(), 0x8001)
    assert arch.name() == "arm_thumb"
    assert addr == 0x8000


def test_effective_arch_arm_even_addr_keeps_arm():
    """An ARM arch with an even (halfword-aligned) entry is a plain
    ARM function: keep `arm`, keep the address."""
    arch, addr = _api._effective_arch_and_addr(strider.SleighArch.arm(), 0x8000)
    assert arch.name() == "arm"
    assert addr == 0x8000


def test_effective_arch_arm_thumb_odd_addr_strips_bit():
    """When the arch is already `arm_thumb`, an odd entry still gets
    its interworking low bit stripped to a halfword-aligned address."""
    arch, addr = _api._effective_arch_and_addr(
        strider.SleighArch.arm_thumb(), 0x8001
    )
    assert arch.name() == "arm_thumb"
    assert addr == 0x8000


def test_effective_arch_non_arm_odd_addr_unchanged():
    """x86 (and other non-ARM arches) never interwork: an odd address
    is passed through verbatim with the arch unchanged."""
    arch, addr = _api._effective_arch_and_addr(
        strider.SleighArch.x86_64(), 0x401001
    )
    assert arch.name() == "x86_64"
    assert addr == 0x401001


# ── Basic load + analyze + find ────────────────────────────────────────


def test_load_returns_program():
    """`strider.load(path)` returns a Program with the auto-picked
    arch + cc.  The arch name should match what `file(1)` reports
    for the fixture."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load(str(elf))
    assert s.arch.name() == "x86_64"
    assert s.cc.name() == "x86_64_systemv"


def test_load_x86_32bit():
    """x86 (32-bit) fixtures pick the `x86` arch + `x86_cdecl` cc."""
    elf = fixture_path("x86", "arithmetic")
    s = strider.load(str(elf))
    assert s.arch.name() == "x86"
    assert s.cc.name() == "x86_cdecl"


def test_load_aarch64():
    """aarch64 fixtures pick `aarch64` + `aarch64_aapcs64`."""
    elf = fixture_path("aarch64", "arithmetic")
    s = strider.load(str(elf))
    assert s.arch.name() == "aarch64"
    assert s.cc.name() == "aarch64_aapcs64"


def test_load_missing_file_raises():
    """Non-existent paths surface as FileNotFoundError."""
    with pytest.raises(FileNotFoundError):
        strider.load("/nonexistent/path/foo.elf")


def test_load_non_elf_raises(tmp_path):
    """A file that isn't an ELF surfaces as ValueError (no magic)."""
    p = tmp_path / "not-an-elf.bin"
    p.write_bytes(b"NOTELF\x00" * 16)
    with pytest.raises(ValueError):
        strider.load(str(p))


def test_functions_iterator_lists_add():
    """The `add` symbol is defined in every arithmetic.elf fixture
    (added by the test harness Makefile).  `Program.functions()`
    should yield it."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load(str(elf))
    fns = list(s.functions())
    assert "add" in fns, f"missing 'add' in {fns!r}"


def test_analyze_by_name_returns_analysis():
    """`Program.analyze(symbol_name)` returns an Analysis with a
    non-empty IR graph."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load(str(elf))
    a = s.analyze("add")
    assert isinstance(a, strider.Analysis)
    assert a.function.node_count() > 0
    assert a.entry == s.symbol("add")
    assert a.name == "add"


def test_analyze_by_address_returns_analysis():
    """`Program.analyze(<int>)` also works."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load(str(elf))
    addr = s.symbol("add")
    a = s.analyze(addr)
    assert a.entry == addr
    assert a.name is None  # no name supplied


def test_analyze_unknown_symbol_raises():
    """An undefined symbol surfaces as ReaderError."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load(str(elf))
    with pytest.raises(strider.errors.StriderError):
        s.analyze("definitely_not_a_real_function_xyz")


def test_analyze_wrong_type_raises():
    """Passing a non-str/non-int raises TypeError."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load(str(elf))
    with pytest.raises(TypeError):
        s.analyze(1.5)  # type: ignore[arg-type]


def test_find_against_pattern_returns_list():
    """`Analysis.find(pat)` forwards to `Function.find_all`."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load(str(elf))
    a = s.analyze("add")
    # `add(a, b)` returns `a + b` — find every IntBinaryOp("Add") in
    # the lifted graph (at least one must exist: the actual add op).
    matches = a.find(strider.pattern.add(strider.pattern.any_(), strider.pattern.any_()))
    assert isinstance(matches, list)
    assert len(matches) >= 1, "expected at least one Add node in add(a,b)"


def test_fingerprint_returns_machine_addresses():
    """Every matched value node should carry a non-empty
    asm-fingerprint (asm-fingerprints are always-on per G3)."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load(str(elf))
    a = s.analyze("add")
    matches = a.find(strider.pattern.add(strider.pattern.any_(), strider.pattern.any_()))
    assert matches, "test fixture has no Add nodes — investigate"
    fp = a.fingerprint(matches[0].root)
    assert isinstance(fp, list)
    assert all(isinstance(addr, int) for addr in fp)
    # An IntBinaryOp("Add") lifted from a real add instruction must
    # carry at least one source address — empty fingerprints are only
    # allowed on structural node kinds (phis / Entry / InitialMemory /
    # FunctionArg).
    assert len(fp) >= 1, f"empty fingerprint on Add match {matches[0].root}"
    # The fingerprint addresses should be plausible machine instruction
    # addresses (within or near the function entry).
    entry = a.entry
    for addr in fp:
        # No tight bound on function size but addresses must be within
        # the loaded ELF text region — a generous 1 MB window.
        assert entry - (1 << 20) <= addr <= entry + (1 << 20), (
            f"fingerprint address {addr:#x} is implausibly far from "
            f"function entry {entry:#x}"
        )


def test_fingerprint_accepts_match_directly():
    """`Analysis.fingerprint(match)` (no `.root`) is supported as a
    convenience — the match's root id is read internally."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load(str(elf))
    a = s.analyze("add")
    matches = a.find(strider.pattern.add(strider.pattern.any_(), strider.pattern.any_()))
    assert matches
    fp_via_root = a.fingerprint(matches[0].root)
    fp_via_match = a.fingerprint(matches[0])
    assert fp_via_root == fp_via_match


def test_fingerprint_rejects_bad_type():
    """`Analysis.fingerprint(<float>)` should raise TypeError."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load(str(elf))
    a = s.analyze("add")
    with pytest.raises(TypeError):
        a.fingerprint(1.5)  # type: ignore[arg-type]


def test_strider_repr():
    """`Program.__repr__` includes the arch + cc names."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load(str(elf))
    r = repr(s)
    assert "x86_64" in r
    assert "x86_64_systemv" in r


def test_analysis_repr():
    """`Analysis.__repr__` includes the function name and entry."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load(str(elf))
    a = s.analyze("add")
    r = repr(a)
    assert "add" in r
    assert "nodes=" in r
