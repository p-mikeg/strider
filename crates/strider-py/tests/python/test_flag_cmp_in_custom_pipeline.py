"""Regression test: a custom pipeline that includes
`FlagCmpCanonicalize` must canonicalise the same flag-cmp shapes the
default pipeline does.

The canonical bug shape is `list_empty(head)`: `head->next == &head`
compiles on x86_64 (`-O2`) to a `cmp QWORD PTR [rdi+K], rdi+K`
(mem vs reg+K), which Sleigh expands to a flag-tree the lifter
normalises to::

    Equal(Add(LOAD(rdi+K), Neg(Add(rdi, K))), 0)

`opt::FlagCmpCanonicalize` rewrites this to::

    Equal(LOAD(rdi+K), Add(rdi, K))

— the canonical shape pattern queries match on
(``int_eq(load(<base>+K), add(<base>, K))``).  ``FlagCmpCanonicalize``
was not exposed to Python, so a custom pipeline that omitted it left
the flag-tree shape in the IR and pattern queries failed silently.

This test uses the in-repo fixture `fixtures/cases/list_empty.c` —
`is_thread_group_empty(task*)` — which has the exact ``head->next ==
&head`` shape at struct offset 64 (4 bytes `pid` + 60 bytes pad =
64).  See the C file for the layout.
"""

from __future__ import annotations

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

from .conftest import fixture_path


def _build_user_pipeline_with_fcc(sl, sleigh, cc, mem):
    """A bsdfinder-style custom pipeline that bolts
    ``FlagCmpCanonicalize`` on top of the user's chosen passes.

    The `mem` arg is unused here — `LoadReadOnly()` receives its rom
    image via the orchestrator's `OptCtx` plumbing (see
    `strider.run(..., rom=mem)`); the pass instance is now a marker.
    """
    del mem
    pipe = strider.OptimizerPipeline.empty()
    pipe.add(opt.ConstantFold())
    pipe.add(opt.KnownBits())
    pipe.add(opt.FlagCmpCanonicalize())
    pipe.add(opt.IfCondInversion())
    pipe.add(opt.PhiCollapse())
    pipe.add(opt.RegionCollapse())
    pipe.add(opt.DeadBranchElimination())
    pipe.add(opt.LoadReadOnly())
    pipe.add(opt.LoadForward(sl, cc, sleigh))
    pipe.add_post(opt.FunctionArgDetect(sl, cc))
    pipe.add_post(opt.CallStackArgCollect(sl, cc))
    return pipe


def test_list_empty_pattern_matches_under_custom_pipeline_with_fcc():
    """With `FlagCmpCanonicalize` in the custom pipeline, the
    `head->next == &head` shape must be matchable as
    `int_eq(load(<base>+K), add(<base>, K))` — the same way the
    orchestrator's default-pipeline path matches it."""
    elf = fixture_path("x64", "list_empty")
    loaded = strider.load_elf(str(elf))
    mem = loaded.memory_map()
    sleigh = strider.SleighArch.x86_64()
    cc = strider.CallingConvention.x86_64_systemv()
    sl = strider.Sleigh(sleigh, mem)

    entry, max_size = loaded.symbol_addr_and_size("is_thread_group_empty")
    pipe = _build_user_pipeline_with_fcc(sl, sleigh, cc, mem)

    res = strider.run(
        arch=sleigh,
        cc=cc,
        mem=mem,
        rom=mem,
        entry=entry,
        function_max_size=max_size,
        pipeline=pipe,
    )

    o = Capture()
    pat = int_eq(
        load(addr=add(function_arg(0), any_int_const(o))),
        add(function_arg(0), any_int_const(o)),
    )
    hits = list(res.function.find_all(pat, ignore_casts=True))
    offsets = sorted({h.uint(o) for h in hits if h.uint(o) is not None})
    # `offsetof(struct task, head)` in the C fixture: int (4) + char[60]
    # (60) = 64.  GCC at -O2 emits exactly `cmp [rdi+0x40], rdi+0x40`.
    assert 64 in offsets, (
        f"expected list_empty test at offset 64 to canonicalise; "
        f"got hits at {offsets}"
    )
