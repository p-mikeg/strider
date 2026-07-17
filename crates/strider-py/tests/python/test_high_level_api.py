"""Tests for the high-level Python API facade.

Covers `strider.load_elf(path)` / `load_elf_from_segments` /
`load_elf_from_sections`, `ElfLifter.analyze(name)` (returns the same
`(Cfg, Function, unresolved)` tuple as the base `Lifter.analyze`), and the
`ElfLifter.functions()` iterator.  Each test skips cleanly when the
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


def test_load_returns_elf_lifter():
    """`strider.load_elf(path)` returns an `ElfLifter` (which IS a
    `Lifter`) with the auto-picked arch + cc.  The arch name should
    match what `file(1)` reports for the fixture."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load_elf(str(elf))
    assert isinstance(s, strider.Lifter)
    assert s.arch.name() == "x86_64"
    assert s.cc.name() == "x86_64_systemv"


def test_load_elf_accepts_os_pathlike():
    """Every loader takes an `os.PathLike`, not just `str`.

    `fixture_path` (and `pathlib.Path` generally) is what callers actually
    have in hand; requiring `str(path)` at every call site is friction the
    stdlib already solves with `os.fspath`.  A custom `__fspath__` object
    must work too — that is the whole point of the protocol.
    """
    elf = fixture_path("x64", "arithmetic")
    if not elf.exists():
        pytest.skip("fixture not built")

    class Wrapper:
        def __fspath__(self) -> str:
            return str(elf)

    for arg in (elf, Wrapper()):
        for loader in (
            strider.load_elf,
            strider.load_elf_from_segments,
            strider.load_elf_from_sections,
        ):
            lift = loader(arg)
            assert isinstance(lift, strider.Lifter)
            assert lift.arch.name() == "x86_64"


def test_load_elf_rejects_non_path():
    """A non-path argument still fails loudly rather than being coerced."""
    with pytest.raises(TypeError):
        strider.load_elf(1234)


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
    (added by the test harness Makefile).  `ElfLifter.functions()`
    should yield it."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load_elf(str(elf))
    fns = list(s.functions())
    assert "add" in fns, f"missing 'add' in {fns!r}"


def test_load_elf_from_segments_symbol_analyze():
    """`strider.load_elf_from_segments(path)` returns an `ElfLifter`
    (an `ElfLifter` IS a `Lifter`); `symbol()` resolves a name and
    `analyze(name)` returns the base `(Cfg, Function, unresolved)` tuple."""
    elf = fixture_path("x64", "arithmetic")
    lift = strider.load_elf_from_segments(str(elf))
    assert isinstance(lift, strider.Lifter)
    addr = lift.symbol("add")
    assert isinstance(addr, int)
    _cfg, graph, unresolved = lift.analyze("add")
    assert graph.node_count() > 0
    assert isinstance(unresolved, list)


def test_load_elf_from_sections_symbol_analyze():
    """`strider.load_elf_from_sections(path)` forces the section-walk
    region strategy but produces an equally-usable `ElfLifter`."""
    elf = fixture_path("x64", "arithmetic")
    lift = strider.load_elf_from_sections(str(elf))
    assert isinstance(lift, strider.Lifter)
    addr = lift.symbol("add")
    assert isinstance(addr, int)
    _cfg, graph, unresolved = lift.analyze("add")
    assert graph.node_count() > 0
    assert isinstance(unresolved, list)


def test_analyze_returns_cfg_first():
    """`analyze` returns the final CFG as the FIRST tuple element,
    followed by `Function` then the unresolved-indirect-branch list."""
    elf = fixture_path("x64", "arithmetic")
    lift = strider.load_elf_from_segments(str(elf))
    cfg, function, unresolved = lift.analyze("add")
    assert isinstance(cfg, strider.Cfg)
    assert function.node_count() > 0 and isinstance(unresolved, list)


def test_analyze_by_name_returns_tuple():
    """`ElfLifter.analyze(symbol_name)` returns the same
    `(Cfg, Function, unresolved)` tuple as the base `Lifter.analyze`, with
    a non-empty IR graph."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load_elf(str(elf))
    result = s.analyze("add")
    assert isinstance(result, tuple) and len(result) == 3
    cfg, function, unresolved = result
    assert isinstance(cfg, strider.Cfg)
    assert function.node_count() > 0
    assert isinstance(unresolved, list)


def test_analyze_by_address_returns_tuple():
    """`ElfLifter.analyze(<int>)` also works."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load_elf(str(elf))
    addr = s.symbol("add")
    _cfg, function, unresolved = s.analyze(addr)
    assert function.node_count() > 0
    assert isinstance(unresolved, list)


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
    """`Function.find_all(pat)` works directly on the tuple's function."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load_elf(str(elf))
    _cfg, function, _unresolved = s.analyze("add")
    # `add(a, b)` returns `a + b` — find every IntBinaryOp("Add") in
    # the lifted graph (at least one must exist: the actual add op).
    matches = function.find_all(
        strider.pattern.add(strider.pattern.anything(), strider.pattern.anything())
    )
    assert isinstance(matches, list)
    assert len(matches) >= 1, "expected at least one Add node in add(a,b)"


def test_find_one_returns_match_when_present():
    """`Function.find_one(pat)` returns the first `Match` when the
    pattern matches at least once, equal to `find_all(pat)[0]`."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load_elf(str(elf))
    _cfg, function, _unresolved = s.analyze("add")
    pat = strider.pattern.add(strider.pattern.anything(), strider.pattern.anything())
    matches = function.find_all(pat)
    assert matches, "fixture has no Add nodes — investigate"
    one = function.find_one(pat)
    assert one is not None
    assert isinstance(one, strider.Match)
    # `find_one` is the first `find_all` hit (same preorder).
    assert one.root == matches[0].root


