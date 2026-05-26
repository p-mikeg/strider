"""Regression test for the bounded-lift TailCall crash.

Before the fix, calling `strider.run` with `function_max_size=`
crashed with `StriderError: invalid region index NodeIndex(N)` on
any function whose lifted region graph contained a direct `jmp`
whose target lay outside `[start, start+size)` — the cfg builder
emitted `RegionTerminator::TailCall { target }` for that region,
and the IR lift had no handler for that terminator kind.

The minimal Rust reproduction is in
`crates/strider/tests/bounded_lift_tail_call.rs` (synthetic
`mov eax, 5; jmp out-of-bounds`).  This Python test exercises the
exact scenario the user originally reported: bounded lift of
`vmspace_exitfree` on a real FreeBSD amd64 13.0 kernel, where the
function's region graph reaches a `jmp` past the function bound
via a chain of intra-binary control flow.

The test is opt-in: skipped cleanly when the user's kernel
fixture isn't present, since this binary is too large to ship
inside the repo.
"""

from __future__ import annotations

import pathlib

import pytest

import strider


KERNEL_PATH = pathlib.Path(__file__).resolve().parents[4] / ".." / "bsdfinder" / "kernels" / "amd64" / "13.0" / "kernel"


def _require_kernel() -> pathlib.Path:
    if not KERNEL_PATH.exists():
        pytest.skip(f"kernel fixture missing: {KERNEL_PATH}")
    return KERNEL_PATH


def test_bounded_lift_vmspace_exitfree_amd64_13():
    """Bounded lift of `vmspace_exitfree` on amd64 13.0 must not
    crash with `invalid region index`.  The function body itself is
    small (size=133) and ends with `jmp uma_zfree_arg`; transitively
    reachable code reaches a `jmp` past `[start, start+133)`, which
    used to crash with the NodeIndex error before TailCall lifting
    was wired in.

    Synthetic-bytes pins for the dounmount fall-through and
    vmspace_exitfree backward-jmp shapes live in
    `crates/strider/tests/bounded_lift_tail_call.rs` — that is the
    canonical regression coverage.  This Python test stays only as
    real-binary smoke confirmation for callers that already have the
    kernel fixture; it skips cleanly when missing."""
    kernel = _require_kernel()
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(kernel), apply_relocations=True)
    addr, size = mem.function_max_size("vmspace_exitfree")
    assert size is not None, "vmspace_exitfree must have a recorded st_size"
    arch = strider.SleighArch.x86_64()
    cc = strider.CallingConvention.x86_64_systemv()
    result = strider.run(
        arch=arch, cc=cc, mem=mem, rom=mem, entry=addr,
        function_max_size=size,
        allow_code_before_start_addr=True,
    )
    assert result.function.node_count() > 0
