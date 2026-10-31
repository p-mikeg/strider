"""An alternation's arm nests through the edge its slot takes, as in Rust, so
`one_of([x])` answers what `x` does in every slot, raw ones included: a raw
input or `any_input` takes any edge, and a `call()` there nests through
whichever of its outputs feeds it."""

import pytest

import strider
from strider import pattern as p

from .conftest import fixture_path


@pytest.fixture(scope="module")
def functions():
    out = []
    for case in ("calls", "memory", "control"):
        prog = strider.lift.load_elf(str(fixture_path("x86", case)))
        for sym in prog.functions():
            try:
                out.append(prog.analyze(sym.name).function)
            except strider.StriderError:
                pass
    return out


def _count(functions, pat):
    return sum(len(f.find_all(pat)) for f in functions)


_SLOTS = {
    "region.any_input": lambda x: p.region().any_input(x),
    "ret.any_input": lambda x: p.ret().any_input(x),
    "load.input(0)": lambda x: p.load().input(0, x),
    "call.input(0)": lambda x: p.call().input(0, x),
    "call_other.arg(1)": lambda x: p.call_other().arg(1, x),
    "load.mem": lambda x: p.load().mem(x),
    "store.mem": lambda x: p.store().mem(x),
    "call.ctrl": lambda x: p.call().ctrl(x),
}
_ARMS = {
    "call": p.call,
    "call.res": lambda: p.call().res(),
    "call_other": p.call_other,
    "if_else": p.if_else,
}


@pytest.mark.parametrize("slot", sorted(_SLOTS))
@pytest.mark.parametrize("arm", sorted(_ARMS))
def test_a_lone_arm_answers_what_the_operand_does(functions, slot, arm):
    if arm == "if_else" and slot in ("load.mem", "store.mem"):
        pytest.skip("not a memory producer")
    at, make = _SLOTS[slot], _ARMS[arm]
    plain = _count(functions, at(make()))
    assert _count(functions, at(p.one_of([make()]))) == plain
    assert _count(functions, at(p.first_of([make()]))) == plain


@pytest.mark.parametrize("slot, make, want", [
    (lambda x: p.region().any_input(x), p.call, 2),
    (lambda x: p.load().input(0, x), p.call, 18),
    (lambda x: p.call().input(0, x), p.call, 24),
    (lambda x: p.ret().any_input(x), p.call_other, 1),
    (lambda x: p.call_other().arg(1, x), p.call, 1),
    # A memory slot still takes the token alone.
    (lambda x: p.store().mem(x), lambda: p.call().res(), 28),
    (lambda x: p.load().mem(x), lambda: p.call().res(), 18),
])
def test_a_call_in_a_raw_slot_binds_any_of_its_edges(functions, slot, make, want):
    """The totals the Rust DSL gives for the same patterns."""
    got = [_count(functions, slot(w(make())))
           for w in (lambda x: x, lambda x: p.one_of([x]), lambda x: p.first_of([x]))]
    assert got == [want] * 3


@pytest.mark.parametrize("value_slot", [
    lambda x: p.int_add(x, 1),
    lambda x: p.if_else().cond(x),
    lambda x: p.load().addr(x),
    lambda x: p.switch().selector(x),
])
@pytest.mark.parametrize("control_only", [p.entry, p.region])
def test_a_control_producer_in_a_value_slot_is_refused(functions, value_slot, control_only):
    with pytest.raises(strider.StriderError, match="only a control edge"):
        functions[0].find_all(value_slot(control_only()))
