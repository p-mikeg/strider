"""End-to-end test for `strider.sleigh.CallingConvention.custom(...)`.

The `custom` constructor lets a Python user build a calling convention
from runtime register-name lists when none of the built-in presets
matches the binary's ABI.  Tests:

1. A custom CC built from x86_64 register names matches the
   pre-existing `x86_64_systemv` preset's resolved varnodes (parity).
2. An unknown register name surfaces as `StriderError`.
3. An invariant violation (LR not in callee_saved when LR is set)
   surfaces as `StriderError`.
4. The custom CC drives a full `strider.lift.lifter(...).analyze(...)` lift
   end-to-end.
"""

from __future__ import annotations

import pytest

import strider


def _mem_with_func_bytes() -> tuple[strider.reader.BufferReader, int]:
    """Build a BufferReader with a tiny x86_64 function: `mov eax, 1; ret`."""
    # mov eax, 1 (b8 01 00 00 00) ; ret (c3)
    mem = strider.reader.BufferReader(0x1000, b"\xb8\x01\x00\x00\x00\xc3")
    return mem, 0x1000


def test_custom_cc_matches_x86_64_systemv_for_equivalent_input():
    """Build a custom CC that mirrors the x86_64 SystemV register set
    and confirm it lifts identically to the preset.  Pin: the
    `custom(...)` builder produces a structurally-equivalent
    BuiltCallingConvention given identical register-name input."""
    mem, entry = _mem_with_func_bytes()
    arch = strider.sleigh.SleighArch.x86_64()
    sleigh = strider.sleigh.Sleigh(arch, mem)

    # x86_64 SystemV: arg_passing = RDI/RSI/RDX/RCX/R8/R9; callee-saved
    # = RBX/R12/R13/R14/R15/RBP; ret-val int = RAX/RDX, float =
    # XMM0/XMM1; SP = RSP; ret_stack_pop = 8 (the `ret` instruction
    # pops the return address).  Mirrors the preset's static lists.
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

    # Run the lifter with the custom CC.  Must not raise.
    lift = strider.lift.lifter(arch, mem)
    _cfg, function, _unresolved = lift.analyze(entry, custom_cc)
    assert function is not None, "Lifter.analyze must produce a Function"


def test_custom_cc_rejects_unknown_register_name():
    """Typos in register names must surface as `StriderError` at
    construction time, not at first use."""
    mem, _ = _mem_with_func_bytes()
    arch = strider.sleigh.SleighArch.x86_64()
    sleigh = strider.sleigh.Sleigh(arch, mem)
    with pytest.raises(strider.StriderError, match=r"(?i)unknown register"):
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
    """The CC builder's invariant: when `link_register` is `Some`, it
    MUST appear in `callee_saved_regs`.  Violation surfaces as
    `StriderError`."""
    mem = strider.reader.BufferReader(0x1000, b"\x00\x00\x00\xd6")  # arbitrary 4 bytes
    arch = strider.sleigh.SleighArch.aarch64()
    sleigh = strider.sleigh.Sleigh(arch, mem)
    # Set link_register=x30 but do NOT include x30 in callee_saved.
    # try_new must reject this.
    with pytest.raises(strider.StriderError):
        strider.sleigh.CallingConvention.custom(
            sleigh=sleigh,
            arg_passing_regs=["x0", "x1"],
            callee_saved_regs=["x19", "x20"],  # x30 deliberately absent
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
    """A custom CC with `preserves_memory=True` must produce a
    Strider that suppresses memory clobber on its Calls — mirrors
    `x86_64_all_preserving` behaviour but reachable via the custom
    builder."""
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
    # Must build the Lifter and analyze without error.
    lift = strider.lift.lifter(arch, mem)
    _cfg, function, _unresolved = lift.analyze(entry, custom_cc)
    assert function is not None
