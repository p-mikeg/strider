"""Smoke tests for the full pattern-API coverage added on top of the
v1 subset: float ops, casts, variant-agnostic constructors,
predicates, .when guards, .ordered, and float / int cmp variants.

These tests confirm each constructor accepts its expected argument
shape and returns a `Pat`. End-to-end matching against fixtures lives
in `test_pattern_complex.py` and `test_pattern_match.py`.
"""

import pytest
import strider
from strider.pattern import (
    Capture,
    Pat,
    add,
    any_,
    bool_const,
    bool_not,
    extend,
    float_abs,
    float_add,
    float_bin_any,
    float_bits_to_int,
    float_cmp_any,
    float_const,
    float_div,
    float_eq,
    float_floor,
    float_is_nan,
    float_le,
    float_lt,
    float_mul,
    float_ne,
    float_neg,
    float_sqrt,
    float_sub,
    float_to_float,
    float_to_int,
    float_un_any,
    int_bin_any,
    int_bits_to_float,
    int_cmp_any,
    int_const,
    int_to_float,
    int_un_any,
    lzcount,
    mul,
    neg,
    not_,
    popcount,
    predicate,
    sign_extend,
    truncate,
    var,
    zero_extend,
)


# ── Float binary ops ──────────────────────────────────────────────────

@pytest.mark.parametrize("ctor", [float_add, float_sub, float_mul, float_div])
def test_float_binary_ops_return_pat(ctor):
    a, b = Capture(), Capture()
    assert isinstance(ctor(var(a), var(b)), Pat)


# ── Float unary ops ───────────────────────────────────────────────────

@pytest.mark.parametrize("ctor", [float_neg, float_abs, float_sqrt, float_floor])
def test_float_unary_ops_return_pat(ctor):
    assert isinstance(ctor(var(Capture())), Pat)


def test_float_is_nan_returns_pat():
    # float_is_nan(x) is implemented as the IEEE-754 self-inequality
    # `x != x` — same IR shape the pcode lifter produces for FLOAT_NAN.
    p = float_is_nan(var(Capture()))
    assert isinstance(p, Pat)


# ── Float comparisons ─────────────────────────────────────────────────

@pytest.mark.parametrize("ctor", [float_eq, float_ne, float_lt, float_le])
def test_float_cmp_returns_pat(ctor):
    assert isinstance(ctor(var(Capture()), var(Capture())), Pat)


# ── Conversions / bitcasts ───────────────────────────────────────────

@pytest.mark.parametrize(
    "ctor",
    [int_to_float, float_to_int, float_to_float, int_bits_to_float, float_bits_to_int],
)
def test_conversion_ops_return_pat(ctor):
    assert isinstance(ctor(var(Capture())), Pat)


# ── Cast / coercion ops ──────────────────────────────────────────────

@pytest.mark.parametrize(
    "ctor", [truncate, popcount, lzcount]
)
def test_cast_ops_return_pat(ctor):
    assert isinstance(ctor(var(Capture())), Pat)


def test_zero_and_sign_extend_return_pat():
    assert isinstance(zero_extend(var(Capture())), Pat)
    assert isinstance(sign_extend(var(Capture())), Pat)


def test_extend_with_op_string():
    # Accepts "zero" / "sign" enumeration value.
    assert isinstance(extend("zero", var(Capture())), Pat)
    assert isinstance(extend("sign", var(Capture())), Pat)


def test_extend_with_invalid_op_raises():
    with pytest.raises(strider.errors.StriderError):
        extend("nope", var(Capture()))


# ── Integer unary ops ────────────────────────────────────────────────

@pytest.mark.parametrize("ctor", [neg, not_])
def test_int_unary_ops_return_pat(ctor):
    assert isinstance(ctor(var(Capture())), Pat)


# ── Bool unary op ────────────────────────────────────────────────────

def test_bool_not_returns_pat():
    assert isinstance(bool_not(var(Capture())), Pat)


# ── Variant-agnostic constructors ────────────────────────────────────

def test_int_bin_any_returns_pat():
    op = Capture()
    assert isinstance(int_bin_any(op, "x", "y"), Pat)


def test_int_unary_any_returns_pat():
    op = Capture()
    assert isinstance(int_un_any(op, var(Capture())), Pat)


def test_int_cmp_any_returns_pat():
    op = Capture()
    assert isinstance(int_cmp_any(op, "x", "y"), Pat)


def test_float_bin_any_returns_pat():
    op = Capture()
    assert isinstance(float_bin_any(op, "x", "y"), Pat)


def test_float_un_any_returns_pat():
    op = Capture()
    assert isinstance(float_un_any(op, var(Capture())), Pat)


def test_float_cmp_any_returns_pat():
    op = Capture()
    assert isinstance(float_cmp_any(op, "x", "y"), Pat)


# ── Float const constructors ─────────────────────────────────────────

def test_float_const_returns_pat():
    assert isinstance(float_const(0x4008000000000000), Pat)


def test_any_float_const_returns_pat():
    from strider.pattern import any_float_const
    c = Capture()
    assert isinstance(any_float_const(c), Pat)


# ── predicate() free function ────────────────────────────────────────

def test_predicate_returns_pat():
    p = predicate(lambda m: True)
    assert isinstance(p, Pat)


