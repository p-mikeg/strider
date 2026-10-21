"""`.ordered()` on a finalized `Pat`, not only on a typed builder.

`int_add(a, b).ordered()` is the spelling users reach for; the builder form
`int_binary("Add", a, b).ordered()` is the one that already worked.
"""

from __future__ import annotations

import re

import pytest

import strider
from strider import pattern as p
from strider import template as tpl
from .conftest import built_lifter_and_function, fixture_path


@pytest.fixture(scope="module")
def add_fn():
    """`arithmetic.c::add`: one `Add` over two distinct `InitialVar`s."""
    _cfg, fn, _unresolved = strider.lift.load_elf(
        str(fixture_path("x64", "arithmetic"))
    ).analyze("add")
    return fn


@pytest.fixture(scope="module")
def mul_add_fn():
    """`patterns.c::mul_then_add`: `a * b + c`."""
    _cfg, fn, _unresolved = strider.lift.load_elf(
        str(fixture_path("x64", "patterns"))
    ).analyze("mul_then_add")
    return fn


def _add_operands(fn):
    node = [i for i in fn.node_ids() if "IntBinaryOp(Add)" in fn.node(i).kind()][0]
    return fn.node(node).inputs()


def test_ordered_pins_the_commutative_retry(add_fn):
    a, b = p.Capture("a"), p.Capture("b")

    both = add_fn.find_all(p.int_add(p.var(a), p.var(b)))
    assert len(both) == 2

    pinned = add_fn.find_all(p.int_add(p.var(a), p.var(b)).ordered())
    assert len(pinned) == 1
    assert pinned[0].node(a) == _add_operands(add_fn)[0]
    assert pinned[0].node(b) == _add_operands(add_fn)[1]


def test_ordered_on_a_non_commutative_op_is_a_no_op():
    _cfg, fn, _unresolved = strider.lift.load_elf(
        str(fixture_path("x64", "arithmetic"))
    ).analyze("shl")
    free = fn.find_all(p.int_shl(p.anything().capture("l"), p.anything().capture("r")))
    pinned = fn.find_all(
        p.int_shl(p.anything().capture("l"), p.anything().capture("r")).ordered()
    )
    assert len(free) >= 1
    assert len(pinned) == len(free)


@pytest.mark.parametrize(
    "pat, name",
    [
        (p.anything(), "anything()"),
        (p.var(p.Capture("v")), "var()"),
        (p.int_const(3), "int_const()"),
        (p.int_const(), "int_const()"),
        (p.one_of([p.int_add(1, 2), p.int_mul(1, 2)]), "one_of()"),
        (p.int_neg(p.anything()), "int_neg()"),
    ],
)
def test_ordered_raises_on_a_shape_with_no_operand_pair(pat, name):
    with pytest.raises(strider.StriderError, match=re.escape(name)):
        pat.ordered()


def test_ordered_composes_with_the_other_chainable_verbs(add_fn):
    a = p.Capture("a")
    operands = _add_operands(add_fn)

    for pat in (
        p.int_add(p.var(a), p.anything()).ordered().capture("root"),
        p.int_add(p.var(a), p.anything()).capture("root").ordered(),
        p.int_add(p.var(a), p.anything()).ordered().of_width(64),
        p.int_add(p.var(a), p.anything()).of_width(64).ordered(),
        p.int_add(p.var(a), p.anything()).ordered().value_ty("i64"),
        p.int_add(p.var(a), p.anything()).value_ty("i64").ordered(),
        p.int_add(p.var(a), p.anything()).ordered().when(lambda m: True),
        p.int_add(p.var(a), p.anything()).when(lambda m: True).ordered(),
    ):
        hits = add_fn.find_all(pat)
        assert len(hits) == 1
        assert hits[0].node(a) == operands[0]


def test_ordered_pins_the_outer_op_only(mul_add_fn):
    a, b, c = p.Capture("a"), p.Capture("b"), p.Capture("c")

    def hits(pat):
        return len(mul_add_fn.find_all(pat, ignore_casts=True))

    # The two bindings differ only in the inner `mul`'s operand order: the
    # outer swap cannot bind, since `c` is not a `mul`.
    assert hits(p.int_add(p.int_mul(p.var(a), p.var(b)), p.var(c))) == 2
    assert hits(p.int_add(p.int_mul(p.var(a), p.var(b)), p.var(c)).ordered()) == 2
    assert hits(p.int_add(p.int_mul(p.var(a), p.var(b)).ordered(), p.var(c))) == 1


@pytest.fixture(scope="module")
def ne_fn():
    """`control.c::factorial` with its one `a < b` grafted into `a != b`: the
    optimised x64 corpus keeps no int `!=` over two distinct values."""
    _cfg, fn, _unresolved = strider.lift.load_elf(
        str(fixture_path("x64", "control"))
    ).analyze("factorial")
    x, y = p.Capture(), p.Capture()
    assert fn.rewrite(find=p.int_slt(p.var(x), p.var(y)), replace=tpl.int_ne(x, y)) == 1
    return fn


def _ne_operands(fn):
    """The `!=`'s comparison operands, in IR slot order."""
    for i in fn.node_ids():
        if "IntBinaryOp(Xor)" not in fn.node(i).kind():
            continue
        for inp in fn.node(i).inputs():
            if "IntCmpOp(Equal)" in inp.kind():
                return inp.inputs()
    raise AssertionError("fixture has no != shape")


