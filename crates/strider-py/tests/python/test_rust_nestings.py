"""A builder nests wherever its Rust twin does: any match slot but a memory
one takes it, and Rust refuses the pairings that can never match."""

import pytest

import strider
from strider import pattern as p

from .conftest import built_function


@pytest.fixture(scope="module")
def clamp():
    return built_function("x86", "control", "clamp")


@pytest.fixture(scope="module")
def array_sum():
    return built_function("x86", "memory", "array_sum")


def test_a_branch_feeds_a_merge(clamp):
    assert clamp.find_all(p.region().any_input(p.if_else()))


def test_a_raw_input_takes_a_memory_producer(array_sum):
    """`Load` input 0 is its memory predecessor."""
    plain = array_sum.find_all(p.load().mem(p.store()))
    assert plain
    assert len(array_sum.find_all(p.load().input(0, p.store()))) == len(plain)


def test_an_into_pat_pattern_nests_as_its_builder(array_sum):
    a = p.Capture()
    plain = array_sum.find_all(p.int_add(p.load(), p.var(a)))
    assert plain
    assert len(array_sum.find_all(p.int_add(p.load().into_pat(), p.var(a)))) == len(plain)


def test_a_memory_producer_in_a_value_slot_is_refused_as_rust_refuses_it(clamp):
    with pytest.raises(strider.StriderError, match="can never match"):
        clamp.find_all(p.int_add(p.store(), 1))  # type: ignore[arg-type]  # the refusal under test


def test_a_memory_slot_takes_a_wildcard(array_sum):
    loads = array_sum.find_all(p.load())
    assert loads
    assert len(array_sum.find_all(p.load().mem(p.anything()))) == len(loads)
    m = p.Capture()
    bound = array_sum.find_all(p.load().mem(p.var(m)))
    assert len(bound) == len(loads)
    assert all(h.has(m) for h in bound)
    assert len(array_sum.find_all(p.load().mem(m))) == len(loads)


def test_a_memory_slot_still_refuses_a_value_shape(array_sum):
    with pytest.raises(strider.StriderError, match="requires a memory producer"):
        array_sum.find_all(p.load().mem(p.int_add(p.anything(), 1)))


@pytest.mark.parametrize(
    "token",
    [
        lambda: p.anything().of_width(8),
        lambda: p.anything().value_ty("i32"),
        lambda: p.one_of([p.store()]).of_width(8),
        lambda: p.var(p.Capture()).bool_valued(),
    ],
    ids=["of_width", "value_ty", "one_of_width", "bool_valued"],
)
def test_a_memory_slot_refuses_a_width_or_type_on_the_token(array_sum, token):
    """A memory token has no value type, so these could never match."""
    with pytest.raises(strider.StriderError, match="requires a memory producer"):
        array_sum.find_all(p.load().mem(token()))


def test_a_memory_slot_takes_a_captured_or_guarded_wildcard(array_sum):
    loads = len(array_sum.find_all(p.load()))
    for token in (p.anything().capture(p.Capture()), p.anything().when(lambda m: True)):
        assert len(array_sum.find_all(p.load().mem(token))) == loads
