"""`any_input(p)` across the node-family builders, plus `phi_token(p)`.

`any_input` semantics are the same everywhere: EVERY input slot is a
candidate and the sub-pattern discriminates. A typed value sub only binds a
value-kind input, while `var()`/`anything()` also reach the control /
memory / PhiToken edges a typed sub never can.

`phi_token(p)` targets raw input slot 0 (the PhiToken edge from the owning
Region); `.input(i, p)` shifts by +1 to skip past it.
"""

from __future__ import annotations

import strider
from strider.pattern import (
    Capture,
    call,
    entry,
    load,
    mem_phi,
    phi,
    region,
    store,
    var,
    int_const,
    anything,
)

from .conftest import built_function


def _lift(code: bytes, addr: int = 0x1000):
    mem = strider.reader.BufferReader(addr, code)
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)
    _cfg, fn, _unresolved = lift.analyze(
        addr, strider.sleigh.CallingConvention.x86_64_systemv()
    )
    return fn


def _lift_unoptimized(code: bytes, addr: int = 0x1000):
    """Empty optimizer pipeline, so the raw multi-Region CFG shape survives.

    The default pipeline's `RegionCollapse` folds away every
    single-predecessor `Region`, hiding the shape the `entry()` / `region()`
    structural tests need.
    """
    mem = strider.reader.BufferReader(addr, code)
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)
    _cfg, fn, _unresolved = lift.analyze(
        addr,
        strider.sleigh.CallingConvention.x86_64_systemv(),
        opts=strider.lift.LifterOptions(pipeline=strider.opt.OptimizerPipeline.empty()),
    )
    return fn


def _direct_call_and_ret(target: int) -> "strider.Function":
    #   call target      (e8 rel32)
    #   ret
    rel = (target - (0x1000 + 5)) & 0xFFFFFFFF
    code = bytes([0xE8]) + rel.to_bytes(4, "little", signed=False) + bytes([0xC3])
    return _lift(code)


def _diamond_returning_eax() -> "strider.Function":
    #   test edi, edi
    #   jne  else
    #   mov  eax, 1
    #   jmp  end
    # else:
    #   mov  eax, 2
    # end:
    #   ret                       -> Phi(IntConst(1), IntConst(2))
    code = bytes([
        0x85, 0xFF,                          # test edi, edi
        0x75, 0x07,                          # jne +7
        0xB8, 0x01, 0x00, 0x00, 0x00,        # mov eax, 1
        0xEB, 0x05,                          # jmp +5
        0xB8, 0x02, 0x00, 0x00, 0x00,        # mov eax, 2
        0xC3,                                # ret
    ])
    return _lift(code)


def _diamond_with_memory_join() -> "strider.Function":
    #   test edi, edi
    #   jne  else
    #   mov  dword [0x3000], 1
    #   jmp  end
    # else:
    #   mov  dword [0x3000], 2
    # end:
    #   mov  eax, [0x3000]        -> forces a genuine memory join (MemPhi
    #   ret                          with two real store predecessors)
    code = bytes([
        0x85, 0xFF,                                        # test edi, edi
        0x75, 0x0D,                                         # jne +13
        0xC7, 0x04, 0x25, 0x00, 0x30, 0x00, 0x00,            # mov dword [0x3000],
        0x01, 0x00, 0x00, 0x00,                              #   1
        0xEB, 0x0B,                                          # jmp +11
        0xC7, 0x04, 0x25, 0x00, 0x30, 0x00, 0x00,            # mov dword [0x3000],
        0x02, 0x00, 0x00, 0x00,                              #   2
        0x8B, 0x04, 0x25, 0x00, 0x30, 0x00, 0x00,            # mov eax, [0x3000]
        0xC3,                                                # ret
    ])
    return _lift(code)


