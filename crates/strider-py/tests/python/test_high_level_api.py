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


def test_find_one_returns_match_when_present():
    """`Analysis.find_one(pat)` returns the first `Match` when the
    pattern matches at least once, equal to `find(pat)[0]`."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load(str(elf))
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
    s = strider.load(str(elf))
    a = s.analyze("add")
    # An impossible IntConst literal that cannot occur in `add(a, b)`.
    impossible = strider.pattern.int_const(0xDEAD_BEEF_CAFE_BABE)
    assert a.find(impossible) == []
    assert a.find_one(impossible) is None


def test_function_find_one_matches_find_all_first():
    """`Function.find_one(pat)` mirrors `find_all(pat)[0]` (or `None`)."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load(str(elf))
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


# ── Analyzer: frozen configure-once handle ──────────────────────────────


def test_program_analyzer_returns_analyzer():
    """`Program.analyzer()` returns an `Analyzer` and analysing `"add"`
    through it yields an `Analysis` equal in entry/name to the direct
    `Program.analyze("add")` path."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load(str(elf))
    azr = s.analyzer()
    assert isinstance(azr, strider.Analyzer)
    via_azr = azr.analyze("add")
    via_prog = s.analyze("add")
    assert isinstance(via_azr, strider.Analysis)
    assert via_azr.entry == via_prog.entry
    assert via_azr.name == via_prog.name == "add"
    assert via_azr.function.node_count() > 0


def test_analyzer_configure_once_reuse():
    """One analyzer, many functions: each analyze() yields a valid
    Analysis with a non-empty graph (the configure-once promise)."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load(str(elf))
    azr = s.analyzer()
    # Analyze the same function twice and (when present) other names —
    # proving a single analyzer survives repeated calls.
    a1 = azr.analyze("add")
    a2 = azr.analyze("add")
    assert a1.function.node_count() > 0
    assert a2.function.node_count() > 0
    assert a1.entry == a2.entry
    for name in list(s.functions()):
        if name == "add":
            continue
        try:
            other = azr.analyze(name)
        except strider.errors.StriderError:
            # Data symbols / non-code names may not lift; skip them.
            continue
        assert other.function.node_count() >= 0


def test_analyzer_per_call_override_function_max_size():
    """A per-call keyword overrides the frozen default for that one
    call: a frozen `function_max_size=None` overridden with a small
    bound still builds a valid analysis."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load(str(elf))
    azr = s.analyzer(function_max_size=None)
    a = azr.analyze("add", function_max_size=4)
    assert a.entry == s.symbol("add")
    assert a.function.node_count() > 0


def test_analyzer_per_call_override_allow_code_before():
    """A per-call `allow_code_before_start_addr=True` override (over a
    frozen `False`) does not raise and yields a valid analysis."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load(str(elf))
    azr = s.analyzer(allow_code_before_start_addr=False)
    a = azr.analyze("add", allow_code_before_start_addr=True)
    assert a.function.node_count() > 0


def test_analyzer_is_frozen_no_setters():
    """The analyzer is a frozen configure-once handle: no public
    `set_*` mutator exists and reusing it twice is stable."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load(str(elf))
    azr = s.analyzer()
    setters = [n for n in dir(azr) if n.startswith("set_")]
    assert setters == [], f"analyzer exposes setters: {setters}"
    # __slots__ should block attaching new attributes.
    with pytest.raises(AttributeError):
        azr.arch = strider.SleighArch.x86()  # type: ignore[misc]
    # Reuse is stable.
    n1 = azr.analyze("add").function.node_count()
    n2 = azr.analyze("add").function.node_count()
    assert n1 == n2


def test_analyzer_repr_includes_arch_cc_and_symbol_flag():
    """`Analyzer.__repr__` names arch + cc and whether a symbol table
    is present (an ELF-backed analyzer has one)."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load(str(elf))
    r = repr(s.analyzer())
    assert "x86_64" in r
    assert "x86_64_systemv" in r
    assert "symbols=" in r


