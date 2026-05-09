"""End-to-end tests for the strider.errors typed-exception subclasses.

Each subclass of `StriderError` should be the exception type the user
catches when a specific category of failure occurs.  This module pins
that promise: trigger each kind of failure through real Python calls
(no mocks, no Rust-side helpers) and assert the raised exception is
the expected typed subclass.

Hierarchy under test (defined in `crates/strider-py/src/errors.rs`):

    StriderError
        ├── LiftError
        ├── ReaderError
        ├── PatternError
        ├── RewriteError
        ├── UnresolvedIndirectBranchError
        └── UnknownCallOtherError

Subclasses we cannot reliably trigger from a hermetic Python-only
fixture (no checked-in test ELF that exercises them) are pinned at
runtime via `pytest.mark.skip(reason=...)` so the gap is visible
rather than silently absent.
"""

from __future__ import annotations

import pytest

import strider
from strider import errors


# ── Static hierarchy sanity ────────────────────────────────────────────────

def test_typed_subclasses_inherit_strider_error():
    """Every subclass must inherit `StriderError` so `except StriderError`
    still catches them all."""
    for sub in (
        errors.LiftError,
        errors.ReaderError,
        errors.PatternError,
        errors.RewriteError,
        errors.UnresolvedIndirectBranchError,
        errors.UnknownCallOtherError,
    ):
        assert issubclass(sub, errors.StriderError), f"{sub.__name__} must inherit StriderError"


# ── ReaderError ────────────────────────────────────────────────────────────

def test_reader_error_on_overflowing_region_addr():
    """`MemoryMap.add_region(start, data)` overflows when
    `start + data.len() > u64::MAX`.  The Rust side surfaces this as
    `ReaderError` (see `MemRegion::new` in `crates/reader/src/lib.rs`).
    """
    mem = strider.MemoryMap()
    # 0xFFFFFFFFFFFFFFFE + 4 bytes overflows u64 (max + 4).
    with pytest.raises(errors.ReaderError):
        mem.add_region(0xFFFF_FFFF_FFFF_FFFE, b"\x00\x00\x00\x00")


def test_reader_error_on_missing_elf_path():
    """`add_region_from_elf("/nonexistent")` must raise `ReaderError`
    rather than fall through to `StriderError` or panic."""
    mem = strider.MemoryMap()
    with pytest.raises(errors.ReaderError):
        mem.add_region_from_elf("/nonexistent/path/to/some.elf")


def test_reader_error_on_unknown_symbol():
    """`MemoryMap.symbol("...")` for a name not in any loaded ELF must
    raise `ReaderError` (the no-ELF-loaded case still returns a
    typed error, not `KeyError` or generic `StriderError`)."""
    mem = strider.MemoryMap()
    with pytest.raises(errors.ReaderError):
        mem.symbol("definitely_not_a_real_symbol_xyz")


# ── PatternError ───────────────────────────────────────────────────────────

def test_pattern_error_on_reserved_capture_name():
    """The Capture intern table reserves `"_"` and `"any_"` for
    explicit free constructors; passing them as a `.cap(name)` string
    on any pattern builder routes through `intern_str`, which raises
    `PatternError` (see `crates/strider-py/src/pattern.rs`)."""
    from strider.pattern import any_
    p = any_()
    with pytest.raises(errors.PatternError):
        p.cap("_")
    with pytest.raises(errors.PatternError):
        p.cap("any_")


def test_pattern_error_on_ordered_finalized_pat():
    """Calling `.ordered()` on a finalized `Pat` (i.e. one returned by
    a free constructor like `add(...)`) is a no-op trap that the API
    converts into a `PatternError` so the misuse is visible."""
    from strider.pattern import add, var, Capture
    x, y = Capture(), Capture()
    pat = add(var(x), var(y))  # finalized Pat from free ctor
    with pytest.raises(errors.PatternError):
        pat.ordered()


# ── LiftError ──────────────────────────────────────────────────────────────

def test_lift_error_on_unmapped_entry_address():
    """`strider.run` with an entry address outside any region must
    raise `LiftError` (the cfg builder asks Sleigh to lift bytes
    that aren't there).  The empty-MemoryMap case is the cleanest
    minimal repro."""
    mem = strider.MemoryMap()  # no regions added
    arch = strider.SleighArch.x86_64()
    cc = strider.CallingConvention.x86_64_systemv()
    # Any address — the orchestrator will fail on the very first lift
    # because the buffered reader has no data at all.
    with pytest.raises(errors.StriderError):
        # We accept any subclass of StriderError here — LiftError is
        # the expected one, but the empty-region path may surface as
        # a generic StriderError on some Rust-side error chains.  The
        # subclass-specificity is pinned by the next test.
        strider.run(
            arch=arch,
            cc=cc,
            mem=mem,
            entry=0x1000,
            allow_code_before_start_addr=True,
        )


def test_lift_error_subclass_when_explicit_lift_fails():
    """The explicit-error pathway: when an empty-MemoryMap lift hits
    the lift-error converter, the resulting exception MUST be a
    `LiftError` (or its UnknownCallOther refinement, which is also
    a StriderError subclass).  Pin both so we know which converter
    fired."""
    mem = strider.MemoryMap()
    arch = strider.SleighArch.x86_64()
    cc = strider.CallingConvention.x86_64_systemv()
    raised: BaseException | None = None
    try:
        strider.run(
            arch=arch,
            cc=cc,
            mem=mem,
            entry=0x1000,
            allow_code_before_start_addr=True,
        )
    except errors.StriderError as e:
        raised = e
    assert raised is not None, "expected an exception from empty-MemoryMap lift"
    # Strengthened by round 10 R10-1F F-05: must be the typed `LiftError`
    # subclass (or its `UnknownCallOtherError` refinement, also a
    # LiftError descendant for our purposes).  The previous assertion
    # against the base `StriderError` would silently pass even if the
    # wrong converter fired.
    assert isinstance(raised, (errors.LiftError, errors.UnknownCallOtherError)), (
        f"expected LiftError (or UnknownCallOtherError), got "
        f"{type(raised).__name__}: {raised!r}"
    )


