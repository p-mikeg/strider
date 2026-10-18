from __future__ import annotations

from typing import Any

import pytest

import strider


def _mem_with_func_bytes() -> tuple[strider.reader.BufferReader, int]:
    # mov eax, 1 (b8 01 00 00 00) ; ret (c3)
    mem = strider.reader.BufferReader(0x1000, b"\xb8\x01\x00\x00\x00\xc3")
    return mem, 0x1000


def test_custom_cc_matches_x86_64_systemv_for_equivalent_input():
    """Identical register-name input must produce a CC equivalent to the
    `x86_64_systemv` preset."""
    mem, entry = _mem_with_func_bytes()
    arch = strider.sleigh.SleighArch.x86_64()
    sleigh = strider.sleigh.Sleigh(arch, mem)

    # Mirrors the preset's static lists; ret_stack_pop = 8 because `ret`
    # pops the return address.
    custom_cc = strider.sleigh.CallingConvention.custom(
        sleigh=sleigh,
        arg_passing_regs=["RDI", "RSI", "RDX", "RCX", "R8", "R9"],
        callee_saved_regs=["RBX", "R12", "R13", "R14", "R15", "RBP"],
        ret_val_regs=["RAX", "RDX"],
        ret_val_regs_float=["XMM0", "XMM1"],
        stack_pointer="RSP",
        stack_arg_base=None,
        stack_arg_increment=8,
        ret_stack_pop=8,
        link_register=None,
        preserves_memory=False,
    )
    assert custom_cc.name() == "custom"

    lift = strider.lift.lifter(arch, mem)
    _cfg, function, _unresolved = lift.analyze(entry, custom_cc)
    assert function is not None, "Lifter.analyze must produce a Function"


def test_custom_cc_rejects_unknown_register_name():
    """Typos must fail at construction time, not at first use."""
    mem, _ = _mem_with_func_bytes()
    arch = strider.sleigh.SleighArch.x86_64()
    sleigh = strider.sleigh.Sleigh(arch, mem)
    with pytest.raises(strider.StriderError, match=r"unknown sleigh register name"):
        strider.sleigh.CallingConvention.custom(
            sleigh=sleigh,
            arg_passing_regs=["DEFINITELY_NOT_A_REG"],
            callee_saved_regs=[],
            ret_val_regs=[],
            ret_val_regs_float=[],
            stack_pointer="RSP",
            stack_arg_base=None,
            stack_arg_increment=8,
            ret_stack_pop=8,
            link_register=None,
            preserves_memory=False,
        )


def test_custom_cc_rejects_invariant_violation_lr_not_in_callee_saved():
    """A set `link_register` MUST also appear in `callee_saved_regs`."""
    mem = strider.reader.BufferReader(0x1000, b"\x00\x00\x00\xd6")  # arbitrary 4 bytes
    arch = strider.sleigh.SleighArch.aarch64()
    sleigh = strider.sleigh.Sleigh(arch, mem)
    with pytest.raises(strider.StriderError):
        strider.sleigh.CallingConvention.custom(
            sleigh=sleigh,
            arg_passing_regs=["x0", "x1"],
            callee_saved_regs=["x19", "x20"],  # the link register x30 belongs here
            ret_val_regs=["x0"],
            ret_val_regs_float=["q0"],
            stack_pointer="sp",
            stack_arg_base=None,
            stack_arg_increment=8,
            ret_stack_pop=0,
            link_register="x30",
            preserves_memory=False,
        )


def test_custom_cc_preserves_memory_chain():
    """`preserves_memory=True` must suppress memory clobber on Calls,
    same as `CallingConvention.<preset>().preserves_all()`."""
    mem, entry = _mem_with_func_bytes()
    arch = strider.sleigh.SleighArch.x86_64()
    sleigh = strider.sleigh.Sleigh(arch, mem)
    custom_cc = strider.sleigh.CallingConvention.custom(
        sleigh=sleigh,
        arg_passing_regs=[],
        # Every userland callee-clobber is callee-saved here.
        callee_saved_regs=["RAX", "RCX", "RDX", "RDI", "RSI", "R8", "R9", "R10", "R11",
                            "RBX", "R12", "R13", "R14", "R15", "RBP",
                            "XMM0", "XMM1"],
        ret_val_regs=[],
        ret_val_regs_float=[],
        stack_pointer="RSP",
        stack_arg_base=None,
        stack_arg_increment=8,
        ret_stack_pop=8,
        link_register=None,
        preserves_memory=True,
    )
    lift = strider.lift.lifter(arch, mem)
    _cfg, function, _unresolved = lift.analyze(entry, custom_cc)
    assert function is not None


