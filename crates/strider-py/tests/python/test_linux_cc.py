"""Existence + smoke test for the one Linux calling-convention preset
that diverges from a userland ABI: `x86_linux_kernel` (regparm-3).

Mirror of `crates/strider-target/tests/linux_cc_presets.rs` for the
Python side.  Every other arch's kernel CC is byte-identical to its
userland preset (callers use that directly), and syscall ABIs are
`CallOther`, not calling conventions — so neither has a binding here.

The test:

  1. Calls `strider.CallingConvention.x86_linux_kernel()`.
  2. Asserts the returned object has `name() == "x86_linux_kernel"`.
  3. Builds a `strider.Lifter(arch, mem, cc)` against an x86 fixture ELF
     — the smoke check that the regparm-3 arg registers (EAX, EDX, ECX)
     all resolve through the Python binding (a typo would fail at
     `Strider.__new__`).
"""

from __future__ import annotations

import strider

from .conftest import fixture_path


def test_x86_linux_kernel_exists_and_name_round_trips():
    cc = strider.CallingConvention.x86_linux_kernel()
    assert isinstance(cc, strider.CallingConvention)
    assert cc.name() == "x86_linux_kernel"


def test_x86_linux_kernel_constructs_strider(x86_indirect_branch_elf):
    # x86_linux_kernel is the only kernel preset that DIFFERS from its
    # userland counterpart (regparm(3) vs cdecl).  Build a Lifter and
    # confirm the arg-passing register names (EAX, EDX, ECX) all resolve
    # through Sleigh.
    elf = fixture_path("x86", "indirect_branch")
    mem = strider.load_elf(str(elf)).reader()
    s = strider.Lifter(
        strider.SleighArch.x86(),
        mem,
        strider.CallingConvention.x86_linux_kernel(),
    )
    assert s is not None
