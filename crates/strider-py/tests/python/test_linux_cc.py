"""The one Linux calling-convention preset that diverges from a userland
ABI: `x86_linux_kernel` (regparm-3).

Mirror of `crates/strider-target/tests/linux_cc_presets.rs`.  Every other
arch's kernel CC is byte-identical to its userland preset (callers use
that directly), and syscall ABIs are `CallOther` rather than calling
conventions, so neither gets a Python binding.
"""

from __future__ import annotations

import strider
from strider.pattern import call

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
        opts=strider.lift.LifterOptions(
            cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True),
        ),
    )
    assert function is not None


def _stack_args_call(per_address_ccs):
    """`push $0x5000; call 0x2000; add $4, %esp; ret` at 0x1000: one argument
    on the stack, as a variadic or `asmlinkage` callee takes it."""
    code = bytes(
        [
            0x68, 0x00, 0x50, 0x00, 0x00,  # push $0x5000
            0xE8, 0xF6, 0x0F, 0x00, 0x00,  # call 0x2000
            0x83, 0xC4, 0x04,              # add $4, %esp
            0xC3,                          # ret
        ]
    )
    cc = strider.sleigh.CallingConvention
    lifter = strider.lift.lifter(
        strider.sleigh.SleighArch.x86(), strider.reader.BufferReader(0x1000, code)
    )
    result = lifter.analyze(
        0x1000,
        cc.x86_linux_kernel(),
        opts=strider.lift.LifterOptions(per_address_ccs=per_address_ccs),
    )
    assert not result.cfg.incomplete
    return [
        len(result.function.find_all(call().target(0x2000).arg(i, 0x5000)))
        for i in range(4)
    ]


def test_x86_linux_kernel_stack_only_callee_needs_cdecl():
    # The regparm registers fill arguments 0-2, so the stack argument is 3.
    assert _stack_args_call(None) == [0, 0, 0, 1]
    cdecl = strider.sleigh.CallingConvention.x86_cdecl()
    assert _stack_args_call({0x2000: cdecl}) == [1, 0, 0, 0]
