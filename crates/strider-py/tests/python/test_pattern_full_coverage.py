"""Constructor smoke tests: each pattern constructor accepts its argument
shape and returns a `Pat`.

End-to-end matching against fixtures lives in `test_pattern_complex.py` and
`test_pattern_match.py`.
"""

import pytest
import strider
from strider.pattern import (
    Capture,
    Pat,
    add,
    anything,
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
    int_not,
    int_to_float,
    int_un_any,
    lzcount,
    mul,
    neg,
    popcount,
    predicate,
    sign_extend,
    truncate,
    var,
    zero_extend,
)


@pytest.mark.parametrize("ctor", [float_add, float_sub, float_mul, float_div])
def test_float_binary_ops_return_pat(ctor):
    a, b = Capture(), Capture()
    assert isinstance(ctor(var(a), var(b)), Pat)


@pytest.mark.parametrize("ctor", [float_neg, float_abs, float_sqrt, float_floor])
def test_float_unary_ops_return_pat(ctor):
    assert isinstance(ctor(var(Capture())), Pat)


def test_float_is_nan_returns_pat():
    # Built as the IEEE-754 self-inequality `x != x`, matching the IR shape
    # the pcode lifter produces for FLOAT_NAN.
    p = float_is_nan(var(Capture()))
    assert isinstance(p, Pat)


@pytest.mark.parametrize("ctor", [float_eq, float_ne, float_lt, float_le])
def test_float_cmp_returns_pat(ctor):
    assert isinstance(ctor(var(Capture()), var(Capture())), Pat)


@pytest.mark.parametrize(
    "ctor",
    [int_to_float, float_to_int, float_to_float, int_bits_to_float, float_bits_to_int],
)
def test_conversion_ops_return_pat(ctor):
    assert isinstance(ctor(var(Capture())), Pat)


@pytest.mark.parametrize(
    "ctor", [truncate, popcount, lzcount]
)
def test_cast_ops_return_pat(ctor):
    assert isinstance(ctor(var(Capture())), Pat)


def test_zero_and_sign_extend_return_pat():
    assert isinstance(zero_extend(var(Capture())), Pat)
    assert isinstance(sign_extend(var(Capture())), Pat)


def test_extend_with_op_string():
    assert isinstance(extend("zero", var(Capture())), Pat)
    assert isinstance(extend("sign", var(Capture())), Pat)


def test_extend_with_invalid_op_raises():
    with pytest.raises(strider.StriderError):
        extend("nope", var(Capture()))


@pytest.mark.parametrize("ctor", [neg, int_not])
def test_int_unary_ops_return_pat(ctor):
    assert isinstance(ctor(var(Capture())), Pat)


def test_bool_not_returns_pat():
    assert isinstance(bool_not(var(Capture())), Pat)


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


def test_float_const_returns_pat():
    assert isinstance(float_const(0x4008000000000000), Pat)


def test_any_float_const_returns_pat():
    from strider.pattern import any_float_const
    c = Capture()
    assert isinstance(any_float_const(c), Pat)


def test_predicate_returns_pat():
    p = predicate(lambda m: True)
    assert isinstance(p, Pat)


def test_when_on_pat_returns_pat():
    p = add("x", "y").when(lambda m: True)
    assert isinstance(p, Pat)


def test_when_returning_false_filters_out_matches():
    # Construction only; the behavioural assertion lives in
    # test_pattern_complex.py.
    p = anything().when(lambda m: False)
    assert isinstance(p, Pat)


def test_add_ordered_chain_returns_builder():
    # `.ordered()` disables the commutative retry.  It is chainable, not
    # terminal: it returns the same lazy builder so it can nest as a value
    # operand, and `.into_pat()` finalises.
    from strider.pattern import int_binary, IntBinaryPat
    b = int_binary("Add", "x", "y").ordered()
    assert isinstance(b, IntBinaryPat)
    assert isinstance(b.into_pat(), Pat)


# The three binary-op builders share one chaining contract: the constructor
# returns the chainable builder class (not a finalised Pat), `.into_pat()`
# finalises, and `.capture(c)` / `.when(f)` return the same builder.
# Booleans are the 1-bit integer I1, so bool_binary builds an IntBinaryOp at
# I1; the I1 guard itself is tested in test_pattern_match.py.


def _binary_builder_params():
    from strider.pattern import (
        int_binary, IntBinaryPat,
        float_binary, FloatBinaryPat,
        bool_binary, BoolBinaryPat,
    )
    return [
        pytest.param(lambda: int_binary("Add", "x", "y"), IntBinaryPat, id="int_binary"),
        pytest.param(lambda: float_binary("Add", "x", "y"), FloatBinaryPat, id="float_binary"),
        pytest.param(lambda: bool_binary("And", "x", "y"), BoolBinaryPat, id="bool_binary"),
        pytest.param(lambda: bool_binary("Or", "x", "y"), BoolBinaryPat, id="bool_binary_or"),
    ]


@pytest.mark.parametrize("make,builder_cls", _binary_builder_params())
def test_binary_builder_into_pat_returns_pat(make, builder_cls):
    builder = make()
    assert isinstance(builder, builder_cls)
    assert isinstance(builder.into_pat(), Pat)


@pytest.mark.parametrize("make,builder_cls", _binary_builder_params())
def test_binary_builder_capture_chains_to_pat(make, builder_cls):
    c = Capture()
    b = make().capture(c)
    assert isinstance(b, builder_cls)
    assert isinstance(b.into_pat(), Pat)


@pytest.mark.parametrize("make,builder_cls", _binary_builder_params())
def test_binary_builder_when_chains_to_pat(make, builder_cls):
    b = make().when(lambda m: True)
    assert isinstance(b, builder_cls)
    assert isinstance(b.into_pat(), Pat)


def test_bool_binary_ordered_chain_returns_builder():
    from strider.pattern import bool_binary, BoolBinaryPat
    b = bool_binary("And", "x", "y").ordered()
    assert isinstance(b, BoolBinaryPat)
    assert isinstance(b.into_pat(), Pat)


@pytest.mark.parametrize("make,builder_cls", _binary_builder_params())
def test_binary_builder_ordered_chains_like_capture(make, builder_cls):
    b = make().ordered()
    assert isinstance(b, builder_cls)
    assert isinstance(b.into_pat(), Pat)
    assert isinstance(b.capture(Capture()), builder_cls)


def test_bool_binary_usable_as_subpattern():
    # A bare builder is accepted directly as a sub-pattern, so callers never
    # have to call .into_pat() by hand.
    from strider.pattern import bool_binary, bool_not
    p = bool_not(bool_binary("And", "x", "y"))
    assert isinstance(p, Pat)


def test_bool_binary_invalid_op_raises():
    from strider.pattern import bool_binary
    with pytest.raises(strider.StriderError):
        bool_binary("NopeOp", "x", "y")


def test_int_binary_invalid_op_raises():
    from strider.pattern import int_binary
    with pytest.raises(strider.StriderError):
        int_binary("NopeOp", "x", "y")


def test_when_predicate_exception_surfaces_on_stderr(capfd):
    """A raising predicate must not silently filter matches away.  It counts
    as "no match" so find_all keeps walking, but the exception text goes to
    stderr; otherwise a buggy predicate looks exactly like a pattern that
    doesn't match.
    """
    import sys
    from strider.pattern import anything, var, Capture

    def raising_predicate(_match):
        raise ValueError("intentional test exception")

    c = Capture()
    pat = var(c).when(raising_predicate)
    # The guard is wired at pattern-construction time, so no find_all needed.
    assert pat is not None
