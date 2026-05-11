"""Regression: a Python `ReadOnlyMemory.read` that raises
`KeyboardInterrupt` or `SystemExit` propagates out of the Rust
adapter rather than being swallowed and surfaced as a generic
`ReaderError`.

Pre-fix: the adapter caught any `PyErr` and converted it to
`ReaderError`, which would mask Ctrl-C during a long pattern walk.
Post-fix: the adapter explicitly re-raises both exit-style
exceptions before falling back to the typed conversion.

The exercise path: `strider.run(rom=...)` wires the Python ROM into
the optimizer's `LoadReadOnly` pass.  Bytes encoding a load from a
constant absolute address (`mov eax, ds:[0x2000]`) force the
optimizer to consult the ROM for the bytes at `0x2000`, which is
where the Rust `PyReadOnlyMemoryAdapter::read` adapter calls
through to the Python subclass.  Round-13 F1 noted that the prior
form of this test invoked `rom.read(...)` directly on the Python
subclass, which dispatched through Python MRO straight to the
subclass override — never crossing into the Rust adapter — so the
re-raise guard was not actually verified.
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


def test_keyboard_interrupt_in_rom_read_propagates():
    arch = strider.SleighArch.x86_64()
    cc = strider.CallingConvention.x86_64_systemv()
    mem = _build_mem()
    rom = _KbdRom()
    with pytest.raises(KeyboardInterrupt):
        strider.run(
            arch=arch,
            cc=cc,
            mem=mem,
            entry=0x1000,
            rom=rom,
            allow_code_before_start_addr=True,
        )


def test_system_exit_in_rom_read_propagates():
    arch = strider.SleighArch.x86_64()
    cc = strider.CallingConvention.x86_64_systemv()
    mem = _build_mem()
    rom = _SysExitRom()
    with pytest.raises(SystemExit):
        strider.run(
            arch=arch,
            cc=cc,
            mem=mem,
            entry=0x1000,
            rom=rom,
            allow_code_before_start_addr=True,
        )
