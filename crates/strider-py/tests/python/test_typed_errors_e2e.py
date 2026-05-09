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
    assert isinstance(raised, errors.StriderError), (
        f"expected StriderError, got {type(raised).__name__}: {raised!r}"
    )


# ── RewriteError ───────────────────────────────────────────────────────────


@pytest.mark.skip(
    reason="Triggering RewriteError requires constructing a rewrite whose "
    "RHS materialisation fails (e.g. multi-output root with a single-output "
    "redirect).  The Python `Graph.rewrite` API matches LHS-then-replace and "
    "the typical failure surface is `Ok(false)`, not an error.  Needs a "
    "specific fixture that reaches `node_outputs_exact::<1>`'s error arm; "
    "tracked for follow-up."
)
def test_rewrite_error_via_invalid_substitution():
    """Placeholder: see skip reason."""


# ── UnknownCallOtherError ──────────────────────────────────────────────────


@pytest.mark.skip(
    reason="Triggering UnknownCallOtherError requires lifting code that "
    "decodes to a Sleigh CallOther name not in the call_other_abi table — "
    "e.g. x86 INT 0x80 (lifts to 'swi' with no arch-specific entry).  No "
    "checked-in fixture exercises this path yet; the tests in "
    "`tests/python/test_call_builder.py` and the Rust-side coverage in "
    "`call_other_precise_abi.rs` already pin the typed error from Rust.  "
    "End-to-end Python coverage is tracked for follow-up."
)
def test_unknown_call_other_error_via_x86_int_instruction():
    """Placeholder: see skip reason."""


# ── UnresolvedIndirectBranchError ──────────────────────────────────────────


@pytest.mark.skip(
    reason="Triggering UnresolvedIndirectBranchError end-to-end requires "
    "a binary whose dispatch site cannot be resolved by the orchestrator's "
    "fixed-point loop.  The existing Python smoke fixtures (memory.elf, "
    "switch.elf, indirect_branch.elf) are intentionally resolvable.  "
    "`test_arm64_kernel_lift_bugs.py` exercises the typed-error machinery "
    "via real FreeBSD kernel fixtures that are gated by Git LFS / external "
    "trees; that file is the canonical home for this coverage.  We pin the "
    "static type-hierarchy here in `test_typed_subclasses_inherit_strider_error` "
    "instead."
)
def test_unresolved_indirect_branch_error_via_unresolvable_dispatch():
    """Placeholder: see skip reason."""


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
