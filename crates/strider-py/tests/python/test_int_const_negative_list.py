"""A negative in `int_const([..])` carries the same 128-bit two's complement
the scalar form does.

Sign-extending the list form to 64 bits instead binds a different constant
above `I64`, so the graph holds both the 128-bit and the 64-bit all-ones
value and the two must not be confused. Wide constants are minted the way
`test_wide_const` does it, by folding a `movdqa` out of a `BufferReader`
serving as both code and ROM.
"""

from __future__ import annotations

import strider
from strider.pattern import Capture, int_const

ALL_ONES_128 = (1 << 128) - 1
ALL_ONES_64 = (1 << 64) - 1


def _two_wide_consts_function():
    # 0x1000: 66 0f 6f 04 25 00 20 00 00   movdqa xmm0, [0x2000]
    # 0x1009: 66 0f 7f 07                  movdqa [rdi], xmm0
    # 0x100d: 66 0f 6f 04 25 10 20 00 00   movdqa xmm0, [0x2010]
    # 0x1016: 66 0f 7f 47 10               movdqa [rdi+0x10], xmm0
    # 0x101b: c3                           ret
    code = bytes(
        [0x66, 0x0F, 0x6F, 0x04, 0x25, 0x00, 0x20, 0x00, 0x00]
        + [0x66, 0x0F, 0x7F, 0x07]
        + [0x66, 0x0F, 0x6F, 0x04, 0x25, 0x10, 0x20, 0x00, 0x00]
        + [0x66, 0x0F, 0x7F, 0x47, 0x10]
        + [0xC3]
    )
    buf = bytearray(0x1000 + 32)
    buf[: len(code)] = code
    buf[0x1000 : 0x1000 + 16] = ALL_ONES_128.to_bytes(16, "little")
    buf[0x1010 : 0x1010 + 16] = ALL_ONES_64.to_bytes(16, "little")
    mem = strider.reader.BufferReader(0x1000, bytes(buf))
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem, rom=mem)
    _cfg, function, _unresolved = lift.analyze(
        0x1000,
        strider.sleigh.CallingConvention.x86_64_systemv(),
        opts=strider.lift.LifterOptions(
            cfg=strider.cfg.CfgOptions(function_max_size=len(code))
        ),
    )
    return function


def test_the_graph_holds_both_all_ones_widths():
    f = _two_wide_consts_function()
    wide = [f.node(n).uint() for n in f.node_ids() if f.node(n).wide_const_bytes()]
    assert ALL_ONES_128 in wide
    assert ALL_ONES_64 in wide


def test_negative_in_a_list_binds_what_the_scalar_form_binds():
    f = _two_wide_consts_function()
    scalar = f.find_all(int_const(-1))
    listed = f.find_all(int_const([-1]))
    assert len(scalar) == 1
    assert [m.root for m in listed] == [m.root for m in scalar]


def test_a_list_negative_reads_back_as_the_128_bit_all_ones():
    f = _two_wide_consts_function()
    c = Capture()
    hits = f.find_all(int_const([-1]).capture(c))
    assert [m.uint(c) for m in hits] == [ALL_ONES_128]
