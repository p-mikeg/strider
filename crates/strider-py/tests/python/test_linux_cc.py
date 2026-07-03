"""Existence + smoke test for the one Linux calling-convention preset
that diverges from a userland ABI: `x86_linux_kernel` (regparm-3).

Mirror of `crates/strider-target/tests/linux_cc_presets.rs` for the
Python side.  Every other arch's kernel CC is byte-identical to its
userland preset (callers use that directly), and syscall ABIs are
`CallOther`, not calling conventions — so neither has a binding here.

The test:

  1. Calls `strider.CallingConvention.x86_linux_kernel()`.
  2. Asserts the returned object has `name() == "x86_linux_kernel"`.
  3. Builds a `strider.lifter(arch, mem)` against an x86 fixture ELF and
     calls `analyze(entry, cc)` — the smoke check that the regparm-3
     arg registers (EAX, EDX, ECX) all resolve through the Python
     binding (a typo would fail CC resolution inside `analyze`).
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
    # through Sleigh — `cc` is resolved against the register table
    # inside `analyze` (the handle itself stores no default cc), so the
    # smoke check drives an actual `analyze` call rather than just
    # construction.
    elf = fixture_path("x86", "indirect_branch")
    loaded = strider.load_elf(str(elf))
    mem = loaded.reader()
    addr = loaded.symbol("indirect_branch_resolved")
    s = strider.lifter(strider.SleighArch.x86(), mem)
    _cfg, function, _unresolved = s.analyze(
        addr,
        strider.CallingConvention.x86_linux_kernel(),
        opts=strider.LifterOptions(cfg=strider.CfgOptions(allow_code_before_start_addr=True)),
    )
    assert function is not None
