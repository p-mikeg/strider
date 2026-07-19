"""Regression: a reference cycle through a custom Python reader and its
`Lifter` must be collectable by the cyclic GC.

A Lifter holds its reader internally, so a reader that points back at the
Lifter closes a cycle whose second edge is invisible to Python. Until the
Lifter exposed that edge to the GC, both objects leaked for the process
lifetime.
"""

from __future__ import annotations

import gc
import weakref

import strider

_CODE = bytes([0x31, 0xC0, 0xC3])  # xor eax,eax; ret


class _Reader:
    def read(self, addr, size):
        off = addr - 0x1000
        return bytes(_CODE[off : off + size]).ljust(size, b"\x00")


def test_custom_reader_lifter_cycle_is_collectable():
    class L(strider.lift.Lifter):  # Python subclass => weakref-able
        pass

    r = _Reader()
    lift = L(strider.sleigh.SleighArch.x86_64(), r)
    r.back = lift  # cycle: reader -> lifter -> (held internally) -> reader

    wl, wr = weakref.ref(lift), weakref.ref(r)
    del lift, r
    gc.collect()

    assert wl() is None, "lifter leaked: cycle through custom reader not collected"
    assert wr() is None, "reader leaked: cycle through custom reader not collected"


def test_custom_rom_lifter_cycle_is_collectable():
    class L(strider.lift.Lifter):
        pass

    class _Rom:
        def read(self, addr, size):
            return bytes(size)

    mem = strider.reader.BufferReader(0x1000, _CODE)
    rom = _Rom()
    lift = L(strider.sleigh.SleighArch.x86_64(), mem, rom=rom)
    rom.back = lift  # cycle through the rom callback

    wl, wr = weakref.ref(lift), weakref.ref(rom)
    del lift, rom
    gc.collect()

    assert wl() is None, "lifter leaked: cycle through custom rom not collected"
    assert wr() is None, "rom leaked: cycle through custom rom not collected"
