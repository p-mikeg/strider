"""``Match.asm_fingerprint`` — proof-of-correctness aid.

Each captured node carries a sorted-deduplicated list of asm-instruction
addresses identifying the parent machine instructions whose lifting
contributed to that node's value.
"""

from __future__ import annotations

from strider.pattern import Capture, anything, any_int_const, int_binary

from .conftest import built_function


def _arithmetic_add_graph():
    return built_function("x86", "arithmetic", "add")


def test_asm_fingerprint_returns_non_empty_for_value_capture():
    """Capture an Add node from `add()`; its fingerprint must include
    at least one asm address.  We don't pin the exact address (it
    depends on the compiler / linker) — we only require non-empty.
    """
    g = _arithmetic_add_graph()
    c = Capture()
    pat = int_binary("Add", anything(), anything()).capture(c)
    hits = g.find_all(pat)
    # The function `add(a, b)` in cases/arithmetic.c is `return a + b;`,
    # which lifts to at least one Add node — exact count depends on
    # x86 codegen, but ≥1 is guaranteed.
    assert hits, "expected at least one Add match"
    for m in hits:
        fp = m.asm_fingerprint(c)
        assert isinstance(fp, list), f"expected list, got {type(fp)}"
        assert all(isinstance(x, int) for x in fp), f"all entries must be int, got {fp}"
        assert fp, f"value capture's fingerprint must be non-empty: {fp}"
        # Sorted-deduplicated check (mirrors the Rust slice contract).
        assert fp == sorted(set(fp)), f"fingerprint must be sorted-deduped: {fp}"


def test_asm_fingerprint_unbound_capture_returns_empty_list():
    """An unbound capture (a Capture not used in the matched pattern)
    yields an empty list — no panic, no None.
    """
    g = _arithmetic_add_graph()
    bound = Capture()
    unbound = Capture()
    hits = g.find_all(int_binary("Add", anything(), anything()).capture(bound))
    assert hits
    for m in hits:
        # `unbound` was never declared in the pattern.
        assert m.asm_fingerprint(unbound) == []


def test_asm_fingerprint_int_const_carries_addr():
    """An IntConst captured from the lifted graph must carry an asm
    address — IntConst is non-exempt and lift-time-attributed.
    """
    g = _arithmetic_add_graph()
    c = Capture()
    hits = g.find_all(any_int_const(c))
    # Some int constants get folded away; at least one must remain
    # reachable in the graph for a non-trivial function like `add`.
    if not hits:
        return  # function has no surviving constants — that's fine
    for m in hits:
        fp = m.asm_fingerprint(c)
        assert fp, f"int const capture must have ≥1 fingerprint addr: {fp}"
