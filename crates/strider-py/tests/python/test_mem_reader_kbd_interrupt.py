"""Regression: `KeyboardInterrupt` / `SystemExit` raised inside a custom
`MemReader.read` must propagate, not be converted to `StriderError`.

Every exception used to be folded into a read error, which masked Ctrl-C
during long lifts.
"""

import pytest
import strider


class _KbdReader(strider.reader.MemReader):
    def read(self, addr: int, size: int) -> bytes:
        raise KeyboardInterrupt("interrupted")


class _SysExitReader(strider.reader.MemReader):
    def read(self, addr: int, size: int) -> bytes:
        raise SystemExit(42)


def test_keyboard_interrupt_in_mem_reader_read_propagates():
    arch = strider.sleigh.SleighArch.x86_64()
    cc = strider.sleigh.CallingConvention.x86_64_systemv()
    reader = _KbdReader()
    with pytest.raises(KeyboardInterrupt):
        strider.lift.lifter(arch, reader).analyze(
            0x1000, cc, opts=strider.lift.LifterOptions(cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True))
        )


def test_system_exit_in_mem_reader_read_propagates():
    arch = strider.sleigh.SleighArch.x86_64()
    cc = strider.sleigh.CallingConvention.x86_64_systemv()
    reader = _SysExitReader()
    with pytest.raises(SystemExit):
        strider.lift.lifter(arch, reader).analyze(
            0x1000, cc, opts=strider.lift.LifterOptions(cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True))
        )
