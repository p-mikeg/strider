import re

import pytest

import strider
from strider.pattern import Capture, anything, var, int_add, int_const, int_mul, int_shl
from strider import template as tpl

from .conftest import built_function, built_lifter_and_function

_RHS_CAPTURE = r"anything but var\(\) / anything\(\)"


def _build_graph(symbol="array_sum"):
    return built_function("x86", "memory", symbol, optimize=False)


def test_rewrite_returns_fire_count():
    g = _build_graph()
    x, y = Capture(), Capture()
    before = len(g.find_all(int_add(var(x), var(y))))
    assert before > 0
    n = g.rewrite(find=int_add(var(x), var(y)), replace=var(x))
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
            (int_add(var(a), var(b)), var(a)),
            (int_mul(var(c), var(d)), var(c)),
        ]
    )
    assert n > 0
    assert g.find_all(int_mul(var(c), var(d))) == []


def test_rewrite_then_reoptimize():
    lift, g = built_lifter_and_function("x86", "memory", "array_sum", optimize=False)
    x, y = Capture(), Capture()
    assert g.rewrite(find=int_add(var(x), var(y)), replace=var(x)) > 0
    lift.optimize(g)
    assert g.node_count() > 0


def test_rewrite_takes_template():
    """`replace` accepts a `strider.template` build expression, not just a
    bare `strider.pattern.Pat`."""
    g = _build_graph()
    c, d = Capture(), Capture()
    n = g.rewrite(find=int_add(var(c), var(d)), replace=tpl.var(c))
    assert n > 0


def test_rewrite_with_nested_template_build():
    """`replace` composes purely from `strider.template` constructors, with
    no `strider.pattern.Pat` anywhere on the RHS."""
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


def test_match_only_pat_rejected_as_replace():
    """A match-only `Pat` (e.g. a wildcard) is still rejected as a rewrite
    RHS: the back-compat `Pat` path runs the same build-valid-subset check
    that `strider.template` enforces by type."""
    g = _build_graph()
    x = Capture()
    with pytest.raises(strider.StriderError):
        g.rewrite(find=int_add(var(x), int_const(0)), replace=strider.pattern.anything())


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


def _x_times_4():
    """`array_sum` scales its index by 4 at three sites."""
    g = _build_graph()
    x = Capture()
    assert len(g.find_all(int_mul(var(x), int_const(4)))) == 3
    return g, x


def test_capture_over_a_composite_rhs_raises():
    """On a RHS a capture stands for the value it matched, so it would replace
    the composite it wraps: every `x*4` became a plain `x`."""
    g, x = _x_times_4()
    with pytest.raises(strider.StriderError, match=_RHS_CAPTURE):
        g.rewrite(
            find=int_mul(var(x), int_const(4)),
            replace=int_shl(var(x), int_const(2)).capture(x),
        )
    with pytest.raises(strider.StriderError, match=_RHS_CAPTURE):
        g.rewrite(
            find=int_mul(var(x), int_const(4)),
            replace=tpl.int_add(
                int_shl(var(x), int_const(2)).capture(x), tpl.int_const(0)
            ),
        )
    assert len(g.find_all(int_mul(var(x), int_const(4)))) == 3


def test_capture_over_a_var_rhs_is_the_captured_value():
    g, x = _x_times_4()
    assert g.rewrite(find=int_mul(var(x), int_const(4)), replace=var(x).capture(x)) == 3
    assert g.find_all(int_mul(var(x), int_const(4))) == []


def test_capture_over_anything_rhs_is_the_captured_value():
    """`anything()` has no operands to drop, so `anything().capture(x)` is
    `var(x)`."""
    g, x = _x_times_4()
    assert g.rewrite(find=int_mul(var(x), int_const(4)), replace=anything().capture(x)) == 3
    assert g.find_all(int_mul(var(x), int_const(4))) == []


def test_capture_over_a_constant_or_a_capture_rhs_raises():
    """`int_const` builds a node of its own and a chained capture wraps a
    capture, so neither is a leaf a capture can stand in for."""
    g, x = _x_times_4()
    y = Capture()
    for replace in (int_const(4).capture(x), var(x).capture(y).capture(x)):
        with pytest.raises(strider.StriderError, match=_RHS_CAPTURE):
            g.rewrite(find=int_mul(var(x), int_const(4)), replace=replace)
    assert len(g.find_all(int_mul(var(x), int_const(4)))) == 3


def test_capture_over_another_var_rhs_is_the_captured_value():
    """`var(y).capture(x)` is `x`'s matched value; `y` builds nothing."""
    g, x = _x_times_4()
    y = Capture()
    assert g.rewrite(find=int_mul(var(x), int_const(4)), replace=var(y).capture(x)) == 3
    plain, x = _x_times_4()
    assert plain.rewrite(find=int_mul(var(x), int_const(4)), replace=var(x)) == 3
    assert g.to_text() == plain.to_text()


@pytest.mark.parametrize(
    ("replace", "name"),
    [
        (lambda: anything(), "anything()"),
        (lambda: strider.pattern.one_of([int_const(0), int_const(1)]), "one_of()"),
        (lambda: strider.pattern.float_le(var(Capture()), var(Capture())), "float_le()"),
        (lambda: var(Capture()).when(lambda m: True), ".when()"),
        (lambda: tpl.int_add(anything(), 1), "anything()"),
        (lambda: strider.pattern.first_of([int_const(0), int_const(1)]), "first_of()"),
        (lambda: strider.pattern.any_int_binary(Capture(), anything(), anything()), "any_int_binary()"),
        (lambda: strider.pattern.ret().into_pat(), "a control / variadic builder"),
    ],
    ids=["anything", "one_of", "float_le", "when", "nested", "first_of", "any_int_binary", "control"],
)
def test_a_match_only_rhs_is_refused_by_its_constructor(replace, name):
    """A match-only `Pat` as a RHS, at the top or nested in a `Template`, is
    refused naming the constructor that built it."""
    g = _build_graph()
    x = Capture()
    with pytest.raises(
        strider.StriderError, match=re.escape(f"cannot use {name} as a rewrite RHS")
    ):
        g.rewrite(find=int_add(var(x), int_const(0)), replace=replace())


def test_a_pat_rhs_builds_what_its_template_twin_builds():
    """A build-valid `Pat` RHS and the `Template` spelling it builds one
    graph."""
    x = Capture()
    via_pat, via_template = _build_graph(), _build_graph()
    fired_pat = via_pat.rewrite(
        find=int_mul(var(x), int_const(4)), replace=int_shl(var(x), int_const(2))
    )
    fired_template = via_template.rewrite(
        find=int_mul(var(x), int_const(4)),
        replace=tpl.int_shl(tpl.var(x), tpl.int_const(2)),
    )
    assert fired_pat == fired_template == 3
    assert via_pat.to_text() == via_template.to_text()
