"""`FlagCmpCanonicalize` must run in the pipeline
`Lifter.analyze` drives, or `head->next == &head` shapes stay as
flag-trees and pattern queries fail silently.

`list_empty(head)` compiles on x86_64 -O2 to `cmp QWORD PTR [rdi+K],
rdi+K`, which Sleigh expands to a flag-tree the lifter normalises to
`Equal(Add(LOAD(rdi+K), Neg(Add(rdi, K))), 0)`.  The pass rewrites that
to the queryable `Equal(LOAD(rdi+K), Add(rdi, K))`.

Fixture: `fixtures/cases/list_empty.c::is_thread_group_empty`, whose
`head` sits at struct offset 64 (4-byte `pid` + 60 bytes pad).
"""

from __future__ import annotations

import strider
from strider.pattern import (
    Capture,
    int_add,
    int_const,
    function_arg,
    int_eq,
    load,
)

from .conftest import fixture_path


def test_list_empty_pattern_matches_under_custom_pipeline_with_fcc():
    """The `head->next == &head` shape must be matchable as
    `int_eq(load(<base>+K), int_add(<base>, K))` after `analyze`."""
    elf = fixture_path("x64", "list_empty")
    loaded = strider.lift.load_elf(str(elf))
    mem = loaded.reader()
    sleigh = strider.sleigh.SleighArch.x86_64()
    cc = strider.sleigh.CallingConvention.x86_64_systemv()

    sym = loaded.symbol("is_thread_group_empty")

    lift = strider.lift.lifter(sleigh, mem, rom=mem)
    _cfg, function, _unresolved = lift.analyze(
        sym.address,
        cc,
        opts=strider.lift.LifterOptions(
            cfg=strider.cfg.CfgOptions(function_max_size=sym.size)
        ),
    )

    o = Capture()
    pat = int_eq(
        load(addr=int_add(function_arg(0), int_const(o))),
        int_add(function_arg(0), int_const(o)),
    )
    hits = list(function.find_all(pat, ignore_casts=True))
    offsets = sorted({h.uint(o) for h in hits if h.uint(o) is not None})
    # offsetof(struct task, head) = 4 + 60 = 64; GCC -O2 emits exactly
    # `cmp [rdi+0x40], rdi+0x40`.
    assert 64 in offsets, (
        f"expected list_empty test at offset 64 to canonicalise; "
        f"got hits at {offsets}"
    )
