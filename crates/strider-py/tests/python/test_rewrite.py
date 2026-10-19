import re

import pytest

import strider
from strider.pattern import Capture, anything, var, int_add, int_const, int_mul
from strider import template as tpl

from .conftest import built_function, built_lifter_and_function


def _build_graph(symbol="array_sum"):
    return built_function("x86", "memory", symbol, optimize=False)


def test_rewrite_returns_fire_count():
    g = _build_graph()
    x, y = Capture(), Capture()
    before = len(g.find_all(int_add(var(x), var(y))))
    assert before > 0
    n = g.rewrite(find=int_add(var(x), var(y)), replace=tpl.var(x))
    assert n > 0
    assert len(g.find_all(int_add(var(x), var(y)))) < before


def test_rewrite_all_returns_fire_count():
    """The count sums every pair, so both rules have to be applied."""
    g = _build_graph()
    a, b, c, d = Capture(), Capture(), Capture(), Capture()
    adds = len(g.find_all(int_add(var(a), var(b))))
    muls = len(g.find_all(int_mul(var(c), var(d))))
    assert adds > 0 and muls > 0
    n = g.rewrite_all(
        [
            (int_add(var(a), var(b)), tpl.var(a)),
            (int_mul(var(c), var(d)), tpl.var(c)),
        ]
    )
    assert n > 0
    assert g.find_all(int_mul(var(c), var(d))) == []


def test_rewrite_then_reoptimize():
    lift, g = built_lifter_and_function("x86", "memory", "array_sum", optimize=False)
    x, y = Capture(), Capture()
    assert g.rewrite(find=int_add(var(x), var(y)), replace=tpl.var(x)) > 0
    lift.optimize(g)
    assert g.node_count() > 0


def test_a_bare_capture_replace_is_its_var():
    """`replace=c` is sugar for `replace=template.var(c)`."""
    x, y = Capture(), Capture()
    bare, spelled = _build_graph(), _build_graph()
    fired = bare.rewrite(find=int_add(var(x), var(y)), replace=x)
    assert fired > 0
    assert spelled.rewrite(find=int_add(var(x), var(y)), replace=tpl.var(x)) == fired
    assert bare.to_text() == spelled.to_text()


def test_rewrite_with_nested_template_build():
    """`replace` composes from `strider.template` constructors."""
    g = _build_graph()
    x, y = Capture(), Capture()
    n = g.rewrite(
        find=int_add(var(x), var(y)),
        replace=tpl.int_add(tpl.var(x), tpl.int_const(0)),
    )
    assert n > 0


def test_template_has_one_int_constant_constructor():
    """`template.int_const` takes the whole signed range, so the build side
    needs no second spelling."""
    assert not hasattr(tpl, "int_const_any_width")
    assert isinstance(tpl.int_const(-50), strider.template.Template)


def test_template_is_a_distinct_type_from_pat():
    """`strider.template.var(c)` and `strider.pattern.var(c)` return
    distinct, non-interchangeable Python types."""
    c = Capture()
    t = tpl.var(c)
    p = var(c)
    assert isinstance(t, strider.template.Template)
    assert not isinstance(t, type(p))
    assert not isinstance(p, strider.template.Template)


@pytest.mark.parametrize(
    "replace",
    [lambda: anything(), lambda: var(Capture()), lambda: int_const(0)],
    ids=["anything", "var", "int_const"],
)
def test_a_pat_is_not_a_replacement(replace):
    """A `strider.pattern.Pat` matches and never builds, so `replace` refuses
    one at conversion, a build-valid one included."""
    g = _build_graph()
    x = Capture()
    with pytest.raises(
        TypeError, match=r"(?s)argument 'replace'.*'Pat' object cannot be converted to 'Template'"
    ):
        g.rewrite(find=int_add(var(x), int_const(0)), replace=replace())  # type: ignore[arg-type]
    with pytest.raises(TypeError, match=r"argument 'pairs'"):
        g.rewrite_all([(int_add(var(x), int_const(0)), replace())])  # type: ignore[list-item]


def test_template_takes_the_same_wide_constants_as_the_match_side():
    """`int_const` interns a u128, so `[2**127, 2**128)` has a carrier; the
    match side already accepts one."""
    g = _build_graph()
    x, y = Capture(), Capture()
    big = 2 ** 127
    assert int_add(var(x), big) is not None
    n = g.rewrite(find=int_add(var(x), var(y)), replace=tpl.int_add(tpl.var(x), big))
    assert n > 0


def test_nested_template_string_operand_names_what_was_passed():
    g = _build_graph()
    x = Capture()
    with pytest.raises(strider.StriderError, match="string"):
        # Deliberate: a bare string is not a template operand.
        g.rewrite(
            find=int_add(var(x), int_const(0)),
            replace=tpl.int_add("x", 1),  # type: ignore[arg-type]
        )


def test_var_rhs_is_the_captured_value():
    """`array_sum` scales its index by 4 at three sites; `x * 4 -> x` leaves
    none."""
    g = _build_graph()
    x = Capture()
    assert len(g.find_all(int_mul(var(x), int_const(4)))) == 3
    assert g.rewrite(find=int_mul(var(x), int_const(4)), replace=tpl.var(x)) == 3
    assert g.find_all(int_mul(var(x), int_const(4))) == []


@pytest.mark.parametrize(
    ("operand", "name"),
    [
        (lambda: anything(), "anything()"),
        (lambda: var(Capture()), "var()"),
        (lambda: int_const(0), "int_const()"),
        (lambda: strider.pattern.one_of([int_const(0), int_const(1)]), "one_of()"),
        (lambda: strider.pattern.float_le(var(Capture()), var(Capture())), "float_le()"),
        (lambda: var(Capture()).when(lambda m: True), ".when()"),
        (lambda: var(Capture()).capture(Capture()), ".capture()"),
        (lambda: strider.pattern.first_of([int_const(0), int_const(1)]), "first_of()"),
        (lambda: strider.pattern.any_int_binary(Capture(), anything(), anything()), "any_int_binary()"),
        (lambda: strider.pattern.ret().into_pat(), "a control / variadic builder"),
    ],
    ids=[
        "anything", "var", "int_const", "one_of", "float_le", "when", "capture", "first_of",
        "any_int_binary", "control",
    ],
)
def test_a_pat_nested_in_a_template_is_refused_by_its_constructor(operand, name):
    """A `strider.pattern.Pat` nested in a `Template` operand is refused naming
    the constructor that built it, a build-valid one included, and the graph
    is left as it was."""
    g = _build_graph()
    x = Capture()
    before = g.to_text()
    with pytest.raises(
        strider.StriderError, match=re.escape(f"cannot use {name} as a rewrite RHS")
    ):
        g.rewrite(
            find=int_add(var(x), int_const(0)),
            replace=tpl.int_add(operand(), 1),  # type: ignore[arg-type]
        )
    assert g.to_text() == before
