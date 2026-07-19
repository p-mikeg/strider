"""End-to-end tests for `strider.StriderError`, the single Python error class.

Each test triggers a failure category that used to be its own typed subclass
(`LiftError`, `ReaderError`, `PatternError`, `RewriteError`,
`UnresolvedIndirectBranchError`, `UnknownCallOtherError`) and pins the message
substring instead of the type identity.  If a typed hierarchy ever comes back,
those substrings already document the discriminating text.
"""

from __future__ import annotations

import pytest

import strider


def test_reader_error_on_overflowing_region_addr():
    """`BufferReader` rejects a base_addr + len that overflows u64."""
    with pytest.raises(strider.StriderError, match=r"(?i)overflow"):
        # 0xFFFFFFFFFFFFFFFE + 4 bytes overflows u64.
        strider.reader.BufferReader(0xFFFF_FFFF_FFFF_FFFE, b"\x00\x00\x00\x00")


def test_reader_error_on_missing_elf_path():
    """A missing path is checked up front, so it raises FileNotFoundError
    rather than StriderError."""
    with pytest.raises(FileNotFoundError):
        strider.lift.load_elf("/nonexistent/path/to/some.elf")


def test_reader_error_on_unknown_symbol(x86_memory_elf):
    """An unknown symbol name raises StriderError, not KeyError or a panic."""
    elf = strider.lift.load_elf(str(x86_memory_elf))
    with pytest.raises(strider.StriderError):
        elf.symbol("definitely_not_a_real_symbol_xyz")


def test_pattern_error_on_reserved_capture_name():
    """`"_"` and `"any_"` are reserved for the free constructors, so they
    are rejected as `.cap(name)` strings."""
    from strider.pattern import anything
    p = anything()
    with pytest.raises(strider.StriderError):
        p.cap("_")
    with pytest.raises(strider.StriderError):
        p.cap("any_")


def test_pattern_error_on_ordered_finalized_pat():
    """`.ordered()` on an already-finalized `Pat` would silently no-op, so
    it raises instead."""
    from strider.pattern import add, var, Capture
    x, y = Capture(), Capture()
    pat = add(var(x), var(y))
    with pytest.raises(strider.StriderError):
        pat.ordered()


def test_lift_error_on_unmapped_entry_address():
    """An entry outside every mapped region raises: the CFG builder asks
    Sleigh to lift bytes that aren't there."""
    mem = strider.reader.BufferReader(0x9000, b"\x00")  # entry 0x1000 stays unmapped
    arch = strider.sleigh.SleighArch.x86_64()
    cc = strider.sleigh.CallingConvention.x86_64_systemv()
    with pytest.raises(strider.StriderError):
        strider.lift.lifter(arch, mem).analyze(
            0x1000, cc, opts=strider.lift.LifterOptions(cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True))
        )


def test_rewrite_error_via_multi_output_lhs_root():
    """A rewrite whose find-root is a `Call` is rejected: rewrite roots must
    have exactly one value output, and `Call` has several (control, memory,
    ret-val and clobber registers).  The rewrite fails on shape, never fires.

    Bytes: `E8 05 00 00 00; C3` = `call +5; ret`, giving exactly one Call.
    """
    bytes_ = b"\xe8\x05\x00\x00\x00\xc3\x00\x00\x00\x00\x00\xc3"
    mem = strider.reader.BufferReader(0x1000, bytes_)
    arch = strider.sleigh.SleighArch.x86_64()
    cc = strider.sleigh.CallingConvention.x86_64_systemv()
    s = strider.lift.lifter(arch, mem)
    _cfg, g, _unresolved = s.analyze(
        0x1000, cc, opts=strider.lift.LifterOptions(cfg=strider.cfg.CfgOptions(function_max_size=0x100))
    )

    from strider.pattern import call, int_const

    with pytest.raises(strider.StriderError):
        g.rewrite(find=call(), replace=int_const(0))


def test_unknown_call_other_via_x86_clflushopt_instruction():
    """An unclassified CallOther raises rather than silently lifting to an
    empty CallOther.  `clflushopt [eax]` (`66 0F AE 38`) is the current
    uncovered user-op; plain `clflush` used to serve here until it was
    classified as a cache-maintenance memory clobber.  If anyone classifies
    `clflushopt`, port this to another uncovered op (e.g. `clwb`).
    """
    bytes_ = b"\x66\x0f\xae\x38\xc3"
    mem = strider.reader.BufferReader(0x1000, bytes_)
    arch = strider.sleigh.SleighArch.x86_64()
    cc = strider.sleigh.CallingConvention.x86_64_systemv()
    s = strider.lift.lifter(arch, mem)

    with pytest.raises(strider.StriderError):
        s.analyze(0x1000, cc)


def test_unresolved_indirect_branch_via_jmp_rax():
    """Regression: an unresolvable indirect branch used to raise StriderError.
    It is now reported through `analyze`'s third return value.

    `FF E0` = `jmp rax`, where rax is the function-entry value, so no
    classifier arm can resolve it.
    """
    bytes_ = b"\xff\xe0"
    mem = strider.reader.BufferReader(0x1000, bytes_)
    arch = strider.sleigh.SleighArch.x86_64()
    cc = strider.sleigh.CallingConvention.x86_64_systemv()

    _cfg, _function, unresolved = strider.lift.lifter(arch, mem).analyze(0x1000, cc)
    assert unresolved, (
        "expected the unresolvable jmp rax to be reported in "
        "unresolved_indirect_branches"
    )


def test_strider_error_catches_every_error_category():
    """`except StriderError` stays the single catch-all: every category above
    raises StriderError or a subclass of it."""
    raised: BaseException | None = None
    try:
        strider.reader.BufferReader(0xFFFF_FFFF_FFFF_FFFF, b"\x00\x00")
    except strider.StriderError as e:
        raised = e
    assert raised is not None, "expected StriderError from overflowing BufferReader"
    assert isinstance(raised, strider.StriderError)
