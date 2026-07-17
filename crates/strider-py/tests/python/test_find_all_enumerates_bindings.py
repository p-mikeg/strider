"""`find_all` must report EVERY distinct binding, not just the first.

A commutative node whose operands both satisfy a captured sub-pattern admits
two valid bindings.  Reporting only one silently makes "one hit => unambiguous"
false, which is the guarantee `find_unique` is built on.

Dedup is by the capture->binding MAP, not by root: two orderings that produce
the SAME map are ONE match; two that produce DIFFERENT maps are two.
"""

import pytest

import strider
from strider import pattern as p


@pytest.fixture(scope="module")
def add_fn():
    lift = strider.load_elf("fixtures/out/x64/arithmetic.elf")
    _cfg, fn, _unresolved = lift.analyze("add")
    return fn


def _add_node(fn):
    return [i for i in fn.node_ids() if "IntBinaryOp(Add)" in fn.node(i).kind()][0]


def test_capture_on_commutative_operand_reports_both_bindings(add_fn):
    """add(anything().capture(k), anything()) binds k to EACH operand."""
    operands = add_fn.node(_add_node(add_fn)).inputs()
    assert len(operands) == 2

    k = p.Capture()
    hits = add_fn.find_all(p.add(p.anything().capture(k), p.anything()))
    assert len(hits) == 2, "both operands are valid bindings for k"

    bound = [h.node(k) for h in hits]
    assert len(set(bound)) == 2, "the two bindings must be distinct"
    # Natural operand ordering is reported before the swapped one.
    assert bound == list(operands)


def test_no_captures_on_commutative_operands_does_not_duplicate(add_fn):
    """A capture-free commutative pattern yields the SAME map both ways => 1."""
    hits = add_fn.find_all(p.add(p.anything(), p.anything()))
    assert len(hits) == 1


def test_identical_operand_binding_dedups_to_one(add_fn):
    """add(var(x), var(x))-shaped: the swap yields an identical map => 1 hit."""
    x = p.Capture()
    # Bind the same capture to both operands: only a single-value operand pair
    # matches, and both orderings then produce the identical map.
    hits = add_fn.find_all(p.add(p.var(x), p.var(x)))
    # The two operands of this `add` are distinct, so identity binding fails.
    assert hits == []


def test_ordered_suppresses_commutative_retry(add_fn):
    """`.ordered()` pins the operand slots => exactly one ordering."""
    k = p.Capture()
    hits = add_fn.find_all(
        p.int_binary("Add", p.anything().capture(k), p.anything()).ordered()
    )
    assert len(hits) == 1
    assert hits[0].node(k) == add_fn.node(_add_node(add_fn)).inputs()[0]


def test_find_all_first_hit_is_the_natural_ordering(add_fn):
    """Enumeration order is deterministic: natural operand order first.

    `find_all(...)[0]` is the idiom that replaced `find_one`, so which
    binding lands at index 0 is now part of the contract, not an accident.
    """
    k = p.Capture()
    hits = add_fn.find_all(p.add(p.anything().capture(k), p.anything()))
    assert hits
    assert hits[0].node(k) == add_fn.node(_add_node(add_fn)).inputs()[0]


def test_find_unique_raises_on_genuine_ambiguity(add_fn):
    """`find_unique` is fail-closed: a second DISTINCT binding must raise."""
    k = p.Capture()
    with pytest.raises(strider.errors.StriderError, match="exactly one match"):
        add_fn.find_unique(p.add(p.anything().capture(k), p.anything()))


def test_find_unique_still_accepts_a_genuinely_unique_match(add_fn):
    """The capture-free pattern is still unambiguous => find_unique succeeds."""
    hit = add_fn.find_unique(p.add(p.anything(), p.anything()))
    assert hit is not None
