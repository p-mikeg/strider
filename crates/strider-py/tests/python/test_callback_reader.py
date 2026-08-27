from __future__ import annotations

import threading

import pytest

import strider
from strider.reader import MemReader, BufferReader, ReadOnlyMemory
from strider.sleigh import SleighArch, CallingConvention
from strider.opt import LoadReadOnly
from strider.pattern import any_int, Capture, load

from .conftest import symbol_addr


class CountingReader(MemReader):
    """Wraps a BufferReader, but counts every Python-side read."""

    def __init__(self, inner: BufferReader):
        super().__init__()
        self.inner = inner
        self.calls = 0
        self.lock = threading.Lock()

    def read(self, addr: int, size: int):
        with self.lock:
            self.calls += 1
        return self.inner.read(addr, size)


def make_counting_reader(inner: BufferReader) -> CountingReader:
    return CountingReader(inner)


def test_mem_reader_subclass_default_raises():
    r = MemReader()
    with pytest.raises(NotImplementedError):
        r.read(0, 4)


def test_callback_reader_lifts_array_sum(x86_memory_elf):
    inner = strider.lift.load_elf(str(x86_memory_elf)).reader()
    reader = make_counting_reader(inner)
    addr = symbol_addr(x86_memory_elf, "array_sum")

    arch = SleighArch.x86()
    cc = CallingConvention.x86_cdecl()
    s = strider.lift.lifter(arch, reader)
    _cfg, g, _unresolved = s.analyze(
        addr, cc, opts=strider.lift.LifterOptions(cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True))
    )

    from strider.pattern import ret
    hits = g.find_all(ret())
    assert len(hits) >= 1, "expected ret() to match at least once"

    assert reader.calls > 0, "PyMemReader.read was never invoked"


def test_run_via_callback_reader(x86_memory_elf):
    """A callback reader must survive the full indirect-branch
    fixed-point loop, not just a single lift pass."""
    inner = strider.lift.load_elf(str(x86_memory_elf)).reader()
    reader = make_counting_reader(inner)
    addr = symbol_addr(x86_memory_elf, "array_sum")

    lift = strider.lift.lifter(SleighArch.x86(), reader)
    _cfg, function, _unresolved = lift.analyze(
        addr, CallingConvention.x86_cdecl(),
        opts=strider.lift.LifterOptions(cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True)),
    )
    assert function.node_count() > 0
    assert reader.calls > 0


class ConstReadOnlyMemory(ReadOnlyMemory):
    """Fixed bytes at 0x4000, None elsewhere.

    `read` takes only `(addr, size)` with no space: `LoadReadOnly` fires
    on RAM loads alone, so non-RAM reads return None without ever
    reaching the user's method.
    """

    def __init__(self):
        super().__init__()
        self.data = bytes.fromhex("deadbeef00000000")
        self.calls = 0

    def read(self, addr: int, size: int):
        self.calls += 1
        if addr < 0x4000 or addr >= 0x4000 + len(self.data):
            return None
        if addr - 0x4000 + size > len(self.data):
            return None
        # RAW bytes: the optimizer decodes per the run's endianness, the
        # callback must not.
        return self.data[addr - 0x4000:addr - 0x4000 + size]


def test_read_only_memory_subclass_default_raises():
    r = ReadOnlyMemory()
    with pytest.raises(NotImplementedError):
        r.read(0x4000, 4)


def test_load_readonly_accepts_callback_subclass():
    # The rom flows through `strider.lift.lifter(..., rom=...)`, not the
    # pass instance, so the constructor takes no arguments.
    _ = ConstReadOnlyMemory()
    pipe = LoadReadOnly()
    assert pipe is not None


def test_run_with_callback_rom_doesnt_crash(x86_memory_elf):
    """A callback ROM must not crash the pipeline even when no load
    actually folds."""
    inner = strider.lift.load_elf(str(x86_memory_elf)).reader()
    addr = symbol_addr(x86_memory_elf, "array_sum")
    rom = ConstReadOnlyMemory()

    lift = strider.lift.lifter(SleighArch.x86(), inner, rom=rom)
    _cfg, function, _unresolved = lift.analyze(
        addr, CallingConvention.x86_cdecl(),
        opts=strider.lift.LifterOptions(cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True)),
    )
    assert function.node_count() > 0


class _OverLongReader(strider.reader.MemReader):
    def read(self, addr, size):  # noqa: ARG002 - mirrors the ABC sig
        # More bytes than asked for, which the adapter must reject.
        return b"\x90" * (size + 16)


def test_mem_reader_over_long_return_errors():
    reader = _OverLongReader()
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), reader)
    with pytest.raises(strider.StriderError):
        lift.analyze(0x1000, strider.sleigh.CallingConvention.x86_64_systemv())
