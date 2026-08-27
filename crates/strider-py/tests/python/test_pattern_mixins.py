"""The shared builder vocabulary: the mixin protocols, the uniform `ctrl`
slot, and the generic `input` / `output` escape hatches beneath the named
accessors."""

from __future__ import annotations

import pytest

import strider
from strider import pattern as p
from .conftest import lift_bytes as _fn


def _load_store_fn():
    # mov rax, [rdi] ; mov [rsi], rax ; ret
    return _fn(bytes([0x48, 0x8B, 0x07, 0x48, 0x89, 0x06, 0xC3]))


def _diamond_fn():
    # if (edi != 0) { eax = 1 } else { eax = 2 }; return eax
    return _fn(bytes([
        0x85, 0xFF,
        0x75, 0x07,
        0xB8, 0x01, 0x00, 0x00, 0x00,
        0xEB, 0x05,
        0xB8, 0x02, 0x00, 0x00, 0x00,
        0xC3,
    ]))


def _call_fn():
    # call 0x1020 ; ret
    return _fn(bytes([0xE8, 0x1B, 0x00, 0x00, 0x00, 0xC3]))


def _indirect_branch_fn():
    # jmp rax, unresolvable
    mem = strider.reader.BufferReader(0x2000, bytes([0xFF, 0xE0]))
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)
    _cfg, fn, unresolved = lift.analyze(
        0x2000,
        strider.sleigh.CallingConvention.x86_64_systemv(),
        opts=strider.lift.LifterOptions(
            cfg=strider.cfg.CfgOptions(function_max_size=2)
        ),
    )
    assert unresolved == [0x2000]
    return fn


# --- the mixins are runtime-checkable protocols --------------------------

@pytest.mark.parametrize("builder", [
    p.call(), p.call_other(), p.ret(), p.if_else(), p.load(), p.store(),
    p.phi(), p.mem_phi(), p.entry(), p.region(), p.indirect_branch(),
    p.unreachable(), p.switch(), p.any_function_arg(),
])
def test_every_node_builder_is_a_nodepat(builder):
    assert isinstance(builder, p.NodePat)


def test_a_non_builder_is_not_a_nodepat():
    assert not isinstance(42, p.NodePat)
    assert not isinstance(p.Capture(), p.NodePat)


def test_pat_is_not_a_node_builder():
    # `Pat` is the finished pattern, not a builder: no `into_pat`.
    assert not isinstance(p.int_add(1, 2), p.NodePat)


def test_input_protocol_membership():
    assert isinstance(p.load(), p.InputPat)
    assert isinstance(p.if_else(), p.InputPat)
    # `Entry` is `inputs: []`, so it declares no input slots.
    assert not isinstance(p.entry(), p.InputPat)


def test_ctrl_protocol_membership():
    for b in (p.call(), p.call_other(), p.ret(), p.if_else(),
              p.indirect_branch(), p.unreachable(), p.switch()):
        assert isinstance(b, p.CtrlPat)
    assert not isinstance(p.load(), p.CtrlPat)


def test_mem_protocol_membership():
    for b in (p.call(), p.call_other(), p.indirect_branch()):
        assert isinstance(b, p.MemPat)
    assert not isinstance(p.ret(), p.MemPat)


def test_output_protocol_membership():
    for b in (p.call(), p.load(), p.store(), p.entry(), p.if_else()):
        assert isinstance(b, p.OutputPat)
    # The three sinks are `outputs: []`.
    for b in (p.ret(), p.indirect_branch(), p.unreachable()):
        assert not isinstance(b, p.OutputPat)


# --- `preceded_by` is now `ctrl` -----------------------------------------

def test_ret_ctrl_matches_what_preceded_by_matched():
    fn = _load_store_fn()
    assert len(fn.find_all(p.ret().ctrl(p.anything()))) == 1


def test_preceded_by_is_gone():
    for b in (p.ret(), p.switch(), p.indirect_branch(), p.unreachable()):
        assert not hasattr(b, "preceded_by")


# --- `IfPat.ctrl` is new -------------------------------------------------

def test_if_ctrl_constrains_the_control_predecessor():
    fn = _diamond_fn()
    assert len(fn.find_all(p.if_else())) == 1
    assert len(fn.find_all(p.if_else().ctrl(p.anything()))) == 1
    # A value pattern can never bind a Control edge.
    assert fn.find_all(p.if_else().ctrl(p.int_const(1))) == []


def test_if_ctrl_binds_the_entry_edge():
    fn = _diamond_fn()
    c = p.Capture()
    hits = fn.find_all(p.if_else().ctrl(p.entry().capture(c)))
    assert len(hits) == 1


