"""Regression tests for the AArch64 zero-pcode-op gap bugs.

AArch64 instructions like ``nop``, ``paciasp``, ``autiasp``, and
several hint-class encodings lift to **zero** pcode ops on the Sleigh
spec strider uses.  Two distinct cfg-builder failures were caused by
this:

1. **"region at PcodeInsnAddr ... has no instructions"** — the cfg
   builder's outer loop walked across one or more zero-pcode-op
   machine instructions before reaching an already-explored
   region's start.  The fall-through path tried to finalise the
   current builder with empty ``insns`` and ``add_region`` rejected
   it.  Fix: when ``self.insns`` is empty at fall-through, hot-wire
   the parent edge straight into the existing region instead.

2. **"split address ... not found in region's instruction list"** —
   a branch target landed at the address of a zero-pcode-op
   instruction that wasn't recorded in the region's ``insns``, so
   ``contains_addr``'s lexicographic range test said yes but the
   exact-match ``position`` lookup said no.  Fix: round down to the
   largest insn whose address is ≤ the requested split address.

The kernels live under ``../bsdfinder/kernels/linux``.  Each test
falls back to a skip when the kernel image is absent.
"""

from __future__ import annotations

import pathlib

import pytest

import strider


def _kernel_path(arch: str, version: str) -> pathlib.Path:
    # Search upward for a sibling ``bsdfinder`` directory.  Works both
    # from the main checkout (where the workspace root sits next to
    # ``../bsdfinder``) and from a git worktree under
    # ``.claude/worktrees/<branch>/`` (where the same sibling is
    # several levels up).
    for parent in pathlib.Path(__file__).resolve().parents:
        candidate = parent.parent / "bsdfinder" / "kernels" / "linux" / arch / version / "vmlinux"
        if candidate.exists():
            return candidate
    pytest.skip(f"bsdfinder kernel missing: linux/{arch}/{version}/vmlinux")


def _lift(kernel: pathlib.Path, symbol: str, *, arch_name: str):
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(kernel))
    mem.apply_elf_relocations(str(kernel))
    syms = mem.symbols()
    if symbol not in syms:
        pytest.skip(f"{kernel}: symbol {symbol!r} not found")
    entry = syms[symbol]
    addr, size = mem.symbol_addr_and_size(symbol)

    if arch_name == "aarch64":
        sleigh_arch = strider.SleighArch.aarch64()
        cc = strider.CallingConvention.aarch64_aapcs64()
    elif arch_name == "x86_64":
        sleigh_arch = strider.SleighArch.x86_64()
        cc = strider.CallingConvention.x86_64_systemv()
    else:
        raise AssertionError(f"unsupported arch: {arch_name}")

    per_addr = {}
    if arch_name == "x86_64":
        ap = strider.CallingConvention.x86_64_all_preserving()
        for stub in ("__fentry__", "mcount"):
            if stub in syms:
                per_addr[syms[stub]] = ap

    return strider.run(
        arch=sleigh_arch,
        cc=cc,
        mem=mem,
        rom=mem,
        entry=entry,
        function_max_size=size,
        allow_code_before_start_addr=True,
        per_address_ccs=per_addr,
    )


# ── Bug 1: empty-insns fall-through across zero-pcode-op stretches ────────────


def test_aarch64_4_19_wait_consider_task_lifts_cleanly():
    """``wait_consider_task`` on aarch64 4.19 contains a literal ``nop``
    that falls through into an explored region's start.  Pre-fix this
    tripped ``add_region``'s non-empty invariant with
    ``"region at PcodeInsnAddr ... has no instructions"``.
    """
    kernel = _kernel_path("aarch64", "4.19.0-arm64")
    result = _lift(kernel, "wait_consider_task", arch_name="aarch64")
    assert result.function.node_count() > 0


# ── Bug 3: split-into-zero-pcode-op-hole ──────────────────────────────────────


@pytest.mark.parametrize("version", ["5.10.0-arm64", "6.1.0-arm64"])
def test_aarch64_task_active_pid_ns_lifts_cleanly(version: str):
    """``task_active_pid_ns`` on aarch64 5.10 and 6.1 ends with
    ``autiasp; ret``.  ``autiasp`` lifts to zero pcode ops, so a
    preceding ``cbz`` that branches to its address forces a region
    split into a hole that no recorded insn occupies.  Pre-fix this
    raised ``"split address ... not found in region NodeIndex(N)'s
    instruction list"``.
    """
    kernel = _kernel_path("aarch64", version)
    result = _lift(kernel, "task_active_pid_ns", arch_name="aarch64")
    assert result.function.node_count() > 0
