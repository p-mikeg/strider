"""Per-arch complex pattern tests.

Mirror of `crates/strider/tests/complex_patterns.rs` (the high-level
structural assertions the Rust suite makes).  A complete 1:1 port is
out of scope — Python's `Matcher` doesn't yet expose the full
`ignore_casts_mask` or every `IntCmpOp` variant — but we can pin the
load / store / call / if shapes per fixture, which covers the same
"structural pattern survives the optimiser" contract.

Each test runs once per arch via the parametrised `arch_id` fixture.
"""

from __future__ import annotations

from strider import pattern as pat
from strider.pattern import any_, var, Capture

from ._helpers import (
    analyze,
    count_calls,
    count_int_binop,
    count_loads,
    count_pat,
    count_stores,
)


def test_read_struct_fields(arch_id, fixtures_dir):
    g = analyze(arch_id, "complex", "read_struct_fields", fixtures_dir=fixtures_dir)
    assert count_loads(g) >= 3, "≥3 Loads (s->a, s->b, s->c)"


def test_write_struct_fields(arch_id, fixtures_dir):
    g = analyze(arch_id, "complex", "write_struct_fields", fixtures_dir=fixtures_dir)
    assert count_stores(g) >= 3, "≥3 Stores (s->a=, s->b=, s->c=)"


def test_nested_struct_field(arch_id, fixtures_dir):
    g = analyze(arch_id, "complex", "nested_struct_field", fixtures_dir=fixtures_dir)
    # The C source dereferences `outer->inner->x`.  At -O2 most arches
    # collapse the chain via ConstantFold + LoadForward into a
    # single surviving Load (the innermost field read).  Pin ≥1 — the
    # structural property we care about is "the field-read shape
    # survives the optimiser".
    assert count_loads(g) >= 1


def test_bit_test_zero(arch_id, fixtures_dir):
    g = analyze(arch_id, "complex", "bit_test_zero", fixtures_dir=fixtures_dir)
    # The bit-test idiom takes several shapes after optimisation: x86
    # keeps `And`; some compilers strength-reduce to `ShiftRight`; ARM
    # at -O2 emits `tst rN, #imm` which lifts to `IntCarry / IntSless`
    # against zero with no AND.  All paths must end in a branch on the
    # bit-test result, so we anchor on the If.
    assert count_pat(g, pat.if_()) >= 1


def test_if_bit_clear_call(arch_id, fixtures_dir):
    g = analyze(arch_id, "complex", "if_bit_clear_call", fixtures_dir=fixtures_dir)
    # The function calls helper() iff a single bit is clear — at least
    # one If and one Call must survive.
    assert count_pat(g, pat.if_()) >= 1
    assert count_calls(g) >= 1


def test_call_with_field_arg(arch_id, fixtures_dir):
    g = analyze(arch_id, "complex", "call_with_field_arg", fixtures_dir=fixtures_dir)
    assert count_calls(g) >= 1
    assert count_loads(g) >= 1


def test_dispatch_on_flag(arch_id, fixtures_dir):
    g = analyze(arch_id, "complex", "dispatch_on_flag", fixtures_dir=fixtures_dir)
    # 4-arm dispatch with bit tests + indirect calls.
    assert count_pat(g, pat.if_()) >= 1
    assert count_calls(g) >= 1


def test_multi_arg_call_in_branch(arch_id, fixtures_dir):
    g = analyze(arch_id, "complex", "multi_arg_call_in_branch", fixtures_dir=fixtures_dir)
    assert count_pat(g, pat.if_()) >= 1
    assert count_calls(g) >= 1


def test_complex_dispatch(arch_id, fixtures_dir):
    g = analyze(arch_id, "complex", "complex_dispatch", fixtures_dir=fixtures_dir)
    assert count_pat(g, pat.if_()) >= 1
    assert count_calls(g) >= 1


def test_call_uses_call_return(arch_id, fixtures_dir):
    g = analyze(arch_id, "complex", "call_uses_call_return", fixtures_dir=fixtures_dir)
    # Two calls; the second's arg is the first's return value.
    assert count_calls(g) >= 2
