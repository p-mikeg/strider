"""Wide (>64-bit) integer constants reaching Python.

The only way to mint one is to have `LoadReadOnly` fold a >8-byte
constant-address load, hence the hand-assembled `movdqa` / `fld` against a
`BufferReader` that doubles as code and ROM.
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
    mem = strider.reader.BufferReader(0x1000, bytes(buf))
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem, rom=mem)
    _cfg, function, _unresolved = lift.analyze(
        0x1000, strider.sleigh.CallingConvention.x86_64_systemv(),
        opts=strider.lift.LifterOptions(cfg=strider.cfg.CfgOptions(function_max_size=len(code))),
    )
    return function


def test_wide_const_node_is_minted_by_load_readonly_fold():
    f = _wide_const_function()
    # Wideness shows up via `wide_const_bytes`, not the kind string, which
    # is a plain `IntConst(constN)` for every interned constant.
    wide_ids = [n for n in f.node_ids() if f.node(n).wide_const_bytes() is not None]
    assert wide_ids, "expected a wide IntConst node from the 16-byte fold"
    assert all(f.node(n).kind().startswith("IntConst") for n in wide_ids)


def test_wide_const_match_uint_returns_full_u128():
    # The full 128 bits reach Python's arbitrary-precision int, untruncated.
    f = _wide_const_function()
    c = Capture()
    hits = f.find_all(any_int_const(c))
    assert len(hits) == 1
    assert hits[0].const_uint(c) == WIDE


def test_wide_const_match_int_is_unsigned_below_bit127():
    # const_int reads the stored u128 as i128; WIDE's bit 127 is clear, so
    # the signed and unsigned readings agree.
    f = _wide_const_function()
    c = Capture()
    hits = f.find_all(any_int_const(c))
    assert hits[0].const_int(c) == WIDE


def test_wide_const_int_const_literal_matches():
    # `int_const(literal)` accepts a >64-bit Python int and matches the
    # interned wide constant.
    f = _wide_const_function()
    assert len(f.find_all(int_const(WIDE))) == 1
    assert f.find_all(int_const(WIDE ^ 1)) == []


def test_wide_const_node_const_int_returns_full_value():
    # `Node.const_int()` / `const_uint()` cover every width up to 128 bits,
    # so a wide constant surfaces its full value rather than None.
    f = _wide_const_function()
    wide_ids = [n for n in f.node_ids() if f.node(n).wide_const_bytes() is not None]
    assert wide_ids
    for nid in wide_ids:
        node = f.node(nid)
        v = node.const_int()
        assert isinstance(v, int)
        assert v == WIDE
        assert node.const_uint() == WIDE
        # Agrees with the raw-bytes escape hatch.
        wb = node.wide_const_bytes()
        assert wb is not None
        raw = bytes(wb)
        assert v == int.from_bytes(raw, "little")


# Top bits set, so a u64 truncation or a signed misread would be caught.
I80_VALUE = (0x7FFF << 64) | 0x8000_0000_0000_0001


def _i80_const_function():
    # The `fld` result alone is dead (st0 is not live-out under SysV), so
    # the lift would cull the folded constant; the `fstp` to [rdi] is what
    # keeps the 10-byte value reachable.
    # 0x1000: db 2c 25 00 20 00 00   fld   tbyte [0x2000]
    # 0x1007: db 3f                  fstp  tbyte [rdi]
    # 0x1009: c3                     ret
    code = bytes(
        [0xDB, 0x2C, 0x25, 0x00, 0x20, 0x00, 0x00, 0xDB, 0x3F, 0xC3]
    )
    buf = bytearray(0x1000 + 10)
    buf[: len(code)] = code
    buf[0x1000 : 0x1000 + 10] = I80_VALUE.to_bytes(10, "little")
    mem = strider.reader.BufferReader(0x1000, bytes(buf))
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem, rom=mem)
    _cfg, function, _unresolved = lift.analyze(
        0x1000, strider.sleigh.CallingConvention.x86_64_systemv(),
        opts=strider.lift.LifterOptions(cfg=strider.cfg.CfgOptions(function_max_size=len(code))),
    )
    return function


def test_i80_const_node_const_int_returns_full_value():
    # The 10-byte x87 load folds to an I80 constant; const_int decodes all
    # 80 bits.
    f = _i80_const_function()
    wide_ids = [n for n in f.node_ids() if f.node(n).wide_const_bytes() is not None]
    assert wide_ids, "expected a wide IntConst node from the 10-byte fold"
    for nid in wide_ids:
        node = f.node(nid)
        v = node.const_int()
        assert isinstance(v, int)
        assert v == I80_VALUE
        wb = node.wide_const_bytes()
        assert wb is not None
        raw = bytes(wb)
        assert len(raw) == 10  # 80-bit x87 extended = 10 bytes
        assert v == int.from_bytes(raw, "little")


# There is no I256/I512 test because neither width is reachable with the
# bundled x86-64 Sleigh spec: `vmovdqa ymm0, [abs]` trips the wide-container
# guard (ymm0 sits inside the tracked zmm0), and `vmovdqa64 zmm0, [abs]`
# lifts to an unclassified CallOther, so no 64-byte Load is minted.
# `const_int()` / `const_uint()` stop at 128 bits and return None above it;
# use `wide_const_bytes()` there.  The wider byte serialisation is unit
# tested in strider-ir.


def test_small_const_node_const_int_exact_value():
    # Regression guard: a plain I64 constant still surfaces its exact value.
    # `mov rax, imm64; ret` keeps it live through the return-value register.
    value = 0x1122_3344_5566_7788
    code = bytes([0x48, 0xB8]) + value.to_bytes(8, "little") + bytes([0xC3])
    mem = strider.reader.BufferReader(0x1000, code)
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem, rom=mem)
    _cfg, f, _unresolved = lift.analyze(
        0x1000, strider.sleigh.CallingConvention.x86_64_systemv(),
        opts=strider.lift.LifterOptions(cfg=strider.cfg.CfgOptions(function_max_size=len(code))),
    )
    consts = [
        n
        for n in f.node_ids()
        if f.node(n).kind().startswith("IntConst") and f.node(n).const_int() == value
    ]
    assert consts, "expected the imm64 IntConst to surface its exact value"
    # Small constants have no wide-bytes representation.
    assert f.node(consts[0]).wide_const_bytes() is None
