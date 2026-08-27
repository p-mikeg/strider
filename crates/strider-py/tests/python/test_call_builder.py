"""The fixture is `switch.c::dispatch_value`, whose case 5 calls a noinline
helper `f(value->a)`.  After resolution that yields a `Call` whose target is
an `IntConst` of `f`'s address and whose arg 0 is a `Load`, giving a real
graph for `.target()` / `.arg()` and the universal
`.capture()` / `.when()` / `.into_pat()`.
"""

from __future__ import annotations

import pytest

import strider
from strider.pattern import (
    Capture, Pat, anything, var, call, int_const, load, function_arg,
)

from .conftest import built_function, fixture_path


def _switch_graph():
    return built_function("x86", "switch", "dispatch_value")


def test_call_returns_builder_chainable():
    # `.target()` / `.arg()` return the same builder so chaining stays valid.
    b = call().target(0x1000).arg(0, int_const(8))
    assert b is not None
    p = b.into_pat()
    assert isinstance(p, Pat)


def test_into_pat_returns_pat():
    p = call().into_pat()
    assert isinstance(p, Pat)


# `.capture()` / `.when()` return the builder rather than an
# eagerly finalised `Pat`, so chaining stays typed.  Materialise with
# `.into_pat()`, or pass the builder straight in as a PatLike.
def test_capture_returns_builder_with_into_pat_then_pat():
    c = Capture()
    b = call().capture(c)
    assert b is not None
    assert isinstance(b.into_pat(), Pat)


def test_capture_name_returns_builder_with_into_pat_then_pat():
    b = call().capture("call_site")
    assert b is not None
    assert isinstance(b.into_pat(), Pat)


def test_when_predicate_returns_builder_with_into_pat_then_pat():
    b = call().when(lambda m: True)
    assert b is not None
    assert isinstance(b.into_pat(), Pat)


def test_when_predicate_filters_control_builder():
    # A control builder honours `.when()` at its root.
    g = _switch_graph()

    baseline = g.find_all(call())
    assert len(baseline) >= 1, "expected at least one Call in switch.elf"

    rejecting = g.find_all(call().when(lambda m: False))
    assert len(rejecting) == 0, ".when(False) must reject every Call match"

    passing = g.find_all(call().when(lambda m: True))
    assert len(passing) == len(baseline), ".when(True) must keep every match"


def test_target_accepts_pat_like():
    # PatLike is a Pat, a Capture, a raw int, or another typed builder.
    assert isinstance(call().target(int_const(0x1234)).into_pat(), Pat)
    assert isinstance(call().target(Capture("tgt")).into_pat(), Pat)
    assert isinstance(call().target(anything()).into_pat(), Pat)


def test_arg_accepts_pat_like():
    assert isinstance(call().arg(0, int_const(8)).into_pat(), Pat)
    assert isinstance(call().arg(0, Capture("x")).into_pat(), Pat)
    assert isinstance(call().arg(0, function_arg(0)).into_pat(), Pat)


def test_ctrl_accepts_control_producer():
    # The Call's control predecessor is composable, like the macro-built
    # node builders (ret/if/switch) already are.
    from strider.pattern import region, entry
    assert isinstance(call().ctrl(region()).into_pat(), Pat)
    assert isinstance(call().ctrl(entry()).into_pat(), Pat)


def test_find_all_accepts_unfinalised_builder():
    # find_all takes PatLike, so an unfinalised builder works as-is.
    g = _switch_graph()
    hits = g.find_all(call())
    assert len(hits) >= 1, "expected at least one Call (case 5 -> f())"


def test_call_target_int_matches_known_address():
    # A raw int target coerces to int_const(addr): "a call to this address".
    f_addr = strider.lift.load_elf(str(fixture_path("x86", "switch"))).symbol("f").address
    g = _switch_graph()
    hits = g.find_all(call().target(f_addr))
    assert len(hits) >= 1, f"expected >=1 Call to {f_addr:#x}; got {len(hits)}"


def test_call_target_list_matches_when_address_in_set():
    # A list target fires if the target equals any listed address,
    # for queries like "any of these known callees".
    f_addr = strider.lift.load_elf(str(fixture_path("x86", "switch"))).symbol("f").address
    g = _switch_graph()

    hits = g.find_all(call().target([0xDEAD_BEEF, f_addr, 0xCAFE_BABE]))
    assert len(hits) >= 1, (
        f"expected >=1 Call when {f_addr:#x} is in the target set; got {len(hits)}"
    )

    hits_none = g.find_all(call().target([0xDEAD_BEEF, 0xCAFE_BABE]))
    assert len(hits_none) == 0


def test_call_target_empty_list_matches_nothing():
    # An empty candidate set is a well-defined "no call qualifies", so a
    # caller building the list programmatically needs no special case.
    g = _switch_graph()
    assert g.find_all(call().target([])) == []
    assert not hasattr(call(), "targets")


def test_int_const_set_standalone():
    # The int-set form, usable directly under target().
    from strider.pattern import int_const
    elf = fixture_path("x86", "switch")
    loaded = strider.lift.load_elf(str(elf))
    mem = loaded.reader()
    f_addr = loaded.symbol("f").address
    g = _switch_graph()
    hits = g.find_all(call().target(int_const([f_addr, 0xDEAD_BEEF])))
    assert len(hits) >= 1


def test_call_arg0_constraint_filters_out_non_matches():
    # `f` takes `value->a` as arg 0.  The optimiser may collapse the
    # surrounding casts, but the Load itself survives.
    g = _switch_graph()
    hits = g.find_all(call().arg(0, load()))
    assert len(hits) >= 1, (
        "expected case-5 Call whose arg 0 is a Load(value->a)"
    )

    # The constant is picked to be unlikely to appear by accident.
    hits_neg = g.find_all(call().arg(0, int_const(0xDEAD_BEEF_CAFE)))
    assert len(hits_neg) == 0


def test_call_target_capture_round_trips():
    # `.target(var(c))` binds c to the target's IntConst output, readable
    # back via Match.uint(c).
    g = _switch_graph()
    c = Capture()
    hits = g.find_all(call().target(var(c)))
    assert len(hits) >= 1
    elf = fixture_path("x86", "switch")
    loaded = strider.lift.load_elf(str(elf))
    mem = loaded.reader()
    f_addr = loaded.symbol("f").address
    seen_f = False
    for m in hits:
        u = m.uint(c)
        if u is not None and u == f_addr:
            seen_f = True
            break
    assert seen_f, f"no Call's target captured to f's address ({f_addr:#x})"




def test_call_arg_huge_index_builds_but_never_matches():
    # An out-of-range positional index is not a build-time error; the
    # constraint can never bind.
    b = call().arg(1_000_000, anything())
    assert isinstance(b.into_pat(), Pat)
    g = _switch_graph()
    assert g.find_all(call().arg(1_000_000, anything())) == []


def test_call_arg_negative_index_overflows_at_chain_time():
    # The index is unsigned, so a negative int fails eagerly at chain time
    # as OverflowError, not StriderError.
    import pytest

    with pytest.raises(OverflowError):
        call().arg(-1, anything())
