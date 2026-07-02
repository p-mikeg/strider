"""Regression test: the default pipeline's `FlagCmpCanonicalize` must
canonicalise `head->next == &head`-style flag-cmp shapes.

The canonical bug shape is `list_empty(head)`: `head->next == &head`
compiles on x86_64 (`-O2`) to a `cmp QWORD PTR [rdi+K], rdi+K`
(mem vs reg+K), which Sleigh expands to a flag-tree the lifter
normalises to::

    Equal(Add(LOAD(rdi+K), Neg(Add(rdi, K))), 0)

`opt::FlagCmpCanonicalize` rewrites this to::

    Equal(LOAD(rdi+K), Add(rdi, K))

— the canonical shape pattern queries match on
(``int_eq(load(<base>+K), add(<base>, K))``).  ``FlagCmpCanonicalize``
must run in the pipeline `Lifter.analyze` drives, or the flag-tree
shape stays in the IR and pattern queries fail silently.  (There used
to be a custom-pipeline entry point that could omit it entirely; the
single-`Lifter` collapse removed that knob — `analyze` always runs the
canonical default pipeline, which includes `FlagCmpCanonicalize`.)

This test uses the in-repo fixture `fixtures/cases/list_empty.c` —
`is_thread_group_empty(task*)` — which has the exact ``head->next ==
&head`` shape at struct offset 64 (4 bytes `pid` + 60 bytes pad =
64).  See the C file for the layout.
"""

from __future__ import annotations

import strider
from strider.pattern import (
    Capture,
    add,
    any_int_const,
    function_arg,
    int_eq,
    load,
)

from .conftest import fixture_path


def test_list_empty_pattern_matches_under_custom_pipeline_with_fcc():
    """`FlagCmpCanonicalize` (part of the canonical default pipeline
    `Lifter.analyze` always runs) must canonicalise the
    `head->next == &head` shape so it is matchable as
    `int_eq(load(<base>+K), add(<base>, K))`."""
    elf = fixture_path("x64", "list_empty")
    loaded = strider.load_elf(str(elf))
    mem = loaded.reader()
    sleigh = strider.SleighArch.x86_64()
    cc = strider.CallingConvention.x86_64_systemv()

    entry, max_size = loaded._elf.symbol_addr_and_size("is_thread_group_empty")

    lift = strider.lifter(sleigh, mem, rom=mem)
    function, _unresolved = lift.analyze(
        entry, cc, opts=strider.LifterOptions(cfg=strider.CfgOptions(function_max_size=max_size))
    )

    o = Capture()
    pat = int_eq(
        load(addr=add(function_arg(0), any_int_const(o))),
        add(function_arg(0), any_int_const(o)),
    )
    hits = list(function.find_all(pat, ignore_casts=True))
    offsets = sorted({h.uint(o) for h in hits if h.uint(o) is not None})
    # `offsetof(struct task, head)` in the C fixture: int (4) + char[60]
    # (60) = 64.  GCC at -O2 emits exactly `cmp [rdi+0x40], rdi+0x40`.
    assert 64 in offsets, (
        f"expected list_empty test at offset 64 to canonicalise; "
        f"got hits at {offsets}"
    )
