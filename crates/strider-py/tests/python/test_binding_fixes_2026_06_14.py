"""Regression tests for the 2026-06-14 deep-audit binding fixes.

Each test pins one numbered finding from
`docs/superpowers/reviews/2026-06-14/strider-py.md`:

  * PY-2  — `CallOtherPat.ctrl()` / `.mem()` route to the correct
            control / memory input slot and actually match.
  * PY-3  — pure typed-builder nesting respects the native-recursion
            depth guard (raises `StriderError`, not a process abort).
  * PY-4  — a `MemReader.read` over-long return is rejected, not
            silently truncated.
  * PY-6  — a mid-run optimize failure still invalidates outstanding
            handles.
  * PY-9  — `int_ne` builder + `Load/Store.stack_offset` field mirror.
  * PY-10 — `BufferReader.read` clamps an unbounded Python `size`
            before allocating.
"""

from __future__ import annotations

import pytest

import strider
from strider.pattern import (
    call_other,
    mem_phi,
    int_ne,
    int_const,
    load,
    store,
    anything,
    Pat,
)

from .conftest import symbol_addr


# cpuid (0F A2) ; ret (C3) — lifts to a CallOther with control + memory
# inputs plus implicit reads, ideal for exercising ctrl/mem.
CPUID_BYTES = b"\x0f\xa2\xc3"
ENTRY = 0x1000


def _cpuid_function():
    mem = strider.BufferReader(ENTRY, CPUID_BYTES)
    lift = strider.lifter(strider.SleighArch.x86_64(), mem)
    _cfg, function, _unresolved = lift.analyze(
        ENTRY, strider.CallingConvention.x86_64_systemv()
    )
    return function


# ── PY-2 — CallOtherPat.ctrl()/.mem() route to the right slot ─────────


def test_call_other_ctrl_mem_methods_match():
    fn = _cpuid_function()
    baseline = len(fn.find_all(call_other()))
    assert baseline >= 1

    # `.ctrl(anything())` must route through the control-relaxed match
    # compiler at slot 0 (a CONTROL edge, not a value) and still match
    # every CallOther.  Before the fix it pushed onto the value-args slot,
    # so `output_ok` rejected the control edge and it matched nothing.
    ctrl_hits = fn.find_all(call_other().ctrl(anything()))
    assert len(ctrl_hits) == baseline

    # `.mem(...)` must route through the MEMORY-input compiler.  Before
    # the fix it pushed a memory producer onto the value-args slot, where
    # `compile_operand_match` rejected it outright with a StriderError
    # ("a control / variadic builder ... cannot be nested as a value
    # operand").  Post-fix it compiles + queries cleanly (the cpuid
    # fixture's memory predecessor is InitialMemory, so a MemPhi/Store
    # operand legitimately matches nothing — but it must NOT raise).
    assert fn.find_all(call_other().mem(mem_phi())) == []
    assert fn.find_all(call_other().mem(store())) == []


def test_call_other_ctrl_mem_compile_to_pat():
    # Builder finalisation contract still holds for the dedicated methods.
    assert isinstance(call_other().ctrl(anything()).into_pat(), Pat)
    assert isinstance(call_other().mem(mem_phi()).into_pat(), Pat)


# ── PY-3 — typed-builder nesting respects the depth guard ─────────────


def test_deeply_nested_typed_builder_raises_not_aborts():
    # Build a deeply-nested `load(addr=load(addr=...))` chain purely via
    # typed builders.  The compile recursion funnels through
    # `compile_operand_match`, so the depth guard must convert a
    # stack-overflow abort into a clean `StriderError`.
    fn = _cpuid_function()
    deep = load()
    for _ in range(5000):
        deep = load().addr(deep)
    with pytest.raises(strider.errors.StriderError):
        fn.find_all(deep)


# ── PY-4 — MemReader over-long return is rejected ─────────────────────


class _OverLongReader(strider.MemReader):
    def read(self, addr, size):  # noqa: ARG002 - mirrors the ABC sig
        # Return MORE bytes than requested — a Python bug that used to be
        # silently truncated.
        return b"\x90" * (size + 16)


