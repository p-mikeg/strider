"""Regression: a Python `MemReader.read` that raises
`KeyboardInterrupt` or `SystemExit` must propagate out of the Rust
adapter rather than being swallowed and surfaced as a generic
`ReaderError`.

Pre-fix: `PyMemReaderAdapter::read` caught any `PyErr` and converted
it to `MemReadError`, masking Ctrl-C during long lifts.
Post-fix: the adapter explicitly re-raises both exit-style exceptions
before falling back to the typed conversion (mirrors the existing
`PyReadOnlyMemoryAdapter::read` guard).
"""

import pytest
import strider


class _KbdReader(strider.MemReader):
    def read(self, addr: int, size: int) -> bytes:
        raise KeyboardInterrupt("interrupted")


class _SysExitReader(strider.MemReader):
    def read(self, addr: int, size: int) -> bytes:
        raise SystemExit(42)


def test_keyboard_interrupt_in_mem_reader_read_propagates():
    arch = strider.SleighArch.x86_64()
    cc = strider.CallingConvention.x86_64_systemv()
    reader = _KbdReader()
    with pytest.raises(KeyboardInterrupt):
        strider.run(
            arch=arch,
            cc=cc,
            mem=reader,
            entry=0x1000,
            allow_code_before_start_addr=True,
        )


def test_system_exit_in_mem_reader_read_propagates():
    arch = strider.SleighArch.x86_64()
    cc = strider.CallingConvention.x86_64_systemv()
    reader = _SysExitReader()
    with pytest.raises(SystemExit):
        strider.run(
            arch=arch,
            cc=cc,
            mem=reader,
            entry=0x1000,
            allow_code_before_start_addr=True,
        )
