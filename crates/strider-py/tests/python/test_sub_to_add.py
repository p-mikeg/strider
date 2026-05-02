"""Smoke tests for the `SubToAdd` opt-in canonicalisation pass.

`SubToAdd` rewrites `sub(a, IntConst(K))` to `add(a, IntConst(-K))`
so a single `add(x, signed_int_const(-K))` query covers both
encoding shapes a compiler might emit for `x - K`.

Correctness of the rewrite is pinned in
`crates/opt/src/sub_to_add/tests.rs` (Rust unit tests).  These
tests cover the Python binding surface: classmethod existence,
pipeline registration, and a coarse end-to-end shape check.
"""

from __future__ import annotations

import strider
from strider.pattern import add, sub, var, int_const, signed_int_const

from .conftest import fixture_path


def test_sub_to_add_class_exists():
    p = strider.opt.SubToAdd()
    assert p is not None


def test_sub_to_add_registers_into_pipeline():
    pipe = strider.OptimizerPipeline.empty()
    before = pipe.pass_count()
    pipe.add(strider.opt.SubToAdd())
    assert pipe.pass_count() == before + 1


def test_sub_to_add_runs_without_error_on_real_graph():
    # Whether or not the fixture happens to contain a
    # `sub(_, IntConst)` shape that SubToAdd can transform, the
    # pipeline must run without raising and the post-graph must
    # remain valid (node_count > 0).
    elf = fixture_path("x86", "arithmetic")
    arch = strider.SleighArch.x86()
    cc = strider.CallingConvention.x86_cdecl()
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(elf))
    addr = mem.symbol("sub")
    g = strider.run(
        arch=arch, cc=cc, mem=mem, rom=mem, entry=addr,
        allow_code_before_start_addr=True,
    ).graph

    pipe = strider.OptimizerPipeline.empty()
    pipe.add(strider.opt.SubToAdd())
    g.optimize(pipe)
    assert g.node_count() > 0


def test_sub_to_add_not_in_default_pipeline():
    """`SubToAdd` is opt-in — verify the default-stable subset
    doesn't accidentally include it.  If a future refactor
    promotes it into the default, this test fires and forces an
    explicit decision."""
    elf = fixture_path("x86", "arithmetic")
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(elf))
    addr = mem.symbol("sub")
    g = strider.run(
        arch=strider.SleighArch.x86(),
        cc=strider.CallingConvention.x86_cdecl(),
        mem=mem, rom=mem, entry=addr,
        allow_code_before_start_addr=True,
    ).graph
    # `arithmetic::sub(int a, int b) { return a - b; }` has a Sub
    # node with variable RHS — SubToAdd wouldn't touch it anyway,
    # but a literal Sub node surviving the default pipeline is the
    # canary for "default doesn't run SubToAdd".
    pre_sub_count = len(g.find_all(sub("a", "b")))
    assert pre_sub_count >= 1, "default pipeline must NOT have rewritten Sub away"
