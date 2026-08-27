"""Python objects held by a pyclass must be visible to the cyclic collector.

Without `__traverse__` the collector cannot see the edge, so any cycle
routed through one of these handles leaks for the process lifetime.
`gc.get_referents` calls `tp_traverse`, so it reports exactly what the
collector can see.
"""

from __future__ import annotations

import gc
import weakref

import pytest

import strider
from strider import pattern as p
from strider import template as tpl

from .conftest import fixture_path


def test_pat_when_predicate_is_traversed():
    def guard(m):
        return True

    assert guard in gc.get_referents(p.var(p.Capture()).when(guard))


def test_pat_operands_are_traversed():
    inner = p.anything()
    assert inner in gc.get_referents(p.int_add(inner, 1))


def test_nested_pat_operands_are_traversed():
    """A derived `Pat` reports the `Pat` it wraps, which reports the operand:
    one report per held reference, all the way down."""
    inner = p.anything()
    base = p.int_add(inner, 1)
    assert base in gc.get_referents(base.of_width(32))
    assert inner in gc.get_referents(base)


def test_one_of_arms_are_traversed():
    arm = p.anything()
    assert arm in gc.get_referents(p.one_of([arm, p.int_const()]))


def test_builder_when_predicate_is_traversed():
    def guard(m):
        return True

    assert guard in gc.get_referents(p.call().when(guard))


def test_builder_operands_are_traversed():
    target = p.int_const()
    assert target in gc.get_referents(p.call().target(target))


def test_call_output_parent_is_traversed():
    call = p.call()
    assert call in gc.get_referents(call.output(2))


def test_template_operands_are_traversed():
    inner = tpl.int_const(1)
    assert inner in gc.get_referents(tpl.int_add(inner, 2))


def test_join_constraint_holds_its_operands():
    inner = p.constraints.dominates(p.Capture(), p.Capture())
    assert inner in gc.get_referents(p.constraints.negate(inner))


def test_lifter_options_nested_objects_are_traversed():
    cfg = strider.cfg.CfgOptions()
    pipeline = strider.opt.OptimizerPipeline.default()
    opts = strider.lift.LifterOptions(cfg=cfg, pipeline=pipeline)
    referents = gc.get_referents(opts)
    assert cfg in referents
    assert pipeline in referents


def test_builder_when_cycle_is_collectable():
    """The concrete leak: a predicate closing over the builder it guards."""

    class Sentinel:
        pass

    sentinel = Sentinel()
    ref = weakref.ref(sentinel)
    holder = []
    call = p.call()
    # holder -> call -> predicate -> holder, with the sentinel along for the ride.
    call.when(lambda m, _h=holder, _s=sentinel: bool(_h))
    holder.append(call)
    del sentinel, call, holder
    gc.collect()

    assert ref() is None, "cycle through a builder's .when() predicate leaked"


def test_into_pat_when_cycle_is_collectable():
    """`into_pat` compiles the predicate into an opaque `Pattern`; the builder
    it came from is what keeps that predicate GC-visible."""

    class Sentinel:
        pass

    sentinel = Sentinel()
    ref = weakref.ref(sentinel)
    holder = []
    # holder -> pat -> builder -> predicate -> holder.
    pat = p.call().when(lambda m, _h=holder, _s=sentinel: bool(_h)).into_pat()
    holder.append(pat)
    del sentinel, pat, holder
    gc.collect()

    assert ref() is None, "cycle through a finished pattern's .when() predicate leaked"


def test_into_pat_operand_cycle_is_collectable():
    """The same for an operand sub-pattern carrying the predicate."""

    class Sentinel:
        pass

    sentinel = Sentinel()
    ref = weakref.ref(sentinel)
    holder = []
    operand = p.anything().when(lambda m, _h=holder, _s=sentinel: bool(_h))
    pat = p.store().data(operand).into_pat()
    holder.append(pat)
    del sentinel, operand, pat, holder
    gc.collect()

    assert ref() is None, "cycle through a finished pattern's operand leaked"


