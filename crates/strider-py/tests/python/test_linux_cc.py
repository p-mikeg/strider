"""The one Linux calling-convention preset that diverges from a userland
ABI: `x86_linux_kernel` (regparm-3).

Mirror of `crates/strider-target/tests/linux_cc_presets.rs`.  Every other
arch's kernel CC is byte-identical to its userland preset (callers use
that directly), and syscall ABIs are `CallOther` rather than calling
conventions, so neither gets a Python binding.
"""

from __future__ import annotations

import strider

from .conftest import fixture_path


def test_x86_linux_kernel_exists_and_name_round_trips():
    cc = strider.sleigh.CallingConvention.x86_linux_kernel()
    assert isinstance(cc, strider.sleigh.CallingConvention)
    assert cc.name() == "x86_linux_kernel"


def test_x86_linux_kernel_constructs_strider(x86_indirect_branch_elf):
    # The cc is resolved against Sleigh's register table inside `analyze`,
    # not at construction, so only a real `analyze` call proves the
    # regparm-3 registers (EAX, EDX, ECX) resolve; a typo would fail there.
    elf = fixture_path("x86", "indirect_branch")
    loaded = strider.lift.load_elf(str(elf))
    mem = loaded.reader()
    addr = loaded.symbol("indirect_branch_resolved").address
    s = strider.lift.lifter(strider.sleigh.SleighArch.x86(), mem)
    _cfg, function, _unresolved = s.analyze(
        addr,
        strider.sleigh.CallingConvention.x86_linux_kernel(),
        opts=strider.lift.LifterOptions(cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True)),
    )
    assert function is not None