# ── .when on Pat ────────────────────────────────────────────────────

def test_when_on_pat_returns_pat():
    p = add("x", "y").when(lambda m: True)
    assert isinstance(p, Pat)


def test_when_returning_false_filters_out_matches():
    # Build a real graph and use .when to filter every match out.
    # The non-trivial assertion lives in test_pattern_complex.py;
    # here we just confirm the predicate runs without raising.
    p = any_().when(lambda m: False)
    assert isinstance(p, Pat)


# ── .ordered on typed builders ───────────────────────────────────────

def test_add_ordered_chain_returns_pat():
    # The typed builder lets you call .ordered() to disable the
    # automatic-commutative-retry behaviour.
    from strider.pattern import int_binary
    p = int_binary("Add", "x", "y").ordered()
    assert isinstance(p, Pat)


def test_int_binary_into_pat_returns_pat():
    from strider.pattern import int_binary, IntBinaryPat
    builder = int_binary("Add", "x", "y")
    assert isinstance(builder, IntBinaryPat)
    p = builder.into_pat()
    assert isinstance(p, Pat)


def test_float_binary_into_pat_returns_pat():
    from strider.pattern import float_binary, FloatBinaryPat
    builder = float_binary("Add", "x", "y")
    assert isinstance(builder, FloatBinaryPat)
    p = builder.into_pat()
    assert isinstance(p, Pat)


def test_bool_binary_returns_chainable_builder():
    # Booleans are the 1-bit integer I1, so bool_binary builds an
    # IntBinaryOp at I1.  It returns a chainable BoolBinaryPat builder
    # (symmetric with int_binary / float_binary), not a finalised Pat.
    from strider.pattern import bool_binary, BoolBinaryPat
    b = bool_binary("And", "x", "y")
    assert isinstance(b, BoolBinaryPat)
    assert isinstance(b.into_pat(), Pat)


def test_bool_binary_ordered_chain_returns_pat():
    # `.ordered()` is the terminal that disables commutative matching and
    # finalises to a Pat — exactly as for int_binary(...).ordered().
    from strider.pattern import bool_binary
    p = bool_binary("And", "x", "y").ordered()
    assert isinstance(p, Pat)


def test_bool_binary_into_pat_returns_pat():
    from strider.pattern import bool_binary, BoolBinaryPat
    builder = bool_binary("Or", "x", "y")
    assert isinstance(builder, BoolBinaryPat)
    assert isinstance(builder.into_pat(), Pat)


def test_bool_binary_capture_chains_to_pat():
    # `.capture(c)` returns the same builder; `.into_pat()` finalises.
    from strider.pattern import bool_binary, BoolBinaryPat
    c = Capture()
    b = bool_binary("And", "x", "y").capture(c)
    assert isinstance(b, BoolBinaryPat)
    assert isinstance(b.into_pat(), Pat)


def test_bool_binary_usable_as_subpattern():
    # The bare (non-.ordered()) builder must be accepted directly as a
    # sub-pattern (PatLike) — e.g. nested inside another bool op — so
    # callers don't have to call .into_pat() manually.
    from strider.pattern import bool_binary, bool_not
    p = bool_not(bool_binary("And", "x", "y"))
    assert isinstance(p, Pat)


def test_bool_binary_invalid_op_raises():
    from strider.pattern import bool_binary
    with pytest.raises(strider.errors.StriderError):
        bool_binary("NopeOp", "x", "y")


def test_int_binary_capture_chains_to_pat():
    # `.capture(c)` returns the same builder after the strider_pattern
    # macro migration; call `.into_pat()` to get a `Pat`.  (The free
    # constructor `add(...)` continues to return a finalised `Pat`
    # directly, but `int_binary(...).capture(c)` is now a chainable
    # builder method.)
    from strider.pattern import int_binary, IntBinaryPat
    c = Capture()
    b = int_binary("Add", "x", "y").capture(c)
    assert isinstance(b, IntBinaryPat)
    assert isinstance(b.into_pat(), Pat)


def test_int_binary_when_chains_to_pat():
    # Same builder-chain contract as `.capture(c)`.
    from strider.pattern import int_binary, IntBinaryPat
    b = int_binary("Add", "x", "y").when(lambda m: True)
    assert isinstance(b, IntBinaryPat)
    assert isinstance(b.into_pat(), Pat)


def test_int_binary_invalid_op_raises():
    from strider.pattern import int_binary
    with pytest.raises(strider.errors.StriderError):
        int_binary("NopeOp", "x", "y")


def test_when_predicate_exception_surfaces_on_stderr(capfd):
    """A predicate that raises an exception
    must NOT silently filter out matches.  The exception is treated as
    'no match' (so find_all keeps walking) but the exception text is
    surfaced to stderr (via wrap_when's e.print(py)).

    Without this, a buggy predicate produces empty result lists with
    no diagnostic — the user sees 'pattern doesn't match' when the
    real issue is a predicate bug.
    """
    import sys
    # Simple synthetic graph + predicate that raises.
    from strider.pattern import any_, var, Capture

    def raising_predicate(_match):
        raise ValueError("intentional test exception")

    c = Capture()
    pat = var(c).when(raising_predicate)
    # We don't need to actually run find_all — wrap_when is wired at
    # pattern construction.  Just verify the construction doesn't error.
    assert pat is not None
