"""A reader can point back at its `Lifter` weakly, so the handle and its
decoder state go with the last strong reference, not a cyclic GC pass."""

import gc
import weakref
from typing import Optional

import strider


class _Reader(strider.reader.MemReader):
    lifter: Optional["weakref.ref[strider.lift.Lifter]"] = None

    def read(self, addr, size):
        return b"\xc3"[: size] if addr == 0x1000 else None


def test_a_weak_back_reference_frees_the_lifter_without_gc():
    gc.disable()
    try:
        r = _Reader()
        lifter = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), r)
        r.lifter = weakref.ref(lifter)
        lifter.analyze(0x1000, strider.sleigh.CallingConvention.x86_64_systemv())
        del lifter
        assert r.lifter is not None and r.lifter() is None
    finally:
        gc.enable()
