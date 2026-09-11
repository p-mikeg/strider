"""`Node` is the single source of truth for per-node reads; `Match`'s
value/op readers forward onto `Match.node(key)`.
"""

from __future__ import annotations

import pytest

import strider
from strider.pattern import Capture, anything, int_add, int_const, var

from .conftest import built_function, fixture_path


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
    a = _analyze_add()
    for nid in a.node_ids():
        assert a.node(nid).kind() == a.node(nid).kind()


def test_node_inputs_returns_nodes():
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
    assert saw_inputs, "expected at least one node with inputs in int_add(a, b)"


def test_node_inputs_map_to_real_producers():
    a = _analyze_add()
    valid_ids = set(a.node_ids())
    add_matches = a.find_all(int_add(anything(), anything()))
    assert add_matches, "no Add node in int_add(a, b); investigate the fixture"
    add_node = a.node(add_matches[0].root)
    ins = add_node.inputs()
    assert len(ins) >= 2, "an Add node should have >= 2 inputs"
    for child in ins:
        assert child.id in valid_ids


def test_node_sint_on_int_const():
    a = _analyze_add()
    c = Capture()
    const_hits = a.find_all(int_const(c))
    if const_hits:
        cnode = const_hits[0].node(c)
        assert cnode is not None
        assert cnode.kind() == "IntConst"
        v = cnode.sint()
        assert v is None or isinstance(v, int)

    add_hits = a.find_all(int_add(anything(), anything()))
    assert add_hits
    add_node = a.node(add_hits[0].root)
    assert add_node.sint() is None


def test_node_boolean_is_none_on_non_bool():
    a = _analyze_add()
    add_hits = a.find_all(int_add(anything(), anything()))
    assert add_hits
    add_node = a.node(add_hits[0].root)
    assert add_node.boolean() is None


def test_node_asm_fingerprint_is_int_list():
    a = _analyze_add()
    add_hits = a.find_all(int_add(anything(), anything()))
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
    add_hits = a.find_all(int_add(anything(), anything()))
    assert add_hits
    n = a.node(add_hits[0].root)
    assert isinstance(n.asm_fingerprint(), list)
    assert not hasattr(n, "fingerprint")


def test_node_repr():
    a = _analyze_add()
    add_hits = a.find_all(int_add(anything(), anything()))
    assert add_hits
    nid = add_hits[0].root
    n = a.node(nid)
    r = repr(n)
    assert r == f"Node(#{nid} {n.kind()})"
    assert r.startswith(f"Node(#{nid} ")


def test_node_eq_and_hash():
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
    a = _analyze_add()
    c = Capture()
    hits = a.find_all(int_add(c, anything()))
    assert hits
    child = hits[0].node(c)
    assert isinstance(child, strider.ir.Node)
    assert child.id in set(a.node_ids())


def test_match_node_unbound_capture():
    """An absent capture reads back as `None` via `node_opt`; the plain
    `node` raises."""
    a = _analyze_add()
    add_hits = a.find_all(int_add(anything(), anything()))
    assert add_hits
    never_bound = Capture()
    assert add_hits[0].node_opt(never_bound) is None
    with pytest.raises(strider.StriderError):
        add_hits[0].node(never_bound)


def test_getter_raises_while_opt_returns_none():
    """Plain value getters raise on an unbound capture; the `_opt` form
    returns None."""
    a = _analyze_add()
    hits = a.find_all(int_add(anything(), anything()))
    assert hits
    unbound = Capture()
    assert hits[0].uint_opt(unbound) is None
    with pytest.raises(strider.StriderError):
        hits[0].uint(unbound)


def test_function_node_invalid_id_message_names_the_id():
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


def _stale_and_fresh():
    """A `Node` taken before a graph-bumping rewrite and one taken after, both
    naming the entry node, which no rewrite removes."""
    g = built_function("x86", "memory", "array_sum", optimize=False)
    x, y = Capture(), Capture()
    stale = g.node(g.entry_node())
    assert g.rewrite(find=int_add(var(x), var(y)), replace=var(x)) > 0
    return stale, g.node(g.entry_node())


def test_repr_of_a_stale_node_does_not_raise():
    stale, _fresh = _stale_and_fresh()
    with pytest.raises(strider.StriderError):
        stale.kind()
    assert "%r" % stale


def test_a_stale_node_is_not_equal_to_a_fresh_one():
    stale, fresh = _stale_and_fresh()
    assert stale != fresh
    assert len({stale, fresh}) == 2
