"""`field`, `code_ptr` and `PhiPat` / `MemPhiPat.input_from`."""

from __future__ import annotations

import pytest

import strider
import strider.pattern as p
import strider.pattern.constraints as cons

from .conftest import fixture_path


def _analyze(arch: str, case: str, fn: str):
    prog = strider.lift.load_elf(str(fixture_path(arch, case)))
    return prog.analyze(fn).function


# `int NOINLINE struct_field_load(const struct point *p) { return p->x + p->y; }`:
# `p->x` sits at offset 0, which lifts as a bare `Load(p)` with no `Add`.
@pytest.mark.parametrize("arch", ["x64", "arm", "mips32le"])
def test_field_reads_offset_zero_as_the_bare_base(arch):
    fn = _analyze(arch, "memory", "struct_field_load")
    f = p.field(p.function_arg(0))
    offsets = sorted(f.offset(m) for m in fn.find_all(f.load()))
    assert offsets == [0, 4]


def test_field_known_offset_matches_only_that_offset():
    fn = _analyze("x64", "memory", "struct_field_load")
    assert len(fn.find_all(p.field(p.function_arg(0), offset=4).load())) == 1
    at_zero = p.field(p.function_arg(0), offset=0)
    assert [at_zero.offset(m) for m in fn.find_all(at_zero.load())] == [0]
    assert at_zero.capture is None


def test_field_named_capture_is_shared_by_name():
    fn = _analyze("x64", "memory", "struct_field_load")
    f = p.field(p.function_arg(0), offset="off")
    assert f.capture == "off"
    assert sorted(f.offset(m) for m in fn.find_all(f.load())) == [0, 4]


# `void NOINLINE struct_field_store(struct point *p, int x, int y)`.
def test_field_store_reads_the_stored_offset():
    fn = _analyze("x64", "memory", "struct_field_store")
    f = p.field(p.function_arg(0))
    hits = fn.find_all(f.store(p.function_arg(2)), ignore_casts=True)
    assert [f.offset(m) for m in hits] == [4]


# `int NOINLINE apply_indirect(fnptr f, int x) { return f(x); }`: the ARM and
# MIPS call target is `f & -2`, the x64 one is `f` itself.
@pytest.mark.parametrize("arch", ["x64", "arm", "arm_thumb", "arm_be8", "mips32le"])
def test_code_ptr_matches_a_masked_and_a_bare_target(arch):
    fn = _analyze(arch, "calls", "apply_indirect")
    assert len(fn.find_all(p.call().target(p.code_ptr(p.function_arg(0))))) == 1


def test_code_ptr_does_not_accept_another_mask():
    # and rdi,-4; call rdi; ret
    fn = _analyze_bytes(bytes([0x48, 0x83, 0xE7, 0xFC, 0xFF, 0xD7, 0xC3]))
    masked = p.call().target(p.int_and(p.function_arg(0), p.int_const(-4)))
    assert len(fn.find_all(masked)) == 1
    assert len(fn.find_all(p.call().target(p.code_ptr(p.function_arg(0))))) == 0


def _analyze_bytes(code: bytes):
    mem = strider.reader.BufferReader(0x1000, code)
    return strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem).analyze(
        0x1000, strider.sleigh.CallingConvention.x86_64_systemv()
    ).function


def _const_diamond():
    """`if (edi == 0) eax = 2 else eax = 1`; the arms merge distinct constants."""
    code = bytes([0x85, 0xFF, 0x74, 0x07, 0xB8, 0x01, 0, 0, 0,
                  0xEB, 0x05, 0xB8, 0x02, 0, 0, 0, 0xC3])
    return _analyze_bytes(code)


@pytest.mark.parametrize("edge", ["true", "false"])
def test_input_from_ties_each_edge_to_its_own_value(edge):
    fn = _const_diamond()
    t, f = p.Capture(), p.Capture()
    guard = p.if_else().capture_true(t).capture_false(f)
    e = t if edge == "true" else f

    def hits(k):
        phi = p.phi().input_from(e, p.int_const(k))
        return len(fn.find_all([guard, phi], constraints=phi.constraints()))

    assert hits(1) + hits(2) == 1


def test_input_from_agrees_with_the_hand_built_constraint():
    fn = _const_diamond()
    t, f, ph, v = p.Capture(), p.Capture(), p.Capture(), p.Capture()
    guard = p.if_else().capture_true(t).capture_false(f)
    for edge in (t, f):
        for k in (1, 2):
            manual = p.phi().any_input(p.int_const(k).capture(v)).capture(ph)
            want = len(fn.find_all(
                [guard, manual], constraints=[cons.phi_input_from_edge(ph, edge, v)]
            ))
            phi = p.phi().input_from(edge, p.int_const(k))
            assert len(fn.find_all([guard, phi], constraints=phi.constraints())) == want


def test_input_from_follows_a_later_capture():
    fn = _const_diamond()
    t, ph = p.Capture(), p.Capture()
    guard = p.if_else().capture_true(t)
    phi = p.phi().input_from(t, p.int_const([1, 2])).capture(ph)
    hits = fn.find_all([guard, phi], constraints=phi.constraints())
    assert len(hits) == 1 and hits[0].has(ph)


def test_constraints_is_empty_without_input_from():
    assert p.phi().constraints() == []
    assert p.mem_phi().constraints() == []


def test_input_from_on_a_capture_adds_no_input():
    """A `Capture` names a value another pattern binds, as a store's memory
    token feeding a `MemPhi`: only the constraint is added."""
    t, v = p.Capture(), p.Capture()
    phi = p.mem_phi().input_from(t, v)
    assert len(phi.constraints()) == 1
