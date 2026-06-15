"""Regression tests for the 2026-06-14 deep-audit binding fixes.

Each test pins one numbered finding from
`docs/superpowers/reviews/2026-06-14/strider-py.md`:

  * PY-2  — `CallOtherPat.ctrl()` / `.mem()` route to the correct
            control / memory input slot and actually match.
  * PY-3  — pure typed-builder nesting respects the native-recursion
            depth guard (raises `StriderError`, not a process abort).
  * PY-4  — a `MemReader.read` over-long return is rejected, not
            silently truncated.
  * PY-5  — reusing one pipeline across two `run(rom=...)` calls
            behaves predictably (the caller's object is not mutated).
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
    any_,
    Pat,
)

from .conftest import symbol_addr


# cpuid (0F A2) ; ret (C3) — lifts to a CallOther with control + memory
# inputs plus implicit reads, ideal for exercising ctrl/mem.
CPUID_BYTES = b"\x0f\xa2\xc3"
ENTRY = 0x1000


def _cpuid_function():
    mem = strider.BufferReader(ENTRY, CPUID_BYTES)
    result = strider.run(
        arch=strider.SleighArch.x86_64(),
        cc=strider.CallingConvention.x86_64_systemv(),
        mem=mem,
        entry=ENTRY,
    )
    return result.function


# ── PY-2 — CallOtherPat.ctrl()/.mem() route to the right slot ─────────


def test_call_other_ctrl_mem_methods_match():
    fn = _cpuid_function()
    baseline = len(fn.find_all(call_other()))
    assert baseline >= 1

    # `.ctrl(any_())` must route through the control-relaxed match
    # compiler at slot 0 (a CONTROL edge, not a value) and still match
    # every CallOther.  Before the fix it pushed onto the value-args slot,
    # so `output_ok` rejected the control edge and it matched nothing.
    ctrl_hits = fn.find_all(call_other().ctrl(any_()))
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
    assert isinstance(call_other().ctrl(any_()).into_pat(), Pat)
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
    with pytest.raises(strider.errors.StriderError):
        strider.run(
            arch=strider.SleighArch.x86_64(),
            cc=strider.CallingConvention.x86_64_systemv(),
            mem=reader,
            entry=ENTRY,
        )


# ── PY-5 — run(rom=, pipeline=) leaves the caller's pipeline usable ───


def test_run_twice_with_rom_and_same_pipeline(x86_memory_elf):
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.SleighArch.x86()
    cc = strider.CallingConvention.x86_cdecl()
    mem = strider.load_elf(str(x86_memory_elf)).reader()

    pipe = strider.OptimizerPipeline.empty()
    pipe.add(strider.opt.ConstantFold())

    first = strider.run(
        arch=arch, cc=cc, mem=mem, entry=addr, rom=mem,
        pipeline=pipe, allow_code_before_start_addr=True,
    )
    assert first.function.node_count() > 0

    # The pipeline is drained on use (documented), so a second run with
    # the SAME object must fail with the clean "already drained" error —
    # NOT silently double-prepend a LoadReadOnly pass or otherwise
    # mutate the caller's object into an inconsistent state.
    with pytest.raises(strider.errors.StriderError):
        strider.run(
            arch=arch, cc=cc, mem=mem, entry=addr, rom=mem,
            pipeline=pipe, allow_code_before_start_addr=True,
        )


def test_run_with_rom_fresh_pipeline_each_call(x86_memory_elf):
    addr = symbol_addr(x86_memory_elf, "array_sum")
    arch = strider.SleighArch.x86()
    cc = strider.CallingConvention.x86_cdecl()
    mem = strider.load_elf(str(x86_memory_elf)).reader()

    def _run():
        pipe = strider.OptimizerPipeline.empty()
        pipe.add(strider.opt.ConstantFold())
        return strider.run(
            arch=arch, cc=cc, mem=mem, entry=addr, rom=mem,
            pipeline=pipe, allow_code_before_start_addr=True,
        )

    a = _run()
    b = _run()
    # Identical inputs (fresh pipeline each time) produce identical shape.
    assert a.function.node_count() == b.function.node_count()


# ── PY-6 — optimize bumps the generation, invalidating handles ────────
#
# The fix bumps the generation BEFORE running the pipeline (before the
# `?`), so an in-place mutation that errors mid-run still invalidates
# outstanding handles.  Bumping-first means even a *successful* optimize
# invalidates handles, which is the observable contract this test pins.


def test_optimize_invalidates_outstanding_handles(x86_memory_elf):
    from strider.pattern import Capture

    a = _analysis(x86_memory_elf, "array_sum")
    fn = a.function

    c = Capture()
    add_hits = fn.find_all(load().capture(c))
    if not add_hits:
        pytest.skip("no load node to hold a stale handle against")
    handle = add_hits[0]
    # Sanity: the graph-deref accessor works before optimize.
    assert handle.node(c) is not None

    pipe = strider.OptimizerPipeline.empty()
    pipe.add(strider.opt.ConstantFold())
    fn.optimize(pipe)

    # The handle is now stale (generation bumped): a graph-dereferencing
    # accessor must raise rather than read the mutated arena.
    with pytest.raises(strider.errors.StriderError):
        handle.node(c)


def _analysis(elf_path, sym):
    addr = symbol_addr(elf_path, sym)
    mem = strider.load_elf(str(elf_path)).reader()
    result = strider.run(
        arch=strider.SleighArch.x86(),
        cc=strider.CallingConvention.x86_cdecl(),
        mem=mem,
        entry=addr,
        allow_code_before_start_addr=True,
    )
    return result


# ── PY-9 — int_ne builder + Load/Store.stack_offset mirror ────────────


def test_int_ne_builder_compiles():
    p = int_ne(int_const(1), int_const(2))
    assert isinstance(p, Pat)


def test_int_ne_finds_lowered_shape(x86_memory_elf):
    # int_ne is the lifter-canonical `Xor(IntEqual(a,b),1):I1` shape.
    # Just assert it compiles + queries without error on a real graph.
    fn = _analysis(x86_memory_elf, "array_sum").function
    hits = fn.find_all(int_ne(any_(), any_()))
    assert isinstance(hits, list)


def test_load_store_stack_offset_field_compiles():
    assert isinstance(load().stack_offset(0).into_pat(), Pat)
    assert isinstance(store().stack_offset(8).into_pat(), Pat)


def test_load_stack_offset_filters(x86_memory_elf):
    fn = _analysis(x86_memory_elf, "array_sum").function
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
