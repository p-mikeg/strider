"""`load()/store().stack_offset(n)` pins an access to a known SP offset."""

from strider.pattern import Pat, load, store

from .conftest import built_lifter_and_function


def test_load_store_stack_offset_field_compiles():
    assert isinstance(load().stack_offset(0).into_pat(), Pat)
    assert isinstance(store().stack_offset(8).into_pat(), Pat)


def test_load_stack_offset_filters():
    _lift, fn = built_lifter_and_function("x86", "memory", "array_sum")
    # A wildly-out-of-range SP offset should match nothing.
    hits = fn.find_all(load().stack_offset(0x7FFF_FFFF))
    assert hits == []