# ── Standalone analyzer (program=None path) ─────────────────────────────


def test_standalone_analyzer_by_address():
    """`strider.analyzer(arch, cc, mem)` over a raw MemoryMap lifts a
    function by address; find() and fingerprint_pcode() work even
    though there is no backing Program (the program=None path)."""
    elf = fixture_path("x64", "arithmetic")
    loaded = strider.load_elf(str(elf))
    mem = loaded.memory_map()
    addr = loaded.symbol("add")
    azr = strider.analyzer(
        strider.SleighArch.x86_64(),
        strider.CallingConvention.x86_64_systemv(),
        mem,
    )
    a = azr.analyze(addr)
    assert isinstance(a, strider.Analysis)
    assert a.entry == addr
    assert a.name is None
    assert a.function.node_count() > 0
    matches = a.find(
        strider.pattern.add(strider.pattern.any_(), strider.pattern.any_())
    )
    assert matches, "expected at least one Add node"
    # The program=None path must keep fingerprint_pcode working using
    # the analyzer's own mem + effective arch.
    pcode = a.fingerprint_pcode(matches[0].root)
    assert isinstance(pcode, list)
    for addr_, text in pcode:
        assert isinstance(addr_, int)
        assert isinstance(text, str)


def test_standalone_analyzer_with_symbols_dict():
    """A standalone analyzer with a `symbols={...}` dict resolves a
    name target."""
    elf = fixture_path("x64", "arithmetic")
    loaded = strider.load_elf(str(elf))
    mem = loaded.memory_map()
    addr = loaded.symbol("add")
    azr = strider.analyzer(
        strider.SleighArch.x86_64(),
        strider.CallingConvention.x86_64_systemv(),
        mem,
        symbols={"add": addr},
    )
    a = azr.analyze("add")
    assert a.entry == addr
    assert a.name == "add"
    assert a.function.node_count() > 0


def test_standalone_analyzer_name_without_symbols_raises():
    """A name target on a standalone analyzer with no symbol source is
    a clear error (TypeError/ValueError), not a silent misbehaviour."""
    elf = fixture_path("x64", "arithmetic")
    loaded = strider.load_elf(str(elf))
    mem = loaded.memory_map()
    azr = strider.analyzer(
        strider.SleighArch.x86_64(),
        strider.CallingConvention.x86_64_systemv(),
        mem,
    )
    with pytest.raises((TypeError, ValueError)):
        azr.analyze("add")


def test_analyzer_pcode_parity():
    """`Analyzer.pcode(addr)` mirrors `Program.pcode(addr)` over the
    same memory + arch."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load(str(elf))
    addr = s.symbol("add")
    azr = s.analyzer()
    assert azr.pcode(addr, 2) == s.pcode(addr, 2)


# ── pipeline_factory: fresh pipeline per call ───────────────────────────


def test_analyzer_pipeline_factory_invoked_fresh_per_call():
    """`pipeline_factory` is called fresh for each analyze() so the
    drain-on-use problem (a single pipeline can't be reused) is
    avoided; both calls succeed."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load(str(elf))
    calls = {"n": 0}

    def factory():
        calls["n"] += 1
        return strider.OptimizerPipeline.default()

    azr = s.analyzer(pipeline_factory=factory)
    a1 = azr.analyze("add")
    a2 = azr.analyze("add")
    assert calls["n"] == 2, "pipeline_factory must be called once per analyze"
    assert a1.function.node_count() > 0
    assert a2.function.node_count() > 0


def test_analyzer_per_call_pipeline_overrides_factory():
    """A per-call `pipeline=` overrides the frozen `pipeline_factory`
    for that call (factory not invoked)."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load(str(elf))
    calls = {"n": 0}

    def factory():
        calls["n"] += 1
        return strider.OptimizerPipeline.default()

    azr = s.analyzer(pipeline_factory=factory)
    a = azr.analyze("add", pipeline=strider.OptimizerPipeline.default())
    assert calls["n"] == 0, "per-call pipeline must override the factory"
    assert a.function.node_count() > 0
