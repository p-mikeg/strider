"""The `Node` handle and graph traversal API.

`Node` is the single source of truth for per-node reads: `Function` does
not duplicate the id-keyed readers, and `Match`'s value/op readers just
forward onto `Match.node(key)`.  Runs against a real lifted graph
(`x64/arithmetic.elf`) rather than a mock.
"""

from __future__ import annotations

import pytest

import strider
from strider.pattern import Capture, anything, any_int_const, add

from .conftest import fixture_path


def _analyze_add():
    elf = fixture_path("x64", "arithmetic")
    _cfg, function, _unresolved = strider.lift.load_elf(str(elf)).analyze("add")
    return function


def test_function_node_returns_node():
    a = _analyze_add()
    some_id = a.node_ids()[0]
    n = a.node(some_id)
    assert isinstance(n, strider.ir.Node)
    assert n.id == some_id


def test_function_node_invalid_id_raises():
    a = _analyze_add()
    bad = max(a.node_ids()) + 10_000
    with pytest.raises(strider.StriderError):
        a.node(bad)


def test_function_has_no_id_keyed_readers():
    """Reads are single-source on `Node`; the duplicate id-keyed readers
    must not come back on `Function`."""
    a = _analyze_add()
    assert not hasattr(a, "node_kind")
    assert not hasattr(a, "asm_fingerprint")
    assert not hasattr(a, "wide_const_bytes")
    assert not hasattr(a, "call_other_name")


def test_node_kind_is_consistent_across_handles():
    """Two separately-constructed handles for the same id must agree."""
    a = _analyze_add()
    for nid in a.node_ids():
        assert a.node(nid).kind() == a.node(nid).kind()


def test_node_inputs_returns_nodes():
    """`Node.inputs()` returns the producer `Node`s feeding each input
    edge."""
    a = _analyze_add()
    saw_inputs = False
    for nid in a.node_ids():
        n = a.node(nid)
        ins = n.inputs()
        assert isinstance(ins, list)
        for child in ins:
            assert isinstance(child, strider.ir.Node)
            assert isinstance(child.kind(), str)
        if ins:
            saw_inputs = True
    assert saw_inputs, "expected at least one node with inputs in add(a, b)"


def test_node_inputs_map_to_real_producers():
    """Every input `Node`'s id must round-trip through `Function.node`."""
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
    """The value on an IntConst node, `None` on anything else."""
    a = _analyze_add()
    c = Capture()
    const_hits = a.find_all(any_int_const(c))
    if const_hits:
        cnode = const_hits[0].node(c)
        assert cnode is not None
        assert cnode.kind() == "IntConst"
        v = cnode.const_int()
        assert v is None or isinstance(v, int)

    add_hits = a.find_all(add(anything(), anything()))
    assert add_hits
    add_node = a.node(add_hits[0].root)
    assert add_node.const_int() is None


def test_node_const_bool_is_none_on_non_bool():
    a = _analyze_add()
    add_hits = a.find_all(add(anything(), anything()))
    assert add_hits
    add_node = a.node(add_hits[0].root)
    assert add_node.const_bool() is None


def test_node_asm_fingerprint_is_int_list():
    """A list of ints, stable across handles for the same id."""
    a = _analyze_add()
    add_hits = a.find_all(add(anything(), anything()))
    assert add_hits
    nid = add_hits[0].root
    n = a.node(nid)
    fp = n.asm_fingerprint()
    assert isinstance(fp, list)
    assert all(isinstance(x, int) for x in fp)
    assert fp == a.node(nid).asm_fingerprint()
    # An Add lifted from a real add instruction carries >= 1 source addr.
    assert len(fp) >= 1


def test_node_asm_fingerprint_name():
    """`Node.fingerprint` was renamed to `Node.asm_fingerprint`; the old
    name must not be reachable."""
    a = _analyze_add()
    add_hits = a.find_all(add(anything(), anything()))
    assert add_hits
    n = a.node(add_hits[0].root)
    assert isinstance(n.asm_fingerprint(), list)
    assert not hasattr(n, "fingerprint")


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
    """Equal and hash-equal, so `Node` works as a set / dict key."""
    a = _analyze_add()
    nid = a.node_ids()[0]
    n1 = a.node(nid)
    n2 = a.node(nid)
    assert n1 == n2
    assert hash(n1) == hash(n2)
    assert len({n1, n2}) == 1

    other_id = a.node_ids()[-1]
    if other_id != nid:
        assert n1 != a.node(other_id)


def test_match_node_returns_node_for_bound_capture():
    """`add(c, anything())` binds `c` to the left operand's producer."""
    a = _analyze_add()
    c = Capture()
    hits = a.find_all(add(c, anything()))
    assert hits
    child = hits[0].node(c)
    assert isinstance(child, strider.ir.Node)
    assert child.id in set(a.node_ids())


def test_match_node_unbound_capture_returns_none():
    """A capture absent from the match reads back as `None`."""
    a = _analyze_add()
    add_hits = a.find_all(add(anything(), anything()))
    assert add_hits
    never_bound = Capture()
    assert add_hits[0].node(never_bound) is None


def test_function_node_invalid_id_message_names_the_id():
    """The out-of-range error message names the offending id."""
    a = _analyze_add()
    bad = max(a.node_ids()) + 10_000
    with pytest.raises(strider.StriderError, match=f"no node with id {bad}"):
        a.node(bad)


def test_function_node_negative_id_overflows_at_conversion():
    """A negative id fails the unsigned conversion eagerly: OverflowError,
    not StriderError."""
    a = _analyze_add()
    with pytest.raises(OverflowError):
        a.node(-1)
