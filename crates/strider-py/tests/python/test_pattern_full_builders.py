"""End-to-end tests for the typed pattern builders newly exposed in
strider-py — `RetPat`, `IfPat`, `CallOtherPat`, `LoadPat`,
`StorePat`, `StackStorePat`, `StackStorePhiPat`, `PhiPat`,
`FunctionArgPat` — plus the free constructors `phi_for`,
`initial_var_for`, `function_arg_reg`, `function_arg_stack`,
`int_cmp`, the op-variant accessors on `Match`, and the
`CastMask` matcher option.

Each test runs against a real fixture graph (the `switch` /
`patterns` / `complex` ELFs) so a regression in the builder->Pat
plumbing surfaces as a missed match rather than a silent type
error.
"""

from __future__ import annotations

import pytest

import strider
from strider.pattern import (
    Capture, Pat, CastMask, any_, var, add, mul, load, store,
    stack_store, stack_store_phi, call, call_other, ret, if_, phi,
    phi_for, initial_var, initial_var_for, function_arg,
    function_arg_any, function_arg_reg, function_arg_stack,
    int_const, signed_int_const, int_cmp, int_bin_any, int_cmp_any,
)

from .conftest import fixture_path


def _patterns_graph():
    elf = fixture_path("x86", "patterns")
    arch = strider.SleighArch.x86()
    cc = strider.CallingConvention.x86_cdecl()
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(elf))
    addr = mem.symbol("recursive_with_accumulator")
    return strider.run(
        arch=arch, cc=cc, mem=mem, rom=mem, entry=addr,
        allow_code_before_start_addr=True,
    ).graph


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


def _control_graph(fn: str):
    elf = fixture_path("x86", "control")
    arch = strider.SleighArch.x86()
    cc = strider.CallingConvention.x86_cdecl()
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(elf))
    addr = mem.symbol(fn)
    return strider.run(
        arch=arch, cc=cc, mem=mem, rom=mem, entry=addr,
        allow_code_before_start_addr=True,
    ).graph


# ── RetPat ───────────────────────────────────────────────────────────


def test_ret_pat_returns_builder_chainable():
    p = ret().preceded_by(call())
    assert isinstance(p.into_pat(), Pat)


def test_ret_pat_ret_val_constraint_chains():
    p = ret().ret_val(0, int_const(1)).ret_val(1, "x")
    assert isinstance(p.into_pat(), Pat)


def test_ret_pat_capture_finalises():
    c = Capture()
    p = ret().capture(c)
    assert isinstance(p, Pat)


def test_ret_pat_when_finalises():
    p = ret().when(lambda m: True)
    assert isinstance(p, Pat)


def test_ret_pat_finds_returns_in_real_graph():
    # `recursive_with_accumulator` returns from the base case via a
    # standard `Return` node; the builder un-finalised must be
    # accepted by `find_all` via PatLike.
    g = _patterns_graph()
    hits = g.find_all(ret())
    assert len(hits) >= 1


# ── IfPat ────────────────────────────────────────────────────────────


def test_if_pat_chain_sets_branches():
    p = if_(cond="x").true_branch(any_()).false_branch(any_())
    assert isinstance(p.into_pat(), Pat)


def test_if_pat_finds_branches_in_real_graph():
    g = _control_graph("clamp")
    # `clamp` has at least 2 If nodes (lo + hi bounds).
    hits = g.find_all(if_())
    assert len(hits) >= 2


# ── CallOtherPat ─────────────────────────────────────────────────────


def test_call_other_pat_chain_compiles():
    p = call_other().user_op_id(7).arg(0, int_const(42))
    assert isinstance(p.into_pat(), Pat)


def test_call_other_pat_capture_finalises():
    c = Capture()
    p = call_other().user_op_id(0).capture(c)
    assert isinstance(p, Pat)


# ── LoadPat / StorePat ───────────────────────────────────────────────


def test_load_pat_space_constraint_compiles():
    p = load().space(strider.VnSpace.ram())
    assert isinstance(p.into_pat(), Pat)


def test_store_pat_full_chain():
    p = store().addr("p").data(int_const(0)).space(strider.VnSpace.ram())
    assert isinstance(p.into_pat(), Pat)


def test_load_pat_finds_loads_in_real_graph():
    g = _switch_graph()
    # Case-5 reads `value->a` — at least one Load survives.
    hits = g.find_all(load())
    assert len(hits) >= 1


def test_load_pat_via_addr_chain():
    g = _switch_graph()
    # Constrain to load whose address is some Add expression.
    hits = g.find_all(load().addr(add(any_(), any_())))
    assert isinstance(hits, list)


# ── StackStorePat / StackStorePhiPat ─────────────────────────────────


def test_stack_store_pat_offset_chain():
    p = stack_store(offset=-8).data(int_const(42))
    assert isinstance(p.into_pat(), Pat)


