"""An `into_pat()` `Pat` stays usable after a call that refused its
arguments."""

import pytest

import strider
from strider import pattern as p

from .conftest import lift_bytes


def _function():
    return lift_bytes(bytes([0x83, 0xC0, 0x01, 0xC3]))  # add eax, 1; ret


def _guarded_pat():
    return p.entry().when(lambda _m: True).into_pat()


def test_rewrite_refusing_a_when_guard_leaves_the_pattern_usable():
    fn = _function()
    pat = _guarded_pat()
    with pytest.raises(strider.StriderError, match="when"):
        fn.rewrite(pat, strider.template.int_const(0))
    assert fn.find_all(pat) is not None


def test_find_all_refusing_a_constraint_leaves_the_pattern_usable():
    fn = _function()
    pat = p.entry().into_pat()
    with pytest.raises((strider.StriderError, TypeError, ValueError)):
        fn.find_all(pat, constraints=[42])  # type: ignore[arg-type]  # the refusal under test
    assert fn.find_all(pat) is not None


def test_find_unique_refusing_a_constraint_leaves_the_pattern_usable():
    fn = _function()
    pat = p.entry().into_pat()
    with pytest.raises((strider.StriderError, TypeError, ValueError)):
        fn.find_unique(pat, constraints=[42])  # type: ignore[arg-type]  # the refusal under test
    assert fn.find_unique(pat) is not None
