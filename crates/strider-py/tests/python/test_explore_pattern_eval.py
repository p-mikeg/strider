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
