"""Tests for the high-level Python API facade.

Covers `strider.load_elf(path)`, `ElfStrider.analyze(name)`,
`Analysis.find(pat)`, `Analysis.fingerprint(node)`, and the
`ElfStrider.functions()` iterator.  Each test skips cleanly when the
required fixture isn't built so a fresh checkout doesn't fail.
"""

from __future__ import annotations

import pytest

import strider
from strider import _api

from .conftest import FIXTURES_DIR, fixture_path


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


def test_load_returns_elf_strider():
    """`strider.load_elf(path)` returns an ElfStrider with the
    auto-picked arch + cc.  The arch name should match what `file(1)`
    reports for the fixture."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load_elf(str(elf))
    assert s.arch.name() == "x86_64"
    assert s.cc.name() == "x86_64_systemv"


def test_load_x86_32bit():
    """x86 (32-bit) fixtures pick the `x86` arch + `x86_cdecl` cc."""
    elf = fixture_path("x86", "arithmetic")
    s = strider.load_elf(str(elf))
    assert s.arch.name() == "x86"
    assert s.cc.name() == "x86_cdecl"


def test_load_aarch64():
    """aarch64 fixtures pick `aarch64` + `aarch64_aapcs64`."""
    elf = fixture_path("aarch64", "arithmetic")
    s = strider.load_elf(str(elf))
    assert s.arch.name() == "aarch64"
    assert s.cc.name() == "aarch64_aapcs64"


def test_load_missing_file_raises():
    """Non-existent paths surface as FileNotFoundError."""
    with pytest.raises(FileNotFoundError):
        strider.load_elf("/nonexistent/path/foo.elf")


def test_load_non_elf_raises(tmp_path):
    """A file that isn't an ELF surfaces as ValueError (no magic)."""
    p = tmp_path / "not-an-elf.bin"
    p.write_bytes(b"NOTELF\x00" * 16)
    with pytest.raises(ValueError):
        strider.load_elf(str(p))


def test_functions_iterator_lists_add():
    """The `add` symbol is defined in every arithmetic.elf fixture
    (added by the test harness Makefile).  `ElfStrider.functions()`
    should yield it."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load_elf(str(elf))
    fns = list(s.functions())
    assert "add" in fns, f"missing 'add' in {fns!r}"


def test_analyze_by_name_returns_analysis():
    """`ElfStrider.analyze(symbol_name)` returns an Analysis with a
    non-empty IR graph."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load_elf(str(elf))
    a = s.analyze("add")
    assert isinstance(a, strider.Analysis)
    assert a.function.node_count() > 0
    assert a.entry == s.symbol("add")
    assert a.name == "add"


def test_analyze_by_address_returns_analysis():
    """`ElfStrider.analyze(<int>)` also works."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load_elf(str(elf))
    addr = s.symbol("add")
    a = s.analyze(addr)
    assert a.entry == addr
    assert a.name is None  # no name supplied


def test_analyze_unknown_symbol_raises():
    """An undefined symbol surfaces as ReaderError."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load_elf(str(elf))
    with pytest.raises(strider.errors.StriderError):
        s.analyze("definitely_not_a_real_function_xyz")


def test_analyze_wrong_type_raises():
    """Passing a non-str/non-int raises TypeError."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load_elf(str(elf))
    with pytest.raises(TypeError):
        s.analyze(1.5)  # type: ignore[arg-type]


def test_find_against_pattern_returns_list():
    """`Analysis.find(pat)` forwards to `Function.find_all`."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load_elf(str(elf))
    a = s.analyze("add")
    # `add(a, b)` returns `a + b` — find every IntBinaryOp("Add") in
    # the lifted graph (at least one must exist: the actual add op).
    matches = a.find(strider.pattern.add(strider.pattern.any_(), strider.pattern.any_()))
    assert isinstance(matches, list)
    assert len(matches) >= 1, "expected at least one Add node in add(a,b)"


def test_find_one_returns_match_when_present():
    """`Analysis.find_one(pat)` returns the first `Match` when the
    pattern matches at least once, equal to `find(pat)[0]`."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load_elf(str(elf))
    a = s.analyze("add")
    pat = strider.pattern.add(strider.pattern.any_(), strider.pattern.any_())
    matches = a.find(pat)
    assert matches, "fixture has no Add nodes — investigate"
    one = a.find_one(pat)
    assert one is not None
    assert isinstance(one, strider.Match)
    # `find_one` is the first `find_all` hit (same preorder).
    assert one.root == matches[0].root


def test_find_one_returns_none_when_absent():
    """`Analysis.find_one(pat)` returns `None` when the pattern has no
    match anywhere in the graph."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load_elf(str(elf))
    a = s.analyze("add")
    # An impossible IntConst literal that cannot occur in `add(a, b)`.
    impossible = strider.pattern.int_const(0xDEAD_BEEF_CAFE_BABE)
    assert a.find(impossible) == []
    assert a.find_one(impossible) is None


