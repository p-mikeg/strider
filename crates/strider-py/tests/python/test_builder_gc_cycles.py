"""A cycle made only of pattern builders must be collectable.

`__traverse__` alone lets the collector SEE a cycle; breaking it needs
`__clear__` on at least one object in the loop. With none, CPython finds the
garbage, cannot clear it, and promotes it to the old generation forever.
"""

import gc
from typing import NamedTuple, Optional

import strider
from strider import pattern as p
from strider import template as tpl


def _live(name: str) -> int:
    gc.collect()
    return sum(1 for o in gc.get_objects() if type(o).__name__ == name)


def test_mutually_referencing_builders_are_collected():
    base = _live("StorePat")
    for _ in range(200):
        a = p.store()
        b = p.store()
        a.mem(b)
        b.mem(a)
        del a, b
    gc.collect()
    gc.collect()
    assert _live("StorePat") == base


def test_self_referencing_builder_is_collected():
    base = _live("StorePat")
    for _ in range(200):
        a = p.store()
        a.mem(a)
        del a
    gc.collect()
    gc.collect()
    assert _live("StorePat") == base


def test_builder_holding_a_when_closure_cycle_is_collected():
    base = _live("LoadPat")
    for _ in range(200):
        holder: list = []
        a = p.load()
        a.when(lambda *_: bool(holder))
        holder.append(a)
        del a, holder
    gc.collect()
    gc.collect()
    assert _live("LoadPat") == base


class _Marker:
    pass


def test_pat_and_template_cycles_are_collected():
    """`Pat` and `Template` have no `tp_clear`, but they are immutable, so a
    cycle through one also runs through a container that has one."""
    base = _live("_Marker")
    for _ in range(50):
        held: list = [_Marker()]
        held.append(p.anything().when(lambda _m, _h=held: True))
        # Deliberate: an operand is stored unchecked until the pattern compiles.
        held.append(p.int_add(held, p.anything()))  # type: ignore[arg-type]
        held.append(tpl.int_add(held, tpl.int_const(1)))  # type: ignore[arg-type]
        del held
    gc.collect()
    gc.collect()
    assert _live("_Marker") == base


class _TupleReader(NamedTuple):
    """A reader with no `tp_clear` of its own to break a cycle."""

    lifter: object
    marker: _Marker

    def read(self, addr: int, size: int) -> Optional[bytes]:
        return b"\xc3" * size


def test_a_cleared_lifter_releases_its_reader():
    """The engine's adapters hold the reader too, so a `__clear__` that drops
    only its own handles leaves the edge alive and the cycle uncollectable."""
    base = _live("_Marker")
    arch = strider.sleigh.SleighArch.x86_64()
    for _ in range(20):
        lift = strider.lift.lifter(arch, strider.reader.BufferReader(0x1000, b"\xc3"))
        # Deliberate: a duck-typed reader, not a `MemReader` subclass.
        lift._rebuild(arch, _TupleReader(lift, _Marker()))  # type: ignore[arg-type]
        del lift
    gc.collect()
    gc.collect()
    assert _live("_Marker") == base

