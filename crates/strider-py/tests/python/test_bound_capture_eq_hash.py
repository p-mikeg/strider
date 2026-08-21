"""`BoundCapture` value comparison and hashability.

`m[c] == v` means the captured constant equals `v` at its width, so it matches
both the signed and unsigned spelling of that value. Because one bound capture
then compares equal to two Python ints with different hashes, it cannot have a
consistent hash and is unhashable.
"""

from __future__ import annotations

import pytest
from strider.pattern import Capture, int_const

from .conftest import built_function


def _bound_neg_const():
    """A `BoundCapture` holding the 32-bit constant -8 (0xFFFFFFF8)."""
    g = built_function("x86", "memory", "array_sum")
    c = Capture()
    for hit in g.find_all(int_const(c)):
        if hit.sint(c) == -8:
            return hit[c]
    raise AssertionError("fixture memory/array_sum must carry a -8 constant")


def test_bound_capture_eq_matches_signed_and_unsigned():
    bc = _bound_neg_const()
    assert bc == -8, "the signed spelling of the value must compare equal"
    assert bc == 0xFFFFFFF8, "the unsigned spelling must also compare equal"
    assert bc != -7


def test_bound_capture_is_unhashable():
    bc = _bound_neg_const()
    with pytest.raises(TypeError):
        hash(bc)
