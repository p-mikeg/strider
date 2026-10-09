import pytest

import strider
from strider import pattern as p
from .conftest import lift_bytes as _lift


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


def test_a_raw_int_operand_spans_the_whole_u128_carrier():
    """`IntConst` interns a u128, so an operand reaches 2**128-1. `i128`
    extraction alone stops at 2**127-1, and above that the value reached the
    operand-kind error at query time rather than at construction."""
    fn = _lift(bytes([0x83, 0xC7, 0x05, 0x89, 0xF8, 0xC3]))
    for v in (0, 1, 2**127 - 1, 2**127, 2**128 - 1, -1, -(2**127)):
        fn.find_all(p.int_add(p.anything(), v))  # compiles, may match nothing


def test_an_int_operand_past_the_carrier_is_rejected():
    fn = _lift(bytes([0x83, 0xC7, 0x05, 0x89, 0xF8, 0xC3]))
    with pytest.raises(strider.StriderError):
        fn.find_all(p.int_add(p.anything(), 2**200))