def test_ordered_pins_a_lowered_int_ne(ne_fn):
    """`int_ne(a, b)` matches both ways round; `.ordered()` pins it to the
    order the IR holds."""
    a, b = p.Capture("a"), p.Capture("b")

    both = ne_fn.find_all(p.int_ne(p.var(a), p.var(b)))
    assert len(both) == 2

    pinned = ne_fn.find_all(p.int_ne(p.var(a), p.var(b)).ordered())
    assert len(pinned) == 1
    lhs, rhs = _ne_operands(ne_fn)
    assert pinned[0].node(a) == lhs
    assert pinned[0].node(b) == rhs


def test_ordered_pins_a_lowered_int_ne_under_the_chainable_verbs(ne_fn):
    a, b = p.Capture("a"), p.Capture("b")

    def operands():
        return p.var(a), p.var(b)

    for pat in (
        p.int_ne(*operands()).ordered().capture("root"),
        p.int_ne(*operands()).capture("root").ordered(),
        p.int_ne(*operands()).ordered().of_width(1),
        p.int_ne(*operands()).of_width(1).ordered(),
        p.int_ne(*operands()).ordered().when(lambda m: True),
        p.int_ne(*operands()).when(lambda m: True).ordered(),
    ):
        assert len(ne_fn.find_all(pat)) == 1


@pytest.mark.parametrize(
    "pat",
    [
        p.int_ne(p.anything(), p.anything()),
        p.int_le(p.anything(), p.anything()),
        p.int_sle(p.anything(), p.anything()),
        p.float_ne(p.anything(), p.anything()),
        p.float_le(p.anything(), p.anything()),
        p.float_is_nan(p.anything()),
    ],
)
def test_ordered_accepts_the_comparisons_the_caller_writes(pat):
    assert pat.ordered() is not None


def test_ordered_is_a_no_op_where_the_operands_are_already_ordered():
    """`float_is_nan(x)` writes its one operand into both equality slots, so
    `.ordered()` pins an order that already holds."""
    _cfg, fn, _unresolved = strider.lift.load_elf(
        str(fixture_path("x64", "floats"))
    ).analyze("f64_compare")
    pat = p.float_is_nan(p.anything().capture("x"))
    free = fn.find_all(pat, ignore_casts=True)
    assert len(free) >= 1
    assert len(fn.find_all(pat.ordered(), ignore_casts=True)) == len(free)


@pytest.mark.parametrize(
    "symbol, ctor",
    [("sub", p.int_sub), ("shl", p.int_shl), ("udiv", p.int_div), ("smod", p.int_srem)],
)
def test_ordered_is_a_no_op_on_an_unlowered_non_commutative_binary(symbol, ctor):
    _cfg, fn, _unresolved = strider.lift.load_elf(
        str(fixture_path("x64", "arithmetic"))
    ).analyze(symbol)
    pat = ctor(p.anything().capture("l"), p.anything().capture("r"))
    free = fn.find_all(pat)
    assert len(free) >= 1
    assert len(fn.find_all(pat.ordered())) == len(free)


def test_ordered_pins_through_inputs_of_width(add_fn, ne_fn):
    a, b = p.Capture("a"), p.Capture("b")

    pinned = add_fn.find_all(p.inputs_of_width(64, p.int_add(p.var(a), p.var(b))).ordered())
    assert len(pinned) == 1
    assert pinned[0].node(a) == _add_operands(add_fn)[0]

    pinned = ne_fn.find_all(p.bool_inputs(p.int_ne(p.var(a), p.var(b))).ordered())
    assert len(pinned) == 1, "the pin must land on the Equal under the Xor"
    assert [pinned[0].node(a), pinned[0].node(b)] == _ne_operands(ne_fn)


@pytest.fixture(scope="module")
def float_ne_fn():
    """`floats.c::f64_compare` with each `a < b` grafted into `a != b`: the
    corpus lifts no float `!=` over two distinct values."""
    _lift, fn = built_lifter_and_function("x64", "floats", "f64_compare", optimize=False)
    x, y = p.Capture(), p.Capture()
    assert fn.rewrite(find=p.float_lt(p.var(x), p.var(y)), replace=tpl.float_ne(x, y))
    return fn


def _float_ne_sites(fn):
    """`(root, lhs, rhs)` per float `!=` over two distinct values, in IR slot
    order."""
    sites = set()
    for i in fn.node_ids():
        if "IntBinaryOp(Xor)" not in fn.node(i).kind():
            continue
        for inp in fn.node(i).inputs():
            if "FloatCmpOp(Equal)" not in inp.kind():
                continue
            l, r = inp.inputs()
            if l != r:
                sites.add((i, l.id, r.id))
    return sites


@pytest.mark.parametrize(
    "build",
    [
        lambda a, b: p.float_ne(a, b).ordered(),
        lambda a, b: p.float_ne(a, b).ordered().ordered(),
    ],
    ids=["ordered", "ordered-twice"],
)
def test_ordered_pins_a_lowered_float_ne(float_ne_fn, build):
    a, b = p.Capture("a"), p.Capture("b")

    def bindings(pat):
        return {
            (m.roots[0], m.node(a).id, m.node(b).id)
            for m in float_ne_fn.find_all(pat)
            if m.node(a) != m.node(b)
        }

    sites = _float_ne_sites(float_ne_fn)
    assert len(sites) == 2
    swapped = {(root, r, l) for root, l, r in sites}
    assert bindings(p.float_ne(p.var(a), p.var(b))) == sites | swapped
    assert bindings(build(p.var(a), p.var(b))) == sites
