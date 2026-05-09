"""Regression: a Python `ReadOnlyMemory.read` that raises
`KeyboardInterrupt` or `SystemExit` propagates out of the Rust
adapter rather than being swallowed and surfaced as a generic
`ReaderError`.

Pre-fix: the adapter caught any `PyErr` and converted it to
`ReaderError`, which would mask Ctrl-C during a long pattern walk.
Post-fix: the adapter explicitly re-raises both exit-style
exceptions before falling back to the typed conversion.
"""

import pytest
import strider


class _KbdRom(strider.ReadOnlyMemory):
    def read(self, addr: int, size: int) -> bytes:
        raise KeyboardInterrupt("interrupted")


class _SysExitRom(strider.ReadOnlyMemory):
    def read(self, addr: int, size: int) -> bytes:
        raise SystemExit(42)


def test_keyboard_interrupt_in_rom_read_propagates():
    rom = _KbdRom()
    with pytest.raises(KeyboardInterrupt):
        rom.read(0x1000, 8)


def test_system_exit_in_rom_read_propagates():
    rom = _SysExitRom()
    with pytest.raises(SystemExit):
        rom.read(0x1000, 8)