# --- the generic `input` escape hatch ------------------------------------

def test_load_input_addresses_raw_slots():
    fn = _load_store_fn()
    # `Load` is `[mem(0), addr(1)]`, so slot 1 is what `.addr()` names.
    named = fn.find_all(p.load().addr(p.anything()))
    raw = fn.find_all(p.load().input(1, p.anything()))
    assert len(raw) == len(named) == 1
    # Slot 0 is the memory edge: no value pattern binds it.
    assert fn.find_all(p.load().input(0, p.int_const(5))) == []


def test_store_input_addresses_raw_slots():
    fn = _load_store_fn()
    # `Store` is `[mem(0), addr(1), data(2)]`; the stored value is the load.
    assert len(fn.find_all(p.store().input(2, p.load()))) == 1
    assert fn.find_all(p.store().input(2, p.int_const(7))) == []


def test_any_input_reaches_every_builder():
    fn = _diamond_fn()
    assert len(fn.find_all(p.if_else().any_input(p.anything()))) >= 1


# --- chaining keeps the precise builder ----------------------------------

def test_capture_chain_returns_the_same_builder():
    fn = _call_fn()
    c = p.Capture()
    pat = p.call().capture(c).arg(0, p.anything())
    assert isinstance(pat, type(p.call()))
    assert isinstance(pat.into_pat(), p.Pat)
    fn.find_all(p.call().capture(c))


# --- the generic `output` escape hatch -----------------------------------

def test_load_output_slot_zero_is_the_loaded_value():
    fn = _load_store_fn()
    c = p.Capture()
    hits = fn.find_all(p.load().output(0).capture(c))
    assert len(hits) == 1
    assert hits[0].has(c)


def test_any_output_matches_some_output():
    fn = _load_store_fn()
    assert len(fn.find_all(p.load().any_output().of_width(64))) == 1
    assert fn.find_all(p.load().any_output().of_width(7)) == []


def test_output_reaches_every_builder_that_has_outputs():
    fn = _diamond_fn()
    c = p.Capture()
    for pat in (
        p.if_else().output(0).capture(c),
        p.if_else().any_output().capture(c),
        p.phi().output(0).capture(c),
        p.region().output(0).capture(c),
        p.entry().output(0).capture(c),
    ):
        assert len(fn.find_all(pat)) >= 1
    store_fn = _load_store_fn()
    # A `Store`'s one output is the new memory token.
    assert len(store_fn.find_all(p.store().output(0).capture(c))) == 1
    assert len(store_fn.find_all(p.store().any_output().capture(c))) == 1


def test_call_output_terminal_is_unchanged():
    fn = _call_fn()
    c = p.Capture()
    hits = fn.find_all(p.call().output(2).capture(c))
    assert len(hits) >= 1


# --- `target` takes a list, like `CallPat.target` ------------------------

def test_indirect_branch_target_accepts_a_list():
    fn = _indirect_branch_fn()
    single = fn.find_all(p.indirect_branch().target(p.anything()))
    assert len(single) == 1
    assert len(fn.find_all(p.indirect_branch().target([p.anything()]))) == 1
    assert len(
        fn.find_all(p.indirect_branch().target([p.int_const(5), p.anything()]))
    ) == 1


def test_indirect_branch_target_empty_list_matches_nothing():
    fn = _indirect_branch_fn()
    assert fn.find_all(p.indirect_branch().target([])) == []


def test_switch_address_accepts_a_list(x86_switch_elf):
    fn = _switch_fn(x86_switch_elf)
    assert len(fn.find_all(p.switch().selector(p.anything()))) >= 1
    assert len(fn.find_all(p.switch().selector([p.anything()]))) >= 1


def test_switch_address_empty_list_matches_nothing(x86_switch_elf):
    fn = _switch_fn(x86_switch_elf)
    assert fn.find_all(p.switch().selector([])) == []


def _switch_fn(elf_path):
    loaded = strider.lift.load_elf(str(elf_path))
    mem = loaded.reader()
    addr = loaded.symbol("dispatch_value").address
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86(), mem, rom=mem)
    _cfg, fn, _unresolved = lift.analyze(
        addr,
        strider.sleigh.CallingConvention.x86_cdecl(),
        opts=strider.lift.LifterOptions(
            cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True)
        ),
    )
    return fn


# --- `phi_input` (predecessor-indexed) vs `input` (raw) -------------------

