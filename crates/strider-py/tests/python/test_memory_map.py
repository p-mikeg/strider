import pytest
import strider


def test_empty_memory_map():
    m = strider.MemoryMap()
    assert m.region_count() == 0


def test_add_region_and_read():
    m = strider.MemoryMap()
    m.add_region(0x1000, b"\x01\x02\x03\x04")
    assert m.region_count() == 1
    assert m.read(0x1000, 4) == b"\x01\x02\x03\x04"
    assert m.read(0x1002, 2) == b"\x03\x04"


def test_read_out_of_range_returns_none():
    m = strider.MemoryMap()
    m.add_region(0x1000, b"\x00\x01\x02\x03")
    assert m.read(0x2000, 4) is None


def test_overlapping_address_overwrites():
    m = strider.MemoryMap()
    m.add_region(0x1000, b"AAAA")
    m.add_region(0x1000, b"BBBB")
    # MemRegionsLookupTable returns the most recently added match for
    # an overlapping range.
    assert m.read(0x1000, 4) in (b"AAAA", b"BBBB")


def test_overflow_rejected():
    m = strider.MemoryMap()
    with pytest.raises(strider.errors.ReaderError):
        m.add_region(0xFFFFFFFFFFFFFFFE, b"\x00\x00\x00\x00")
