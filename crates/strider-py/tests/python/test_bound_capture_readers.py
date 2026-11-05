"""`m[c].reader()` is `m.reader(c)` for every keyed `Match` reader."""

from __future__ import annotations

from typing import Any, Callable

import pytest

import strider
from strider.pattern import Capture, int_add, int_const, load, var

from .conftest import built_function

#: Keyed `Match` readers, each mirrored on `BoundCapture` without the key.
READERS = [
    f"{name}{suffix}"
    for name in (
        "uint", "sint", "boolean", "float_bits", "op", "value_type", "vn", "node"
    )
    for suffix in ("", "_opt")
] + ["asm_fingerprint"]

KEYS = ["base", "off", "sum", "ld", "nope"]


def _hit():
    """A load of `sum = base + off` on array_sum: `base` binds a var, `off`
    a constant, `sum` the add, `ld` the load, and `nope` nothing."""
    fn = built_function("x86", "memory", "array_sum")
    base, off, ld = Capture("base"), Capture("off"), Capture("ld")
    addr = int_add(var(base), int_const(off)).capture(Capture("sum"))
    pat = load(addr=addr).capture(ld)
    hits = fn.find_all(pat, ignore_casts=True)
    assert hits
    return fn, hits[0]


def _outcome(call: Callable[[], Any]) -> tuple[str, Any]:
    try:
        got = call()
    except strider.StriderError as e:
        return "raise", str(e)
    # A `Node` handle compares by identity; its id is what it names.
    if isinstance(got, strider.ir.Node):
        return "node", got.id
    return "value", got


def test_reader_list_covers_match() -> None:
    """Every public `Match` method taking a key, bar `has` (a `BoundCapture`
    property), is in `READERS`."""
    keyed = {
        n
        for n in dir(strider.pattern.Match)
        if not n.startswith("_") and callable(getattr(strider.pattern.Match, n))
    } - {"has"}
    assert keyed == set(READERS)


@pytest.mark.parametrize("name", READERS)
@pytest.mark.parametrize("by_name", [False, True])
def test_reader_agrees_with_match(name: str, by_name: bool) -> None:
    _fn, hit = _hit()
    outcomes = set()
    for key in KEYS:
        k: Any = key if by_name else Capture(key)
        want = _outcome(lambda: getattr(hit, name)(k))
        assert _outcome(lambda: getattr(hit[k], name)()) == want, (name, key)
        outcomes.add(want[0])
    if name in ("uint", "op", "node", "value_type"):
        # Both branches reached: some key reads, `nope` raises.
        assert outcomes == {"raise", "value" if name != "node" else "node"}


@pytest.mark.parametrize("name", READERS)
def test_stale_reader_raises_like_match(name: str) -> None:
    fn, hit = _hit()
    bound = hit["off"]
    fn.compact()
    with pytest.raises(strider.StriderError) as want:
        getattr(hit, name)("off")
    with pytest.raises(strider.StriderError) as got:
        getattr(bound, name)()
    assert str(got.value) == str(want.value)


def test_readers_read_the_binding() -> None:
    _fn, hit = _hit()
    assert hit["off"].uint() == int(hit["off"])
    assert hit["sum"].op() == "Add"
    assert hit["ld"].asm_fingerprint() != []
    assert hit["nope"].asm_fingerprint() == []
    assert hit["nope"].uint_opt() is None