def _mem_join_fn():
    #   test edi, edi
    #   jne  else
    #   mov  dword [0x3000], 1
    #   jmp  end
    # else:
    #   mov  dword [0x3000], 2
    # end:
    #   mov  eax, [0x3000]     -> MemPhi with two real store predecessors
    #   ret
    return _fn(bytes([
        0x85, 0xFF,
        0x75, 0x0D,
        0xC7, 0x04, 0x25, 0x00, 0x30, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
        0xEB, 0x0B,
        0xC7, 0x04, 0x25, 0x00, 0x30, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00,
        0x8B, 0x04, 0x25, 0x00, 0x30, 0x00, 0x00,
        0xC3,
    ]))


def _diamond_unoptimized_fn():
    """`_diamond_fn`'s bytes with an empty pipeline, so `RegionCollapse`
    leaves the merge `Region` and its two control predecessors standing."""
    mem = strider.reader.BufferReader(0x1000, bytes([
        0x85, 0xFF,
        0x75, 0x07,
        0xB8, 0x01, 0x00, 0x00, 0x00,
        0xEB, 0x05,
        0xB8, 0x02, 0x00, 0x00, 0x00,
        0xC3,
    ]))
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)
    _cfg, fn, _unresolved = lift.analyze(
        0x1000,
        strider.sleigh.CallingConvention.x86_64_systemv(),
        opts=strider.lift.LifterOptions(
            pipeline=strider.opt.OptimizerPipeline.empty()
        ),
    )
    return fn


def test_phi_input_zero_is_raw_input_one():
    """`Phi` is `[phi_token, value0, ...]`, so predecessor 0's value sits at
    raw slot 1 and the two spellings select the same edge."""
    fn = _diamond_fn()
    c = p.Capture()
    shifted = {m.uint(c) for m in fn.find_all(
        p.phi().phi_input(0, p.int_const().capture(c))
    )}
    raw = {m.uint(c) for m in fn.find_all(
        p.phi().input(1, p.int_const().capture(c))
    )}
    assert shifted
    assert shifted == raw


def test_phi_raw_input_zero_reaches_the_phi_token():
    """`input(0, _)` is the ownership edge `phi_token` names, not a value: a
    typed value sub never binds it, an untyped wildcard does."""
    fn = _diamond_fn()
    assert fn.find_all(p.phi().input(0, p.int_const(1))) == []
    c = p.Capture()
    assert len(fn.find_all(p.phi().input(0, p.var(c)))) >= 1


def test_mem_phi_phi_input_zero_is_raw_input_one():
    fn = _mem_join_fn()
    shifted = fn.find_all(p.mem_phi().phi_input(0, p.store()))
    raw = fn.find_all(p.mem_phi().input(1, p.anything()))
    assert len(shifted) >= 1
    assert len(raw) >= 1
    assert fn.find_all(p.mem_phi().input(0, p.int_const(1))) == []


# --- the Load/Store shared vocabulary ------------------------------------

def test_mem_access_protocol_membership():
    for b in (p.load(), p.store()):
        assert isinstance(b, p.MemAccessPat)
    for b in (p.call(), p.mem_phi(), p.entry()):
        assert not isinstance(b, p.MemAccessPat)


def test_mem_reaches_load_and_store():
    """`mem_in` was `mem` under a second name: both are the memory
    predecessor, `input_mem` at the kind's memory slot."""
    fn = _mem_join_fn()
    # The trailing load reads the join; the two stores precede it.
    assert len(fn.find_all(p.load().mem(p.mem_phi()))) == 1
    assert fn.find_all(p.store().mem(p.mem_phi())) == []
    assert len(fn.find_all(p.mem_phi().phi_input(0, p.store()))) >= 1
    for b in (p.load(), p.store()):
        assert isinstance(b, p.MemPat)
        assert not hasattr(b, "mem_in")


def test_mem_chain_keeps_the_concrete_builder():
    chained = p.load().mem(p.store()).addr(p.anything())
    assert isinstance(chained, type(p.load()))


# --- `ordered` is a mixin ------------------------------------------------

def test_ordered_protocol_membership():
    c = p.Capture()
    for b in (
        p.int_add(1, 2),
        p.int_binary("Add", 1, 2),
        p.float_binary("Add", p.anything(), p.anything()),
        p.bool_binary("And", p.anything(), p.anything()),
    ):
        assert isinstance(b, p.OrderedPat)
    assert not isinstance(p.load(), p.OrderedPat)
    assert not isinstance(c, p.OrderedPat)


# --- `RegionPat` inherits the raw `input` --------------------------------

def test_region_input_constrains_a_control_predecessor():
    fn = _diamond_unoptimized_fn()
    assert len(fn.find_all(p.region().input(0, p.entry()))) >= 1
    # A typed value sub can never bind a Control edge.
    assert fn.find_all(p.region().input(0, p.int_const(1))) == []
