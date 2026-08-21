from __future__ import annotations

import pytest

import strider

from .conftest import FIXTURES_DIR, fixture_path





def test_load_returns_elf_lifter():
    """`load_elf` auto-picks arch + cc from the ELF header."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.lift.load_elf(str(elf))
    assert isinstance(s, strider.lift.Lifter)
    assert s.arch.name() == "x86_64"
    assert s.cc.name() == "x86_64_systemv"


def test_load_elf_accepts_os_pathlike():
    """Every loader takes any `os.PathLike`, not just `str`, including a
    custom `__fspath__` object."""
    elf = fixture_path("x64", "arithmetic")
    if not elf.exists():
        pytest.skip("fixture not built")

    class Wrapper:
        def __fspath__(self) -> str:
            return str(elf)

    for arg in (elf, Wrapper()):
        for from_segments in (True, False):
            lift = strider.lift.load_elf(arg, from_segments=from_segments)
            assert isinstance(lift, strider.lift.Lifter)
            assert lift.arch.name() == "x86_64"


def test_load_elf_rejects_non_path():
    """A non-path argument still fails loudly rather than being coerced."""
    with pytest.raises(TypeError):
        strider.lift.load_elf(1234)


def test_load_x86_32bit():
    elf = fixture_path("x86", "arithmetic")
    s = strider.lift.load_elf(str(elf))
    assert s.arch.name() == "x86"
    assert s.cc.name() == "x86_cdecl"


def test_load_aarch64():
    elf = fixture_path("aarch64", "arithmetic")
    s = strider.lift.load_elf(str(elf))
    assert s.arch.name() == "aarch64"
    assert s.cc.name() == "aarch64_aapcs64"


def test_load_missing_file_raises():
    with pytest.raises(FileNotFoundError):
        strider.lift.load_elf("/nonexistent/path/foo.elf")


def test_load_non_elf_raises(tmp_path):
    p = tmp_path / "not-an-elf.bin"
    p.write_bytes(b"NOTELF\x00" * 16)
    with pytest.raises(ValueError):
        strider.lift.load_elf(str(p))


def test_functions_iterator_lists_add():
    """Every arithmetic.elf fixture defines `add`, so `functions()` must
    yield it."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.lift.load_elf(str(elf))
    fns = [sym.name for sym in s.functions()]
    assert "add" in fns, f"missing 'add' in {fns!r}"


def test_load_elf_from_segments_symbol_analyze():
    """Default (segment-walk) load: `symbol()` resolves a name and
    `analyze(name)` returns the base `(Cfg, Function, unresolved)` tuple."""
    elf = fixture_path("x64", "arithmetic")
    lift = strider.lift.load_elf(str(elf))
    assert isinstance(lift, strider.lift.Lifter)
    add = lift.symbol("add")
    assert add.name == "add" and add.is_function
    assert isinstance(add.address, int) and add.address > 0
    _cfg, graph, unresolved = lift.analyze("add")
    assert graph.node_count() > 0
    assert isinstance(unresolved, list)


def test_load_elf_from_sections_symbol_analyze():
    """`from_segments=False` forces the section-walk strategy but yields an
    equally usable lifter."""
    elf = fixture_path("x64", "arithmetic")
    lift = strider.lift.load_elf(str(elf), from_segments=False)
    assert isinstance(lift, strider.lift.Lifter)
    add = lift.symbol("add")
    assert add.name == "add" and isinstance(add.address, int)
    _cfg, graph, unresolved = lift.analyze("add")
    assert graph.node_count() > 0
    assert isinstance(unresolved, list)


def test_load_elf_flag_selects_strategy():
    """`from_segments` is the only strategy selector; the old
    `load_elf_from_segments` / `load_elf_from_sections` names are gone."""
    elf = fixture_path("x64", "arithmetic")
    a = strider.lift.load_elf(str(elf))
    b = strider.lift.load_elf(str(elf), from_segments=False)
    assert isinstance(a, strider.lift.ElfLifter)
    assert isinstance(b, strider.lift.ElfLifter)
    assert not hasattr(strider, "load_elf_from_segments")
    assert not hasattr(strider, "load_elf_from_sections")


def test_analyze_returns_cfg_first():
    elf = fixture_path("x64", "arithmetic")
    lift = strider.lift.load_elf(str(elf))
    cfg, function, unresolved = lift.analyze("add")
    assert isinstance(cfg, strider.cfg.Cfg)
    assert function.node_count() > 0 and isinstance(unresolved, list)


def test_analyze_by_name_returns_named_result():
    """Analyzing by symbol name returns the same `AnalyzeResult` as the
    base `Lifter.analyze`."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.lift.load_elf(str(elf))
    result = s.analyze("add")
    assert isinstance(result, strider.lift.AnalyzeResult)
    assert isinstance(result.cfg, strider.cfg.Cfg)
    assert result.function.node_count() > 0
    assert isinstance(result.unresolved, list)


def test_analyze_result_unpacks_as_a_triple():
    """`AnalyzeResult` keeps the legacy destructuring shape: a 3-sequence of
    `(cfg, function, unresolved)`, so old tuple-unpacking call sites work."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.lift.load_elf(str(elf))
    result = s.analyze("add")
    assert len(result) == 3
    cfg, function, unresolved = result
    assert cfg is result.cfg
    assert function is result.function
    assert unresolved == result.unresolved
    assert result[0] is result.cfg and result[-1] == result.unresolved
    with pytest.raises(IndexError):
        result[3]


