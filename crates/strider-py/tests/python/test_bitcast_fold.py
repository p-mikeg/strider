"""A bitcast of a constant the pipeline exposes mid-run folds to a constant."""

from __future__ import annotations

from strider import pattern as p

from .conftest import lift_bytes

_MOVD_XMM0_EAX = bytes([0x66, 0x0F, 0x6E, 0xC0])
_ADDSS_XMM0_XMM0 = bytes([0xF3, 0x0F, 0x58, 0xC0])
_MOVD_EAX_XMM0 = bytes([0x66, 0x0F, 0x7E, 0xC0])
_RET = bytes([0xC3])


def _doubled(load_eax: bytes) -> bytes:
    return load_eax + _MOVD_XMM0_EAX + _ADDSS_XMM0_XMM0 + _MOVD_EAX_XMM0 + _RET


def _returns_two(code: bytes) -> bool:
    function = lift_bytes(code)
    return bool(function.find_all(p.ret().ret_val(0, p.int_const(0x4000_0000))))


def test_a_float_from_folded_int_bits_folds():
    # mov eax, 1; add eax, 0x3f7fffff: eax is 1.0f only once the add folds.
    code = bytes([0xB8, 0x01, 0, 0, 0, 0x05, 0xFF, 0xFF, 0x7F, 0x3F])
    assert _returns_two(_doubled(code)), lift_bytes(_doubled(code)).to_text()


def test_the_bits_of_a_folded_float_fold():
    # mov eax, 0x3f800000: 1.0f + 1.0f is 2.0f, 0x40000000.
    code = bytes([0xB8, 0x00, 0x00, 0x80, 0x3F])
    assert _returns_two(_doubled(code)), lift_bytes(_doubled(code)).to_text()