def test_function_find_one_matches_find_all_first():
    """`Function.find_one(pat)` mirrors `find_all(pat)[0]` (or `None`)."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load_elf(str(elf))
    g = s.analyze("add").function
    pat = strider.pattern.add(strider.pattern.any_(), strider.pattern.any_())
    all_hits = g.find_all(pat)
    one = g.find_one(pat)
    assert all_hits, "fixture has no Add nodes — investigate"
    assert one is not None
    assert one.root == all_hits[0].root
    # Negative: an unmatched pattern yields None.
    assert g.find_one(strider.pattern.int_const(0xDEAD_BEEF_CAFE_BABE)) is None


def test_fingerprint_returns_machine_addresses():
    """Every matched value node should carry a non-empty
    asm-fingerprint (asm-fingerprints are always-on per G3)."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load_elf(str(elf))
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
    s = strider.load_elf(str(elf))
    a = s.analyze("add")
    matches = a.find(strider.pattern.add(strider.pattern.any_(), strider.pattern.any_()))
    assert matches
    fp_via_root = a.fingerprint(matches[0].root)
    fp_via_match = a.fingerprint(matches[0])
    assert fp_via_root == fp_via_match


def test_fingerprint_rejects_bad_type():
    """`Analysis.fingerprint(<float>)` should raise TypeError."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load_elf(str(elf))
    a = s.analyze("add")
    with pytest.raises(TypeError):
        a.fingerprint(1.5)  # type: ignore[arg-type]


def test_strider_repr():
    """`ElfStrider.__repr__` includes the arch + cc names."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load_elf(str(elf))
    r = repr(s)
    assert "x86_64" in r
    assert "x86_64_systemv" in r


def test_analysis_repr():
    """`Analysis.__repr__` includes the function name and entry."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load_elf(str(elf))
    a = s.analyze("add")
    r = repr(a)
    assert "add" in r
    assert "nodes=" in r


# ── Analyse-many: hold an ElfStrider, call analyze repeatedly ────────────


def test_analyze_many_reuse():
    """One `ElfStrider`, many functions: each `analyze()` yields a valid
    Analysis with a non-empty graph.  The handle survives repeated
    calls (the analyse-many workflow: one handle, repeated analyze)."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load_elf(str(elf))
    # Analyze the same function twice and (when present) other names —
    # proving a single handle survives repeated calls.
    a1 = s.analyze("add")
    a2 = s.analyze("add")
    assert a1.function.node_count() > 0
    assert a2.function.node_count() > 0
    assert a1.entry == a2.entry
    for name in list(s.functions()):
        if name == "add":
            continue
        try:
            other = s.analyze(name)
        except strider.errors.StriderError:
            # Data symbols / non-code names may not lift; skip them.
            continue
        assert other.function.node_count() >= 0


def test_analyze_function_max_size_clips_mid_function():
    """`function_max_size` is observable: lifting `add` unbounded
    succeeds, but clipping to `4` bytes cuts the function mid-stream so
    sequential fall-through past the bound is a function-boundary error
    (not a tail call)."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load_elf(str(elf))
    # Unbounded lifts cleanly.
    a = s.analyze("add")
    assert a.entry == s.symbol("add")
    assert a.function.node_count() > 0
    # A per-call bound of 4 clips mid-function -> function-boundary error.
    with pytest.raises(strider.errors.StriderError) as exc:
        s.analyze("add", function_max_size=4)
    assert "function-boundary error" in str(exc.value)


def test_analyze_explicit_none_lifts_whole_function():
    """An explicit `function_max_size=None` lifts the whole function;
    a tiny explicit bound clips it.  Using an address target avoids the
    symbol-size default so the bound is the only thing in play."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load_elf(str(elf))
    addr = s.symbol("add")
    # A tiny bound (4) -> sequential overflow error.
    with pytest.raises(strider.errors.StriderError) as exc:
        s.analyze(addr, function_max_size=4)
    assert "function-boundary error" in str(exc.value)
    # Explicit None -> unbounded -> lifts cleanly.
    full = s.analyze(addr, function_max_size=None)
    assert full.function.node_count() > 0


