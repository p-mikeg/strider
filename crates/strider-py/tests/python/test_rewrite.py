"""Pattern → pattern rewrite tests against a real graph."""

from strider.pattern import Capture, var, add, int_const

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
