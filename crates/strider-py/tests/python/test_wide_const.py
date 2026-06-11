"""Wide (>64-bit) integer constants reaching Python.

`IntConst(IntPayload::Wide(..))` nodes (I128 here) are minted when
`LoadReadOnly` folds a 16-byte constant-address load — hand-assembled
`movdqa xmm0, [abs]` against a `BufferReader` that doubles as code and
ROM.  These tests pin how the wide value surfaces through the pattern
API and the `Node` handle.
"""

from __future__ import annotations

import strider
from strider.pattern import Capture, any_int_const, int_const

WIDE = 0x0123456789ABCDEF_FEDCBA9876543210  # does not fit in u64


def _wide_const_function():
    # 0x1000: 66 0f 6f 04 25 00 20 00 00   movdqa xmm0, [0x2000]
    # 0x1009: c3                            ret
    code = bytes([0x66, 0x0F, 0x6F, 0x04, 0x25, 0x00, 0x20, 0x00, 0x00, 0xC3])
    buf = bytearray(0x1000 + 16)
    buf[: len(code)] = code
    buf[0x1000 : 0x1000 + 16] = WIDE.to_bytes(16, "little")
    mem = strider.BufferReader(0x1000, bytes(buf))
    return strider.run(
        arch=strider.SleighArch.x86_64(),
        cc=strider.CallingConvention.x86_64_systemv(),
        mem=mem,
        rom=mem,
        entry=0x1000,
        function_max_size=len(code),
    ).function


def test_wide_const_node_is_minted_by_load_readonly_fold():
    f = _wide_const_function()
    wide_kinds = [f.node_kind(n) for n in f.node_ids() if "Wide" in f.node_kind(n)]
    assert wide_kinds, "expected an IntConst(Wide(..)) node from the 16-byte fold"
    assert all(k.startswith("IntConst") for k in wide_kinds)


def test_wide_const_match_uint_returns_full_u128():
    # Match.uint(c) carries the full 128-bit value into an arbitrary-
    # precision Python int — no truncation to 64 bits.
    f = _wide_const_function()
    c = Capture()
    hits = f.find_all(any_int_const(c))
    assert len(hits) == 1
    assert hits[0].uint(c) == WIDE


def test_wide_const_match_int_is_unsigned_below_bit127():
    # Match.int(c) interprets the stored u128 as i128.  WIDE's bit 127
    # is clear, so the signed reading equals the unsigned one.
    f = _wide_const_function()
    c = Capture()
    hits = f.find_all(any_int_const(c))
    assert hits[0].int(c) == WIDE


def test_wide_const_int_const_literal_matches():
    # `int_const(literal)` accepts a >64-bit Python int and matches the
    # interned wide constant.
    f = _wide_const_function()
    assert len(f.find_all(int_const(WIDE))) == 1
    # A different wide literal does not match.
    assert f.find_all(int_const(WIDE ^ 1)) == []


def test_wide_const_node_const_int_returns_none():
    # Pinned current behaviour: the `Node.const_int()` point accessor
    # surfaces only small (<= 64-bit payload) constants; for a wide
    # IntConst it returns None rather than the interned value.  The
    # pattern-API extraction (`Match.uint`) is the wide-value path.
    f = _wide_const_function()
    wide_ids = [n for n in f.node_ids() if "Wide" in f.node_kind(n)]
    assert wide_ids
    for nid in wide_ids:
        assert f.node(nid).const_int() is None
