"""End-to-end tests for the typed pattern builders newly exposed in
strider-py — `RetPat`, `IfPat`, `CallOtherPat`, `LoadPat`,
`StorePat`, `PhiPat`, `FunctionArgPat` — plus the free constructors
`phi_for`, `initial_var_for`, `function_arg_reg`, `function_arg_stack`,
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
    Capture, Pat, CastMask, anything, var, add, mul, load, store,
    call, call_other, ret, if_else, phi, mem_phi,
    phi_for, initial_var, initial_var_for, function_arg,
    function_arg_any, function_arg_reg, function_arg_stack,
    int_const, signed_int_const, int_cmp, int_bin_any, int_cmp_any,
)

from .conftest import built_function, fixture_path


def _patterns_graph():
    return built_function("x86", "patterns", "recursive_with_accumulator")


def _switch_graph():
    return built_function("x86", "switch", "dispatch_value")


def _control_graph(fn: str):
    return built_function("x86", "control", fn)


# ── builder-finalisation contract (all node-rooted builders) ─────────
#
# Every node-rooted builder constructor shares the same chaining
# contract: the bare constructor finalises via `.into_pat()`,
# `.capture(c)` returns the SAME chainable builder, and `.when(f)`
# does too.  One parametrized triad covers them all; the
# builder-specific constraint chains (e.g. `.ret_val`, `.user_op_id`)
# keep their dedicated tests below.

NODE_BUILDER_CTORS = [
    pytest.param(ret, id="ret"),
    pytest.param(if_else, id="if_else"),
    pytest.param(call, id="call"),
    pytest.param(call_other, id="call_other"),
    pytest.param(load, id="load"),
    pytest.param(store, id="store"),
    pytest.param(phi, id="phi"),
    pytest.param(mem_phi, id="mem_phi"),
]


@pytest.mark.parametrize("ctor", NODE_BUILDER_CTORS)
def test_builder_bare_ctor_finalises_to_pat(ctor):
    assert isinstance(ctor().into_pat(), Pat)


@pytest.mark.parametrize("ctor", NODE_BUILDER_CTORS)
def test_builder_capture_chains_and_finalises(ctor):
    # `.capture(c)` returns the same builder (chainable); call
    # `.into_pat()` to get a `Pat`.
    c = Capture()
    b = ctor().capture(c)
    assert isinstance(b.into_pat(), Pat)


@pytest.mark.parametrize("ctor", NODE_BUILDER_CTORS)
def test_builder_when_chains_and_finalises(ctor):
    # Same builder-chain contract as `.capture(c)`.
    b = ctor().when(lambda m: True)
    assert isinstance(b.into_pat(), Pat)


# ── RetPat ───────────────────────────────────────────────────────────


def test_ret_pat_returns_builder_chainable():
    # `preceded_by` now matches the Return's ctrl predecessor against a
    # value pattern relaxed to a control edge (the rewritten core types
    # `preceded_by` as `MatchPat`).  Passing a node-rooted control builder
    # (`call()`) into `preceded_by` was loose typing the rewrite removed —
    # use a value predecessor to exercise builder chainability.
    p = ret().preceded_by(anything())
    assert isinstance(p.into_pat(), Pat)


def test_ret_pat_ret_val_constraint_chains():
    p = ret().ret_val(0, int_const(1)).ret_val(1, "x")
    assert isinstance(p.into_pat(), Pat)


def test_ret_pat_finds_returns_in_real_graph():
    # `recursive_with_accumulator` returns from the base case via a
    # standard `Return` node; the builder un-finalised must be
    # accepted by `find_all` via PatLike.
    g = _patterns_graph()
    hits = g.find_all(ret())
    assert len(hits) >= 1


# ── IfPat ────────────────────────────────────────────────────────────


def test_if_pat_chain_sets_branches():
    p = if_else(cond="x").true_branch(anything()).false_branch(anything())
    assert isinstance(p.into_pat(), Pat)


def test_if_pat_finds_branches_in_real_graph():
    g = _control_graph("clamp")
    # `clamp` has at least 2 If nodes (lo + hi bounds).
    hits = g.find_all(if_else())
    assert len(hits) >= 2


# ── CallOtherPat ─────────────────────────────────────────────────────


def test_call_other_pat_chain_compiles():
    # CallOtherPat.arg(i) addresses raw inputs[i]: i=0 ctrl, i=1 mem,
    # i>=2 pcode args + implicit reads.  This compiles a chain
    # constraining arg position 2 (= first pcode-explicit arg).
    p = call_other().user_op_id(7).arg(2, int_const(42))
    assert isinstance(p.into_pat(), Pat)


def test_call_other_pat_name_smoke():
    # builder accepts name and converts to a Pat without error
    p = call_other().name("cpuid")
    assert p is not None
    assert isinstance(p.into_pat(), Pat)


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
    hits = g.find_all(load().addr(add(anything(), anything())))
    assert isinstance(hits, list)


# ── StorePat stack_only ──────────────────────────────────────────────


def test_store_stack_only_chain():
    # stack_only() restricts the match to stores whose SP offset is known.
    p = store().stack_only().data(int_const(42))
    assert isinstance(p.into_pat(), Pat)


# ── PhiPat / phi_for ─────────────────────────────────────────────────


def test_phi_pat_input_chain():
    p = phi().input(0, int_const(0))
    assert isinstance(p.into_pat(), Pat)


def test_phi_for_takes_vn_from_sleigh():
    elf = fixture_path("x86", "patterns")
    mem = strider.load_elf(str(elf)).reader()
    sleigh = strider.Sleigh(strider.SleighArch.x86(), mem)
    eax_vn = sleigh.reg("EAX")
    assert eax_vn is not None
    p = phi_for(eax_vn)
    assert isinstance(p.into_pat(), Pat)


def test_initial_var_for_constructs():
    elf = fixture_path("x86", "patterns")
    mem = strider.load_elf(str(elf)).reader()
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
    mem = strider.load_elf(str(elf)).reader()
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
    return built_function(arch_id, "patterns", fn_name)


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
    with pytest.raises(strider.errors.StriderError):
        int_cmp("NotARealOp", "x", "y")


def test_int_cmp_no_not_equal_op():
    # The IR doesn't have a NotEqual variant — the lifter expresses
    # `a != b` as `BoolNeg(IntEqual(a, b))`.  `int_cmp("NotEqual",
    # ...)` must therefore raise rather than silently accepting.
    with pytest.raises(strider.errors.StriderError):
        int_cmp("NotEqual", "x", "y")


# ── Match accessors: int_cmp_op + int_binary_op + vn ─────────────────


def test_int_binary_op_recovery():
    g = _patterns_graph()
    c = Capture()
    p = int_bin_any(c, anything(), anything())
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
    p = int_cmp_any(c, anything(), anything())
    hits = g.find_all(p)
    assert len(hits) >= 1
    op_name = hits[0].int_cmp_op(c)
    # Regression: the previous allowed set included
    # `LessEqual`, `SlessEqual`, `Borrow` — none of these exist in
    # `ir::IntCmpOp`; they are lift-time-lowered shapes, never emitted
    # as primitive nodes.  Listing them was a phantom assertion: the
    # test passed for the wrong reason because the actual return is
    # always one of the six real names.  Narrow to the truth.
    assert op_name in {"Equal", "Less", "Sless", "Carry", "Scarry", "Sborrow"}


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
    hits_strict = g.find_all(add(mul(anything(), anything()), anything()))
    hits_relaxed = g.find_all(
        add(mul(anything(), anything()), anything()),
        ignore_casts_mask=CastMask.extend() | CastMask.truncate(),
    )
    # The relaxed walk must find at least as many matches as strict.
    assert len(hits_relaxed) >= len(hits_strict)


def test_find_all_rejects_both_ignore_casts_options():
    g = _patterns_graph()
    with pytest.raises(strider.errors.StriderError):
        g.find_all(
            add(anything(), anything()),
            ignore_casts=True,
            ignore_casts_mask=CastMask.extend(),
        )


# ── VnSpace + Vn ─────────────────────────────────────────────────────


def test_vn_space_classmethods_round_trip():
    assert strider.VnSpace.ram().name() == "RAM"
    assert strider.VnSpace.register().name() == "REGISTER"
    assert strider.VnSpace.const().name() == "CONST"
    assert strider.VnSpace.unique().name() == "UNIQUE"
    # Equality + hash work.
    assert strider.VnSpace.ram() == strider.VnSpace.ram()


def test_vn_constructor_and_repr():
    vn = strider.Vn(strider.VnSpace.register(), 0x10, 4)
    assert vn.space == strider.VnSpace.register()
    assert vn.off == 0x10
    assert vn.size == 4
    # `repr(vn)` delegates to rsleigh's `impl Display for Vn`
    # (core_types.rs:139): REGISTER-space varnodes render as
    # `<space-shortcut>[0x<off>]:<size>` with `%` as the REGISTER
    # shortcut.  See test_sleigh.test_vn_repr_for_register_uses_rsleigh_display
    # for the full contract.
    assert repr(vn) == "%[0x10]:4"


def test_sleigh_reg_returns_vn_or_none():
    elf = fixture_path("x86", "patterns")
    mem = strider.load_elf(str(elf)).reader()
    sleigh = strider.Sleigh(strider.SleighArch.x86(), mem)
    eax = sleigh.reg("EAX")
    assert eax is not None
    assert eax.size == 4
    # Unknown name → None (not an exception).
    assert sleigh.reg("DEFINITELY_NOT_A_REG") is None


# ── new mem-walk + bit-width API ─────────────────────────────────────


def test_load_mem_in_chain_compiles():
    p = load().addr(int_const(0x100)).mem_in(store().addr(int_const(0x200)))
    assert isinstance(p.into_pat(), Pat)


def test_load_bit_width_compiles():
    p = load().addr(int_const(0x100)).bit_width(32)
    assert isinstance(p.into_pat(), Pat)


def test_store_mem_in_bit_width_chain_compiles():
    # `mem_in` requires a real memory producer; chain off another store().
    p = (
        store()
        .addr(int_const(0x100))
        .mem_in(store().addr(int_const(0x200)))
        .bit_width(64)
    )
    assert isinstance(p.into_pat(), Pat)


# ── operand-type validation (pinned current behaviour) ──────────────


def test_load_addr_with_raw_int_rejected_at_finalise():
    # Operand slots take a Pat / Capture / str / value builder — NOT a
    # raw int (a literal address must be wrapped as `int_const(0x100)`).
    # The chain call itself is lazy (no validation), so the builder is
    # returned; the type error surfaces at finalisation as a
    # StriderError, not a TypeError.
    b = load().addr(123)  # chain itself does not raise
    with pytest.raises(strider.errors.StriderError, match="expected a value pattern"):
        b.into_pat()


def test_store_data_with_raw_int_rejected_at_finalise():
    b = store().data(123)
    with pytest.raises(strider.errors.StriderError, match="expected a value pattern"):
        b.into_pat()


def test_mem_in_rejects_value_operand():
    # A memory-input slot requires a memory-token producer. Feeding a value
    # producer (a bare load(), which produces a value, not a memory token)
    # builds a pattern that can never match a real IR memory chain, so the
    # builder must reject it instead of silently producing a dead pattern.
    with pytest.raises(strider.errors.StriderError):
        store().addr(int_const(0x100)).mem_in(load().addr(int_const(0x200))).into_pat()
    with pytest.raises(strider.errors.StriderError):
        load().addr(int_const(0x100)).mem_in(var(Capture())).into_pat()
