"""A mapping argument is snapshotted before its elements are converted.

Converting a key or value runs arbitrary Python through `__index__`, and PyO3
panics if a dict changes size or keys while it is iterating it. That panic is a
`PanicException`, a `BaseException` no `except Exception` catches, so it tends
to take the interpreter rather than surface as an error.
"""

import types

import strider
from strider.cfg import CfgOptions
from strider.lift import LifterOptions
from strider.sleigh import CallingConvention

from .conftest import fixture_path


class GrowsSource:
    """An `__index__` that mutates the dict it is being read out of."""

    def __init__(self, src, value):
        self._src = src
        self._value = value

    def __index__(self):
        self._src[0x9999] = [1]
        return self._value


def test_known_targets_survives_a_value_that_grows_the_source():
    src = {}
    for i in range(8):
        src[0x1000 + i] = [GrowsSource(src, 0x11)]
    assert dict(CfgOptions(known_targets=src).known_targets)


def test_call_other_abis_survives_a_key_that_grows_the_source():
    src = {}

    class GrowsOnHash(str):
        def __hash__(self):
            src[f"zz{len(src)}"] = strider.sleigh.CallOtherAbi.noop()
            return str.__hash__(self)

    for i in range(4):
        src[GrowsOnHash(f"op{i}")] = strider.sleigh.CallOtherAbi.noop()
    # Must not abort; a mutation mid-conversion is either absorbed or raises.
    try:
        CfgOptions(call_other_abis=src)
    except Exception:
        pass


def test_per_address_ccs_survives_a_key_that_grows_the_source():
    src = {}
    cc = CallingConvention.x86_64_systemv()
    for i in range(8):
        src[GrowsSource(src, 0x2000 + i)] = cc
    seated = LifterOptions(per_address_ccs=src).per_address_ccs
    assert seated is not None and dict(seated)


def test_add_symbols_survives_a_value_that_grows_the_source():
    elf = strider.lift.load_elf(str(fixture_path("x64", "switch")))
    src = {}
    for i in range(8):
        src[f"zz_snap_{i}"] = GrowsSource(src, 0x1000 + i)
    elf.add_symbols(src)
    assert elf.symbol("zz_snap_0") is not None


def test_a_dict_subclass_is_read_from_its_raw_storage():
    """`dict.copy` honours an overridden `keys` for a subclass that also
    overrides `__iter__`, which would seat nothing at all."""

    class Narrowing(dict):
        def keys(self):
            return {}.keys()

        def __iter__(self):
            return iter(())

    opts = CfgOptions(known_targets=Narrowing({0x1000: [1]}))
    assert dict(opts.known_targets) == {0x1000: (1,)}


def test_per_address_ccs_round_trips_through_its_own_view():
    """The getter hands back a `MappingProxyType`, which the constructor has to
    accept, which is why `per_address_ccs` takes any mapping."""
    cc = CallingConvention.x86_64_systemv()
    first = LifterOptions(per_address_ccs={0x1000: cc}).per_address_ccs
    assert isinstance(first, types.MappingProxyType)
    again = LifterOptions(per_address_ccs=first).per_address_ccs
    assert again is not None
    # `CallingConvention` has no value equality, so compare the table's shape.
    assert list(again) == list(first)
    assert [repr(v) for v in again.values()] == [repr(v) for v in first.values()]
