"""Regression tests for a batch of binding bugs found in an audit.

Pinned behaviours:

  * `CallOtherPat.ctrl()` / `.mem()` route to the control / memory input
    slot, not the value-args slot.
  * Deep typed-builder nesting raises StriderError instead of aborting the
    process on stack overflow.
  * A `MemReader.read` that returns too many bytes is rejected, not
    silently truncated.
  * `optimize` invalidates outstanding handles even if it errors mid-run.
  * `int_ne` and `Load/Store.stack_offset` exist and query.
  * `BufferReader.read` clamps an unbounded `size` before allocating.
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


# cpuid (0F A2) ; ret (C3): lifts to a CallOther with control + memory
# inputs plus implicit reads, so it exercises ctrl/mem.
CPUID_BYTES = b"\x0f\xa2\xc3"
ENTRY = 0x1000


def _cpuid_function():
    mem = strider.reader.BufferReader(ENTRY, CPUID_BYTES)
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)
    _cfg, function, _unresolved = lift.analyze(
        ENTRY, strider.sleigh.CallingConvention.x86_64_systemv()
    )
    return function


def test_call_other_ctrl_mem_methods_match():
    fn = _cpuid_function()
    baseline = len(fn.find_all(call_other()))
    assert baseline >= 1

    # Regression: `.ctrl()` used to push onto the value-args slot, so the
    # control edge was rejected and it matched nothing.
    ctrl_hits = fn.find_all(call_other().ctrl(anything()))
    assert len(ctrl_hits) == baseline

    # Regression: `.mem()` likewise landed on the value-args slot and raised
    # ("a control / variadic builder cannot be nested as a value operand").
    # Empty results are correct here (cpuid's memory predecessor is
    # InitialMemory, so no MemPhi/Store matches); raising is not.
    assert fn.find_all(call_other().mem(mem_phi())) == []
    assert fn.find_all(call_other().mem(store())) == []


def test_call_other_ctrl_mem_compile_to_pat():
    assert isinstance(call_other().ctrl(anything()).into_pat(), Pat)
    assert isinstance(call_other().mem(mem_phi()).into_pat(), Pat)


def test_deeply_nested_typed_builder_raises_not_aborts():
    # A 5000-deep operand nest must hit the depth guard and raise, rather
    # than overflowing the native stack and aborting the process.
    fn = _cpuid_function()
    deep = load()
    for _ in range(5000):
        deep = load().addr(deep)
    with pytest.raises(strider.StriderError):
        fn.find_all(deep)


class _OverLongReader(strider.reader.MemReader):
    def read(self, addr, size):  # noqa: ARG002 - mirrors the ABC sig
        # More bytes than asked for; this used to be silently truncated.
        return b"\x90" * (size + 16)


def test_mem_reader_over_long_return_errors():
    reader = _OverLongReader()
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), reader)
    with pytest.raises(strider.StriderError):
        lift.analyze(ENTRY, strider.sleigh.CallingConvention.x86_64_systemv())


# The pipeline-reuse test that lived here covered `strider.run(pipeline=...)`,
# an entry point removed when everything collapsed onto a single `Lifter`.
# The drained-pipeline-raises-on-reuse contract it pinned now lives in
# `test_optimizer_pipeline.py::test_optimize_twice_on_same_pipeline_raises`.


# `optimize` bumps the handle generation BEFORE running the pipeline, so a
# mid-run failure still invalidates outstanding handles.  Bumping first means
# even a successful optimize invalidates them; that is the contract pinned here.
def test_optimize_invalidates_outstanding_handles(x86_memory_elf):
    from strider.pattern import Capture

    lift, fn = _analysis(x86_memory_elf, "array_sum")

    c = Capture()
    add_hits = fn.find_all(load().capture(c))
    if not add_hits:
        pytest.skip("no load node to hold a stale handle against")
    handle = add_hits[0]
    assert handle.node(c) is not None

    pipe = strider.opt.OptimizerPipeline.empty()
    pipe.add(strider.opt.ConstantFold())
    lift.optimize(fn, pipe)

    # Stale handle: dereferencing must raise, not read the mutated arena.
    with pytest.raises(strider.StriderError):
        handle.node(c)


def _analysis(elf_path, sym):
    addr = symbol_addr(elf_path, sym)
    mem = strider.lift.load_elf(str(elf_path)).reader()
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86(), mem)
    _cfg, function, _unresolved = lift.analyze(
        addr, strider.sleigh.CallingConvention.x86_cdecl(),
        opts=strider.lift.LifterOptions(cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True)),
    )
    return lift, function


def test_int_ne_builder_compiles():
    p = int_ne(int_const(1), int_const(2))
    assert isinstance(p, Pat)


def test_int_ne_finds_lowered_shape(x86_memory_elf):
    # int_ne is the lifter-canonical `Xor(IntEqual(a,b),1):I1` shape; this
    # only asserts it compiles and queries against a real graph.
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


def test_buffer_reader_read_huge_size_does_not_oom():
    r = strider.reader.BufferReader(0x1000, b"\x01\x02\x03\x04")
    # Must clamp to the mapped region, not attempt an exabyte allocation.
    out = r.read(0x1000, 2 ** 60)
    assert out == b"\x01\x02\x03\x04"


def test_buffer_reader_read_huge_size_unmapped():
    r = strider.reader.BufferReader(0x1000, b"\x01\x02\x03\x04")
    # Unmapped base: clamp must not allocate, returns None.
    assert r.read(0x9000, 2 ** 60) is None