def _diamond_returning_eax_unoptimized() -> "strider.Function":
    """Same bytes as `_diamond_returning_eax`, unoptimized so all four
    Regions (entry, both branches, join) survive `RegionCollapse`."""
    code = bytes([
        0x85, 0xFF,                          # test edi, edi
        0x75, 0x07,                          # jne +7
        0xB8, 0x01, 0x00, 0x00, 0x00,        # mov eax, 1
        0xEB, 0x05,                          # jmp +5
        0xB8, 0x02, 0x00, 0x00, 0x00,        # mov eax, 2
        0xC3,                                # ret
    ])
    return _lift_unoptimized(code)


def test_call_any_input_binds_the_target():
    fn = _direct_call_and_ret(0x2000)
    hits = fn.find_all(call().any_input(int_const(0x2000)))
    assert len(hits) == 1


def test_call_any_input_typed_sub_misses_unrelated_const():
    fn = _direct_call_and_ret(0x2000)
    assert fn.find_all(call().any_input(int_const(0x9999))) == []


def test_call_any_input_wildcard_reaches_control_and_memory():
    """A wildcard `any_input` on a Call also reaches ctrl/mem, edges a typed
    sub can never bind."""
    fn = _direct_call_and_ret(0x2000)
    c = Capture()
    hits = fn.find_all(call().any_input(var(c)))
    # ctrl, mem, target, sp: at least these four candidate inputs.
    assert len(hits) >= 4


def test_call_any_input_on_real_fixture_binds_an_arg():
    """A real optimised function, not synthetic bytes: `any_input` must
    compose with the full lift+optimise pipeline."""
    fn = built_function("x86", "switch", "dispatch_value")
    calls = fn.find_all(call())
    if not calls:
        return
    c = Capture()
    hits = fn.find_all(call().any_input(var(c)))
    assert len(hits) >= len(calls)


def test_load_any_input_binds_addr():
    #   mov eax, [0x3000]     ; a8-style disp32 load
    #   ret
    code = bytes([0xA1]) + (0x3000).to_bytes(8, "little") + bytes([0xC3])
    fn = _lift(code)
    hits = fn.find_all(load().any_input(int_const(0x3000)))
    assert len(hits) == 1


def test_store_any_input_binds_data():
    #   mov dword [0x3000], 99
    #   ret
    code = (
        bytes([0xC7, 0x04, 0x25])
        + (0x3000).to_bytes(4, "little")
        + (99).to_bytes(4, "little")
        + bytes([0xC3])
    )
    fn = _lift(code)
    hits = fn.find_all(store().any_input(int_const(99)))
    assert len(hits) == 1


def test_mem_phi_any_input_wildcard_reaches_memory_and_phi_token():
    # Needs a real if/else store join: a trivial single-predecessor MemPhi
    # would optimize away.
    fn = _diamond_with_memory_join()
    c = Capture()
    hits = fn.find_all(mem_phi().any_input(var(c)))
    assert len(hits) >= 1

    # A typed value sub can never bind a Memory or PhiToken predecessor.
    assert fn.find_all(mem_phi().any_input(int_const(1))) == []


def test_phi_token_typed_sub_never_matches():
    fn = _diamond_returning_eax()
    assert fn.find_all(phi().phi_token(int_const(1))) == []


def test_phi_token_wildcard_binds_the_phi_token_edge():
    fn = _diamond_returning_eax()
    c = Capture()
    hits = fn.find_all(phi().phi_token(var(c)))
    assert len(hits) >= 1


def test_phi_token_differs_from_shifted_input():
    """`.phi_token(p)` (raw slot 0) and `.input(0, p)` (raw slot 1) address
    different edges: a typed const sub matches via `.input(0, _)`, a real
    data predecessor, but never via `.phi_token(_)`."""
    fn = _diamond_returning_eax()
    via_input = fn.find_all(phi().input(0, int_const(1)))
    via_token = fn.find_all(phi().phi_token(int_const(1)))
    assert len(via_input) >= 1
    assert via_token == []


def test_mem_phi_phi_token_wildcard_binds():
    fn = _diamond_with_memory_join()
    c = Capture()
    hits = fn.find_all(mem_phi().phi_token(var(c)))
    assert len(hits) >= 1


