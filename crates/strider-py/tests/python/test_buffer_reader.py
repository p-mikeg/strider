import pytest
import strider


def test_read_within_region():
    r = strider.BufferReader(0x1000, b"\x01\x02\x03\x04")
    assert r.read(0x1000, 4) == b"\x01\x02\x03\x04"
    assert r.read(0x1002, 2) == b"\x03\x04"


def test_read_unmapped_returns_none():
    r = strider.BufferReader(0x1000, b"\x00\x01\x02\x03")
    assert r.read(0x2000, 4) is None


def test_read_past_region_edge_truncates():
    r = strider.BufferReader(0x1000, b"\x00\x01\x02\x03")
    assert r.read(0x1002, 8) == b"\x02\x03"


def test_base_plus_len_overflow_rejected():
    with pytest.raises(strider.errors.StriderError):
        strider.BufferReader(0xFFFFFFFFFFFFFFFE, b"\x00\x00\x00\x00")