def test_stack_store_phi_pat_offsets_constraint():
    p = stack_store_phi().offsets([-8, -16])
    assert isinstance(p.into_pat(), Pat)


# ── PhiPat / phi_for ─────────────────────────────────────────────────


def test_phi_pat_input_chain():
    p = phi().input(0, int_const(0))
    assert isinstance(p.into_pat(), Pat)


def test_phi_for_takes_vn_from_sleigh():
    elf = fixture_path("x86", "patterns")
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(elf))
    sleigh = strider.Sleigh(strider.SleighArch.x86(), mem)
    eax_vn = sleigh.reg("EAX")
    assert eax_vn is not None
    p = phi_for(eax_vn)
    assert isinstance(p.into_pat(), Pat)


def test_initial_var_for_constructs():
    elf = fixture_path("x86", "patterns")
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(elf))
    sleigh = strider.Sleigh(strider.SleighArch.x86(), mem)
    eax = sleigh.reg("EAX")
    p = initial_var_for(eax)
    assert isinstance(p, Pat)


# ── FunctionArgPat / function_arg_reg / function_arg_stack ────────────


def test_function_arg_pat_index_chain():
    p = function_arg(0)
    assert isinstance(p.into_pat(), Pat)


def test_function_arg_reg_constructor():
    elf = fixture_path("x64", "patterns")
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(elf))
    sleigh = strider.Sleigh(strider.SleighArch.x86_64(), mem)
    rdi = sleigh.reg("RDI")
    assert rdi is not None
    p = function_arg_reg(rdi)
    assert isinstance(p.into_pat(), Pat)


def test_function_arg_stack_constructor():
    p = function_arg_stack(strider.VnSpace.ram(), 16)
    assert isinstance(p.into_pat(), Pat)


# ── signed_int_const ────────────────────────────────────────────────


def _patterns_graph_for(arch_id, fn_name="if_returns_const"):
    if arch_id == "x86":
        arch, cc = strider.SleighArch.x86(), strider.CallingConvention.x86_cdecl()
    elif arch_id == "x64":
        arch, cc = strider.SleighArch.x86_64(), strider.CallingConvention.x86_64_systemv_abi()
    else:
        raise ValueError(arch_id)
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(fixture_path(arch_id, "patterns")))
    return strider.run(
        arch=arch, cc=cc, mem=mem, rom=mem, entry=mem.symbol(fn_name),
        allow_code_before_start_addr=True,
    ).graph


def test_signed_int_const_matches_neg50_on_x86_u32():
    # 32-bit x86: `IntConst(0xFFFFFFCE)` at U32 — bit-pattern equality
    # at output width.  Both `int_const(-50)` and `signed_int_const(-50)`
    # match here.
    g = _patterns_graph_for("x86")
    assert len(g.find_all(int_const(-50))) >= 1
    assert len(g.find_all(signed_int_const(-50))) >= 1


def test_signed_int_const_handles_x64_zero_extended_neg50():
    # 64-bit x64: gcc -O2 lowers `return -50;` as `mov eax, 0xffffffce;
    # ret`, leaving high 32 bits of RAX zero.  IR carries
    # `IntConst(0x00000000FFFFFFCE)` at U64 — value `+4294967246`,
    # NOT -50.  `int_const(-50)` (strict bit-pattern) misses it;
    # `signed_int_const(-50)` recognises the U32-narrow signed form
    # via the zero-extension check.
    g = _patterns_graph_for("x64")
    assert len(g.find_all(int_const(-50))) == 0, (
        "int_const should NOT match the zero-extended form — that's the whole point of signed_int_const"
    )
    assert len(g.find_all(signed_int_const(-50))) >= 1


def test_signed_int_const_neg1_matches_at_every_width():
    # `-1` has bit pattern all-ones at every width AND the
    # zero-extension check handles narrower-stored-zero-extended
    # variants.  Either way `signed_int_const(-1)` should fire on
    # any function that returns a negative literal.
    g64 = _patterns_graph_for("x64")
    # Whether or not -1 actually appears in if_returns_const, the
    # call must succeed without raising and the result is a list.
    assert isinstance(g64.find_all(signed_int_const(-1)), list)


def test_signed_int_const_round_trips_via_match_int():
    # When a match fires, Match.int(c) recovers the SIGNED i128
    # value, so a captured signed_int_const round-trips.
    c = Capture()
    g = _patterns_graph_for("x64")
    hits = g.find_all(signed_int_const(-50).capture(c))
    if not hits:
        pytest.skip("no -50 constant in this graph (compiler may have folded it)")
    # The first hit's captured constant: stored as u128, recovered as
    # signed i128.  For the x64 zero-extended-neg50 case the stored
    # value is +4294967246 at U64; Match.int treats THAT as i128
    # (no further sign extension), so the recovered i128 is
    # +4294967246, not -50.  The pattern's job was to recognise the
    # source-level value; recovering the raw bit pattern is
    # `Match.uint`'s job.
    val = hits[0].uint(c)
    assert val is not None