def test_mem_reader_over_long_return_errors():
    reader = _OverLongReader()
    lift = strider.lifter(strider.SleighArch.x86_64(), reader)
    with pytest.raises(strider.errors.StriderError):
        lift.analyze(ENTRY, strider.CallingConvention.x86_64_systemv())


# PY-5 ("reusing one pipeline across two `run(rom=...)` calls behaves
# predictably") pinned the old `strider.run(pipeline=...)` custom-pipeline
# draining semantics.  That entry point was removed by the single-`Lifter`
# collapse (Task 2 of the strider-py API redesign); a later follow-up
# reintroduced a per-call override as `LifterOptions.pipeline` (draining
# it the same way), but there is no separate reusable-across-two-calls
# entry point.  The underlying "a drained `OptimizerPipeline` object
# raises on reuse" contract is still pinned directly against
# `Lifter.optimize` in
# `test_optimizer_pipeline.py::test_optimize_twice_on_same_pipeline_raises`.


# ── PY-6 — optimize bumps the generation, invalidating handles ────────
#
# The fix bumps the generation BEFORE running the pipeline (before the
# `?`), so an in-place mutation that errors mid-run still invalidates
# outstanding handles.  Bumping-first means even a *successful* optimize
# invalidates handles, which is the observable contract this test pins.


def test_optimize_invalidates_outstanding_handles(x86_memory_elf):
    from strider.pattern import Capture

    lift, fn = _analysis(x86_memory_elf, "array_sum")

    c = Capture()
    add_hits = fn.find_all(load().capture(c))
    if not add_hits:
        pytest.skip("no load node to hold a stale handle against")
    handle = add_hits[0]
    # Sanity: the graph-deref accessor works before optimize.
    assert handle.node(c) is not None

    pipe = strider.OptimizerPipeline.empty()
    pipe.add(strider.opt.ConstantFold())
    lift.optimize(fn, pipe)

    # The handle is now stale (generation bumped): a graph-dereferencing
    # accessor must raise rather than read the mutated arena.
    with pytest.raises(strider.errors.StriderError):
        handle.node(c)


def _analysis(elf_path, sym):
    addr = symbol_addr(elf_path, sym)
    mem = strider.load_elf(str(elf_path)).reader()
    lift = strider.lifter(strider.SleighArch.x86(), mem)
    _cfg, function, _unresolved = lift.analyze(
        addr, strider.CallingConvention.x86_cdecl(),
        opts=strider.LifterOptions(cfg=strider.CfgOptions(allow_code_before_start_addr=True)),
    )
    return lift, function


# ── PY-9 — int_ne builder + Load/Store.stack_offset mirror ────────────


def test_int_ne_builder_compiles():
    p = int_ne(int_const(1), int_const(2))
    assert isinstance(p, Pat)


def test_int_ne_finds_lowered_shape(x86_memory_elf):
    # int_ne is the lifter-canonical `Xor(IntEqual(a,b),1):I1` shape.
    # Just assert it compiles + queries without error on a real graph.
    _lift, fn = _analysis(x86_memory_elf, "array_sum")
    hits = fn.find_all(int_ne(anything(), anything()))
    assert isinstance(hits, list)


def test_load_store_stack_offset_field_compiles():
    assert isinstance(load().stack_offset(0).into_pat(), Pat)
    assert isinstance(store().stack_offset(8).into_pat(), Pat)


def test_load_stack_offset_filters(x86_memory_elf):
    _lift, fn = _analysis(x86_memory_elf, "array_sum")
    # A wildly-out-of-range SP offset should match nothing.
    hits = fn.find_all(load().stack_offset(0x7FFF_FFFF))
    assert hits == []


# ── PY-10 — BufferReader.read clamps unbounded size ───────────────────


def test_buffer_reader_read_huge_size_does_not_oom():
    r = strider.BufferReader(0x1000, b"\x01\x02\x03\x04")
    # An enormous Python-supplied size must not trigger a multi-exabyte
    # allocation; the read is clamped to the mapped region.
    out = r.read(0x1000, 2 ** 60)
    assert out == b"\x01\x02\x03\x04"


def test_buffer_reader_read_huge_size_unmapped():
    r = strider.BufferReader(0x1000, b"\x01\x02\x03\x04")
    # Unmapped base — clamp must not allocate, returns None.
    assert r.read(0x9000, 2 ** 60) is None
