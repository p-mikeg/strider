"""Tests for the `CallPat` builder exposed via `pattern.call()`.

`fixtures/cases/switch.c::dispatch_value` case 5 calls a noinline
helper `f(value->a)`.  After resolution the IR contains a `Call`
node whose:

  - target is the resolved address of `f` (an `IntConst`),
  - arg slot 0 is the loaded `value->a` (the `Load` shape).

Those properties give us a real graph to exercise every builder
method that strider-py now exposes.

Mirrors `crates/pattern/src/pat/builders/call.rs::CallPat` —
`.at(addr)`, `.target(pat)`, `.arg(idx, pat)`, `.ret_output(idx, pat)`,
plus the universal `.capture(c)` / `.cap(name)` / `.when(f)` /
`.into_pat()`.
"""

from __future__ import annotations

import strider
from strider.pattern import (
    Capture, Pat, any_, var, call, int_const, load, function_arg,
)

from .conftest import fixture_path


def _switch_graph():
    elf = fixture_path("x86", "switch")
    arch = strider.SleighArch.x86()
    cc = strider.CallingConvention.x86_cdecl()
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(elf))
    addr = mem.symbol("dispatch_value")
    return strider.run(
        arch=arch, cc=cc, mem=mem, rom=mem, entry=addr,
        allow_code_before_start_addr=True,
    ).function


# ── builder-shape sanity ─────────────────────────────────────────────────


def test_call_returns_builder_chainable():
    # `.at()` / `.arg()` return the SAME builder (chain).  The test
    # validates the chain stays valid; semantics are exercised below.
    b = call().at(0x1000).arg(0, int_const(8)).ret_output(0, var(Capture()))
    assert b is not None
    p = b.into_pat()
    assert isinstance(p, Pat)


def test_into_pat_returns_pat():
    p = call().into_pat()
    assert isinstance(p, Pat)


# After the `#[strider_pattern]` macro migration the `.capture(c)` /
# `.cap(name)` / `.when(f)` builder methods return the same builder
# (so further chaining stays typed), not an eagerly finalised `Pat`.
# Call `.into_pat()` (or pass the builder directly as a `PatLike`) to
# materialise.
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


def test_target_accepts_pat_like():
    # PatLike: Pat, Capture, str ("name"), or another typed builder.
    assert isinstance(call().target(int_const(0x1234)).into_pat(), Pat)
    assert isinstance(call().target("tgt").into_pat(), Pat)
    assert isinstance(call().target(any_()).into_pat(), Pat)


def test_arg_accepts_pat_like():
    assert isinstance(call().arg(0, int_const(8)).into_pat(), Pat)
    assert isinstance(call().arg(0, "x").into_pat(), Pat)
    assert isinstance(call().arg(0, function_arg(0)).into_pat(), Pat)


# ── end-to-end pattern matches against switch.elf ────────────────────────


def test_find_all_accepts_unfinalised_builder():
    # Function.find_all takes PatLike; passing the builder directly
    # (no .into_pat()) must work and find the case-5 call site.
    g = _switch_graph()
    hits = g.find_all(call())
    assert len(hits) >= 1, "expected at least one Call (case 5 → f())"


def test_call_at_address_matches_known_target():
    # Look up f's address; assert call(at=f_addr) finds the case-5 site.
    elf = fixture_path("x86", "switch")
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(elf))
    f_addr = mem.symbol("f")
    g = _switch_graph()
    hits = g.find_all(call(at=f_addr))
    assert len(hits) >= 1, f"expected ≥1 Call to {f_addr:#x}; got {len(hits)}"


def test_call_at_any_matches_when_target_in_set():
    # `.at_any([...])` fires if the call target equals any address in
    # the list — natural for queries that look for "any of these
    # known callees" (e.g. multiple lock-acquire helpers).
    elf = fixture_path("x86", "switch")
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(elf))
    f_addr = mem.symbol("f")
    g = _switch_graph()

    # Set contains f's actual address among unrelated noise → must fire.
    hits = g.find_all(call().at_any([0xDEAD_BEEF, f_addr, 0xCAFE_BABE]))
    assert len(hits) >= 1, (
        f"expected ≥1 Call when {f_addr:#x} is in the target set; got {len(hits)}"
    )

    # Set without f's address → no match.
    hits_none = g.find_all(call().at_any([0xDEAD_BEEF, 0xCAFE_BABE]))
    assert len(hits_none) == 0


def test_call_at_any_empty_set_matches_nothing():
    # An empty target set is vacuously false — pin the contract so
    # callers don't accidentally fall through to "match anything".
    g = _switch_graph()
    hits = g.find_all(call().at_any([]))
    assert len(hits) == 0


def test_int_const_any_of_standalone():
    # The underlying primitive — usable independently of CallPat.
    from strider.pattern import int_const_any_of
    elf = fixture_path("x86", "switch")
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(elf))
    f_addr = mem.symbol("f")
    g = _switch_graph()
    hits = g.find_all(call().target(int_const_any_of([f_addr, 0xDEAD_BEEF])))
    assert len(hits) >= 1


def test_call_arg0_constraint_filters_out_non_matches():
    # `f` is called with `value->a` as arg 0 — a `Load` value (after
    # the destructive optimiser pipeline runs, the surrounding casts
    # may collapse but the Load itself survives).  Constrain arg 0 to
    # `Load` and assert the Call still matches.
    g = _switch_graph()
    hits = g.find_all(call().arg(0, load()))
    assert len(hits) >= 1, (
        "expected case-5 Call whose arg 0 is a Load(value->a)"
    )

    # Sanity: an arg-0 constraint that CANNOT match (impossible
    # IntConst on the load operand) must yield zero hits.  Uses a
    # very specific constant unlikely to appear by accident.
    hits_neg = g.find_all(call().arg(0, int_const(0xDEAD_BEEF_CAFE)))
    assert len(hits_neg) == 0


def test_call_target_capture_round_trips():
    # `.target(var(c))` binds c to the target's IntConst output —
    # users can then read its value via Match.uint(c).
    g = _switch_graph()
    c = Capture()
    hits = g.find_all(call().target(var(c)))
    assert len(hits) >= 1
    # At least one match's target binding must round-trip to f's address.
    elf = fixture_path("x86", "switch")
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(elf))
    f_addr = mem.symbol("f")
    seen_f = False
    for m in hits:
        u = m.uint(c)
        if u is not None and u == f_addr:
            seen_f = True
            break
    assert seen_f, f"no Call's target captured to f's address ({f_addr:#x})"


def test_call_chained_with_ret_output_capture():
    # `.ret_output(0, var(c))` binds c to the value-output of the call's
    # first return-value slot.  The capture isn't required to match a
    # specific shape (var(c) is a wildcard); we just assert the chain
    # finalises to a valid Pat and find_all returns ≥0 matches.
    g = _switch_graph()
    c = Capture()
    p = call().ret_output(0, var(c))
    hits = g.find_all(p)
    assert isinstance(hits, list)
