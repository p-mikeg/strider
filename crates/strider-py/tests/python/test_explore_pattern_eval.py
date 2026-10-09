"""The explorer's `/pattern?q=` evaluator must not execute arbitrary code.

`eval` with `__builtins__` stripped is not a sandbox: attribute walks such
as `().__class__.__base__.__subclasses__()` reach `os.system`. The server
is localhost-bound, but any local process (or any web page issuing a
`fetch`) can drive it.
"""

from __future__ import annotations

import pytest

import strider
import strider.explore


def _function_and_cfg():
    lift = strider.lift.lifter(
        strider.sleigh.SleighArch.x86_64(),
        strider.reader.BufferReader(0x1000, b"\x48\x8b\x00\x48\x83\xc0\x01\xc3"),
    )
    res = lift.analyze(0x1000, strider.sleigh.CallingConvention.x86_64_systemv())
    return res.function, res.cfg


def _escapes(marker):
    """Expressions, each of which must be rejected before it runs."""
    touch = f"'touch {marker}'"
    return {
        "subclasses_comprehension": (
            "[c for c in ().__class__.__base__.__subclasses__() "
            "if c.__name__=='catch_warnings'][0]()._module."
            f"__builtins__['__import__']('os').system({touch})"
        ),
        "attribute": "load.__class__",
        "method_call": "int_const().capture",
        "subscript": "().__class__.__base__.__subclasses__()[0]",
        "lambda": "(lambda: 1)()",
        "fstring": "f'{1}'",
        "dunder_name": "__import__('os')",
        "unknown_name": "os",
        "starred": "load(*[1])",
        "genexp": "list(c for c in ())",
        "binop": "1 + 1",
    }


@pytest.mark.parametrize("case", sorted(_escapes("/tmp/x")))
def test_escape_shapes_are_rejected_without_side_effect(case, tmp_path):
    marker = tmp_path / "pwned"
    expr = _escapes(marker)[case]
    fn, _cfg = _function_and_cfg()
    with pytest.raises(ValueError) as excinfo:
        strider.explore._run_pattern(fn, expr)
    assert not marker.exists(), f"{case}: expression executed"
    assert str(excinfo.value), f"{case}: rejection carries no message"


def test_legitimate_patterns_still_evaluate():
    fn, _cfg = _function_and_cfg()
    for expr in (
        "initial_var()",
        "load(addr=int_add(initial_var(), int_const()))",
        "ret()",
        "int_const(1)",
        "int_const(value=-1)",
    ):
        hits = strider.explore._run_pattern(fn, expr)
        assert isinstance(hits, list)
        assert all(isinstance(h, int) for h in hits)
    assert strider.explore._run_pattern(fn, "ret()"), "ret() must match"


def test_cfg_address_search_still_works():
    _fn, cfg = _function_and_cfg()
    vis = strider.explore._CfgVisualizer(cfg)
    assert vis.search("0x1000") == {"center": cfg.region_at(0x1000)}


def test_visualize_runs_on_the_thread_that_created_the_target():
    """`visualize` decodes through the Lifter, which is pinned to its
    creating thread. Off-thread this raises rather than killing the thread:
    it used to be a `PanicException`, which `except Exception` never caught."""
    import threading

    _fn, cfg = _function_and_cfg()
    out = {}

    def run():
        try:
            strider.explore._CfgVisualizer(cfg)
        except Exception as e:
            out["err"] = type(e).__name__

    t = threading.Thread(target=run)
    t.start()
    t.join(timeout=30)
    assert not t.is_alive()
    assert out.get("err") == "StriderError", out
    assert strider.explore.shutdown() == []


def test_only_names_a_query_can_use_are_offered():
    """Autocomplete and the evaluator share one name table, so every name it
    offers has to evaluate. A type alias, a protocol or the `constraints`
    submodule does not."""
    names = strider.explore._pattern_names()
    assert "int_add" in names and "load" in names
    # `Capture` names a binding: `var(Capture("b"))` is a query.
    assert "Capture" in names
    for junk in ("PatLike", "ValueLike", "ValueTy", "IntCmpOpName", "NodePat",
                 "Match", "BoundCapture", "FieldPat", "field", "constraints"):
        assert junk not in names, junk

    fn, _cfg = _function_and_cfg()
    with pytest.raises(ValueError, match="unknown pattern builder"):
        strider.explore._run_pattern(fn, "PatLike")
    for name in names:
        if name == "Capture":
            continue
        # A bare builder call either evaluates or names the argument it wants,
        # rather than failing as a pyo3 extraction wall of text.
        try:
            strider.explore._run_pattern(fn, f"{name}()")
        except TypeError as exc:
            assert "missing" in str(exc), f"{name}: {exc}"


def test_the_search_bar_placeholder_is_a_query_that_runs():
    """The first hint a user sees. A builder renamed out from under it is only
    visible when the example is evaluated."""
    import re

    m = re.search(
        r'id="q"[^>]*placeholder="strider pattern, e\.g\.\s*(.*?)"',
        strider.explore._FRONTEND,
        re.S,
    )
    assert m is not None, "the search bar has no placeholder to check"
    fn, _cfg = _function_and_cfg()
    assert strider.explore._run_pattern(fn, m.group(1)) == []
