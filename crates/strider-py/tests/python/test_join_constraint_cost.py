"""Composing join constraints must not deep-copy the operand.

`negate` / `all_of` / `any_of` share their nested constraint rather than
cloning it at every wrap, so an N-deep chain costs O(N) time and memory
rather than O(N^2) (N=32000 is an OOM kill at O(N^2)).  Nesting is bounded
too, so pathological input raises.
"""

import time

import pytest

import strider
from strider import pattern as p
from strider.pattern import constraints as cons


def _chain(depth):
    c = cons.dominates(p.Capture("a"), p.Capture("b"))
    for _ in range(depth):
        c = cons.negate(c)
    return c


def _best_of(depth, chains, rounds=3):
    """Fastest wall time to build `chains` chains of `depth` wraps each."""
    best = float("inf")
    for _ in range(rounds):
        t0 = time.perf_counter()
        for _ in range(chains):
            _chain(depth)
        best = min(best, time.perf_counter() - t0)
    return best


def test_wrap_cost_does_not_grow_with_nesting_depth():
    # Same total number of `negate` calls either side, so a per-wrap cost
    # proportional to the operand's size shows up as a ~5x slower deep run.
    deep = _best_of(500, 200)
    shallow = _best_of(100, 1000)
    assert deep < shallow * 4, f"deep={deep:.4f}s shallow={shallow:.4f}s"


def test_nesting_bound_raises_instead_of_dying():
    with pytest.raises(strider.StriderError, match="nesting too deep"):
        _chain(4000)


def test_all_of_nesting_bound_raises():
    c = _chain(0)
    with pytest.raises(strider.StriderError, match="nesting too deep"):
        for _ in range(4000):
            c = cons.all_of([c])


def test_bounded_chain_still_runs_a_query():
    code = bytes([0x31, 0xC0, 0xC3])  # xor eax, eax; ret
    mem = strider.reader.BufferReader(0x1000, code)
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)
    fn = lift.analyze(0x1000, strider.sleigh.CallingConvention.x86_64_systemv()).function
    a, b = p.Capture("a"), p.Capture("b")
    pats = [p.ret().capture(a), p.ret().capture(b)]
    base = cons.dominates(a, b)
    c = base
    for _ in range(200):
        c = cons.negate(c)
    # An even number of negations is the original relation, so the deep chain
    # must answer exactly as the leaf does.
    assert len(fn.find_all(pats, constraints=[c])) == len(
        fn.find_all(pats, constraints=[base])
    )


def test_shared_operand_expansion_is_bounded():
    """`all_of([c, c])` doubles the materialised size per wrap; the node cap
    catches that where a depth cap alone would not."""
    c = _chain(0)
    with pytest.raises(strider.StriderError, match="more than"):
        for _ in range(64):
            c = cons.all_of([c, c])
