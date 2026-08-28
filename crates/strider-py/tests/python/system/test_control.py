from __future__ import annotations

from strider import pattern as pat

from ._helpers import (
    analyze,
    count_int_binop,
    count_regions,
    count_pat,
    count_returns,
)


def test_abs_val(arch_id, fixtures_dir):
    g = analyze(arch_id, "control", "abs_val", fixtures_dir=fixtures_dir)
    assert count_pat(g, pat.if_else()) >= 1


def test_max_val(arch_id, fixtures_dir):
    g = analyze(arch_id, "control", "max_val", fixtures_dir=fixtures_dir)
    assert count_pat(g, pat.if_else()) >= 1


def test_clamp(arch_id, fixtures_dir):
    g = analyze(arch_id, "control", "clamp", fixtures_dir=fixtures_dir)
    assert count_pat(g, pat.if_else()) >= 2


def test_select_three(arch_id, fixtures_dir):
    g = analyze(arch_id, "control", "select_three", fixtures_dir=fixtures_dir)
    assert count_pat(g, pat.if_else()) >= 2


def test_sum_to_n(arch_id, fixtures_dir):
    g = analyze(arch_id, "control", "sum_to_n", fixtures_dir=fixtures_dir)
    assert count_regions(g) >= 1, "sum_to_n loop header missing VarPhi"


def test_factorial(arch_id, fixtures_dir):
    g = analyze(arch_id, "control", "factorial", fixtures_dir=fixtures_dir)
    assert count_regions(g) >= 1


def test_count_bits(arch_id, fixtures_dir):
    g = analyze(arch_id, "control", "count_bits", fixtures_dir=fixtures_dir)
    assert count_regions(g) >= 1
    assert count_int_binop(g, "ShiftRight") >= 1


def test_nested_loops(arch_id, fixtures_dir):
    g = analyze(arch_id, "control", "nested_loops", fixtures_dir=fixtures_dir)
    # Two nested loops in the source, but ARM at -O2 sometimes promotes the
    # outer induction variable to a stack slot, leaving one loop header
    # phi. Only >= 1 is portable.
    assert count_regions(g) >= 1


def test_early_return(arch_id, fixtures_dir):
    g = analyze(arch_id, "control", "early_return", fixtures_dir=fixtures_dir)
    assert count_regions(g) >= 1
    # A shared epilogue collapses both source returns into one node, so only
    # presence is assertable.
    assert count_returns(g) >= 1
