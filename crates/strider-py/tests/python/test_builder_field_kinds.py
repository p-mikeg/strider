"""Every `node_builder!` field kind has to reach the core builder.

The other builder suites mostly assert that a chain COMPILES, which an
`@apply` arm that silently dropped its field would still pass. These pin the
constraint itself: the constrained query must answer differently from the
unconstrained one, so a lost field shows up as a changed count rather than as
nothing at all.
"""

from __future__ import annotations

import gc

import strider
from strider import pattern as p

from .conftest import built_function


def _switch_graph():
    """`dispatch_value` carries five x86 `Load`s, four of them `base + off`."""
    return built_function("x86", "switch", "dispatch_value")


def _clamp_graph():
    """`clamp` carries at least two `If`s."""
    return built_function("x86", "control", "clamp")


def test_pat_field_narrows_the_match():
    g = _switch_graph()
    baseline = len(g.find_all(p.load()))
    assert baseline >= 2
    assert len(g.find_all(p.load().addr(p.int_add(p.anything(), p.anything())))) < baseline


def test_mem_field_narrows_the_match():
    g = _switch_graph()
    assert len(g.find_all(p.load())) >= 1
    # Every load here chains off InitialMemory, so demanding a Store producer
    # must empty the result rather than fall through to the baseline.
    assert g.find_all(p.load().mem(p.store())) == []


def test_an_edge_join_narrows_the_match():
    g = _clamp_graph()
    t = p.Capture("t")

    def iff():
        return p.if_else().capture_true(t)

    assert len(g.find_all([iff(), p.region().any_input(t)])) >= 2
    # No If in `clamp` branches straight into a CallOther.
    assert g.find_all([iff(), p.call_other().ctrl(t)]) == []


def test_scalar_field_narrows_the_match():
    g = _switch_graph()
    baseline = len(g.find_all(p.load()))
    assert baseline >= 1
    widths = {len(g.find_all(p.load().bit_width(w))) for w in (8, 16, 32, 64)}
    assert widths != {baseline}, "bit_width must not leave every load matching"


def test_scalar_inner_field_narrows_the_match():
    g = _switch_graph()
    baseline = len(g.find_all(p.load()))
    assert baseline >= 1
    assert len(g.find_all(p.load().space(strider.sleigh.VnSpace.REGISTER))) < baseline


def test_scalar_clone_field_narrows_the_match():
    # rdtsc; add eax,5; ret -- one CallOther, named `rdtsc`, never `nope`.
    mem = strider.reader.BufferReader(0x1000, b"\x0f\x31\x83\xc0\x05\xc3")
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)
    _cfg, fn, _unresolved = lift.analyze(
        0x1000, strider.sleigh.CallingConvention.x86_64_systemv()
    )
    assert len(fn.find_all(p.call_other())) >= 1
    assert fn.find_all(p.call_other().name("nope")) == []


def test_pattern_field_is_traversed():
    operand = p.anything()
    assert operand in gc.get_referents(p.if_else().cond(operand))


def _live(name: str) -> int:
    gc.collect()
    return sum(1 for o in gc.get_objects() if type(o).__name__ == name)


def test_a_cycle_through_a_pattern_field_is_collectable():
    """`@clear` has to drop the slot `@traverse` reported. The loop runs only
    through `IfPat`, so its own `__clear__` is the one thing that can break
    it: route the cycle through a `list` instead and the list's `tp_clear`
    breaks it whatever the builder does.
    """
    base = _live("IfPat")
    for _ in range(200):
        a = p.if_else()
        # Deliberate: an operand slot stores its operand unchecked until the
        # pattern compiles, so a builder closes the loop.
        a.cond(a)  # type: ignore[arg-type]
        del a
    gc.collect()
    gc.collect()
    assert _live("IfPat") == base
