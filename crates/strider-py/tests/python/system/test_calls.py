"""Per-arch call tests.

Mirror of `crates/strider/tests/calls.rs`: direct, indirect, mutual,
recursive Call nodes.
"""

from __future__ import annotations

from strider import pattern as pat

from ._helpers import analyze, count_calls, count_pat


def test_fib_recursive(arch_id, fixtures_dir):
    g = analyze(arch_id, "calls", "fib_recursive", fixtures_dir=fixtures_dir)
    assert count_calls(g) >= 2, "fib has 2 self-recursive calls"
    assert count_pat(g, pat.if_else()) >= 1


def test_mutual_a(arch_id, fixtures_dir):
    g = analyze(arch_id, "calls", "mutual_a", fixtures_dir=fixtures_dir)
    assert count_calls(g) >= 1
    assert count_pat(g, pat.if_else()) >= 1


def test_mutual_b(arch_id, fixtures_dir):
    g = analyze(arch_id, "calls", "mutual_b", fixtures_dir=fixtures_dir)
    assert count_calls(g) >= 1
    assert count_pat(g, pat.if_else()) >= 1


def test_nested_3deep(arch_id, fixtures_dir):
    g = analyze(arch_id, "calls", "nested_3deep", fixtures_dir=fixtures_dir)
    assert count_calls(g) >= 1


def test_repeat_call_pair(arch_id, fixtures_dir):
    g = analyze(arch_id, "calls", "repeat_call_pair", fixtures_dir=fixtures_dir)
    assert count_calls(g) >= 2


def test_pass_through(arch_id, fixtures_dir):
    g = analyze(arch_id, "calls", "pass_through", fixtures_dir=fixtures_dir)
    assert count_calls(g) >= 1


def test_apply_indirect(arch_id, fixtures_dir):
    g = analyze(arch_id, "calls", "apply_indirect", fixtures_dir=fixtures_dir)
    assert count_calls(g) >= 1
