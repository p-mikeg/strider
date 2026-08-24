"""`Capture("name")` interns the name, tying the handle to the inline string
form so a match reads back by either."""

import pytest

import strider
from strider.pattern import Capture, int_const


def _lift(code: bytes):
    mem = strider.reader.BufferReader(0x1000, code)
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)
    _cfg, fn, _u = lift.analyze(0x1000, strider.sleigh.CallingConvention.x86_64_systemv())
    return fn


def test_named_capture_interns_to_one_variable():
    # Two Capture("off") intern to the same underlying capture (same hash).
    assert hash(Capture("off")) == hash(Capture("off"))
    assert hash(Capture("off")) != hash(Capture("other"))


def test_named_capture_reads_by_handle_and_by_string():
    # add edi, 5 ; mov eax, edi ; ret -> a live const 5.
    fn = _lift(bytes([0x83, 0xC7, 0x05, 0x89, 0xF8, 0xC3]))
    off = Capture("off")
    hits = fn.find_all(int_const(off))
    assert hits
    # The handle and its string name resolve to the same binding.
    assert hits[0].uint(off) == hits[0].uint("off")


def test_reserved_capture_name_raises():
    with pytest.raises(strider.StriderError):
        Capture("_")
    with pytest.raises(strider.StriderError):
        Capture("any_")


def test_capture_repr_shows_name_or_id():
    assert repr(Capture("off")) == "Capture('off')"
    # A fresh capture has no name, so it falls back to its numeric id.
    assert repr(Capture()).startswith("Capture(")
    assert "Capture(Capture" not in repr(Capture())


def test_match_repr_shows_roots_and_bound_captures():
    fn = _lift(bytes([0x83, 0xC7, 0x05, 0x89, 0xF8, 0xC3]))
    off = Capture("off")
    hit = fn.find_all(int_const(off))[0]
    r = repr(hit)
    assert r.startswith("Match(roots=[")
    # The bound capture renders as its named key and hex value.
    assert "Capture('off'): BoundCapture(0x5)" in r


def test_match_repr_empty_dict_when_no_captures():
    fn = _lift(bytes([0x83, 0xC7, 0x05, 0x89, 0xF8, 0xC3]))
    hit = fn.find_all(int_const())[0]
    assert hit.__repr__().endswith(", {})")
