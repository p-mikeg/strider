"""Regression: `KeyboardInterrupt` / `SystemExit` raised inside a Python
`ReadOnlyMemory.read` propagates out of `analyze` instead of being
swallowed and resurfaced as a generic `StriderError`.

The bytes below load from a constant absolute address, so constant-load
folding must consult the rom, driving a callback into the Python
subclass mid-analysis.  Control-flow exceptions have to survive that
callback boundary intact and re-raise unchanged at `analyze`'s edge.
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