# ── RewriteError ───────────────────────────────────────────────────────────


def test_rewrite_error_via_multi_output_lhs_root():
    """Round 9 wave 25 (I-10): trigger `RewriteError` by attempting
    a rewrite whose LHS root is a `Call` node — `Call` has 11 value
    outputs (Control, Memory, ret_val regs, clobber regs).  The
    rewrite-rule engine's `node_outputs_exact::<1>` (which it uses
    to redirect the matched root's value output) rejects multi-output
    roots, surfacing `RewriteError` from the
    `into_rewrite_err`-wrapped `Graph::rewrite` boundary.

    Bytes: `E8 05 00 00 00; C3` = `call +5; ret`.  The function body
    contains exactly one Call node; the rewrite attempt fails on
    its multi-output structure rather than ever firing.
    """
    bytes_ = b"\xe8\x05\x00\x00\x00\xc3\x00\x00\x00\x00\x00\xc3"
    mem = strider.MemoryMap()
    mem.add_region(0x1000, bytes_)
    arch = strider.SleighArch.x86_64()
    cc = strider.CallingConvention.x86_64_systemv()
    sleigh = strider.Sleigh(arch, mem)
    s = strider.Strider(arch, sleigh, cc)
    cfg = strider.build_cfg(sleigh, 0x1000, function_max_size=0x100)
    g = s.analyze_cfg(cfg).graph

    from strider.pattern import call, int_const

    with pytest.raises(errors.RewriteError):
        g.rewrite(find=call(), replace=int_const(0))


# ── UnknownCallOtherError ──────────────────────────────────────────────────


def test_unknown_call_other_error_via_x86_clflush_instruction():
    """Round 9 wave 25 (I-10): trigger `UnknownCallOtherError` by
    lifting x86 `clflush [eax]` (opcode `0F AE 38`) — Sleigh emits a
    CallOther named `"clflush"` which is **not** classified in
    `target::call_other_abi::classify`.  The strict-on-emission
    policy raises `UnknownCallOtherError` instead of silently
    producing an empty CallOther.

    Bytes: `0F AE 38 C3` = `clflush [eax]; ret`.  The `clflush` insn
    is at the function entry; the lift fails before reaching the
    `ret`.

    Why clflush in particular: most common x86 user-ops (cpuid,
    rdtsc, swapgs, in/out, mfence/sfence/lfence, syscall, …) ARE in
    the table.  `clflush` is the canonical uncovered cache-line
    flush; if a future contributor adds it to the table the test
    must be ported to a new uncovered user-op (e.g. `clflushopt`,
    `clwb`).
    """
    bytes_ = b"\x0f\xae\x38\xc3"
    mem = strider.MemoryMap()
    mem.add_region(0x1000, bytes_)
    arch = strider.SleighArch.x86_64()
    cc = strider.CallingConvention.x86_64_systemv()
    sleigh = strider.Sleigh(arch, mem)
    s = strider.Strider(arch, sleigh, cc)
    cfg = strider.build_cfg(sleigh, 0x1000)

    with pytest.raises(errors.UnknownCallOtherError):
        s.analyze_cfg(cfg)


# ── UnresolvedIndirectBranchError ──────────────────────────────────────────


def test_unresolved_indirect_branch_error_via_jmp_rax():
    """Round 9 wave 25 (I-10): trigger `UnresolvedIndirectBranchError`
    via a bare `jmp rax` where RAX is the function-entry value of
    rax (`InitialVar(rax)`).  None of the classifier arms match:
    - `IntConst(_)` requires a folded constant — RAX is not constant.
    - `InitialVar(lr)` requires lr_vn to be set — x86_64 has no LR.
    - `ValuePhi`, jump-table, stack-array all require additional
      shape that's absent here.

    Bytes: `FF E0` = `jmp rax`.  The orchestrator's fixed-point loop
    fails to resolve the placeholder and surfaces
    `UnresolvedIndirectBranchError` after the cap.
    """
    bytes_ = b"\xff\xe0"
    mem = strider.MemoryMap()
    mem.add_region(0x1000, bytes_)
    arch = strider.SleighArch.x86_64()
    cc = strider.CallingConvention.x86_64_systemv()

    with pytest.raises(errors.UnresolvedIndirectBranchError):
        strider.run(arch=arch, cc=cc, mem=mem, entry=0x1000)


# ── Mixed: catch-all parent class still catches every subclass ─────────────

def test_strider_error_catches_reader_error_polymorphically():
    """Polymorphic catch — `except StriderError` must still pick up
    every typed subclass.  Regression guard against an accidental
    `PyException` parent on one of the create_exception! macros.
    """
    mem = strider.MemoryMap()
    raised: BaseException | None = None
    try:
        mem.add_region(0xFFFF_FFFF_FFFF_FFFF, b"\x00\x00")
    except errors.StriderError as e:
        raised = e
    assert isinstance(raised, errors.ReaderError), (
        f"polymorphic catch lost the subclass identity: got {type(raised).__name__}"
    )
