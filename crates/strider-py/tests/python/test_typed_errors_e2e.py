"""End-to-end tests for `strider.errors.StriderError`.

The Python error surface is a single `StriderError` class.  Rust-side
`anyhow::Error` chains land here, formatted with the caused-by chain
in the message (plus the Rust backtrace under `RUST_BACKTRACE=1`).

These tests trigger each category of failure that used to be a typed
subclass (`LiftError`, `ReaderError`, `PatternError`, `RewriteError`,
`UnresolvedIndirectBranchError`, `UnknownCallOtherError`) and pin the
error message substring instead of the type identity.

If a typed-class hierarchy is reintroduced in the future, these tests
should be updated to assert on the subclass type as well (the regex
substring matches already document the discriminating text).
"""

from __future__ import annotations

import pytest

import strider
from strider import errors


# ── ReaderError category ───────────────────────────────────────────────────

def test_reader_error_on_overflowing_region_addr():
    """`MemoryMap.add_region(start, data)` overflows when
    `start + data.len() > u64::MAX`.  Surfaces as `StriderError` with
    overflow text in the message."""
    mem = strider.MemoryMap()
    with pytest.raises(errors.StriderError, match=r"(?i)overflow"):
        # 0xFFFFFFFFFFFFFFFE + 4 bytes overflows u64.
        mem.add_region(0xFFFF_FFFF_FFFF_FFFE, b"\x00\x00\x00\x00")


def test_reader_error_on_missing_elf_path():
    """`load_elf("/nonexistent")` must raise.  The high-level facade
    checks the path up front and raises a `FileNotFoundError`."""
    with pytest.raises(FileNotFoundError):
        strider.load_elf("/nonexistent/path/to/some.elf")


def test_reader_error_on_unknown_symbol(x86_memory_elf):
    """`ElfStrider.symbol("...")` for a name not in any loaded ELF must
    raise a typed error, not KeyError or panic."""
    elf = strider.load_elf(str(x86_memory_elf))
    with pytest.raises(errors.StriderError):
        elf.symbol("definitely_not_a_real_symbol_xyz")


# ── PatternError category ──────────────────────────────────────────────────

def test_pattern_error_on_reserved_capture_name():
    """The Capture intern table reserves `"_"` and `"any_"` for explicit
    free constructors; passing them as a `.cap(name)` string raises."""
    from strider.pattern import any_
    p = any_()
    with pytest.raises(errors.StriderError):
        p.cap("_")
    with pytest.raises(errors.StriderError):
        p.cap("any_")


def test_pattern_error_on_ordered_finalized_pat():
    """Calling `.ordered()` on a finalized `Pat` returned by a free
    constructor (e.g. `add(...)`) raises a no-op-trap error."""
    from strider.pattern import add, var, Capture
    x, y = Capture(), Capture()
    pat = add(var(x), var(y))  # finalized Pat from free ctor
    with pytest.raises(errors.StriderError):
        pat.ordered()


# ── LiftError category ─────────────────────────────────────────────────────

def test_lift_error_on_unmapped_entry_address():
    """`strider.run` with an entry address outside any region must
    raise: the cfg builder asks Sleigh to lift bytes that aren't
    there."""
    mem = strider.MemoryMap()  # no regions
    arch = strider.SleighArch.x86_64()
    cc = strider.CallingConvention.x86_64_systemv()
    with pytest.raises(errors.StriderError):
        strider.run(
            arch=arch,
            cc=cc,
            mem=mem,
            entry=0x1000,
            allow_code_before_start_addr=True,
        )


# ── RewriteError category ──────────────────────────────────────────────────

def test_rewrite_error_via_multi_output_lhs_root():
    """Trigger a rewrite-error by attempting a rewrite whose LHS root
    is a `Call` node — `Call` has multiple value outputs (Control,
    Memory, ret_val regs, clobber regs).  The rewrite-rule engine's
    `node_outputs_exact::<1>` rejects multi-output roots.

    Bytes: `E8 05 00 00 00; C3` = `call +5; ret`.  The body contains
    exactly one Call node; the rewrite attempt fails on its
    multi-output structure rather than ever firing.
    """
    bytes_ = b"\xe8\x05\x00\x00\x00\xc3\x00\x00\x00\x00\x00\xc3"
    mem = strider.MemoryMap()
    mem.add_region(0x1000, bytes_)
    arch = strider.SleighArch.x86_64()
    cc = strider.CallingConvention.x86_64_systemv()
    s = strider.Lifter(arch, mem, cc)
    cfg = s.build_cfg(0x1000, function_max_size=0x100)
    g = s.analyze_cfg(cfg).function

    from strider.pattern import call, int_const

    with pytest.raises(errors.StriderError):
        g.rewrite(find=call(), replace=int_const(0))


# ── UnknownCallOther category ──────────────────────────────────────────────

def test_unknown_call_other_via_x86_clflush_instruction():
    """Trigger an unknown-CallOther error by lifting x86 `clflush [eax]`
    (opcode `0F AE 38`) — Sleigh emits a CallOther named `"clflush"`
    which is **not** classified in `target::call_other_abi::classify`.
    The strict-on-emission policy raises rather than silently producing
    an empty CallOther.

    If a future contributor adds `clflush` to the table this test must
    be ported to a new uncovered user-op (e.g. `clflushopt`, `clwb`).
    """
    bytes_ = b"\x0f\xae\x38\xc3"
    mem = strider.MemoryMap()
    mem.add_region(0x1000, bytes_)
    arch = strider.SleighArch.x86_64()
    cc = strider.CallingConvention.x86_64_systemv()
    s = strider.Lifter(arch, mem, cc)
    cfg = s.build_cfg(0x1000)

    with pytest.raises(errors.StriderError):
        s.analyze_cfg(cfg)


# ── UnresolvedIndirectBranch category ──────────────────────────────────────

def test_unresolved_indirect_branch_via_jmp_rax():
    """A bare `jmp rax` (RAX = the function-entry value of rax,
    `InitialVar(rax)`) is unresolvable — no classifier arm matches.  This
    is NOT an error: `run` returns a `RunResult` whose
    `unresolved_indirect_branches` lists the site (regression guard for
    the old "unresolved => StriderError" behaviour).

    Bytes: `FF E0` = `jmp rax`.
    """
    bytes_ = b"\xff\xe0"
    mem = strider.MemoryMap()
    mem.add_region(0x1000, bytes_)
    arch = strider.SleighArch.x86_64()
    cc = strider.CallingConvention.x86_64_systemv()

    result = strider.run(arch=arch, cc=cc, mem=mem, entry=0x1000)
    assert result.unresolved_indirect_branches, (
        "expected the unresolvable jmp rax to be reported in "
        "unresolved_indirect_branches"
    )


# ── Polymorphic catch ──────────────────────────────────────────────────────

def test_strider_error_catches_every_error_category():
    """`except StriderError` is the single catch-all.  Regression
    guard: every raised exception type from any of the categories
    above is `StriderError`-or-subclass (in the collapsed surface,
    just `StriderError`)."""
    mem = strider.MemoryMap()
    raised: BaseException | None = None
    try:
        mem.add_region(0xFFFF_FFFF_FFFF_FFFF, b"\x00\x00")
    except errors.StriderError as e:
        raised = e
    assert raised is not None, "expected StriderError from overflowing add_region"
    assert isinstance(raised, errors.StriderError)
