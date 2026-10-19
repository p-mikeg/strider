"""Each op has one name, the IR's own: the string `Match.op` / `Node.op()`
reads back is the one string every by-name constructor takes, and a node's
kind names no interner handle."""

import pytest

import strider
from strider import pattern as p
from strider import template as t


def _x86_64(code: bytes):
    mem = strider.reader.BufferReader(0x1000, code)
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)
    return lift.analyze(0x1000, strider.sleigh.CallingConvention.x86_64_systemv()).function


_REFUSED = [
    ("int_binary", "shl"),
    ("int_binary", "add"),
    ("int_binary", "ADD"),
    ("int_cmp", "eq"),
    ("int_cmp", "lt"),
    ("int_cmp", "slt"),
    ("int_cmp", "equal"),
    ("bool_binary", "and"),
    ("float_binary", "mul"),
]


@pytest.mark.parametrize(("fn_name", "name"), _REFUSED)
def test_an_alias_or_another_case_is_refused(fn_name, name):
    with pytest.raises(strider.StriderError, match=f'variant "{name}": expected one of'):
        getattr(p, fn_name)(name, p.anything(), p.anything())


def test_the_template_int_cmp_takes_the_same_names():
    with pytest.raises(strider.StriderError, match='variant "eq": expected one of Equal'):
        t.int_cmp("eq", 1, 2)  # type: ignore[arg-type]


@pytest.mark.parametrize("name", ["zero", "sign", "zero_extend", "sign_extend", "zeroextend"])
def test_extend_takes_the_variant_name_only(name):
    msg = f'unknown ExtendOp variant "{name}": expected one of ZeroExtend, SignExtend'
    with pytest.raises(strider.StriderError, match=msg):
        p.int_extend(name, p.anything())  # type: ignore[arg-type]
    with pytest.raises(strider.StriderError, match=msg):
        t.int_extend(name, 1)  # type: ignore[arg-type]


def test_an_extend_op_read_back_is_the_name_int_extend_takes():
    # movsx eax, bh; movzx edx, bh; ret
    fn = _x86_64(bytes([0x0F, 0xBE, 0xC7, 0x0F, 0xB6, 0xD7, 0xC3]))
    c = p.Capture()
    ops = {m.op(c) for m in fn.find_all(p.int_zero_extend(p.anything()).capture(c))}
    ops |= {m.op(c) for m in fn.find_all(p.int_sign_extend(p.anything()).capture(c))}
    assert ops == {"ZeroExtend", "SignExtend"}
    for op in ops:
        assert fn.find_all(p.int_extend(op, p.anything()))  # type: ignore[arg-type]


def test_an_int_binary_op_read_back_is_the_name_int_binary_takes():
    # rdtsc; ret: the high half is a ShiftRight.
    fn = _x86_64(bytes.fromhex("0f31c3"))
    c = p.Capture()
    hits = fn.find_all(p.any_int_binary(c, p.anything(), p.anything()))
    assert hits
    for m in hits:
        op = m.op(c)
        assert fn.find_all(p.int_binary(op, p.anything(), p.anything()))  # type: ignore[arg-type]


def test_an_interned_payload_renders_bare():
    # rdtsc; ret: a CallOther, its InitialVar operands and an IntConst shift.
    fn = _x86_64(bytes.fromhex("0f31c3"))
    kinds = {fn.node(i).kind() for i in fn.node_ids()}
    assert {"IntConst", "InitialVar", "CallOther"} <= kinds
    assert not [k for k in kinds if k.startswith(("IntConst(", "InitialVar(", "CallOther "))]