def test_a_shared_spine_is_reported_once():
    """`base.capture(..)` shares one repr spine. Reporting an operand once per
    derived `Pat` tells the collector about references that do not exist,
    which is the premature-collection direction."""
    x = p.anything()
    base = p.int_add(x, 1)
    c1 = base.capture("a")
    c2 = base.capture("b")
    reports = sum(1 for owner in (base, c1, c2) for r in gc.get_referents(owner) if r is x)
    assert reports == 1, f"operand reported {reports} times for one held reference"


def test_a_derived_pat_reports_the_pat_it_wraps():
    base = p.int_add(p.anything(), 1)
    assert base in gc.get_referents(base.capture("a"))
    assert base in gc.get_referents(base.of_width(32))
    assert base in gc.get_referents(base.when(lambda m: True))


def test_join_predicate_cycle_is_collectable():
    """`negate()` bakes a second strong handle into a Rust closure, so the
    handle in `operands` alone does not cover it; both are reported."""

    class Pred(p.constraints.JoinPredicate):
        sentinel: object
        cycle: object

        def constraint(self, m):
            return True

    class Sentinel:
        pass

    sentinel = Sentinel()
    ref = weakref.ref(sentinel)
    pred = Pred()
    pred.sentinel = sentinel
    pred.cycle = p.constraints.negate(pred)
    del sentinel, pred
    gc.collect()

    assert ref() is None, "cycle through a JoinPredicate closure leaked"


def test_any_of_predicate_cycle_is_collectable():
    class Pred(p.constraints.JoinPredicate):
        sentinel: object
        cycle: object

        def constraint(self, m):
            return True

    class Sentinel:
        pass

    sentinel = Sentinel()
    ref = weakref.ref(sentinel)
    pred = Pred()
    pred.sentinel = sentinel
    pred.cycle = p.constraints.any_of([pred, p.constraints.all_of([pred])])
    del sentinel, pred
    gc.collect()

    assert ref() is None, "cycle through a nested JoinPredicate closure leaked"


DEEP_CHAIN = """
import strider.pattern as p
from .conftest import fixture_path

x = p.anything()
try:
    for _ in range(50000):
        x = x.of_width(32)
except Exception:
    pass
del x
"""


def test_a_deep_pat_chain_never_crashes_the_interpreter():
    """Unbounded native recursion over the wrapper chain (traverse, drop)
    takes the process down with SIGSEGV; a depth error is fine."""
    import subprocess
    import sys

    r = subprocess.run([sys.executable, "-c", DEEP_CHAIN], capture_output=True)
    assert r.returncode >= 0, f"killed by signal {-r.returncode}: {r.stderr!r}"


class _ReentrantIndex:
    """`__index__` runs arbitrary Python from inside a pattern build: this one
    mutates the builder being compiled, then fails the operand."""

    def __init__(self, builder):
        self.builder = builder

    def __index__(self):
        self.builder.bit_width(32)
        raise ValueError("mid-build")


def _unwind_a_pattern_build():
    builder = p.load()
    # Deliberate: pyo3 reads an operand int through `__index__`, which is
    # where this one re-enters the builder.
    builder.addr(_ReentrantIndex(builder))  # type: ignore[arg-type]
    with pytest.raises(strider.StriderError):
        builder.into_pat()


def test_a_failed_pattern_build_does_not_retain_the_next_when_predicate():
    """The build-time scope collecting `.when()` handles is per pattern. Left
    open by an unwinding build, the next attachment lands in it and the
    predicate is held for the process lifetime."""
    elf = str(fixture_path("x86", "memory"))
    _c, fn, _u = strider.lift.load_elf(elf).analyze("array_sum")

    _unwind_a_pattern_build()

    def guard(m):
        return True

    ref = weakref.ref(guard)
    fn.find_all(p.anything().when(guard))
    del guard
    gc.collect()
    assert ref() is None
