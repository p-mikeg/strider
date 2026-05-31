"""Regression: a Python `ReadOnlyMemory.read` that raises
`KeyboardInterrupt` or `SystemExit` propagates out of the Rust
adapter rather than being swallowed and surfaced as a generic
`StriderError`.

The exercise path: build a custom pipeline that includes a
`strider.opt.LoadReadOnly()` marker and pass the rom into
`strider.run(rom=...)`; the rom flows down through the orchestrator's
`OptCtx` into the pass and forces a callback into the Python rom on
the constant-address load below.  Bytes encoding a load from a
constant absolute address (`mov eax, ds:[0x2000]`) then force the
optimizer to consult the ROM for the bytes at `0x2000`, which is
where the Rust `PyReadOnlyMemoryAdapter::read` adapter calls through
to the Python subclass.

The Rust side stashes control-flow exceptions in the thread-local
PENDING_CONTROL_FLOW cell (see `pattern.rs`) rather than
`PyErr::restore`-ing them on the spot — restoring would leave the
error indicator set between callbacks, and the next callback would
trip CPython's "returned a result with an exception set" guard,
destroying the original `KeyboardInterrupt`/`SystemExit` signal.
The outer `strider.run` boundary then drains the cell and surfaces
the saved PyErr as `Err(...)` to Python.
"""

import pytest
import strider


class _KbdRom(strider.ReadOnlyMemory):
    def read(self, addr: int, size: int) -> bytes:
        raise KeyboardInterrupt("interrupted")


class _SysExitRom(strider.ReadOnlyMemory):
    def read(self, addr: int, size: int) -> bytes:
        raise SystemExit(42)


# x86_64: `mov eax, ds:[0x2000]; ret`
# Encoding: 8B /r with ModRM=04 + SIB=25 + disp32 = absolute-addr load.
_BYTES = bytes.fromhex("8b042500200000c3")


def _build_mem() -> strider.MemoryMap:
    mem = strider.MemoryMap()
    mem.add_region(0x1000, _BYTES)
    return mem


def _build_pipeline_with_load_readonly(arch, cc, mem):
    """Build the orchestrator's default pipeline + a `LoadReadOnly()`
    marker so the custom-pipeline path's rom plumbing fires the pass."""
    sleigh = strider.Sleigh(arch, mem)
    s = strider.Strider(arch, sleigh, cc)
    pipeline = s.build_optimizer_pipeline()
    pipeline.add(strider.opt.LoadReadOnly())
    return pipeline


def test_keyboard_interrupt_in_rom_read_propagates():
    arch = strider.SleighArch.x86_64()
    cc = strider.CallingConvention.x86_64_systemv()
    mem = _build_mem()
    rom = _KbdRom()
    pipeline = _build_pipeline_with_load_readonly(arch, cc, mem)
    with pytest.raises(KeyboardInterrupt):
        strider.run(
            arch=arch,
            cc=cc,
            mem=mem,
            entry=0x1000,
            rom=rom,
            pipeline=pipeline,
            allow_code_before_start_addr=True,
        )


def test_system_exit_in_rom_read_propagates():
    arch = strider.SleighArch.x86_64()
    cc = strider.CallingConvention.x86_64_systemv()
    mem = _build_mem()
    rom = _SysExitRom()
    pipeline = _build_pipeline_with_load_readonly(arch, cc, mem)
    with pytest.raises(SystemExit):
        strider.run(
            arch=arch,
            cc=cc,
            mem=mem,
            entry=0x1000,
            rom=rom,
            pipeline=pipeline,
            allow_code_before_start_addr=True,
        )