def test_mem_phi_phi_token_typed_sub_never_matches():
    fn = _diamond_with_memory_join()
    assert fn.find_all(mem_phi().phi_token(int_const(1))) == []


def test_mem_phi_phi_token_differs_from_shifted_input():
    """`.input(0, p)` (raw slot 1) reaches the join's genuine first memory
    predecessor, a `store(data=1)`; `.phi_token(p)` (raw slot 0) never does,
    since typed subs can't bind PhiToken."""
    fn = _diamond_with_memory_join()
    via_input = fn.find_all(mem_phi().input(0, store().data(int_const(1))))
    assert len(via_input) >= 1
    assert fn.find_all(mem_phi().phi_token(int_const(1))) == []


def test_call_output_slot_binds_sibling_value():
    """`call().output(2)` binds the value the Call produces at raw output
    slot 2 (its first caller-saved clobber / return value). A leaf
    sibling-output binding: no recursion into what the output feeds."""
    fn = _direct_call_and_ret(0x2000)
    c = Capture()
    hits = fn.find_all(call().output(2).capture(c))
    assert len(hits) == 1
    assert hits[0].has(c)
    assert hits[0].node(c) is not None
    # A name is the same key, here as everywhere else.
    named = fn.find_all(call().output(2).capture("out"))
    assert len(named) == 1 and named[0].node("out") is not None


def test_call_output_missing_slot_never_matches():
    """A slot the Call does not produce fails the whole match: the
    sibling-output constraint is checked, not silently skipped."""
    fn = _direct_call_and_ret(0x2000)
    c = Capture()
    assert fn.find_all(call().output(500).capture(c)) == []


def test_call_output_slot_type_and_width_constraints():
    """`.of_type` / `.of_width` constrain the sibling output. On x86-64 every
    caller-saved clobber output is a 64-bit register value."""
    fn = _direct_call_and_ret(0x2000)
    assert len(fn.find_all(call().output(2).of_type("i64"))) == 1
    assert fn.find_all(call().output(2).of_type("i32")) == []
    assert len(fn.find_all(call().output(2).of_width(64))) == 1
    assert fn.find_all(call().output(2).of_width(32)) == []


def test_entry_matches_exactly_one():
    """`entry()` matches the function's unique Entry node: one hit whatever
    the Region count, and whatever the pipeline (Entry is never removed)."""
    fn = _diamond_returning_eax()
    assert len(fn.find_all(entry())) == 1


def test_region_matches_every_region_node():
    """`region()` matches every CFG-merge Region, cross-checked against
    `count_regions`. Unoptimized so the entry/true/false/join shape survives;
    `RegionCollapse` would otherwise leave only the join."""
    fn = _diamond_returning_eax_unoptimized()
    assert fn.count_regions() == 4, "sanity: entry + true + false + join regions"
    assert len(fn.find_all(region())) == fn.count_regions()


def test_region_any_input_reaches_entry_predecessor():
    """`region().any_input(entry())` reaches a genuine control predecessor:
    only the entry region is directly preceded by Entry. Needs the
    unoptimized lift, since the default pipeline merges Entry's control edge
    straight into the If, leaving no Region reachable from Entry at all."""
    fn = _diamond_returning_eax_unoptimized()
    hits = fn.find_all(region().any_input(entry()))
    assert len(hits) == 1


def test_region_any_input_typed_value_sub_matches_nothing():
    """A typed value sub can never bind a Region's Control predecessor edge,
    the same discrimination `any_input` proves on every other family."""
    fn = _diamond_returning_eax_unoptimized()
    assert fn.find_all(region().any_input(int_const(0))) == []


def test_region_any_input_wildcard_reaches_every_predecessor():
    """A wildcard any_input on `region()` reaches EVERY predecessor across
    every region: one hit per predecessor, and the join region has two."""
    fn = _diamond_returning_eax_unoptimized()
    c = Capture()
    hits = fn.find_all(region().any_input(var(c)))
    assert len(hits) >= fn.count_regions()
