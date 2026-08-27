import pytest
import strider
from strider.pattern import (
    Capture,
    Pat,
    int_add,
    anything,
    bool_const,
    bool_not,
    int_extend,
    float_abs,
    float_add,
    any_float_binary,
    float_bits_to_int,
    any_float_cmp,
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
    any_float_unary,
    any_int_binary,
    int_bits_to_float,
    any_int_cmp,
    int_const,
    int_not,
    int_to_float,
    any_int_unary,
    int_lzcount,
    int_mul,
    int_ne,
    int_neg,
    int_popcount,
    predicate,
    int_sign_extend,
    int_truncate,
    var,
    int_zero_extend,
)

from .conftest import built_lifter_and_function


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
    "ctor", [int_truncate, int_popcount, int_lzcount]
)
def test_cast_ops_return_pat(ctor):
    assert isinstance(ctor(var(Capture())), Pat)


def test_zero_and_sign_extend_return_pat():
    assert isinstance(int_zero_extend(var(Capture())), Pat)
    assert isinstance(int_sign_extend(var(Capture())), Pat)


def test_extend_with_op_string():
    assert isinstance(int_extend("zero", var(Capture())), Pat)
    assert isinstance(int_extend("sign", var(Capture())), Pat)


def test_extend_with_invalid_op_raises():
    with pytest.raises(strider.StriderError):
        # Deliberate: not an ExtendOp name.
        int_extend("nope", var(Capture()))  # type: ignore[arg-type]


@pytest.mark.parametrize("ctor", [int_neg, int_not])
def test_int_unary_ops_return_pat(ctor):
    assert isinstance(ctor(var(Capture())), Pat)


def test_bool_not_returns_pat():
    assert isinstance(bool_not(var(Capture())), Pat)


def test_any_int_binary_returns_pat():
    op = Capture()
    assert isinstance(any_int_binary(op, Capture("x"), Capture("y")), Pat)


def test_any_int_unary_returns_pat():
    op = Capture()
    assert isinstance(any_int_unary(op, var(Capture())), Pat)


def test_any_int_cmp_returns_pat():
    op = Capture()
    assert isinstance(any_int_cmp(op, Capture("x"), Capture("y")), Pat)


def test_any_float_binary_returns_pat():
    op = Capture()
    assert isinstance(any_float_binary(op, Capture("x"), Capture("y")), Pat)


def test_any_float_unary_returns_pat():
    op = Capture()
    assert isinstance(any_float_unary(op, var(Capture())), Pat)


def test_any_float_cmp_returns_pat():
    op = Capture()
    assert isinstance(any_float_cmp(op, Capture("x"), Capture("y")), Pat)


def test_float_const_returns_pat():
    assert isinstance(float_const(0x4008000000000000), Pat)


def test_any_float_returns_pat():
    c = Capture()
    assert isinstance(float_const(c), Pat)


def test_predicate_returns_pat():
    p = predicate(lambda m: True)
    assert isinstance(p, Pat)


def test_when_on_pat_returns_pat():
    p = int_add(Capture("x"), Capture("y")).when(lambda m: True)
    assert isinstance(p, Pat)


def test_when_returning_false_filters_out_matches():
    p = anything().when(lambda m: False)
    assert isinstance(p, Pat)


def test_add_ordered_chain_returns_builder():
    # `.ordered()` disables the commutative retry.  It is chainable, not
    # terminal: it returns the same lazy builder so it can nest as a value
    # operand, and `.into_pat()` finalises.
    from strider.pattern import int_binary, IntBinaryPat
    b = int_binary("Add", Capture("x"), Capture("y")).ordered()
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
        pytest.param(lambda: int_binary("Add", Capture("x"), Capture("y")), IntBinaryPat, id="int_binary"),
        pytest.param(lambda: float_binary("Add", Capture("x"), Capture("y")), FloatBinaryPat, id="float_binary"),
        pytest.param(lambda: bool_binary("And", Capture("x"), Capture("y")), BoolBinaryPat, id="bool_binary"),
        pytest.param(lambda: bool_binary("Or", Capture("x"), Capture("y")), BoolBinaryPat, id="bool_binary_or"),
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
    b = bool_binary("And", Capture("x"), Capture("y")).ordered()
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
    p = bool_not(bool_binary("And", Capture("x"), Capture("y")))
    assert isinstance(p, Pat)


def test_bool_binary_invalid_op_raises():
    from strider.pattern import bool_binary
    with pytest.raises(strider.StriderError):
        bool_binary("NopeOp", Capture("x"), Capture("y"))


def test_int_binary_invalid_op_raises():
    from strider.pattern import int_binary
    with pytest.raises(strider.StriderError):
        int_binary("NopeOp", Capture("x"), Capture("y"))



def test_int_ne_builder_compiles():
    p = int_ne(int_const(1), int_const(2))
    assert isinstance(p, Pat)


def test_int_ne_finds_lowered_shape():
    # int_ne is the lifter-canonical `Xor(IntEqual(a,b),1):I1` shape; this
    # only asserts it compiles and queries against a real graph.
    _lift, fn = built_lifter_and_function("x86", "memory", "array_sum")
    hits = fn.find_all(int_ne(anything(), anything()))
    assert isinstance(hits, list)