def test_analyze_result_is_a_real_tuple():
    """It IS a tuple: `isinstance(result, tuple)` and structural `==` against a
    plain tuple both hold, so it interoperates anywhere a tuple is expected."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.lift.load_elf(str(elf))
    result = s.analyze("add")
    assert isinstance(result, tuple)
    assert result == (result.cfg, result.function, result.unresolved)


def test_analyze_by_address_returns_tuple():
    elf = fixture_path("x64", "arithmetic")
    s = strider.lift.load_elf(str(elf))
    addr = s.symbol("add").address
    _cfg, function, unresolved = s.analyze(addr)
    assert function.node_count() > 0
    assert isinstance(unresolved, list)


def test_analyze_unknown_symbol_raises():
    elf = fixture_path("x64", "arithmetic")
    s = strider.lift.load_elf(str(elf))
    with pytest.raises(strider.StriderError):
        s.analyze("definitely_not_a_real_function_xyz")


def test_analyze_wrong_type_raises():
    elf = fixture_path("x64", "arithmetic")
    s = strider.lift.load_elf(str(elf))
    with pytest.raises(TypeError):
        s.analyze(1.5)  # type: ignore[arg-type]


def test_find_against_pattern_returns_list():
    """`find_all` works directly on the function `analyze` returns, with no
    wrapper object."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.lift.load_elf(str(elf))
    _cfg, function, _unresolved = s.analyze("add")
    matches = function.find_all(
        strider.pattern.int_add(strider.pattern.anything(), strider.pattern.anything())
    )
    assert isinstance(matches, list)
    assert len(matches) >= 1, "expected at least one Add node in int_add(a,b)"


def test_find_all_returns_empty_when_absent():
    elf = fixture_path("x64", "arithmetic")
    s = strider.lift.load_elf(str(elf))
    _cfg, function, _unresolved = s.analyze("add")
    # A literal that cannot occur in `int_add(a, b)`.
    impossible = strider.pattern.int_const(0xDEAD_BEEF_CAFE_BABE)
    assert function.find_all(impossible) == []


def test_fingerprint_returns_machine_addresses():
    """Asm-fingerprints are always on, so every matched value node carries
    at least one plausible machine address."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.lift.load_elf(str(elf))
    addr = s.symbol("add").address
    _cfg, function, _unresolved = s.analyze("add")
    matches = function.find_all(
        strider.pattern.int_add(strider.pattern.anything(), strider.pattern.anything())
    )
    assert matches, "test fixture has no Add nodes; investigate the fixture"
    fp = function.node(matches[0].root).asm_fingerprint()
    assert isinstance(fp, list)
    assert all(isinstance(a, int) for a in fp)
    # Empty fingerprints are legal only on structural kinds (phis, Entry,
    # InitialMemory), never on an Add lifted from a real instruction.
    assert len(fp) >= 1, f"empty fingerprint on Add match {matches[0].root}"
    for a in fp:
        # No tight bound on function size; a 1 MB window around the entry
        # is enough to catch a fingerprint pointing somewhere absurd.
        assert addr - (1 << 20) <= a <= addr + (1 << 20), (
            f"fingerprint address {a:#x} is implausibly far from "
            f"function entry {addr:#x}"
        )


def test_fingerprint_matches_via_node_and_match_forwarder():
    """`Match.asm_fingerprint(key)` is a thin forwarder onto the captured
    node's own `asm_fingerprint()`; the two must agree."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.lift.load_elf(str(elf))
    _cfg, function, _unresolved = s.analyze("add")
    c = strider.pattern.Capture()
    matches = function.find_all(
        strider.pattern.int_add(strider.pattern.anything(), strider.pattern.anything()).capture(c)
    )
    assert matches
    fp_via_node = function.node(matches[0].root).asm_fingerprint()
    fp_via_match = matches[0].asm_fingerprint(c)
    assert fp_via_node == fp_via_match


def test_fingerprint_rejects_bad_type():
    elf = fixture_path("x64", "arithmetic")
    s = strider.lift.load_elf(str(elf))
    _cfg, function, _unresolved = s.analyze("add")
    with pytest.raises(TypeError):
        function.node(1.5)  # type: ignore[arg-type]


def test_elf_lifter_repr():
    elf = fixture_path("x64", "arithmetic")
    s = strider.lift.load_elf(str(elf))
    r = repr(s)
    assert "x86_64" in r
    assert "x86_64_systemv" in r


