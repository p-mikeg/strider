import pytest
import strider


def test_read_within_region():
    r = strider.reader.BufferReader(0x1000, b"\x01\x02\x03\x04")
    assert r.read(0x1000, 4) == b"\x01\x02\x03\x04"
    assert r.read(0x1002, 2) == b"\x03\x04"


def test_read_unmapped_returns_none():
    r = strider.reader.BufferReader(0x1000, b"\x00\x01\x02\x03")
    assert r.read(0x2000, 4) is None


def test_read_past_region_edge_truncates():
    r = strider.reader.BufferReader(0x1000, b"\x00\x01\x02\x03")
    assert r.read(0x1002, 8) == b"\x02\x03"


def test_base_plus_len_overflow_rejected():
    with pytest.raises(strider.StriderError):
        strider.reader.BufferReader(0xFFFFFFFFFFFFFFFE, b"\x00\x00\x00\x00")


def test_zero_length_read_within_region_returns_empty_bytes():
    r = strider.reader.BufferReader(0x1000, b"\x01\x02\x03\x04")
    assert r.read(0x1000, 0) == b""


def test_read_ending_exactly_at_last_byte():
    r = strider.reader.BufferReader(0x1000, b"\x01\x02\x03\x04")
    assert r.read(0x1001, 3) == b"\x02\x03\x04"


def test_read_of_last_byte_alone():
    r = strider.reader.BufferReader(0x1000, b"\x01\x02\x03\x04")
    assert r.read(0x1003, 1) == b"\x04"


def test_read_starting_one_past_end_returns_none():
    # One past the region end is unmapped: `None`, not b"" and not a raise.
    r = strider.reader.BufferReader(0x1000, b"\x01\x02\x03\x04")
    assert r.read(0x1004, 1) is None


def test_zero_length_read_outside_region_returns_none():
    # A zero-length read is not a universal no-op: the address must still
    # be mapped, so unmapped ones return None even at len 0.
    r = strider.reader.BufferReader(0x1000, b"\x01\x02\x03\x04")
    assert r.read(0x1004, 0) is None
    assert r.read(0x9999, 0) is None


def test_lifter_reader_returns_the_buffer_reader():
    # `reader()` hands back the exact code source the lifter was built with.
    mem = strider.reader.BufferReader(0x1000, b"\x90\x90\xc3")
    lft = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)
    r = lft.reader()
    assert r is mem
    assert r.read(0x1000, 3) == b"\x90\x90\xc3"


def test_lifter_reader_returns_the_python_mem_reader():
    class MyMem(strider.reader.MemReader):
        def read(self, addr, size):
            return b"\xc3" * size

    m = MyMem()
    lft = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), m)
    assert lft.reader() is m


def test_buffer_reader_read_huge_size_does_not_oom():
    r = strider.reader.BufferReader(0x1000, b"\x01\x02\x03\x04")
    # Must clamp to the mapped region, not attempt an exabyte allocation.
    out = r.read(0x1000, 2 ** 60)
    assert out == b"\x01\x02\x03\x04"


def test_buffer_reader_read_huge_size_unmapped():
    r = strider.reader.BufferReader(0x1000, b"\x01\x02\x03\x04")
    # Unmapped base: clamp must not allocate, returns None.
    assert r.read(0x9000, 2 ** 60) is None
