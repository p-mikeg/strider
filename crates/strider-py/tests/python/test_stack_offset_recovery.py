"""Stack-offset recovery: `StorePat.offset_capture` / `Match.captured_offset`.

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
    Capture, OffsetCapture, add, any_int_const, call, initial_var_for,
    load, store, var,
)

from .conftest import fixture_path


def _stack_graph():
    elf = fixture_path("x86", "stack")
    arch = strider.SleighArch.x86()
    cc = strider.CallingConvention.x86_cdecl()
    loaded = strider.load_elf(str(elf))
    mem = loaded.memory_map()
    addr = loaded.symbol("escape_via_ptr")
    return strider.run(
        arch=arch, cc=cc, mem=mem, rom=mem, entry=addr,
        allow_code_before_start_addr=True,
    ).function, mem, loaded


# ── StorePat.offset_capture / Match.captured_offset accessors ─────────────


def test_match_captured_offset_returns_offset_for_stack_store():
    g, _, _ = _stack_graph()
    c = OffsetCapture()
    hits = g.find_all(store().offset_capture(c))
    assert len(hits) >= 1, "escape_via_ptr should produce ≥1 stack Store"
    for m in hits:
        off = m.captured_offset(c)
        assert isinstance(off, int), (
            f"captured_offset must return an int for an offset_capture; got {off!r}"
        )


def test_match_captured_offset_unbound_capture_returns_none():
    g, _, _ = _stack_graph()
    bound = OffsetCapture()
    unbound = OffsetCapture()
    hits = g.find_all(store().offset_capture(bound))
    assert len(hits) >= 1
    assert hits[0].captured_offset(unbound) is None


def test_store_stack_only_matches_only_stack_stores():
    g, _, _ = _stack_graph()
    # stack_only() restricts to stores whose offset is known to the
    # SP-expr analysis — the same stores that offset_capture matches.
    hits_stack = g.find_all(store().stack_only())
    hits_offset = g.find_all(store().offset_capture(OffsetCapture()))
    # Both filters must agree on count: offset_capture implies stack_only.
    assert len(hits_stack) == len(hits_offset)
    assert len(hits_stack) >= 1


def test_store_stack_only_rejects_non_stack_stores():
    g, _, _ = _stack_graph()
    # Unconstrained store() matches everything including non-stack stores.
    all_stores = g.find_all(store())
    stack_stores = g.find_all(store().stack_only())
    # In escape_via_ptr the full store set may include non-stack stores;
    # stack_only must be a strict subset (or equal if all are stack).
    assert len(stack_stores) <= len(all_stores)


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
    g, mem, loaded = _stack_graph()
    arch = strider.SleighArch.x86()
    sleigh = strider.Sleigh(arch, mem)
    esp = sleigh.reg("ESP")

    take_ptr = loaded.symbol("external_take_ptr")
    k_call = Capture()
    k_load = Capture()

    # `add()` is the commutative free constructor; the prior `.ordered()`
    # call here was a silent no-op (now raises PatternError).  Both
    # operand orderings of `Add(InitialVar(ESP), IntConst)` are matched
    # automatically — the captures still bind to the IntConst leg.
    results = g.find_all_requirements([
        # call(at=external_take_ptr).arg(0, lea esp+K1)
        call().target(strider.pattern.int_const(take_ptr))
            .arg(0, add(initial_var_for(esp), any_int_const(k_call))),
        # any load at esp+K2 — the return-value load of `local`
        load().addr(add(initial_var_for(esp), any_int_const(k_load))),
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
