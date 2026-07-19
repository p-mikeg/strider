"""Tests for the `CallPat` builder exposed via `pattern.call()`.

The fixture is `switch.c::dispatch_value`, whose case 5 calls a noinline
helper `f(value->a)`.  After resolution that yields a `Call` whose target is
an `IntConst` of `f`'s address and whose arg 0 is a `Load`, giving a real
graph for `.at()` / `.target()` / `.arg()` and the universal `.capture()` /
`.cap()` / `.when()` / `.into_pat()`.
"""

from __future__ import annotations

import strider
from strider.pattern import (
    Capture, Pat, anything, var, call, int_const, load, function_arg,
)

from .conftest import built_function, fixture_path


def _switch_graph():
    return built_function("x86", "switch", "dispatch_value")


def test_call_returns_builder_chainable():
    # `.at()` / `.arg()` return the same builder so chaining stays valid;
    # the semantics are exercised further down.
    b = call().at(0x1000).arg(0, int_const(8))
    assert b is not None
    p = b.into_pat()
    assert isinstance(p, Pat)


def test_into_pat_returns_pat():
    p = call().into_pat()
    assert isinstance(p, Pat)


# `.capture()` / `.cap()` / `.when()` return the builder rather than an
# eagerly finalised `Pat`, so chaining stays typed.  Materialise with
# `.into_pat()`, or pass the builder straight in as a PatLike.
def test_capture_returns_builder_with_into_pat_then_pat():
    c = Capture()
    b = call().capture(c)
    assert b is not None
    assert isinstance(b.into_pat(), Pat)


def test_cap_name_returns_builder_with_into_pat_then_pat():
    b = call().cap("call_site")
    assert b is not None
    assert isinstance(b.into_pat(), Pat)


def test_when_predicate_returns_builder_with_into_pat_then_pat():
    b = call().when(lambda m: True)
    assert b is not None
    assert isinstance(b.into_pat(), Pat)


def test_when_predicate_filters_control_builder():
    # Regression: `.when()` used to be silently dropped on control builders,
    # letting every node through.
    g = _switch_graph()

    baseline = g.find_all(call())
    assert len(baseline) >= 1, "expected at least one Call in switch.elf"

    rejecting = g.find_all(call().when(lambda m: False))
    assert len(rejecting) == 0, ".when(False) must reject every Call match"

    passing = g.find_all(call().when(lambda m: True))
    assert len(passing) == len(baseline), ".when(True) must keep every match"


def test_target_accepts_pat_like():
    # PatLike is a Pat, a Capture, a str name, or another typed builder.
    assert isinstance(call().target(int_const(0x1234)).into_pat(), Pat)
    assert isinstance(call().target("tgt").into_pat(), Pat)
    assert isinstance(call().target(anything()).into_pat(), Pat)


def test_arg_accepts_pat_like():
    assert isinstance(call().arg(0, int_const(8)).into_pat(), Pat)
    assert isinstance(call().arg(0, "x").into_pat(), Pat)
    assert isinstance(call().arg(0, function_arg(0)).into_pat(), Pat)


def test_find_all_accepts_unfinalised_builder():
    # find_all takes PatLike, so an unfinalised builder works as-is.
    g = _switch_graph()
    hits = g.find_all(call())
    assert len(hits) >= 1, "expected at least one Call (case 5 → f())"


def test_call_at_address_matches_known_target():
    elf = fixture_path("x86", "switch")
    loaded = strider.lift.load_elf(str(elf))
    mem = loaded.reader()
    f_addr = loaded.symbol("f")
    g = _switch_graph()
    hits = g.find_all(call(at=f_addr))
    assert len(hits) >= 1, f"expected ≥1 Call to {f_addr:#x}; got {len(hits)}"


def test_call_at_any_matches_when_target_in_set():
    # `.at_any([...])` fires if the target equals any address in the list,
    # for queries like "any of these known callees".
    elf = fixture_path("x86", "switch")
    loaded = strider.lift.load_elf(str(elf))
    mem = loaded.reader()
    f_addr = loaded.symbol("f")
    g = _switch_graph()

    # f's address among unrelated noise: must fire.
    hits = g.find_all(call().at_any([0xDEAD_BEEF, f_addr, 0xCAFE_BABE]))
    assert len(hits) >= 1, (
        f"expected ≥1 Call when {f_addr:#x} is in the target set; got {len(hits)}"
    )

    # Without f's address: no match.
    hits_none = g.find_all(call().at_any([0xDEAD_BEEF, 0xCAFE_BABE]))
    assert len(hits_none) == 0


def test_call_at_any_empty_set_matches_nothing():
    # An empty target set is vacuously false, not "match anything".
    g = _switch_graph()
    hits = g.find_all(call().at_any([]))
    assert len(hits) == 0


def test_int_const_any_of_standalone():
    # The primitive under `.at_any()`, usable without CallPat.
    from strider.pattern import int_const_any_of
    elf = fixture_path("x86", "switch")
    loaded = strider.lift.load_elf(str(elf))
    mem = loaded.reader()
    f_addr = loaded.symbol("f")
    g = _switch_graph()
    hits = g.find_all(call().target(int_const_any_of([f_addr, 0xDEAD_BEEF])))
    assert len(hits) >= 1


def test_call_arg0_constraint_filters_out_non_matches():
    # `f` takes `value->a` as arg 0.  The optimiser may collapse the
    # surrounding casts, but the Load itself survives.
    g = _switch_graph()
    hits = g.find_all(call().arg(0, load()))
    assert len(hits) >= 1, (
        "expected case-5 Call whose arg 0 is a Load(value->a)"
    )

    # An unsatisfiable arg-0 constraint yields nothing.  The constant is
    # picked to be unlikely to appear by accident.
    hits_neg = g.find_all(call().arg(0, int_const(0xDEAD_BEEF_CAFE)))
    assert len(hits_neg) == 0


def test_call_target_capture_round_trips():
    # `.target(var(c))` binds c to the target's IntConst output, readable
    # back via Match.const_uint(c).
    g = _switch_graph()
    c = Capture()
    hits = g.find_all(call().target(var(c)))
    assert len(hits) >= 1
    elf = fixture_path("x86", "switch")
    loaded = strider.lift.load_elf(str(elf))
    mem = loaded.reader()
    f_addr = loaded.symbol("f")
    seen_f = False
    for m in hits:
        u = m.const_uint(c)
        if u is not None and u == f_addr:
            seen_f = True
            break
    assert seen_f, f"no Call's target captured to f's address ({f_addr:#x})"




def test_call_arg_huge_index_builds_but_never_matches():
    # An out-of-range positional index is not a build-time error; the
    # constraint simply can never bind.
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
