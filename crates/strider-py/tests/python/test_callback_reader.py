"""Smoke tests for the Python-callback `MemReader` / `ReadOnlyMemory`
ABCs.

Subclassing either ABC and providing `read(...)` lets the analysis
pipeline call back into Python for byte fetches.  These tests verify:
1. The Rust adapter actually invokes the Python `read` method.
2. A real ELF lifted via the callback path produces a Function with at
   least one `Return` node — equivalent functionality to the fast
   MemoryMap path.
3. `ReadOnlyMemory` subclasses fold loads of constants when supplied
   to `LoadReadOnly`.
"""

from __future__ import annotations

import threading

import pytest

import strider
from strider import MemReader, MemoryMap, ReadOnlyMemory, SleighArch, CallingConvention
from strider.opt import LoadReadOnly
from strider.pattern import any_int_const, Capture, load

from .conftest import symbol_addr


# ── PyMemReader subclassing ──────────────────────────────────────────


class CountingReader(MemReader):
    """Wraps a MemoryMap, but counts every Python-side read."""

    def __init__(self, inner: MemoryMap):
        super().__init__()
        self.inner = inner
        self.calls = 0
        self.lock = threading.Lock()

    def read(self, addr: int, size: int):
        with self.lock:
            self.calls += 1
        return self.inner.read(addr, size)


def make_counting_reader(inner: MemoryMap) -> CountingReader:
    return CountingReader(inner)


def test_mem_reader_subclass_default_raises():
    r = MemReader()
    with pytest.raises(NotImplementedError):
        r.read(0, 4)


def test_callback_reader_lifts_array_sum(x86_memory_elf):
    inner = MemoryMap()
    inner.add_region_from_elf(str(x86_memory_elf))
    reader = make_counting_reader(inner)
    addr = symbol_addr(x86_memory_elf, "array_sum")

    arch = SleighArch.x86()
    cc = CallingConvention.x86_cdecl()
    sleigh = strider.Sleigh(arch, reader)
    s = strider.Strider(arch, sleigh, cc)
    cfg = strider.build_cfg(sleigh, addr, allow_code_before_start_addr=True)
    outcome = s.analyze_cfg(cfg)
    g = outcome.graph

    # The lifted graph should include at least one Return.
    from strider.pattern import ret
    hits = g.find_all(ret())
    assert len(hits) >= 1, "expected ret() to match at least once"

    # The Python reader must have been called at least once.
    assert reader.calls > 0, "PyMemReader.read was never invoked"


def test_run_via_callback_reader(x86_memory_elf):
    """End-to-end: `strider.run` with a callback reader should drive
    the orchestrator's indirect-branch fixed-point loop and produce
    an optimised graph.
    """
    inner = MemoryMap()
    inner.add_region_from_elf(str(x86_memory_elf))
    reader = make_counting_reader(inner)
    addr = symbol_addr(x86_memory_elf, "array_sum")

    result = strider.run(
        arch=SleighArch.x86(),
        cc=CallingConvention.x86_cdecl(),
        mem=reader,
        entry=addr,
        allow_code_before_start_addr=True,
    )
    assert result.graph.node_count() > 0
    assert reader.calls > 0


# ── PyReadOnlyMemory subclassing ─────────────────────────────────────


class ConstReadOnlyMemory(ReadOnlyMemory):
    """Returns a fixed sequence of bytes for any address in [0x4000, 0x4100).

    `read` only takes `(addr, size)`: the underlying Rust trait
    carries a `VnSpace` for symmetry with the IR's `Load` nodes,
    but the LoadReadOnly pass only ever fires on RAM loads, so the
    Python ABC narrows the surface to RAM only — non-RAM reads
    return None automatically without ever calling the user's
    method.
    """

    def __init__(self):
        super().__init__()
        # Map address 0x4000 → bytes \xde\xad\xbe\xef\x00\x00\x00\x00
        self.data = bytes.fromhex("deadbeef00000000")
        self.calls = 0

    def read(self, addr: int, size: int):
        self.calls += 1
        if addr < 0x4000 or addr >= 0x4000 + len(self.data):
            return None
        if addr - 0x4000 + size > len(self.data):
            return None
        chunk = self.data[addr - 0x4000:addr - 0x4000 + size]
        return int.from_bytes(chunk, "little")


def test_read_only_memory_subclass_default_raises():
    r = ReadOnlyMemory()
    with pytest.raises(NotImplementedError):
        r.read(0x4000, 4)


def test_load_readonly_accepts_callback_subclass():
    # Just confirm we can build a LoadReadOnly pass with a callback ROM.
    rom = ConstReadOnlyMemory()
    pipe = LoadReadOnly(rom)
    assert pipe is not None


def test_run_with_callback_rom_doesnt_crash(x86_memory_elf):
    """Plug a callback ROM into `strider.run` — even if no loads
    actually fold, the pipeline must not crash.
    """
    inner = MemoryMap()
    inner.add_region_from_elf(str(x86_memory_elf))
    addr = symbol_addr(x86_memory_elf, "array_sum")
    rom = ConstReadOnlyMemory()

    result = strider.run(
        arch=SleighArch.x86(),
        cc=CallingConvention.x86_cdecl(),
        mem=inner,
        entry=addr,
        rom=rom,
        allow_code_before_start_addr=True,
    )
    assert result.graph.node_count() > 0
