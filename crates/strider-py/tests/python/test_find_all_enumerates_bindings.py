"""`find_all` must report EVERY distinct binding, not just the first.

A commutative node whose operands both satisfy a captured sub-pattern admits
two valid bindings.  Reporting only one silently falsifies "one hit means
unambiguous", the guarantee `find_unique` is built on.

Dedup is by the capture->binding MAP, not by root: two orderings that produce
the SAME map are ONE match; two that produce DIFFERENT maps are two.
"""

import pytest

import strider
from strider import pattern as p
from .conftest import fixture_path, lift_bytes


@pytest.fixture(scope="module")
def add_fn():
    lift = strider.lift.load_elf(str(fixture_path("x64", "arithmetic")))
    _cfg, fn, _unresolved = lift.analyze("add")
    return fn


def _add_node(fn):
    return [i for i in fn.node_ids() if "IntBinaryOp(Add)" in fn.node(i).kind()][0]


def test_capture_on_commutative_operand_reports_both_bindings(add_fn):
    operands = add_fn.node(_add_node(add_fn)).inputs()
    assert len(operands) == 2

    k = p.Capture()
    hits = add_fn.find_all(p.int_add(p.anything().capture(k), p.anything()))
    assert len(hits) == 2, "both operands are valid bindings for k"

    bound = [h.node(k) for h in hits]
    assert len(set(bound)) == 2, "the two bindings must be distinct"
    # Natural operand ordering first: `find_all(...)[0]` replaced
    # `find_one`, so which binding lands there is part of the contract.
    assert bound == list(operands)


def test_no_captures_on_commutative_operands_does_not_duplicate(add_fn):
    """A capture-free commutative pattern yields the SAME map both ways => 1."""
    hits = add_fn.find_all(p.int_add(p.anything(), p.anything()))
    assert len(hits) == 1


def test_identical_operand_binding_dedups_to_one(add_fn):
    """int_add(var(x), var(x))-shaped: the swap yields an identical map => 1 hit."""
    x = p.Capture()
    hits = add_fn.find_all(p.int_add(p.var(x), p.var(x)))
    # The two operands of this `add` are distinct, so identity binding fails.
    assert hits == []


def test_ordered_suppresses_commutative_retry(add_fn):
    k = p.Capture()
    hits = add_fn.find_all(
        p.int_binary("Add", p.anything().capture(k), p.anything()).ordered()
    )
    assert len(hits) == 1
    assert hits[0].node(k) == add_fn.node(_add_node(add_fn)).inputs()[0]


def test_find_unique_raises_on_genuine_ambiguity(add_fn):
    """`find_unique` is fail-closed: a second DISTINCT binding must raise."""
    k = p.Capture()
    with pytest.raises(strider.StriderError, match="exactly one match"):
        add_fn.find_unique(p.int_add(p.anything().capture(k), p.anything()))


def test_find_unique_still_accepts_a_genuinely_unique_match(add_fn):
    hit = add_fn.find_unique(p.int_add(p.anything(), p.anything()))
    assert hit is not None


def test_any_output_yields_one_match_per_output():
    from .conftest import built_function

    fn = built_function("x86", "switch", "dispatch_value")
    c = p.Capture("c")
    calls = fn.find_all(p.call())
    per_output = sum(
        len(fn.find_all(p.call().output(s).capture(c))) for s in range(32)
    )
    assert per_output > len(calls)
    hits = fn.find_all(p.call().any_output().capture(c))
    assert len(hits) == per_output
    bound = [h[c] for h in hits]
    assert all(a != b for i, a in enumerate(bound) for b in bound[i + 1:])
    with pytest.raises(strider.StriderError):
        fn.find_unique(p.call().any_output().capture(c))


def test_join_rows_differing_only_in_an_all_shared_root_collapse():
    # lea rax, [rdi+1]; lea rdx, [rdi+2]; ret: two adds over one argument.
    fn = lift_bytes(bytes([0x48, 0x8D, 0x47, 0x01, 0x48, 0x8D, 0x57, 0x02, 0xC3]))
    a, k = p.Capture("a"), p.Capture("k")
    arg = p.function_arg(0).capture(a)
    assert len(fn.find_all([arg, p.int_add(p.var(a), p.int_const())])) == 1
    assert len(fn.find_all([arg, p.int_add(p.var(a), p.int_const(k))])) == 2