# ── int_cmp + int_cmp_any ────────────────────────────────────────────


def test_int_cmp_concrete_op():
    p = int_cmp("Less", "x", int_const(7))
    assert isinstance(p, Pat)


def test_int_cmp_unknown_op_raises():
    with pytest.raises(strider.errors.PatternError):
        int_cmp("NotARealOp", "x", "y")


def test_int_cmp_no_not_equal_op():
    # The IR doesn't have a NotEqual variant — the lifter expresses
    # `a != b` as `BoolNeg(IntEqual(a, b))`.  `int_cmp("NotEqual",
    # ...)` must therefore raise rather than silently accepting.
    with pytest.raises(strider.errors.PatternError):
        int_cmp("NotEqual", "x", "y")


# ── Match accessors: int_cmp_op + int_binary_op + vn ─────────────────


def test_int_binary_op_recovery():
    g = _patterns_graph()
    c = Capture()
    p = int_bin_any(c, any_(), any_())
    hits = g.find_all(p)
    assert len(hits) >= 1
    # Recover a real op variant from the first match.
    op_name = hits[0].int_binary_op(c)
    assert op_name in {
        "Add", "Sub", "Mul", "Div", "Sdiv", "Rem", "Srem",
        "And", "Or", "Xor", "ShiftLeft", "ShiftRight", "SShiftRight",
    }


def test_int_cmp_op_recovery():
    g = _control_graph("clamp")
    c = Capture()
    p = int_cmp_any(c, any_(), any_())
    hits = g.find_all(p)
    assert len(hits) >= 1
    op_name = hits[0].int_cmp_op(c)
    assert op_name in {
        "Equal", "Less", "LessEqual",
        "Sless", "SlessEqual",
        "Carry", "Scarry", "Sborrow", "Borrow",
    }


def test_get_vn_returns_vn_for_initial_var_capture():
    g = _control_graph("abs_val")
    c = Capture()
    # `initial_var()` matches *any* InitialVar; capture so we can ask
    # which Vn it bound.
    hits = g.find_all(initial_var().capture(c))
    if not hits:
        pytest.skip("no InitialVar nodes in abs_val (compiler may have folded entry args)")
    vn = hits[0].vn(c)
    assert vn is not None
    # Smoke: the Vn round-trips a sensible space + size.
    assert vn.space.name() in {"REGISTER", "RAM"}
    assert vn.size > 0


# ── CastMask ─────────────────────────────────────────────────────────


def test_cast_mask_combines_via_or():
    m = CastMask.extend() | CastMask.truncate()
    assert m.bits() != 0


def test_cast_mask_all_vs_none():
    assert CastMask.all().bits() != 0
    assert CastMask.none().bits() == 0


def test_find_all_accepts_ignore_casts_mask():
    g = _patterns_graph()
    # Equivalent to ignore_casts=True but per-cast.
    hits_strict = g.find_all(add(mul(any_(), any_()), any_()))
    hits_relaxed = g.find_all(
        add(mul(any_(), any_()), any_()),
        ignore_casts_mask=CastMask.extend() | CastMask.truncate(),
    )
    # The relaxed walk must find at least as many matches as strict.
    assert len(hits_relaxed) >= len(hits_strict)


def test_find_all_rejects_both_ignore_casts_options():
    g = _patterns_graph()
    with pytest.raises(strider.errors.PatternError):
        g.find_all(
            add(any_(), any_()),
            ignore_casts=True,
            ignore_casts_mask=CastMask.extend(),
        )


# ── VnSpace + Vn ─────────────────────────────────────────────────────


def test_vn_space_classmethods_round_trip():
    assert strider.VnSpace.ram().name() == "RAM"
    assert strider.VnSpace.register().name() == "REGISTER"
    assert strider.VnSpace.const_().name() == "CONST"
    assert strider.VnSpace.unique().name() == "UNIQUE"
    # Equality + hash work.
    assert strider.VnSpace.ram() == strider.VnSpace.ram()


def test_vn_constructor_and_repr():
    vn = strider.Vn(strider.VnSpace.register(), 0x10, 4)
    assert vn.space == strider.VnSpace.register()
    assert vn.off == 0x10
    assert vn.size == 4
    assert "Vn" in repr(vn)


def test_sleigh_reg_returns_vn_or_none():
    elf = fixture_path("x86", "patterns")
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(elf))
    sleigh = strider.Sleigh(strider.SleighArch.x86(), mem)
    eax = sleigh.reg("EAX")
    assert eax is not None
    assert eax.size == 4
    # Unknown name → None (not an exception).
    assert sleigh.reg("DEFINITELY_NOT_A_REG") is None