def test_analyze_many_reuse():
    """One handle, many analyze calls: the analyse-many workflow. Sleigh
    context state is per-analyze, so reuse must not corrupt later lifts."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.lift.load_elf(str(elf))
    _cfg, function1, _unresolved1 = s.analyze("add")
    _cfg, function2, _unresolved2 = s.analyze("add")
    assert function1.node_count() > 0
    assert function2.node_count() > 0
    for sym in list(s.functions()):
        if sym.name == "add":
            continue
        try:
            _cfg, other_function, _unresolved = s.analyze(sym.name)
        except strider.StriderError:
            # Data symbols / non-code names may not lift.
            continue
        assert other_function.node_count() >= 0


def test_analyze_function_max_size_clips_mid_function():
    """A bound that cuts mid-function must be a function-boundary error, not
    silently reinterpreted as a tail call."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.lift.load_elf(str(elf))
    _cfg, function, _unresolved = s.analyze("add")
    assert function.node_count() > 0
    with pytest.raises(strider.StriderError) as exc:
        s.analyze("add", opts=strider.lift.LifterOptions(cfg=strider.cfg.CfgOptions(function_max_size=4)))
    assert "function-boundary error" in str(exc.value)


def test_analyze_explicit_none_lifts_whole_function():
    """Explicit `function_max_size=None` means unbounded, not "fall back to
    a default". An address target is used so the symbol-size default cannot
    interfere."""
    elf = fixture_path("x64", "arithmetic")
    s = strider.lift.load_elf(str(elf))
    addr = s.symbol("add").address
    with pytest.raises(strider.StriderError) as exc:
        s.analyze(addr, opts=strider.lift.LifterOptions(cfg=strider.cfg.CfgOptions(function_max_size=4)))
    assert "function-boundary error" in str(exc.value)
    _cfg, full_function, _unresolved = s.analyze(
        addr, opts=strider.lift.LifterOptions(cfg=strider.cfg.CfgOptions(function_max_size=None))
    )
    assert full_function.node_count() > 0


def test_analyze_allow_code_before_start_addr():
    elf = fixture_path("x64", "arithmetic")
    s = strider.lift.load_elf(str(elf))
    _cfg, function, _unresolved = s.analyze(
        "add", opts=strider.lift.LifterOptions(cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True))
    )
    assert function.node_count() > 0


def test_standalone_strider_by_address():
    """A raw BufferReader with no symbol table still supports the full
    query surface: pattern matching on the function and `fingerprint_pcode`
    on the CFG."""
    elf = fixture_path("x64", "arithmetic")
    loaded = strider.lift.load_elf(str(elf))
    mem = loaded._elf.reader()
    arch = strider.sleigh.SleighArch.x86_64()
    addr = loaded.symbol("add").address
    s = strider.lift.lifter(arch, mem)
    cfg, fn, _unresolved = s.analyze(addr, strider.sleigh.CallingConvention.x86_64_systemv())
    assert fn.node_count() > 0
    matches = fn.find_all(
        strider.pattern.int_add(strider.pattern.anything(), strider.pattern.anything())
    )
    assert matches, "expected at least one Add node"
    pcode = cfg.fingerprint_pcode(fn.node(matches[0].root))
    assert isinstance(pcode, list)
    for addr_, text in pcode:
        assert isinstance(addr_, int)
        assert isinstance(text, str)


def test_standalone_strider_rejects_name_targets():
    """A standalone `Lifter` has no symbol table, so a name target must
    raise rather than misbehave."""
    elf = fixture_path("x64", "arithmetic")
    loaded = strider.lift.load_elf(str(elf))
    mem = loaded._elf.reader()
    s = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)
    with pytest.raises(strider.StriderError, match="ElfLifter"):
        s.analyze("add", strider.sleigh.CallingConvention.x86_64_systemv())


def test_standalone_strider_requires_an_explicit_cc():
    """`cc` is optional in the base signature only so `ElfLifter` (which
    derives one from the ELF header) stays a compatible override. A plain
    `Lifter` stores no default and must say so rather than guess."""
    elf = fixture_path("x64", "arithmetic")
    loaded = strider.lift.load_elf(str(elf))
    mem = loaded._elf.reader()
    s = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)
    with pytest.raises(strider.StriderError, match="cc"):
        s.analyze(loaded.symbol("add").address)


def test_load_x64_object_file_lifts_tzcount():
    """ET_REL objects have no PT_LOAD headers, so the loader falls back to a
    section walk. Every section claims VMA 0 pre-link, so `ElfSectionLayout`
    rebases the colliding ones apart before the regions are built.

    Regression: the loader used to build an empty memory map for `.o` files,
    so `analyze()` silently lifted an empty CFG instead of failing.
    """
    obj = FIXTURES_DIR / "x64" / "tzcount.o"
    if not obj.exists():
        pytest.skip(f"fixture missing: {obj} (run `make CASE=tzcount` in fixtures/)")
    # Relocations off: applying them stitches `tzcount` to its callees, which
    # extends the lifted body past its recorded `st_size`. Only the section
    # walk is under test here.
    p = strider.lift.load_elf(str(obj), apply_relocations=False)
    assert p.symbol("tzcount") is not None
    _cfg, function, _unresolved = p.analyze("tzcount")
    assert function.node_count() > 0, (
        "loading an ET_REL .o should yield a non-empty IR graph; an "
        "empty graph means the loader produced no readable bytes for "
        ".text, so the section-walker dispatch is not engaging for ET_REL."
    )