def test_find_one_returns_none_when_absent():
    """`Function.find_one(pat)` returns `None` when the pattern has no
    match anywhere in the graph."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load_elf(str(elf))
    _cfg, function, _unresolved = s.analyze("add")
    # An impossible IntConst literal that cannot occur in `add(a, b)`.
    impossible = strider.pattern.int_const(0xDEAD_BEEF_CAFE_BABE)
    assert function.find_all(impossible) == []
    assert function.find_one(impossible) is None


def test_function_find_one_matches_find_all_first():
    """`Function.find_one(pat)` mirrors `find_all(pat)[0]` (or `None`)."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load_elf(str(elf))
    _cfg, g, _unresolved = s.analyze("add")
    pat = strider.pattern.add(strider.pattern.anything(), strider.pattern.anything())
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
    addr = s.symbol("add")
    _cfg, function, _unresolved = s.analyze("add")
    matches = function.find_all(
        strider.pattern.add(strider.pattern.anything(), strider.pattern.anything())
    )
    assert matches, "test fixture has no Add nodes — investigate"
    fp = function.node(matches[0].root).fingerprint()
    assert isinstance(fp, list)
    assert all(isinstance(a, int) for a in fp)
    # An IntBinaryOp("Add") lifted from a real add instruction must
    # carry at least one source address — empty fingerprints are only
    # allowed on structural node kinds (phis / Entry / InitialMemory /
    # FunctionArg).
    assert len(fp) >= 1, f"empty fingerprint on Add match {matches[0].root}"
    # The fingerprint addresses should be plausible machine instruction
    # addresses (within or near the function entry).
    for a in fp:
        # No tight bound on function size but addresses must be within
        # the loaded ELF text region — a generous 1 MB window.
        assert addr - (1 << 20) <= a <= addr + (1 << 20), (
            f"fingerprint address {a:#x} is implausibly far from "
            f"function entry {addr:#x}"
        )


def test_fingerprint_matches_via_node_and_match_forwarder():
    """`Node` is the single source of truth for the addr-only
    fingerprint; `Match.asm_fingerprint(key)` is a thin forwarder onto
    `Node.fingerprint()` for a captured node."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load_elf(str(elf))
    _cfg, function, _unresolved = s.analyze("add")
    c = strider.pattern.Capture()
    matches = function.find_all(
        strider.pattern.add(strider.pattern.anything(), strider.pattern.anything()).capture(c)
    )
    assert matches
    fp_via_node = function.node(matches[0].root).fingerprint()
    fp_via_match = matches[0].asm_fingerprint(c)
    assert fp_via_node == fp_via_match


def test_fingerprint_rejects_bad_type():
    """`Function.node(<float>)` should raise TypeError — the same `u32`
    conversion the removed id-keyed `asm_fingerprint(id)` used to reject
    on."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load_elf(str(elf))
    _cfg, function, _unresolved = s.analyze("add")
    with pytest.raises(TypeError):
        function.node(1.5)  # type: ignore[arg-type]


def test_elf_lifter_repr():
    """`ElfLifter.__repr__` includes the arch + cc names."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load_elf(str(elf))
    r = repr(s)
    assert "x86_64" in r
    assert "x86_64_systemv" in r


# ── Analyse-many: hold an ElfLifter, call analyze repeatedly ────────────


def test_analyze_many_reuse():
    """One `ElfLifter`, many functions: each `analyze()` yields a valid
    tuple with a non-empty graph.  The handle survives repeated calls
    (the analyse-many workflow: one handle, repeated analyze)."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load_elf(str(elf))
    # Analyze the same function twice and (when present) other names —
    # proving a single handle survives repeated calls.
    _cfg, function1, _unresolved1 = s.analyze("add")
    _cfg, function2, _unresolved2 = s.analyze("add")
    assert function1.node_count() > 0
    assert function2.node_count() > 0
    for name in list(s.functions()):
        if name == "add":
            continue
        try:
            _cfg, other_function, _unresolved = s.analyze(name)
        except strider.errors.StriderError:
            # Data symbols / non-code names may not lift; skip them.
            continue
        assert other_function.node_count() >= 0


