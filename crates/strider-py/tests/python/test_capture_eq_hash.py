"""`Capture` value semantics: interning implies equality, not just a shared hash.

`Capture("x")` interns the name, so two `Capture("x")` are the SAME capture.
That must hold for `==` and for set/dict membership, not only for `hash`.
"""

from __future__ import annotations

from strider.pattern import Capture


def test_interned_captures_compare_equal_and_dedup_in_a_set():
    a = Capture("x")
    b = Capture("x")
    assert a == b, "same interned name is the same capture"
    assert hash(a) == hash(b)
    assert len({a, b}) == 1, "a set dedups equal captures"
    assert b in {a: 1}, "dict membership resolves through equality"


def test_distinct_and_anonymous_captures_are_not_equal():
    assert Capture("x") != Capture("y")
    assert Capture() != Capture(), "fresh anonymous captures are unique"
    assert Capture("x") != Capture()
