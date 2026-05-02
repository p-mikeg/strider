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
    was wired in."""
    kernel = _require_kernel()
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(kernel), apply_relocations=True)
    addr, size = mem.function_max_size("vmspace_exitfree")
    assert size is not None, "vmspace_exitfree must have a recorded st_size"
    arch = strider.SleighArch.x86_64()
    cc = strider.CallingConvention.x86_64_systemv_abi()
    result = strider.run(
        arch=arch, cc=cc, mem=mem, rom=mem, entry=addr,
        function_max_size=size,
        allow_code_before_start_addr=True,
    )
    assert result.graph.node_count() > 0


def test_bounded_lift_dounmount_amd64_13_does_not_crash_on_oob_fall_through():
    """Bounded lift of `dounmount` on amd64 13.0 must not crash with
    ``invalid tail call at opcode PcodeInsnAddr {...}``.

    `dounmount`'s body ends with a ``call panic`` followed by a NOP
    pad and then an unrelated function (`vfs_op_enter`).  Without
    bound enforcement on fall-through advancement, the cfg builder
    walked past the bound, decoded `vfs_op_enter`'s body, and hit a
    multi-pcode-op instruction (`lock cmpxchg`) whose intra-insn
    CONST branch produced a `PcodeInsnAddr` with non-zero
    `insn_index` AND an OOB `machine_addr` — the validator rejected
    it.

    Fix: `RegionBuilder::build` now terminates the region with a
    `TailCall { target: <oob_addr> }` whenever fall-through crosses
    `start + fn_max_size`.
    """
    kernel = _require_kernel()
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(kernel), apply_relocations=True)
    addr, size = mem.function_max_size("dounmount")
    assert size is not None, "dounmount must have a recorded st_size"
    arch = strider.SleighArch.x86_64()
    cc = strider.CallingConvention.x86_64_systemv_abi()
    result = strider.run(
        arch=arch, cc=cc, mem=mem, rom=mem, entry=addr,
        function_max_size=size,
        allow_code_before_start_addr=True,
    )
    assert result.graph.node_count() > 0


@pytest.mark.parametrize("version", ["11.0", "11.3", "12.0", "12.3"])
def test_bounded_lift_vmspace_exitfree_amd64_le_12x_truncates_at_symbol_boundary(version: str):
    """On amd64 ≤ 12.x, `vmspace_exitfree` is 31 bytes and ends with
    `jmp <vmspace_free>` (a backward jump to a different function).
    Before the cfg-bound fix, `allow_code_before_start_addr=True`
    combined with `function_max_size` let the lifter follow that
    backward `jmp` into `vmspace_free`'s body, ballooning the lifted
    graph to ~38K nodes (one user-visible symptom of the original
    bug report).  After the fix the function lifts to a tight graph
    that fits the 31-byte source extent.
    """
    path = pathlib.Path(__file__).resolve().parents[4] / ".." / "bsdfinder" / "kernels" / "amd64" / version / "kernel"
    if not path.exists():
        pytest.skip(f"kernel fixture missing: {path}")
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(path), apply_relocations=True)
    addr, size = mem.function_max_size("vmspace_exitfree")
    assert size == 31, f"expected vmspace_exitfree size=31 on amd64 {version}, got {size}"
    arch = strider.SleighArch.x86_64()
    cc = strider.CallingConvention.x86_64_systemv_abi()
    result = strider.run(
        arch=arch, cc=cc, mem=mem, rom=mem, entry=addr,
        function_max_size=size,
        allow_code_before_start_addr=True,
    )
    # Tight upper bound — pre-fix this was 16-45K nodes.  A 31-byte
    # function with a tail call has at most a few hundred nodes after
    # optimisation.
    assert result.graph.node_count() < 1000, (
        f"vmspace_exitfree on amd64 {version} lifted to {result.graph.node_count()} nodes; "
        "bound check should have truncated at the 31-byte symbol boundary"
    )
