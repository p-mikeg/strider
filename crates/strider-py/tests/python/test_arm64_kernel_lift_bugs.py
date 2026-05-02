"""Regression tests for two distinct bugs surfaced by lifting real
arm64 FreeBSD kernels.

The kernels are large and not shipped with the repo, so the tests skip
cleanly when ``../bsdfinder/kernels/arm64/{11.1,11.2,11.3,11.4}/kernel``
is missing.

1. ``vmspace_exitfree`` lifted with ``function_max_size`` used to fail
   inside the optimizer with ``StriderError: node nodeNNN has 0 inputs,
   expected 2``.  Root cause: ``DeadBranchElimination`` detached the
   ``If`` it was eliminating even when the dead-branch subgraph had
   data outputs flowing into a *live* node (e.g. the join's ``MemPhi``
   referenced a dead ``Call``'s ``mem_out``).  The validator's
   reachability walk re-reached the now-zero-input ``If`` through
   backward-data from those live consumers and Layer A complained.
   The fix lives in ``crates/opt/src/dead_branch/mod.rs``: DBE now
   forward-walks the dead subgraph and skips the detach when any of
   its data outputs escape to live code.

2. ``vmspace_exit`` lifted with ``function_max_size`` previously
   raised an unresolved-indirect-branch error.  The root cause turned
   out to be the cfg builder following backward ``b`` instructions
   into adjacent functions when ``allow_code_before_start_addr=True``
   was combined with ``function_max_size``: the lifter would walk
   into a *different* function whose body contained a genuinely
   unresolvable indirect branch.  Once the cfg builder was taught
   that ``function_max_size`` defines the function's exact extent
   regardless of the legacy reach-back flag, the lift completes
   cleanly.  The typed ``UnresolvedIndirectBranchError`` machinery
   is still pinned here as a static type-hierarchy check.
"""

from __future__ import annotations

import pathlib

import pytest

import strider
from strider.errors import StriderError, UnresolvedIndirectBranchError  # noqa: F401 — re-exported for typed catches


KERNELS_ROOT = (
    pathlib.Path(__file__).resolve().parents[4]
    / ".."
    / "bsdfinder"
    / "kernels"
    / "arm64"
)


def _kernel_path(version: str) -> pathlib.Path:
    p = KERNELS_ROOT / version / "kernel"
    if not p.exists():
        pytest.skip(f"kernel fixture missing: {p}")
    return p


def _bounded_lift(kernel: pathlib.Path, symbol: str):
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(kernel), apply_relocations=True)
    addr, size = mem.function_max_size(symbol)
    if size is None:
        pytest.fail(f"{symbol} has no recorded st_size in {kernel}")
    arch = strider.SleighArch.aarch64()
    cc = strider.CallingConvention.aarch64_aapcs64()
    return strider.run(
        arch=arch,
        cc=cc,
        mem=mem,
        rom=mem,
        entry=addr,
        function_max_size=size,
        allow_code_before_start_addr=True,
    )


# ── Bug 1: DBE + non-CS dead consumer ─────────────────────────────────────────


@pytest.mark.parametrize("version", ["11.1", "11.2", "11.3", "11.4"])
def test_vmspace_exitfree_bounded_lift_succeeds(version: str):
    """``vmspace_exitfree`` must lift cleanly under ``function_max_size``.

    Before the DBE fix this raised ``StriderError: node nodeNNN has 0
    inputs, expected 2`` on every arm64 kernel in the affected range.
    """
    kernel = _kernel_path(version)
    result = _bounded_lift(kernel, "vmspace_exitfree")
    assert result.graph.node_count() > 0


# ── Bug 2: cfg-bound enforcement on backward jumps ────────────────────────────


@pytest.mark.parametrize("version", ["11.1", "11.2", "11.3", "11.4"])
def test_vmspace_exit_bounded_lift_does_not_walk_into_neighbour(version: str):
    """``vmspace_exit`` must lift cleanly under ``function_max_size``.

    Previously the cfg builder followed backward ``b`` instructions
    below ``start_addr`` into adjacent functions (because
    ``allow_code_before_start_addr=True`` disabled the lower-bound
    check even when ``function_max_size`` was set), and eventually
    surfaced an ``UnresolvedIndirectBranchError`` from a *different*
    function's body.  Once ``is_addr_tail_call`` was taught that
    ``fn_max_size`` defines the function's exact extent regardless
    of the reach-back flag, the lift completes."""
    kernel = _kernel_path(version)
    result = _bounded_lift(kernel, "vmspace_exit")
    assert result.graph.node_count() > 0


def test_unresolved_indirect_branch_error_is_strider_error_subclass():
    """Static check independent of any binary fixture."""
    assert issubclass(UnresolvedIndirectBranchError, StriderError)
