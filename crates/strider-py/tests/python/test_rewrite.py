"""Pattern → pattern rewrite tests against a real graph."""

import pytest

import strider
from strider.pattern import Capture, var, add, int_const
from strider import template as tpl

from .conftest import built_function


def _build_graph(symbol="array_sum"):
    return built_function("x86", "memory", symbol, optimize=False)


def test_rewrite_returns_fire_count():
    g = _build_graph()
    x = Capture()
    # Identity-ish rewrite that may or may not fire — just verify the
    # call returns an integer.
    n = g.rewrite(find=add(var(x), int_const(0)), replace=var(x))
    assert isinstance(n, int)
    assert n >= 0


def test_rewrite_all_returns_fire_count():
    g = _build_graph()
    x, y = Capture(), Capture()
    pairs = [
        (add(var(x), int_const(0)), var(x)),
        (add(int_const(0), var(y)), var(y)),
    ]
    n = g.rewrite_all(pairs)
    assert isinstance(n, int)


def test_rewrite_then_reoptimize():
    g = _build_graph()
    x = Capture()
    g.rewrite(find=add(var(x), int_const(0)), replace=var(x))
    g.reoptimize()
    assert g.node_count() > 0


# ── strider.template — the explicit build-side DSL (Task 7) ─────────────


def test_rewrite_takes_template():
    """The Task 7 brief's TDD case: `replace` accepts a `strider.template`
    build expression, not just a bare `strider.pattern.Pat`."""
    g = _build_graph()
    c = Capture()
    n = g.rewrite(find=add(var(c), int_const(0)), replace=tpl.var(c))
    assert isinstance(n, int)


def test_rewrite_with_nested_template_build():
    """`replace` composes purely from `strider.template` constructors —
    no `strider.pattern.Pat` anywhere on the RHS."""
    g = _build_graph()
    x = Capture()
    n = g.rewrite(
        find=add(var(x), int_const(0)),
        replace=tpl.add(tpl.var(x), tpl.int_const(0)),
    )
    assert isinstance(n, int)
    assert n >= 0


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
    """A match-only `Pat` (e.g. a wildcard) is still rejected as a
    rewrite RHS — `strider.template` narrows the type but the back-compat
    `Pat` path still runs through the same build-valid-subset check."""
    g = _build_graph()
    x = Capture()
    with pytest.raises(strider.errors.StriderError):
        g.rewrite(find=add(var(x), int_const(0)), replace=strider.pattern.any_())