def test_analyze_function_max_size_clips_mid_function():
    """`function_max_size` is observable: lifting `add` unbounded
    succeeds, but clipping to `4` bytes cuts the function mid-stream so
    sequential fall-through past the bound is a function-boundary error
    (not a tail call)."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load_elf(str(elf))
    # Unbounded lifts cleanly.
    _cfg, function, _unresolved = s.analyze("add")
    assert function.node_count() > 0
    # A per-call bound of 4 clips mid-function -> function-boundary error.
    with pytest.raises(strider.errors.StriderError) as exc:
        s.analyze("add", opts=strider.LifterOptions(cfg=strider.CfgOptions(function_max_size=4)))
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
        s.analyze(addr, opts=strider.LifterOptions(cfg=strider.CfgOptions(function_max_size=4)))
    assert "function-boundary error" in str(exc.value)
    # Explicit None -> unbounded -> lifts cleanly.
    _cfg, full_function, _unresolved = s.analyze(
        addr, opts=strider.LifterOptions(cfg=strider.CfgOptions(function_max_size=None))
    )
    assert full_function.node_count() > 0


def test_analyze_allow_code_before_start_addr():
    """`allow_code_before_start_addr=True` does not raise and yields a
    valid analysis."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.load_elf(str(elf))
    _cfg, function, _unresolved = s.analyze(
        "add", opts=strider.LifterOptions(cfg=strider.CfgOptions(allow_code_before_start_addr=True))
    )
    assert function.node_count() > 0


# ── Standalone Strider (non-ELF, address-only) ──────────────────────────


def test_standalone_strider_by_address():
    """`strider.lifter(arch, mem)` over a raw BufferReader lifts a
    function by address; pattern queries work directly on the returned
    `Function` and `fingerprint_pcode` works directly on the `Cfg`
    `analyze` returns — no wrapper needed even though there is no
    backing ELF symbol table."""
    elf = fixture_path("x64", "arithmetic")
    loaded = strider.load_elf(str(elf))
    mem = loaded._elf.reader()
    arch = strider.SleighArch.x86_64()
    addr = loaded.symbol("add")
    s = strider.lifter(arch, mem)
    cfg, fn, _unresolved = s.analyze(addr, strider.CallingConvention.x86_64_systemv())
    assert fn.node_count() > 0
    matches = fn.find_all(
        strider.pattern.add(strider.pattern.anything(), strider.pattern.anything())
    )
    assert matches, "expected at least one Add node"
    # The standalone path must keep fingerprint_pcode working via the
    # Cfg that produced the function.
    pcode = cfg.fingerprint_pcode(fn.node(matches[0].root))
    assert isinstance(pcode, list)
    for addr_, text in pcode:
        assert isinstance(addr_, int)
        assert isinstance(text, str)


def test_standalone_strider_rejects_name_targets():
    """A standalone `Lifter` accepts only address targets; a name
    target raises rather than misbehaving (it is not a symbol table)."""
    elf = fixture_path("x64", "arithmetic")
    loaded = strider.load_elf(str(elf))
    mem = loaded._elf.reader()
    s = strider.lifter(strider.SleighArch.x86_64(), mem)
    with pytest.raises((TypeError, ValueError)):
        s.analyze("add", strider.CallingConvention.x86_64_systemv())


# The custom-pipeline path used to live on `strider.run(..., pipeline=)`:
# passing an `OptimizerPipeline` lifted once and applied it, skipping the
# orchestrator's indirect-branch loop.  The single-`Lifter` collapse (Task
# 2 of the strider-py API redesign) removed that entry point —
# `Lifter.analyze` always drives the canonical default pipeline plus
# indirect-branch resolution.  A caller wanting extra passes on top of
# the fully-resolved graph now calls `Lifter.optimize(function, pipeline)`
# afterwards (already covered by `test_optimizer_pipeline.py`).


# ── ET_REL object-file loading (`*.o`, no PT_LOAD program headers) ──────


def test_load_x64_object_file_lifts_tzcount():
    """`strider.load_elf(<path>.o)` opens an ET_REL relocatable object
    file and lifts a function from it.  ET_REL has no PT_LOAD program
    headers — the loader (`load_elf_from_segments`'s underlying
    strategy) has to walk sections (with first-wins VMA dedup, since
    `.text` and `.text.startup` share VMA 0 pre-link).

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
    _cfg, function, _unresolved = p.analyze("tzcount")
    # Non-empty IR graph: the loader surfaced the `.text` bytes and
    # Sleigh / strider-lift turned them into a function.
    assert function.node_count() > 0, (
        "loading an ET_REL .o should yield a non-empty IR graph; an "
        "empty graph means the loader produced no readable bytes for "
        ".text — the section-walker dispatch isn't engaging for ET_REL."
    )
