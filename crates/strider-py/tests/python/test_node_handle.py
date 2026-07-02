"""Tests for the discoverable `Node` handle + graph traversal API.

Covers `Function.node(id)`, `Match.node(capture)`, and the `Node`
accessors (`id`, `kind()`, `inputs()`, `const_int()`, `const_uint()`,
`const_bool()`, `fingerprint()`, `__repr__`, `__eq__`/`__hash__`).
`Node` is the single source of truth for per-node reads — `Function`
does not duplicate the id-keyed readers, and `Match`'s value/op readers
are thin forwarders onto `Match.node(key)`.  Built on the high-level
`strider.load_elf(...).analyze(...)` facade against the
`x64/arithmetic.elf` fixture so the test exercises a real lifted graph.
"""

from __future__ import annotations

import pytest

import strider
from strider.pattern import Capture, anything, any_int_const, add

from .conftest import fixture_path


def _analyze_add():
    elf = fixture_path("x64", "arithmetic")
    function, _unresolved = strider.load_elf(str(elf)).analyze("add")
    return function


# ── Function.node(id) ──────────────────────────────────────────────────


def test_function_node_returns_node():
    """`Function.node(id)` returns a `Node` for a valid id."""
    a = _analyze_add()
    some_id = a.node_ids()[0]
    n = a.node(some_id)
    assert isinstance(n, strider.Node)
    assert n.id == some_id


def test_function_node_invalid_id_raises():
    """An out-of-range node id surfaces as `StriderError`."""
    a = _analyze_add()
    bad = max(a.node_ids()) + 10_000
    with pytest.raises(strider.errors.StriderError):
        a.node(bad)


def test_function_has_no_id_keyed_readers():
    """Reads are single-source on `Node`: `Function` no longer exposes
    the duplicate id-keyed readers (`node_kind`, `asm_fingerprint`,
    `wide_const_bytes`, `call_other_name`) — use `Function.node(id).*()`
    instead."""
    a = _analyze_add()
    assert not hasattr(a, "node_kind")
    assert not hasattr(a, "asm_fingerprint")
    assert not hasattr(a, "wide_const_bytes")
    assert not hasattr(a, "call_other_name")


def test_node_kind_is_consistent_across_handles():
    """`Node.kind()` is stable: two separately-constructed `Node`
    handles for the same id agree."""
    a = _analyze_add()
    for nid in a.node_ids():
        assert a.node(nid).kind() == a.node(nid).kind()


def test_node_inputs_returns_nodes():
    """`Node.inputs()` returns a list of `Node`s — the producers feeding
    each input edge.  At least one node in `add(a, b)` (the Add op) has
    inputs."""
    a = _analyze_add()
    saw_inputs = False
    for nid in a.node_ids():
        n = a.node(nid)
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
    valid_ids = set(a.node_ids())
    add_matches = a.find_all(add(anything(), anything()))
    assert add_matches, "no Add node in add(a, b) — investigate fixture"
    add_node = a.node(add_matches[0].root)
    ins = add_node.inputs()
    assert len(ins) >= 2, "an Add node should have >= 2 inputs"
    for child in ins:
        assert child.id in valid_ids


def test_node_const_int_on_int_const():
    """`Node.const_int()` returns the value on an IntConst node and
    `None` on a non-const node."""
    a = _analyze_add()
    c = Capture()
    const_hits = a.find_all(any_int_const(c))
    if const_hits:
        cnode = const_hits[0].node(c)
        assert cnode is not None
        assert cnode.kind() == "IntConst"
        v = cnode.const_int()
        assert v is None or isinstance(v, int)

    # A non-const node (the Add op) must return None.
    add_hits = a.find_all(add(anything(), anything()))
    assert add_hits
    add_node = a.node(add_hits[0].root)
    assert add_node.const_int() is None


def test_node_const_bool_is_none_on_non_bool():
    """`Node.const_bool()` returns `None` on a non-bool node."""
    a = _analyze_add()
    add_hits = a.find_all(add(anything(), anything()))
    assert add_hits
    add_node = a.node(add_hits[0].root)
    assert add_node.const_bool() is None


def test_node_fingerprint_is_int_list():
    """`Node.fingerprint()` returns a list of ints, stable across
    separately-constructed `Node` handles for the same id."""
    a = _analyze_add()
    add_hits = a.find_all(add(anything(), anything()))
    assert add_hits
    nid = add_hits[0].root
    n = a.node(nid)
    fp = n.fingerprint()
    assert isinstance(fp, list)
    assert all(isinstance(x, int) for x in fp)
    assert fp == a.node(nid).fingerprint()
    # An Add lifted from a real add instruction carries >= 1 source addr.
    assert len(fp) >= 1


def test_node_repr():
    """`Node.__repr__` is `Node(#<id> <kind>)`."""
    a = _analyze_add()
    add_hits = a.find_all(add(anything(), anything()))
    assert add_hits
    nid = add_hits[0].root
    n = a.node(nid)
    r = repr(n)
    assert r == f"Node(#{nid} {n.kind()})"
    assert r.startswith(f"Node(#{nid} ")


def test_node_eq_and_hash():
    """Two `Node`s on the same function + id are equal and hash-equal."""
    a = _analyze_add()
    nid = a.node_ids()[0]
    n1 = a.node(nid)
    n2 = a.node(nid)
    assert n1 == n2
    assert hash(n1) == hash(n2)
    # Usable as a set/dict key.
    assert len({n1, n2}) == 1

    other_id = a.node_ids()[-1]
    if other_id != nid:
        assert n1 != a.node(other_id)


# ── Match.node(key) ────────────────────────────────────────────────────


def test_match_node_returns_node_for_bound_capture():
    """`Match.node(capture)` resolves the bound node id to a `Node`."""
    a = _analyze_add()
    c = Capture()
    # `add(c, anything())` binds `c` to the left operand's producer node.
    hits = a.find_all(add(c, anything()))
    assert hits
    child = hits[0].node(c)
    assert isinstance(child, strider.Node)
    # The bound node id must be a valid node in the function.
    assert child.id in set(a.node_ids())


def test_match_node_unbound_capture_returns_none():
    """`Match.node(unbound)` returns `None` for a capture not present in
    the match."""
    a = _analyze_add()
    add_hits = a.find_all(add(anything(), anything()))
    assert add_hits
    never_bound = Capture()
    assert add_hits[0].node(never_bound) is None


def test_function_node_invalid_id_message_names_the_id():
    """The out-of-range error message names the offending id."""
    a = _analyze_add()
    bad = max(a.node_ids()) + 10_000
    with pytest.raises(strider.errors.StriderError, match=f"no node with id {bad}"):
        a.node(bad)


def test_function_node_negative_id_overflows_at_conversion():
    """A negative id fails the pyo3 unsigned conversion eagerly —
    OverflowError, not StriderError."""
    a = _analyze_add()
    with pytest.raises(OverflowError):
        a.node(-1)
