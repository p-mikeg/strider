"""Where a value comes from, as a match constraint: the varnode a phi is
tagged with, and the register or stack slot an incoming argument arrives in.

Each is checked against IR where the distinction is real, with a second
carrier the constraint has to reject, and against the standalone helper that
spells the same constraint (`phi_for`, `function_arg_reg`,
`function_arg_stack`).
"""

from __future__ import annotations

import strider
from strider.pattern import (
    Capture,
    any_function_arg,
    function_arg,
    function_arg_reg,
    function_arg_stack,
    phi,
    phi_for,
)

from .conftest import built_function, fixture_path

RAM = strider.sleigh.VnSpace.RAM


_ARCH = {
    "x86": strider.sleigh.SleighArch.x86,
    "x64": strider.sleigh.SleighArch.x86_64,
}


def _reg(arch, case, name):
    mem = strider.lift.load_elf(str(fixture_path(arch, case))).reader()
    vn = strider.sleigh.Sleigh(_ARCH[arch](), mem).reg(name)
    assert vn is not None, f"{arch} has no register {name}"
    return vn


def test_a_phi_is_selected_by_the_varnode_it_is_tagged_with():
    g = built_function("x64", "memory", "array_sum")
    assert g.find_all(phi())
    assert len(g.find_all(phi().for_vn(_reg("x64", "memory", "RAX")))) == 1
    assert not g.find_all(phi().for_vn(_reg("x64", "memory", "RCX")))


def test_a_sub_register_selects_the_phi_tagged_with_its_container():
    g = built_function("x64", "memory", "array_sum")
    whole = g.find_all(phi().for_vn(_reg("x64", "memory", "RAX")))
    assert [m.root for m in whole] == [
        m.root for m in g.find_all(phi().for_vn(_reg("x64", "memory", "AL")))
    ]


def test_the_chained_phi_varnode_constraint_matches_the_standalone_helper():
    g = built_function("x64", "memory", "array_sum")
    rax = _reg("x64", "memory", "RAX")
    assert [m.root for m in g.find_all(phi().for_vn(rax))] == [
        m.root for m in g.find_all(phi_for(rax))
    ]


def test_a_register_passed_argument_is_selected_by_its_register():
    """x86-64 SysV seats the first six integer arguments in registers, so
    `eight_int_args` has one per register and nothing on the stack."""
    g = built_function("x64", "abi", "eight_int_args", optimize=False)
    rdi = _reg("x64", "abi", "RDI")
    assert len(g.find_all(any_function_arg().source_register(rdi))) == 1
    assert not g.find_all(
        any_function_arg().source_register(_reg("x64", "abi", "RAX"))
    )
    assert [m.root for m in g.find_all(any_function_arg().source_register(rdi))] == [
        m.root for m in g.find_all(function_arg_reg(rdi))
    ]


def test_the_register_an_argument_arrives_in_pins_its_index():
    g = built_function("x64", "abi", "eight_int_args", optimize=False)
    rdi = _reg("x64", "abi", "RDI")
    assert len(g.find_all(function_arg(0).source_register(rdi))) == 1
    assert not g.find_all(function_arg(1).source_register(rdi))


def test_a_stack_passed_argument_is_selected_by_its_slot():
    """cdecl puts every argument on the stack, so the same source function
    lifted for x86 is the mirror of the x86-64 case above."""
    g = built_function("x86", "abi", "eight_int_args")
    assert len(g.find_all(any_function_arg().source_stack(RAM, 4))) == 1
    assert not g.find_all(any_function_arg().source_stack(RAM, 0))
    assert [m.root for m in g.find_all(any_function_arg().source_stack(RAM, 4))] == [
        m.root for m in g.find_all(function_arg_stack(RAM, 4))
    ]


def test_a_stack_slot_is_read_in_the_space_it_names():
    g = built_function("x86", "abi", "eight_int_args")
    assert g.find_all(any_function_arg().source_stack(RAM, 4))
    assert not g.find_all(
        any_function_arg().source_stack(strider.sleigh.VnSpace.REGISTER, 4)
    )


def test_the_two_argument_sources_do_not_answer_for_each_other():
    stack_args = built_function("x86", "abi", "eight_int_args")
    register_args = built_function("x64", "abi", "eight_int_args", optimize=False)
    c = Capture()
    assert len(stack_args.find_all(any_function_arg().capture(c))) == 8
    assert not stack_args.find_all(
        any_function_arg().source_register(_reg("x86", "abi", "EAX"))
    )
    assert register_args.find_all(any_function_arg().capture(c))
    assert not [
        off
        for off in range(0, 64, 4)
        if register_args.find_all(any_function_arg().source_stack(RAM, off))
    ]
