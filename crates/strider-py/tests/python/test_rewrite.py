import pytest

import strider
from strider.pattern import Capture, var, int_add, int_const
from strider import template as tpl

from .conftest import built_function, built_lifter_and_function


def _build_graph(symbol="array_sum"):
    return built_function("x86", "memory", symbol, optimize=False)


def test_rewrite_returns_fire_count():
    g = _build_graph()
    x = Capture()
    # May or may not fire; only the return type is pinned.
    n = g.rewrite(find=int_add(var(x), int_const(0)), replace=var(x))
    assert isinstance(n, int)
    assert n >= 0


def test_rewrite_all_returns_fire_count():
    g = _build_graph()
    x, y = Capture(), Capture()
    pairs = [
        (int_add(var(x), int_const(0)), var(x)),
        (int_add(int_const(0), var(y)), var(y)),
    ]
    n = g.rewrite_all(pairs)
    assert isinstance(n, int)


def test_rewrite_then_reoptimize():
    lift, g = built_lifter_and_function("x86", "memory", "array_sum", optimize=False)
    x = Capture()
    g.rewrite(find=int_add(var(x), int_const(0)), replace=var(x))
    lift.optimize(g)
    assert g.node_count() > 0


def test_rewrite_takes_template():
    """`replace` accepts a `strider.template` build expression, not just a
    bare `strider.pattern.Pat`."""
    g = _build_graph()
    c = Capture()
    n = g.rewrite(find=int_add(var(c), int_const(0)), replace=tpl.var(c))
    assert isinstance(n, int)


def test_rewrite_with_nested_template_build():
    """`replace` composes purely from `strider.template` constructors, with
    no `strider.pattern.Pat` anywhere on the RHS."""
    g = _build_graph()
    x = Capture()
    n = g.rewrite(
        find=int_add(var(x), int_const(0)),
        replace=tpl.int_add(tpl.var(x), tpl.int_const(0)),
    )
    assert isinstance(n, int)
    assert n >= 0


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
    x = Capture()
    big = 2 ** 127
    assert int_add(var(x), big) is not None
    g.rewrite(find=int_add(var(x), int_const(0)), replace=tpl.int_add(tpl.var(x), big))


def test_nested_template_string_operand_names_what_was_passed():
    g = _build_graph()
    x = Capture()
    with pytest.raises(strider.StriderError, match="string"):
        g.rewrite(find=int_add(var(x), int_const(0)), replace=tpl.int_add("x", 1))