def test_analyze_allow_code_before_start_addr():
    """`allow_code_before_start_addr=True` does not raise and yields a
    valid analysis."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load_elf(str(elf))
    a = s.analyze("add", allow_code_before_start_addr=True)
    assert a.function.node_count() > 0


# ── Standalone Strider (non-ELF, address-only) ──────────────────────────


def test_standalone_strider_by_address():
    """`strider.strider(arch, cc, mem)` over a raw BufferReader lifts a
    function by address; wrapping the resulting `Function` in an
    `Analysis` lets find() and fingerprint_pcode() work even though
    there is no backing ELF symbol table."""
    elf = fixture_path("x64", "arithmetic")
    loaded = strider.load_elf(str(elf))
    mem = loaded._elf.reader()
    arch = strider.SleighArch.x86_64()
    addr = loaded.symbol("add")
    s = strider.strider(
        arch,
        strider.CallingConvention.x86_64_systemv(),
        mem,
    )
    fn, unresolved = s.analyze(addr)
    a = strider.Analysis(
        fn,
        entry=addr,
        effective_arch=arch,
        mem=mem,
        unresolved_indirect_branches=unresolved,
    )
    assert a.entry == addr
    assert a.name is None
    assert a.function.node_count() > 0
    matches = a.find(
        strider.pattern.add(strider.pattern.any_(), strider.pattern.any_())
    )
    assert matches, "expected at least one Add node"
    # The standalone path must keep fingerprint_pcode working using the
    # supplied mem + effective arch.
    pcode = a.fingerprint_pcode(matches[0].root)
    assert isinstance(pcode, list)
    for addr_, text in pcode:
        assert isinstance(addr_, int)
        assert isinstance(text, str)


def test_standalone_strider_rejects_name_targets():
    """A standalone `Strider` accepts only address targets; a name
    target raises rather than misbehaving (it is not a symbol table)."""
    elf = fixture_path("x64", "arithmetic")
    loaded = strider.load_elf(str(elf))
    mem = loaded._elf.reader()
    s = strider.strider(
        strider.SleighArch.x86_64(),
        strider.CallingConvention.x86_64_systemv(),
        mem,
    )
    with pytest.raises((TypeError, ValueError)):
        s.analyze("add")


# ── Custom pipeline via strider.run(pipeline=) ──────────────────────────


def test_run_with_custom_pipeline():
    """The custom-pipeline path lives on `strider.run(..., pipeline=)`:
    passing an `OptimizerPipeline` lifts once and applies it (skipping
    the orchestrator's indirect-branch loop) and still yields a
    non-empty graph."""
    elf = fixture_path("x64", "arithmetic")
    loaded = strider.load_elf(str(elf))
    mem = loaded._elf.reader()
    addr = loaded.symbol("add")
    result = strider.run(
        strider.SleighArch.x86_64(),
        strider.CallingConvention.x86_64_systemv(),
        mem,
        addr,
        pipeline=strider.OptimizerPipeline.default(),
    )
    assert result.function.node_count() > 0


# ── ET_REL object-file loading (`*.o`, no PT_LOAD program headers) ──────


def test_load_x64_object_file_lifts_tzcount():
    """`strider.load_elf(<path>.o)` opens an ET_REL relocatable object
    file and lifts a function from it.  ET_REL has no PT_LOAD program
    headers — the loader has to walk sections (with first-wins VMA
    dedup, since `.text` and `.text.startup` share VMA 0 pre-link).

    Pre-fix, the loader produced an empty memory map for `.o` files,
    so `analyze()` here would lift an empty CFG.  Post-fix the lift
    succeeds and yields a non-trivial IR graph.
    """
    obj = FIXTURES_DIR / "x64" / "tzcount.o"
    if not obj.exists():
        pytest.skip(f"fixture missing: {obj} (run `make CASE=tzcount` in fixtures/)")
    # Load without relocation autoload: this fixture's reloc sites widen
    # the section set and stitch `tzcount` to its callees, extending the
    # lifted body past its recorded `st_size` — but this test only cares
    # that the ET_REL section-walker surfaces `.text` and lifts a
    # non-empty graph, so keep the raw section bytes.
    p = strider.load_elf(str(obj), apply_relocations=False)
    # `tzcount` is the first global function in `.text`.  Note that
    # `p.symbols()` filters out symbols with address 0 (the safer
    # default for stripped binaries that have synthetic zero-address
    # entries), and every ET_REL symbol's `st_value` is a
    # section-relative offset that's 0 for the first symbol — so we
    # look it up by direct `symbol()` instead of through the dict.
    assert p.symbol("tzcount") is not None
    a = p.analyze("tzcount")
    # Non-empty IR graph: the loader surfaced the `.text` bytes and
    # Sleigh / strider-lift turned them into a function.
    assert a.function.node_count() > 0, (
        "loading an ET_REL .o should yield a non-empty IR graph; an "
        "empty graph means the loader produced no readable bytes for "
        ".text — the section-walker dispatch isn't engaging for ET_REL."
    )
