"""Regression: a Python `ReadOnlyMemory.read` that raises
`KeyboardInterrupt` or `SystemExit` propagates out of the Rust
adapter rather than being swallowed and surfaced as a generic
`StriderError`.

The exercise path: pass the rom into `strider.lift.lifter(..., rom=...)`;
`LoadReadOnly` (part of the canonical default pipeline `Lifter.analyze`
always runs) then consults the rom via the orchestrator's `OptCtx`,
forcing a callback into the Python rom on the constant-address load
below.  Bytes encoding a load from a constant absolute address (`mov
eax, ds:[0x2000]`) then force the optimizer to consult the ROM for the
bytes at `0x2000`, which is where the Rust
`PyReadOnlyMemoryAdapter::read` adapter calls through to the Python
subclass.

The Rust side stashes control-flow exceptions in the thread-local
PENDING_CONTROL_FLOW cell (see `pattern.rs`) rather than
`PyErr::restore`-ing them on the spot — restoring would leave the
error indicator set between callbacks, and the next callback would
trip CPython's "returned a result with an exception set" guard,
destroying the original `KeyboardInterrupt`/`SystemExit` signal.
The outer `Lifter.analyze` boundary then drains the cell and surfaces
the saved PyErr as `Err(...)` to Python.
"""

import pytest
import strider


class _KbdRom(strider.reader.ReadOnlyMemory):
    def read(self, addr: int, size: int) -> bytes:
        raise KeyboardInterrupt("interrupted")


class _SysExitRom(strider.reader.ReadOnlyMemory):
    def read(self, addr: int, size: int) -> bytes:
        raise SystemExit(42)


# x86_64: `mov eax, ds:[0x2000]; ret`
# Encoding: 8B /r with ModRM=04 + SIB=25 + disp32 = absolute-addr load.
_BYTES = bytes.fromhex("8b042500200000c3")


def _build_mem() -> strider.reader.BufferReader:
    mem = strider.reader.BufferReader(0x1000, _BYTES)
    return mem


def test_keyboard_interrupt_in_rom_read_propagates():
    arch = strider.sleigh.SleighArch.x86_64()
    cc = strider.sleigh.CallingConvention.x86_64_systemv()
    mem = _build_mem()
    rom = _KbdRom()
    lift = strider.lift.lifter(arch, mem, rom=rom)
    with pytest.raises(KeyboardInterrupt):
        lift.analyze(0x1000, cc, opts=strider.lift.LifterOptions(cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True)))


def test_system_exit_in_rom_read_propagates():
    arch = strider.sleigh.SleighArch.x86_64()
    cc = strider.sleigh.CallingConvention.x86_64_systemv()
    mem = _build_mem()
    rom = _SysExitRom()
    lift = strider.lift.lifter(arch, mem, rom=rom)
    with pytest.raises(SystemExit):
        lift.analyze(0x1000, cc, opts=strider.lift.LifterOptions(cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True)))
