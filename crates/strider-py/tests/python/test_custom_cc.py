from __future__ import annotations

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

    # Mirrors the preset's static lists; ret_stack_pop = 8 because `ret`
    # pops the return address.
    custom_cc = strider.sleigh.CallingConvention.custom(
        sleigh=arch,
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
    arch = strider.sleigh.SleighArch.x86_64()
    with pytest.raises(
        strider.StriderError, match=r"arg_passing_regs: unknown sleigh register name"
    ):
        strider.sleigh.CallingConvention.custom(
            sleigh=arch,
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


def test_a_link_register_outside_callee_saved_is_a_callee_convention_only():
    """A `link_register` missing from `callee_saved_regs` builds, for a
    per-address callee that clobbers it, and is refused as the function's own
    convention, whose returns go through it."""
    arch = strider.sleigh.SleighArch.aarch64()
    clobbers_lr = strider.sleigh.CallingConvention.custom(
        sleigh=arch,
        arg_passing_regs=["x0", "x1"],
        callee_saved_regs=["x19", "x20"],
        ret_val_regs=["x0"],
        ret_val_regs_float=["q0"],
        stack_pointer="sp",
        stack_arg_base=None,
        stack_arg_increment=8,
        ret_stack_pop=0,
        link_register="x30",
        preserves_memory=False,
    )
    # ret
    lift = strider.lift.lifter(arch, strider.reader.BufferReader(0x1000, b"\xc0\x03\x5f\xd6"))
    with pytest.raises(strider.StriderError, match="callee_saved_regs"):
        lift.analyze(0x1000, clobbers_lr)


def test_the_ppc32_got_thunk_clobbers_only_lr():
    """`bl _GLOBAL_OFFSET_TABLE_-4; mflr` (`-fpic -mbss-plt`): the word before
    the GOT is `blrl`, which leaves LR at the GOT. Its convention keeps every
    GPR and memory but not LR, so `mflr` after the call is not the `bl`'s
    return address and the return through the saved LR still verifies."""
    arch = strider.sleigh.SleighArch.ppc32be()
    got_thunk = strider.sleigh.CallingConvention.custom(
        arch,
        arg_passing_regs=[],
        callee_saved_regs=[f"r{i}" for i in range(32) if i != 1],
        ret_val_regs=[],
        ret_val_regs_float=[],
        stack_pointer="r1",
        stack_arg_base=None,
        stack_arg_increment=4,
        ret_stack_pop=0,
        link_register="LR",
        preserves_memory=True,
    )
    # mflr r0; bl 0x1104; mflr r3; mtlr r0; blr
    code = bytes.fromhex("7c0802a6480001017c6802a67c0803a64e800020")
    lift = strider.lift.lifter(arch, strider.reader.BufferReader(0x1000, code))
    cc = strider.sleigh.CallingConvention.powerpc_sysv32()

    def consts(result):
        return {n.uint() for n in result.function.data_walk()} - {None}

    preset = lift.analyze(0x1000, cc)
    assert 0x1008 in consts(preset), "under the preset LR survives the call"
    thunk = strider.lift.LifterOptions(per_address_ccs={0x1104: got_thunk})
    result = lift.analyze(0x1000, cc, thunk)
    assert result.unresolved == []
    assert 0x1008 not in consts(result)


def test_custom_cc_preserves_memory_chain():
    """`preserves_memory=True` must suppress memory clobber on Calls,
    same as `CallingConvention.<preset>().preserves_all()`."""
    mem, entry = _mem_with_func_bytes()
    arch = strider.sleigh.SleighArch.x86_64()
    custom_cc = strider.sleigh.CallingConvention.custom(
        sleigh=arch,
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
    arch = strider.sleigh.SleighArch.x86()

    def build(ret_pop):
        return strider.sleigh.CallingConvention.custom(
            sleigh=arch, arg_passing_regs=[], callee_saved_regs=[],
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
    arch = strider.sleigh.SleighArch.x86_64()
    with pytest.raises(strider.StriderError, match=match):
        strider.sleigh.CallingConvention.custom(
            sleigh=arch,
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


def test_a_preset_of_another_arch_is_named_as_python_spells_it():
    """The arch by its preset name and the stack pointer as `Vn.__repr__`
    spells it, not as Rust `Debug`."""
    arch = strider.sleigh.SleighArch.x86_64()
    lift = strider.lift.lifter(arch, strider.reader.BufferReader(0x1000, b"\xc3"))
    with pytest.raises(strider.StriderError) as excinfo:
        lift.analyze(0x1000, strider.sleigh.CallingConvention.x86_cdecl())
    msg = str(excinfo.value)
    assert "x86_64: " in msg and "stack pointer %[0x20]:4 is 4 bytes" in msg, msg
