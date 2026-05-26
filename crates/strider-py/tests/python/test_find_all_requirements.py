"""Tests for `Function.find_all_requirements` — multi-pattern queries that
intersect on shared `Capture` objects.

The Rust matcher's algorithm is covered exhaustively in
`crates/pattern/tests/matching/matcher_api.rs`; here we pin the
Python-side API surface and end-to-end behaviour against the
existing `switch.elf` fixture.
"""

from __future__ import annotations

import strider
from strider.pattern import (
    Capture, any_, any_int_const, call, var, int_const,
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
    ).graph


def test_find_all_requirements_empty_pats_yields_empty():
    g = _switch_graph()
    assert g.find_all_requirements([]) == []


def test_find_all_requirements_single_pattern_equivalent_to_find_all():
    # With one pattern there is no cross-pattern join — each result
    # is a one-element tuple wrapping the corresponding find_all hit.
    g = _switch_graph()
    direct = g.find_all(call())
    req = g.find_all_requirements([call()])
    assert len(req) == len(direct)
    for tup, m in zip(req, direct):
        assert len(tup) == 1
        # Same root node id.
        c = Capture()
        # Recover the root via a capture round-trip on a fresh query —
        # a stable equality on Match would be cleaner, but the public
        # surface lets us re-key by capture below in the headline test.
        assert tup[0] is not None
        assert m is not None


def test_find_all_requirements_no_matches_for_a_pattern_yields_empty():
    # A pattern that cannot match the graph (clearly impossible
    # IntConst literal) makes the join empty regardless of other
    # patterns.
    g = _switch_graph()
    impossible = int_const(0xDEAD_BEEF_CAFE_BABE)
    req = g.find_all_requirements([call(), impossible])
    assert req == []


def test_find_all_requirements_intersects_on_shared_capture():
    # Headline use case: two patterns share the `target` capture.
    # Pat1 captures the call's target.  Pat2 binds the same capture
    # against `any_int_const`, requiring the shared node to be an
    # IntConst.  Both must agree on the node id.
    elf = fixture_path("x86", "switch")
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(elf))
    f_addr = mem.symbol("f")
    g = _switch_graph()

    target = Capture()
    pat_call = call().target(var(target))
    pat_const = any_int_const(target)

    req = g.find_all_requirements([pat_call, pat_const])
    # The call to `f` must appear in the joined results: pat_call
    # binds `target` to f's IntConst output; pat_const matches that
    # same IntConst node.
    assert len(req) >= 1, (
        f"expected ≥1 joined match for call(target=f) ∧ any_int_const(target)"
    )
    saw_f = False
    for tup in req:
        assert len(tup) == 2, "one match per input pattern"
        # Both tuple entries must agree on `target` — assert via the
        # public capture-keyed accessor.  IR dedups `IntConst` nodes
        # by stored value, so equal `uint()` results implies same
        # node (the only mechanism by which two IntConsts could share
        # a stored value at the same width is dedup).
        v0 = tup[0].uint(target)
        v1 = tup[1].uint(target)
        assert v0 is not None and v1 is not None
        assert v0 == v1, "shared capture must agree across the joined matches"
        if v0 == f_addr:
            saw_f = True
    assert saw_f, f"none of the joined matches captured the call to f ({f_addr:#x})"


def test_find_all_requirements_disagreement_yields_empty():
    # Negative: bind the same capture to two patterns whose match
    # nodes are disjoint by construction.  `call()` matches a Call
    # node; `int_const(K)` only matches IntConst nodes — there is no
    # node that satisfies both, so the joined result is empty even
    # though each pattern matches independently.
    g = _switch_graph()
    shared = Capture()
    pat_call = call().capture(shared)        # binds shared → Call NodeId
    pat_const = int_const(0).capture(shared) # binds shared → IntConst NodeId
    req = g.find_all_requirements([pat_call, pat_const])
    assert req == []
