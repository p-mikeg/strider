"""Regression test: a custom pipeline that includes
`FlagCmpCanonicalize` must canonicalise the same flag-cmp shapes the
default pipeline does.

The original bug surfaced on Linux x86_64's
``exit_signals(struct task_struct *tsk)``, whose body opens with a
``thread_group_empty(tsk)`` macro that lifts (after the lifter's
flag-tree expansion of ``cmp [rdi+1408], rdi``) to::

    Equal(Add(LOAD(rdi+1408), Neg(Add(rdi, 1408))), 0)

The Rust `opt::default_pipeline()` runs `FlagCmpCanonicalize` which
rewrites this to::

    Equal(LOAD(rdi+1408), Add(rdi, 1408))

— the canonical shape pattern queries match on
(``int_eq(load(<base>+K), add(<base>, K))``).  `FlagCmpCanonicalize`
was not exposed to Python, so a custom pipeline that omitted it left
the flag-tree shape in the IR and pattern queries failed silently.

This test is bound to a specific kernel build (the Debian
``4.19.0-amd64`` vmlinux that ships in `bsdfinder`'s kernel cache); the
case skips cleanly when the binary is missing.
"""

from __future__ import annotations

import pathlib

import pytest

import strider
from strider import opt
from strider.pattern import (
    Capture,
    add,
    any_int_const,
    function_arg,
    int_eq,
    load,
)


_REPO_ROOT = pathlib.Path(__file__).resolve().parents[4]
_VMLINUX = (
    _REPO_ROOT
    / ".."
    / "bsdfinder"
    / "kernels"
    / "linux"
    / "x86_64"
    / "4.19.0-amd64"
    / "vmlinux"
)


def _vmlinux() -> pathlib.Path:
    if not _VMLINUX.exists():
        pytest.skip(f"linux kernel fixture missing: {_VMLINUX}")
    return _VMLINUX.resolve()


def _build_user_pipeline_with_fcc(sl, sleigh, cc, mem):
    """The bsdfinder pipeline (`bsdfinder/offset.py::ResolveCtx._build_pipeline`)
    plus `FlagCmpCanonicalize`."""
    pipe = strider.OptimizerPipeline.empty()
    pipe.add(opt.ConstantFold())
    pipe.add(opt.KnownBits())
    pipe.add(opt.FlagCmpCanonicalize())
    pipe.add(opt.IfCondInversion())
    pipe.add(opt.RedundantPhis())
    pipe.add(opt.DeadBranchElim())
    pipe.add(opt.LoadReadOnly(mem))
    pipe.add(opt.StackStoreDetect(sl, cc))
    pipe.add(opt.StackLoadForward(sl, cc, sleigh))
    pipe.add_post(opt.FunctionArgDetect(sl, cc))
    pipe.add_post(opt.CallStackArgCollect(sl, cc))
    return pipe


def test_thread_group_empty_pattern_matches_under_custom_pipeline_with_fcc():
    """With `FlagCmpCanonicalize` in the custom pipeline, the
    `list_empty(head)` shape that ``thread_group_empty(tsk)`` lifts to
    must be matchable as `int_eq(load(<base>+K), add(<base>, K))` —
    the same way the orchestrator's default-pipeline path matches it.
    """
    vmlinux = _vmlinux()

    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(vmlinux))
    mem.apply_elf_relocations(str(vmlinux))
    syms = mem.symbols()
    if "exit_signals" not in syms or "__fentry__" not in syms:
        pytest.skip("vmlinux missing required symbols (exit_signals/__fentry__)")

    sleigh = strider.SleighArch.x86_64()
    cc = strider.CallingConvention.x86_64_systemv()
    sl = strider.Sleigh(sleigh, mem)

    per_addr = {syms["__fentry__"]: strider.CallingConvention.x86_64_all_preserving()}
    _entry, max_size = mem.function_max_size("exit_signals")
    pipe = _build_user_pipeline_with_fcc(sl, sleigh, cc, mem)

    res = strider.run(
        arch=sleigh,
        cc=cc,
        mem=mem,
        rom=mem,
        entry=syms["exit_signals"],
        allow_code_before_start_addr=True,
        per_address_ccs=per_addr,
        function_max_size=max_size,
        pipeline=pipe,
    )

    o = Capture()
    pat = int_eq(
        load(addr=add(function_arg(0), any_int_const(o))),
        add(function_arg(0), any_int_const(o)),
    )
    hits = list(res.graph.find_all(pat, ignore_casts=True))
    offsets = sorted({h.uint(o) for h in hits if h.uint(o) is not None})
    # Linux 4.19 x86_64 puts `task_struct.thread_group` at offset 1408.
    assert 1408 in offsets, (
        f"expected thread_group test at offset 1408 to canonicalise; "
        f"got hits at {offsets}"
    )
