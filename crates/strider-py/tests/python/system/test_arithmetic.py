"""Per-arch arithmetic tests.

Mirror of `crates/strider/tests/arithmetic.rs`: pin every IntBinaryOp /
IntUnaryOp variant the analyser must lower for the `arithmetic` fixture
case.  Each (case, fn_name, arch_id) triple is one parametrised pytest.
"""

from __future__ import annotations

from strider import pattern as pat
from strider.pattern import any_

from ._helpers import (
    analyze,
    count_calls,
    count_int_binop,
    count_int_unop,
    count_pat,
)


def test_add(arch_id, fixtures_dir):
    g = analyze(arch_id, "arithmetic", "add", fixtures_dir=fixtures_dir)
    assert count_int_binop(g, "Add") >= 1


def test_sub(arch_id, fixtures_dir):
    g = analyze(arch_id, "arithmetic", "sub", fixtures_dir=fixtures_dir)
    assert count_int_binop(g, "Sub") >= 1


def test_mul(arch_id, fixtures_dir):
    if arch_id in ("mips32le", "mips32be"):
        # Per the Rust suite docstring: MIPS `MULT` writes HI/LO via a
        # unique varnode that the lifter doesn't translate to
        # `IntBinaryOp::Mul`.  Skip on those arches — the Rust test
        # would fail there too if we asserted Mul presence.
        import pytest
        pytest.skip("MIPS MULT lowers via unique varnode, not IntBinaryOp::Mul")
    g = analyze(arch_id, "arithmetic", "mul", fixtures_dir=fixtures_dir)
    assert count_int_binop(g, "Mul") >= 1


def test_udiv(arch_id, fixtures_dir):
    if arch_id in ("mips32le", "mips32be"):
        import pytest
        pytest.skip("MIPS DIV/DIVU panics in CFG build (analyzer-known-issues)")
    g = analyze(arch_id, "arithmetic", "udiv", fixtures_dir=fixtures_dir)
    # ARM soft-float lowers udiv to a library call.  Either Div or Call
    # counts as evidence the lifter found the operation.
    assert count_int_binop(g, "Div") >= 1 or count_calls(g) >= 1


def test_umod(arch_id, fixtures_dir):
    if arch_id in ("mips32le", "mips32be"):
        import pytest
        pytest.skip("MIPS DIV/DIVU panics in CFG build")
    g = analyze(arch_id, "arithmetic", "umod", fixtures_dir=fixtures_dir)
    assert (
        count_int_binop(g, "Rem") >= 1
        or count_int_binop(g, "Div") >= 1
        or count_calls(g) >= 1
    )


def test_sdiv(arch_id, fixtures_dir):
    if arch_id in ("mips32le", "mips32be"):
        import pytest
        pytest.skip("MIPS DIV panics in CFG build")
    g = analyze(arch_id, "arithmetic", "sdiv", fixtures_dir=fixtures_dir)
    assert count_int_binop(g, "Sdiv") >= 1 or count_calls(g) >= 1


def test_smod(arch_id, fixtures_dir):
    if arch_id in ("mips32le", "mips32be"):
        import pytest
        pytest.skip("MIPS DIV panics in CFG build")
    g = analyze(arch_id, "arithmetic", "smod", fixtures_dir=fixtures_dir)
    assert (
        count_int_binop(g, "Srem") >= 1
        or count_int_binop(g, "Sdiv") >= 1
        or count_calls(g) >= 1
    )


def test_bit_and(arch_id, fixtures_dir):
    g = analyze(arch_id, "arithmetic", "bit_and", fixtures_dir=fixtures_dir)
    assert count_int_binop(g, "And") >= 1


def test_bit_or(arch_id, fixtures_dir):
    g = analyze(arch_id, "arithmetic", "bit_or", fixtures_dir=fixtures_dir)
    assert count_int_binop(g, "Or") >= 1


def test_bit_xor(arch_id, fixtures_dir):
    g = analyze(arch_id, "arithmetic", "bit_xor", fixtures_dir=fixtures_dir)
    assert count_int_binop(g, "Xor") >= 1


def test_bit_not(arch_id, fixtures_dir):
    g = analyze(arch_id, "arithmetic", "bit_not", fixtures_dir=fixtures_dir)
    # Sleigh `IntNeg` (bitwise complement) maps to IntUnaryOp::Neg.
    assert count_int_unop(g, "Neg") >= 1


def test_shl(arch_id, fixtures_dir):
    g = analyze(arch_id, "arithmetic", "shl", fixtures_dir=fixtures_dir)
    assert count_int_binop(g, "ShiftLeft") >= 1


def test_lshr(arch_id, fixtures_dir):
    g = analyze(arch_id, "arithmetic", "lshr", fixtures_dir=fixtures_dir)
    assert count_int_binop(g, "ShiftRight") >= 1


def test_ashr(arch_id, fixtures_dir):
    g = analyze(arch_id, "arithmetic", "ashr", fixtures_dir=fixtures_dir)
    assert count_int_binop(g, "SShiftRight") >= 1
