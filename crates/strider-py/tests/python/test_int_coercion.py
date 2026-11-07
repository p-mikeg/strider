import pytest

import strider
from strider import pattern as p
from strider import template as t
from .conftest import fixture_path, lift_bytes as _lift


def test_int_operand_matches_like_int_const():
    # add edi, 5 ; mov eax, edi ; ret -> int_add(edi, 5) keeps a live const 5.
    fn = _lift(bytes([0x83, 0xC7, 0x05, 0x89, 0xF8, 0xC3]))
    coerced = fn.find_all(p.int_add(p.anything(), 5))
    explicit = fn.find_all(p.int_add(p.anything(), p.int_const(5)))
    assert coerced, "the raw int should match the const-5 operand"
    assert len(coerced) == len(explicit)


def test_bare_int_query_is_int_const():
    fn = _lift(bytes([0x83, 0xC7, 0x05, 0x89, 0xF8, 0xC3]))
    assert len(fn.find_all(5)) == len(fn.find_all(p.int_const(5)))


def test_a_raw_int_operand_spans_every_declared_width():
    """An operand reaches the widest `ValueType`, 512 bits, on either side of
    the signed / unsigned split: the value is carried unbounded, not in a
    128-bit word."""
    fn = _lift(bytes([0x83, 0xC7, 0x05, 0x89, 0xF8, 0xC3]))
    for v in (0, 1, 2**127 - 1, 2**127, 2**128 - 1, -1, -(2**127), 2**200, 2**512 - 1, -(2**511)):
        fn.find_all(p.int_add(p.anything(), v))  # compiles, may match nothing


def test_an_int_operand_past_the_widest_value_type_is_rejected():
    """No declared width holds it, and converting it would allocate a byte
    per eight bits, so it is refused on the size alone."""
    fn = _lift(bytes([0x83, 0xC7, 0x05, 0x89, 0xF8, 0xC3]))
    for v in (2**512, -(2**512), 2 ** 10**6):
        with pytest.raises(strider.StriderError, match="512 bits"):
            fn.find_all(p.int_add(p.anything(), v))
        with pytest.raises(strider.StriderError, match="512 bits"):
            p.int_const(v)
        with pytest.raises(strider.StriderError, match="512 bits"):
            p.int_const([1, v])
        with pytest.raises(strider.StriderError, match="512 bits"):
            t.int_const(v)


def test_an_out_of_range_size_or_width_raises_strider_error():
    lifter = strider.lift.load_elf(str(fixture_path("x64", "arithmetic")))
    with pytest.raises(strider.StriderError, match="out of range"):
        lifter.add_symbols({"f": (0x1000, -1)})
    with pytest.raises(strider.StriderError, match="out of range"):
        p.value_of_width(-3)
    with pytest.raises(strider.StriderError, match="out of range"):
        p.inputs_of_width(-3, p.anything())
    with pytest.raises(strider.StriderError, match="out of range"):
        strider.sleigh.Vn(strider.sleigh.VnSpace.REGISTER, 0, -1)
