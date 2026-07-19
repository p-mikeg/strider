"""The typed pattern builders (`RetPat`, `IfPat`, `CallOtherPat`, `LoadPat`,
`StorePat`, `PhiPat`, `FunctionArgPat`), the free constructors, the
op-variant accessors on `Match`, and the `CastMask` matcher option.

Tests run against real fixture graphs so a regression in the builder to Pat
plumbing surfaces as a missed match rather than a silent type error.
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
    any_int_const,
)

from .conftest import built_function, fixture_path


def _patterns_graph():
    return built_function("x86", "patterns", "recursive_with_accumulator")


def _switch_graph():
    return built_function("x86", "switch", "dispatch_value")


def _control_graph(fn: str):
    return built_function("x86", "control", fn)


# Every node-rooted builder shares one chaining contract: the bare
# constructor finalises via `.into_pat()`, and `.capture(c)` / `.when(f)`
# both return the SAME chainable builder.  Builder-specific constraint
# chains (`.ret_val`, `.user_op_id`, ...) keep their own tests below.

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
    c = Capture()
    b = ctor().capture(c)
    assert isinstance(b.into_pat(), Pat)


@pytest.mark.parametrize("ctor", NODE_BUILDER_CTORS)
def test_builder_when_chains_and_finalises(ctor):
    b = ctor().when(lambda m: True)
    assert isinstance(b.into_pat(), Pat)


def test_ret_pat_returns_builder_chainable():
    # `preceded_by` takes a value pattern relaxed onto the Return's ctrl
    # predecessor.  Passing a node-rooted control builder (`call()`) used to
    # be accepted under looser typing and no longer is, hence the value
    # predecessor here.
    p = ret().preceded_by(anything())
    assert isinstance(p.into_pat(), Pat)


def test_ret_pat_ret_val_constraint_chains():
    p = ret().ret_val(0, int_const(1)).ret_val(1, "x")
    assert isinstance(p.into_pat(), Pat)


def test_ret_pat_finds_returns_in_real_graph():
    # An un-finalised builder must be accepted by `find_all` directly.
    g = _patterns_graph()
    hits = g.find_all(ret())
    assert len(hits) >= 1


def test_if_pat_chain_sets_branches():
    p = if_else(cond="x").true_branch(anything()).false_branch(anything())
    assert isinstance(p.into_pat(), Pat)


def test_if_pat_finds_branches_in_real_graph():
    g = _control_graph("clamp")
    # `clamp` has at least 2 If nodes (lo + hi bounds).
    hits = g.find_all(if_else())
    assert len(hits) >= 2


def test_call_other_pat_chain_compiles():
    # `arg(i)` addresses raw inputs[i]: i=0 ctrl, i=1 mem, i>=2 pcode args
    # plus implicit reads.  So arg 2 is the first pcode-explicit arg.
    p = call_other().user_op_id(7).arg(2, int_const(42))
    assert isinstance(p.into_pat(), Pat)


def test_call_other_pat_name_smoke():
    p = call_other().name("cpuid")
    assert p is not None
    assert isinstance(p.into_pat(), Pat)


def test_load_pat_space_constraint_compiles():
    p = load().space(strider.sleigh.VnSpace.RAM)
    assert isinstance(p.into_pat(), Pat)


def test_store_pat_full_chain():
    p = store().addr("p").data(int_const(0)).space(strider.sleigh.VnSpace.RAM)
    assert isinstance(p.into_pat(), Pat)


def test_load_pat_finds_loads_in_real_graph():
    g = _switch_graph()
    # Case 5 reads `value->a`, so at least one Load survives.
    hits = g.find_all(load())
    assert len(hits) >= 1


def test_load_pat_via_addr_chain():
    g = _switch_graph()
    hits = g.find_all(load().addr(add(anything(), anything())))
    assert isinstance(hits, list)


def test_store_stack_only_chain():
    # stack_only() restricts the match to stores whose SP offset is known.
    p = store().stack_only().data(int_const(42))
    assert isinstance(p.into_pat(), Pat)


def test_phi_pat_input_chain():
    p = phi().input(0, int_const(0))
    assert isinstance(p.into_pat(), Pat)


def test_phi_for_takes_vn_from_sleigh():
    elf = fixture_path("x86", "patterns")
    mem = strider.lift.load_elf(str(elf)).reader()
    sleigh = strider.sleigh.Sleigh(strider.sleigh.SleighArch.x86(), mem)
    eax_vn = sleigh.reg("EAX")
    assert eax_vn is not None
    p = phi_for(eax_vn)
    assert isinstance(p.into_pat(), Pat)


def test_initial_var_for_constructs():
    elf = fixture_path("x86", "patterns")
    mem = strider.lift.load_elf(str(elf)).reader()
    sleigh = strider.sleigh.Sleigh(strider.sleigh.SleighArch.x86(), mem)
    eax = sleigh.reg("EAX")
    p = initial_var_for(eax)
    assert isinstance(p, Pat)


def test_function_arg_pat_index_chain():
    p = function_arg(0)
    assert isinstance(p.into_pat(), Pat)


def test_function_arg_reg_constructor():
    elf = fixture_path("x64", "patterns")
    mem = strider.lift.load_elf(str(elf)).reader()
    sleigh = strider.sleigh.Sleigh(strider.sleigh.SleighArch.x86_64(), mem)
    rdi = sleigh.reg("RDI")
    assert rdi is not None
    p = function_arg_reg(rdi)
    assert isinstance(p.into_pat(), Pat)


def test_function_arg_stack_constructor():
    p = function_arg_stack(strider.sleigh.VnSpace.RAM, 16)
    assert isinstance(p.into_pat(), Pat)


def _patterns_graph_for(arch_id, fn_name="if_returns_const"):
    return built_function(arch_id, "patterns", fn_name)


def test_signed_int_const_matches_neg50_on_x86_u32():
    # 32-bit x86 stores -50 as `IntConst(0xFFFFFFCE)` at U32, so plain
    # bit-pattern equality at output width already matches.
    g = _patterns_graph_for("x86")
    assert len(g.find_all(int_const(-50))) >= 1
    assert len(g.find_all(signed_int_const(-50))) >= 1


def test_signed_int_const_handles_x64_zero_extended_neg50():
    # gcc -O2 lowers `return -50;` as `mov eax, 0xffffffce; ret`, leaving
    # RAX's high 32 bits zero.  The IR constant is 0x00000000FFFFFFCE at U64,
    # i.e. +4294967246, so strict bit-pattern `int_const(-50)` misses it and
    # only `signed_int_const` recognises the narrow signed form.
    g = _patterns_graph_for("x64")
    assert len(g.find_all(int_const(-50))) == 0, (
        "int_const should NOT match the zero-extended form — that's the whole point of signed_int_const"
    )
    assert len(g.find_all(signed_int_const(-50))) >= 1


def test_signed_int_const_neg1_matches_at_every_width():
    # -1 is all-ones at every width, and the zero-extension check covers the
    # narrower-stored variants, so `signed_int_const(-1)` fires on any
    # function returning a negative literal.  Whether -1 appears in
    # if_returns_const at all is compiler-dependent, so only pin "no raise".
    g64 = _patterns_graph_for("x64")
    assert isinstance(g64.find_all(signed_int_const(-1)), list)


def test_signed_int_const_round_trips_via_match_int():
    c = Capture()
    g = _patterns_graph_for("x64")
    hits = g.find_all(signed_int_const(-50).capture(c))
    if not hits:
        pytest.skip("no -50 constant in this graph (compiler may have folded it)")
    # For the x64 zero-extended -50 the stored value is +4294967246 at U64
    # and const_int does no further sign extension, so it recovers
    # +4294967246, not -50.  The pattern recognises the source-level value;
    # recovering the raw bit pattern is const_uint's job.
    val = hits[0].const_uint(c)
    assert val is not None


def test_int_cmp_concrete_op():
    p = int_cmp("Less", "x", int_const(7))
    assert isinstance(p, Pat)


def test_int_cmp_unknown_op_raises():
    with pytest.raises(strider.StriderError):
        int_cmp("NotARealOp", "x", "y")


def test_int_cmp_no_not_equal_op():
    # The IR has no NotEqual variant; the lifter lowers `a != b` to a
    # negated IntEqual.  So the name must raise, not be silently accepted.
    with pytest.raises(strider.StriderError):
        int_cmp("NotEqual", "x", "y")


def test_int_binary_op_recovery():
    g = _patterns_graph()
    c = Capture()
    p = int_bin_any(c, anything(), anything())
    hits = g.find_all(p)
    assert len(hits) >= 1
    op_name = hits[0].op(c)
    assert op_name in {
        "Add", "Sub", "Mul", "Div", "Sdiv", "Rem", "Srem",
        "And", "Or", "Xor", "ShiftLeft", "ShiftRight", "SShiftRight",
    }


def test_op_and_value_type_replace_the_seven_family_accessors():
    """One `op()` covers every op family, and `value_type()` carries what the
    old `bool_binary_op` encoded: a boolean op is an `IntBinaryOp` at `I1`,
    so the pair distinguishes it from a same-shaped wide bitwise op. With
    `kind()` naming the family, the seven per-family accessors were pure
    duplication and must stay gone."""
    g = _patterns_graph()
    c = Capture()
    hits = g.find_all(int_bin_any(c, anything(), anything()))
    assert len(hits) >= 1
    node = hits[0].node(c)

    # kind() names the family AND the variant; op() is just the variant.
    assert node.kind() == f"IntBinaryOp({node.op()})"
    assert hits[0].op(c) == node.op()
    assert hits[0].value_type(c) == node.value_type()
    assert node.value_type().startswith(("I", "F"))

    for gone in (
        "int_binary_op", "int_unary_op", "int_cmp_op", "bool_binary_op",
        "float_binary_op", "float_unary_op", "float_cmp_op",
    ):
        assert not hasattr(node, gone), f"Node.{gone} should be gone"
        assert not hasattr(hits[0], gone), f"Match.{gone} should be gone"


def test_op_is_none_for_a_node_carrying_no_operation():
    """A node that is not an operation answers `None` from `op()` rather
    than raising."""
    g = _patterns_graph()
    c = Capture()
    hits = g.find_all(any_int_const(c))
    assert len(hits) >= 1
    assert hits[0].node(c).op() is None
    assert hits[0].op(c) is None


def test_int_cmp_op_recovery():
    g = _control_graph("clamp")
    c = Capture()
    p = int_cmp_any(c, anything(), anything())
    hits = g.find_all(p)
    assert len(hits) >= 1
    op_name = hits[0].op(c)
    # Regression: the allowed set used to also list LessEqual, SlessEqual and
    # Borrow, which are lift-time-lowered shapes and never exist as primitive
    # nodes.  That made this a phantom assertion (it passed only because the
    # real answer is always one of the six below).
    assert op_name in {"Equal", "Less", "Sless", "Carry", "Scarry", "Sborrow"}


def test_get_vn_returns_vn_for_initial_var_capture():
    g = _control_graph("abs_val")
    c = Capture()
    hits = g.find_all(initial_var().capture(c))
    if not hits:
        pytest.skip("no InitialVar nodes in abs_val (compiler may have folded entry args)")
    vn = hits[0].vn(c)
    assert vn is not None
    assert vn.space.name() in {"REGISTER", "RAM"}
    assert vn.size > 0


def test_cast_mask_combines_via_or():
    m = CastMask.extend() | CastMask.truncate()
    assert m.bits() != 0


def test_cast_mask_all_vs_none():
    assert CastMask.all().bits() != 0
    assert CastMask.none().bits() == 0


def test_find_all_accepts_ignore_casts_mask():
    g = _patterns_graph()
    # The mask form is ignore_casts=True narrowed to specific cast kinds.
    hits_strict = g.find_all(add(mul(anything(), anything()), anything()))
    hits_relaxed = g.find_all(
        add(mul(anything(), anything()), anything()),
        ignore_casts_mask=CastMask.extend() | CastMask.truncate(),
    )
    assert len(hits_relaxed) >= len(hits_strict)


def test_find_all_rejects_both_ignore_casts_options():
    g = _patterns_graph()
    with pytest.raises(strider.StriderError):
        g.find_all(
            add(anything(), anything()),
            ignore_casts=True,
            ignore_casts_mask=CastMask.extend(),
        )


def test_vn_space_constants_round_trip():
    assert strider.sleigh.VnSpace.RAM.name() == "RAM"
    assert strider.sleigh.VnSpace.REGISTER.name() == "REGISTER"
    assert strider.sleigh.VnSpace.CONST.name() == "CONST"
    assert strider.sleigh.VnSpace.UNIQUE.name() == "UNIQUE"
    assert strider.sleigh.VnSpace.RAM == strider.sleigh.VnSpace.RAM


def test_vn_constructor_and_repr():
    vn = strider.sleigh.Vn(strider.sleigh.VnSpace.REGISTER, 0x10, 4)
    assert vn.space == strider.sleigh.VnSpace.REGISTER
    assert vn.off == 0x10
    assert vn.size == 4
    # REGISTER-space varnodes repr as `<space-shortcut>[0x<off>]:<size>`,
    # with `%` for REGISTER.  Full contract in
    # test_sleigh.test_vn_repr_for_register_uses_rsleigh_display.
    assert repr(vn) == "%[0x10]:4"


def test_sleigh_reg_returns_vn_or_none():
    elf = fixture_path("x86", "patterns")
    mem = strider.lift.load_elf(str(elf)).reader()
    sleigh = strider.sleigh.Sleigh(strider.sleigh.SleighArch.x86(), mem)
    eax = sleigh.reg("EAX")
    assert eax is not None
    assert eax.size == 4
    # Unknown name yields None, not an exception.
    assert sleigh.reg("DEFINITELY_NOT_A_REG") is None


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


def test_load_addr_with_raw_int_rejected_at_finalise():
    # Operand slots take a Pat / Capture / str / value builder, never a raw
    # int (a literal address must be wrapped as `int_const(0x100)`).  Chain
    # calls are lazy, so the error surfaces at finalisation as a
    # StriderError, not as a TypeError at the call.
    b = load().addr(123)
    with pytest.raises(strider.StriderError, match="expected a value pattern"):
        b.into_pat()


def test_store_data_with_raw_int_rejected_at_finalise():
    b = store().data(123)
    with pytest.raises(strider.StriderError, match="expected a value pattern"):
        b.into_pat()


def test_mem_in_rejects_value_operand():
    # A memory-input slot requires a memory-token producer.  A bare load()
    # produces a value, so accepting it would build a pattern that can never
    # match a real memory chain; the builder must reject it rather than
    # silently yield a dead pattern.
    with pytest.raises(strider.StriderError):
        store().addr(int_const(0x100)).mem_in(load().addr(int_const(0x200))).into_pat()
    with pytest.raises(strider.StriderError):
        load().addr(int_const(0x100)).mem_in(var(Capture())).into_pat()
