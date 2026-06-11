"""Tests for the discoverable `Node` handle + graph traversal API.

Covers `Function.node(id)`, `Match.node(capture)`, and the `Node`
accessors (`id`, `kind()`, `inputs()`, `const_int()`, `const_bool()`,
`fingerprint()`, `__repr__`, `__eq__`/`__hash__`).  Built on the
high-level `strider.load_elf(...).analyze(...)` facade against the
`x64/arithmetic.elf` fixture so the test exercises a real lifted graph.
"""

from __future__ import annotations

import pytest

import strider
from strider.pattern import Capture, any_, any_int_const, add

from .conftest import fixture_path


def _analyze_add():
    elf = fixture_path("x64", "arithmetic")
    return strider.load_elf(str(elf)).analyze("add")


# ── Function.node(id) ──────────────────────────────────────────────────


def test_function_node_returns_node():
    """`Function.node(id)` returns a `Node` for a valid id."""
    a = _analyze_add()
    some_id = a.function.node_ids()[0]
    n = a.function.node(some_id)
    assert isinstance(n, strider.Node)
    assert n.id == some_id


def test_function_node_invalid_id_raises():
    """An out-of-range node id surfaces as `StriderError`."""
    a = _analyze_add()
    bad = max(a.function.node_ids()) + 10_000
    with pytest.raises(strider.errors.StriderError):
        a.function.node(bad)


def test_node_kind_matches_function_node_kind():
    """`Node.kind()` agrees with `Function.node_kind(id)`."""
    a = _analyze_add()
    for nid in a.function.node_ids():
        assert a.function.node(nid).kind() == a.function.node_kind(nid)


def test_node_inputs_returns_nodes():
    """`Node.inputs()` returns a list of `Node`s — the producers feeding
    each input edge.  At least one node in `add(a, b)` (the Add op) has
    inputs."""
    a = _analyze_add()
    saw_inputs = False
    for nid in a.function.node_ids():
        n = a.function.node(nid)
        ins = n.inputs()
        assert isinstance(ins, list)
        for child in ins:
            assert isinstance(child, strider.Node)
            # The child must itself be a valid, kind-readable node.
            assert isinstance(child.kind(), str)
        if ins:
            saw_inputs = True
    assert saw_inputs, "expected at least one node with inputs in add(a, b)"


def test_node_inputs_map_to_real_producers():
    """Every input `Node` is a node whose id round-trips through
    `Function.node`."""
    a = _analyze_add()
    valid_ids = set(a.function.node_ids())
    add_matches = a.find(add(any_(), any_()))
    assert add_matches, "no Add node in add(a, b) — investigate fixture"
    add_node = a.function.node(add_matches[0].root)
    ins = add_node.inputs()
    assert len(ins) >= 2, "an Add node should have >= 2 inputs"
    for child in ins:
        assert child.id in valid_ids


def test_node_const_int_on_int_const():
    """`Node.const_int()` returns the value on an IntConst node and
    `None` on a non-const node."""
    a = _analyze_add()
    c = Capture()
    const_hits = a.find(any_int_const(c))
    if const_hits:
        cnode = const_hits[0].node(c)
        assert cnode is not None
        assert cnode.kind() == "IntConst"
        v = cnode.const_int()
        assert v is None or isinstance(v, int)

    # A non-const node (the Add op) must return None.
    add_hits = a.find(add(any_(), any_()))
    assert add_hits
    add_node = a.function.node(add_hits[0].root)
    assert add_node.const_int() is None


def test_node_const_bool_is_none_on_non_bool():
    """`Node.const_bool()` returns `None` on a non-bool node."""
    a = _analyze_add()
    add_hits = a.find(add(any_(), any_()))
    assert add_hits
    add_node = a.function.node(add_hits[0].root)
    assert add_node.const_bool() is None


def test_node_fingerprint_is_int_list():
    """`Node.fingerprint()` returns a list of ints and agrees with
    `Function.asm_fingerprint(id)`."""
    a = _analyze_add()
    add_hits = a.find(add(any_(), any_()))
    assert add_hits
    nid = add_hits[0].root
    n = a.function.node(nid)
    fp = n.fingerprint()
    assert isinstance(fp, list)
    assert all(isinstance(x, int) for x in fp)
    assert fp == a.function.asm_fingerprint(nid)
    # An Add lifted from a real add instruction carries >= 1 source addr.
    assert len(fp) >= 1


def test_node_repr():
    """`Node.__repr__` is `Node(#<id> <kind>)`."""
    a = _analyze_add()
    add_hits = a.find(add(any_(), any_()))
    assert add_hits
    nid = add_hits[0].root
    n = a.function.node(nid)
    r = repr(n)
    assert r == f"Node(#{nid} {n.kind()})"
    assert r.startswith(f"Node(#{nid} ")


def test_node_eq_and_hash():
    """Two `Node`s on the same function + id are equal and hash-equal."""
    a = _analyze_add()
    nid = a.function.node_ids()[0]
    n1 = a.function.node(nid)
    n2 = a.function.node(nid)
    assert n1 == n2
    assert hash(n1) == hash(n2)
    # Usable as a set/dict key.
    assert len({n1, n2}) == 1

    other_id = a.function.node_ids()[-1]
    if other_id != nid:
        assert n1 != a.function.node(other_id)


# ── Match.node(key) ────────────────────────────────────────────────────


def test_match_node_returns_node_for_bound_capture():
    """`Match.node(capture)` resolves the bound node id to a `Node`."""
    a = _analyze_add()
    c = Capture()
    # `add(c, any_())` binds `c` to the left operand's producer node.
    hits = a.find(add(c, any_()))
    assert hits
    child = hits[0].node(c)
    assert isinstance(child, strider.Node)
    # The bound node id must be a valid node in the function.
    assert child.id in set(a.function.node_ids())


def test_match_node_unbound_capture_returns_none():
    """`Match.node(unbound)` returns `None` for a capture not present in
    the match."""
    a = _analyze_add()
    add_hits = a.find(add(any_(), any_()))
    assert add_hits
    never_bound = Capture()
    assert add_hits[0].node(never_bound) is None


def test_function_node_invalid_id_message_names_the_id():
    """The out-of-range error message names the offending id."""
    a = _analyze_add()
    bad = max(a.function.node_ids()) + 10_000
    with pytest.raises(strider.errors.StriderError, match=f"no node with id {bad}"):
        a.function.node(bad)


def test_function_node_negative_id_overflows_at_conversion():
    """A negative id fails the pyo3 unsigned conversion eagerly —
    OverflowError, not StriderError."""
    a = _analyze_add()
    with pytest.raises(OverflowError):
        a.function.node(-1)
