"""Stack-offset recovery: `Match.stack_offset` / `stack_phi_offsets`,
`StackStorePat.offset_any`.

The motivating shape (FreeBSD `dounmount`):

```c
struct nameidata nd;
NDINIT(&nd, ...);
vn_open(&nd, ...);
script_vp = nd.ni_vp;       // load nd.ni_vp = Add(stack_base, K)
```

To recover the `ni_vp` field offset, capture two frame-base-relative
offsets — the call's first argument (= `&nd`, K1) and a downstream
load (= `nd.ni_vp`, K2) — and report `K2 - K1`.  Since constant-fold
collapses `Add(Add(rbp, K1), K_field)` to `Add(rbp, K1+K_field)`, the
shared anchor is the frame base (`InitialVar(rsp_or_rbp)`), not the
intermediate `&nd` value.

We exercise the pattern against `escape_via_ptr` from `stack.c`: a
local variable `local`, address taken, passed to
`external_take_ptr`, then returned (= load from the same stack
slot).
"""

from __future__ import annotations

import strider
from strider.pattern import (
    Capture, add, any_int_const, call, initial_var_for, load, stack_store,
    var,
)

from .conftest import fixture_path


def _stack_graph():
    elf = fixture_path("x86", "stack")
    arch = strider.SleighArch.x86()
    cc = strider.CallingConvention.x86_cdecl()
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(elf))
    addr = mem.symbol("escape_via_ptr")
    return strider.run(
        arch=arch, cc=cc, mem=mem, rom=mem, entry=addr,
        allow_code_before_start_addr=True,
    ).graph, mem


# ── stack_offset / stack_phi_offsets accessors ─────────────────────────────


def test_match_stack_offset_returns_offset_for_captured_stack_store():
    g, _ = _stack_graph()
    c = Capture()
    hits = g.find_all(stack_store().capture(c))
    assert len(hits) >= 1, "escape_via_ptr should produce ≥1 StackStore"
    for m in hits:
        off = m.stack_offset(c)
        assert isinstance(off, int), (
            f"stack_offset must return an int for a StackStore capture; got {off!r}"
        )


def test_match_stack_offset_unbound_capture_returns_none():
    g, _ = _stack_graph()
    bound = Capture()
    unbound = Capture()
    hits = g.find_all(stack_store().capture(bound))
    assert len(hits) >= 1
    assert hits[0].stack_offset(unbound) is None


# ── StackStorePat.offset_any ───────────────────────────────────────────────


def test_stack_store_offset_any_matches_when_in_set():
    g, _ = _stack_graph()
    # Discover the actual offsets, then assert offset_any with that
    # set in addition to noise still matches.
    c = Capture()
    all_hits = g.find_all(stack_store().capture(c))
    assert len(all_hits) >= 1
    offsets = sorted({m.stack_offset(c) for m in all_hits})

    # Set containing all the actual offsets → must hit every store.
    hits = g.find_all(stack_store().offset_any(offsets + [0xDEAD_BEEF, -0xDEAD_BEEF]))
    assert len(hits) == len(all_hits)


def test_stack_store_offset_any_rejects_when_not_in_set():
    g, _ = _stack_graph()
    # A set that cannot contain any real offset (way out of typical
    # stack-frame range) must yield zero matches.
    hits = g.find_all(stack_store().offset_any([0x1234_5678, -0x1234_5678]))
    assert len(hits) == 0


def test_stack_store_offset_any_empty_set_matches_nothing():
    g, _ = _stack_graph()
    hits = g.find_all(stack_store().offset_any([]))
    assert len(hits) == 0


# ── End-to-end: ni_vp-style field-offset recovery ─────────────────────────


def test_field_offset_recovery_via_find_all_requirements():
    """Mirror the motivating recovery: capture the call's arg-passing
    offset and a follow-up load's offset against a shared frame base,
    compute their difference.

    `escape_via_ptr` does:

        int local = seed * 3;
        external_take_ptr(&local);
        return local;     // load from the same stack slot

    With `-fomit-frame-pointer` (the default at -O2 for x86 cdecl
    here), the frame base is ESP, not EBP — so the shared anchor for
    `find_all_requirements` is `InitialVar(ESP)`.  Both `&local`
    (the call arg) and `local` (the return-value load) are at the
    same ESP offset, so the recovered field offset is 0.
    """
    g, mem = _stack_graph()
    arch = strider.SleighArch.x86()
    sleigh = strider.Sleigh(arch, mem)
    esp = sleigh.reg("ESP")

    take_ptr = mem.symbol("external_take_ptr")
    k_call = Capture()
    k_load = Capture()

    results = g.find_all_requirements([
        # call(at=external_take_ptr).arg(0, lea esp+K1)
        call().target(strider.pattern.int_const(take_ptr))
            .arg(0, add(initial_var_for(esp), any_int_const(k_call)).ordered()),
        # any load at esp+K2 — the return-value load of `local`
        load().addr(add(initial_var_for(esp), any_int_const(k_load)).ordered()),
    ])
    # We expect at least one joined match where the load is the
    # post-call read of `local` — same stack slot, so K2 == K1
    # and the recovered field offset is 0.
    assert len(results) >= 1, (
        "expected at least one joined match for "
        "(call(external_take_ptr) ∧ load) on the shared frame base"
    )
    saw_zero_offset = False
    for tup in results:
        k1 = tup[0].uint(k_call)
        k2 = tup[1].uint(k_load)
        if k1 is None or k2 is None:
            continue
        # Field offset is K2 - K1 reduced mod 2^32 (x86 word width).
        field = (k2 - k1) & 0xFFFF_FFFF
        if field == 0:
            saw_zero_offset = True
    assert saw_zero_offset, (
        "expected to see a load at the same offset as &local "
        "(i.e. the return-value read of `local` after external_take_ptr)"
    )