def test_custom_cc_rejects_stack_arch_with_zero_ret_stack_pop():
    """x86 is stack-pushing (no link register): `call` pushes a 4-byte return
    address, so ret_stack_pop=0 forgets it and must be rejected. This is the
    footgun that silently drifts SP after every call."""
    mem, _ = _mem_with_func_bytes()
    sleigh = strider.sleigh.Sleigh(strider.sleigh.SleighArch.x86(), mem)

    def build(ret_pop):
        return strider.sleigh.CallingConvention.custom(
            sleigh=sleigh, arg_passing_regs=[], callee_saved_regs=[],
            ret_val_regs=[], ret_val_regs_float=[], stack_pointer="ESP",
            stack_arg_base=None, stack_arg_increment=4, ret_stack_pop=ret_pop,
            link_register=None, preserves_memory=True)

    with pytest.raises(Exception, match="ret_stack_pop"):
        build(0)
    # The ABI-correct x86 value is accepted.
    assert build(4).name() == "custom"


@pytest.mark.parametrize(
    ("stack_pointer", "stack_arg_base", "stack_arg_increment", "match"),
    [
        ("ESP", None, 8, "stack pointer"),
        ("RSP", 8, 4, "stack-arg increment"),
    ],
    ids=["narrow_sp", "narrow_slot"],
)
def test_custom_cc_rejects_geometry_of_another_width_at_construction(
    stack_pointer, stack_arg_base, stack_arg_increment, match
):
    """x86-64 addresses are 8 bytes, so a 4-byte `ESP`, or 4-byte stack-arg
    slots, describe another arch: raised here rather than at `analyze`."""
    mem, _ = _mem_with_func_bytes()
    sleigh = strider.sleigh.Sleigh(strider.sleigh.SleighArch.x86_64(), mem)
    with pytest.raises(strider.StriderError, match=match):
        strider.sleigh.CallingConvention.custom(
            sleigh=sleigh,
            arg_passing_regs=[],
            callee_saved_regs=[],
            ret_val_regs=[],
            ret_val_regs_float=[],
            stack_pointer=stack_pointer,
            stack_arg_base=stack_arg_base,
            stack_arg_increment=stack_arg_increment,
            ret_stack_pop=8,
            link_register=None,
            preserves_memory=False,
        )


def _systemv_names() -> dict[str, Any]:
    return dict(
        arg_passing_regs=["RDI", "RSI", "RDX", "RCX", "R8", "R9"],
        callee_saved_regs=["RBX", "R12", "R13", "R14", "R15", "RBP"],
        ret_val_regs=["RAX", "RDX"],
        ret_val_regs_float=["XMM0", "XMM1"],
        stack_pointer="RSP",
        stack_arg_base=8,
        stack_arg_increment=8,
        ret_stack_pop=8,
        link_register=None,
        preserves_memory=False,
    )


def test_custom_cc_takes_an_arch_and_resolves_as_a_sleigh_does():
    mem, entry = _mem_with_func_bytes()
    arch = strider.sleigh.SleighArch.x86_64()
    texts = [
        strider.lift.lifter(arch, mem)
        .analyze(entry, strider.sleigh.CallingConvention.custom(sleigh=source, **_systemv_names()))
        .function.to_text()
        for source in (arch, strider.sleigh.Sleigh(arch, mem))
    ]
    # The `default_cc` line carries every resolved varnode.
    assert "default_cc" in texts[0]
    assert texts[0] == texts[1]


def test_an_unknown_register_raises_the_same_through_an_arch():
    mem, _ = _mem_with_func_bytes()
    arch = strider.sleigh.SleighArch.x86_64()
    errors = []
    for source in (arch, strider.sleigh.Sleigh(arch, mem)):
        with pytest.raises(strider.StriderError) as e:
            strider.sleigh.CallingConvention.custom(
                sleigh=source, **{**_systemv_names(), "arg_passing_regs": ["NOPE"]}
            )
        errors.append(str(e.value))
    assert errors[0] == errors[1]
    assert "NOPE" in errors[0]
