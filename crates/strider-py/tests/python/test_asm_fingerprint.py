"""``Match.asm_fingerprint``: the proof-of-correctness aid.

Each captured node carries a sorted-deduplicated list of the addresses of
the machine instructions whose lifting contributed to its value.
"""

from __future__ import annotations

from strider.pattern import Capture, anything, any_int_const, int_binary

from .conftest import built_function


def _arithmetic_add_graph():
    return built_function("x86", "arithmetic", "add")


def test_asm_fingerprint_returns_non_empty_for_value_capture():
    """A captured Add's fingerprint must be non-empty.  The exact address
    is compiler/linker dependent, so it is deliberately not pinned.
    """
    g = _arithmetic_add_graph()
    c = Capture()
    pat = int_binary("Add", anything(), anything()).capture(c)
    hits = g.find_all(pat)
    # `add(a, b)` is `return a + b;`, so at least one Add survives; the
    # exact count depends on x86 codegen.
    assert hits, "expected at least one Add match"
    for m in hits:
        fp = m.asm_fingerprint(c)
        assert isinstance(fp, list), f"expected list, got {type(fp)}"
        assert all(isinstance(x, int) for x in fp), f"all entries must be int, got {fp}"
        assert fp, f"value capture's fingerprint must be non-empty: {fp}"
        assert fp == sorted(set(fp)), f"fingerprint must be sorted-deduped: {fp}"


def test_asm_fingerprint_unbound_capture_returns_empty_list():
    """A Capture never declared in the pattern yields an empty list: no
    panic, no None.
    """
    g = _arithmetic_add_graph()
    bound = Capture()
    unbound = Capture()
    hits = g.find_all(int_binary("Add", anything(), anything()).capture(bound))
    assert hits
    for m in hits:
        assert m.asm_fingerprint(unbound) == []


def test_asm_fingerprint_int_const_carries_addr():
    """IntConst is non-exempt and lift-time-attributed, so a captured one
    must carry an asm address.
    """
    g = _arithmetic_add_graph()
    c = Capture()
    hits = g.find_all(any_int_const(c))
    if not hits:
        return  # constants can all fold away; that's fine
    for m in hits:
        fp = m.asm_fingerprint(c)
        assert fp, f"int const capture must have ≥1 fingerprint addr: {fp}"
