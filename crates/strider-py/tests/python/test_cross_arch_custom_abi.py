"""A `custom(...)` convention or user-op ABI freezes varnodes resolved against
one arch's register table.

The same register name denotes a different varnode on another arch (x86-64
`EDI` is `%[0x38]:4`, x86 `EDI` is `%[0x1c]:4`), so consuming one on the wrong
`Lifter` used to lift a function whose arguments silently were not there.
"""

import pytest

import strider
from strider.sleigh import CallOtherAbi, CallingConvention, Sleigh, SleighArch

# mov eax, edi ; ret
MOV_EAX_EDI_RET = b"\x89\xf8\xc3"
# rdtsc ; ret
RDTSC_RET = b"\x0f\x31\xc3"

X86_64_ARGS = ["EDI", "ESI", "EDX", "ECX"]


def _mem(code=MOV_EAX_EDI_RET):
    return strider.reader.BufferReader(0x1000, code)


def _custom_cc(arch, code=MOV_EAX_EDI_RET):
    return CallingConvention.custom(
        sleigh=Sleigh(arch, _mem(code)),
        arg_passing_regs=X86_64_ARGS,
        callee_saved_regs=[],
        ret_val_regs=["EAX"],
        ret_val_regs_float=[],
        stack_pointer="RSP" if arch.name() == "x86_64" else "ESP",
        stack_arg_base=None,
        stack_arg_increment=8,
        ret_stack_pop=8 if arch.name() == "x86_64" else 4,
        link_register=None,
        preserves_memory=False,
    )


def test_custom_cc_from_the_same_arch_binds_the_argument():
    arch = SleighArch.x86_64()
    lift = strider.lift.lifter(arch, _mem())
    _cfg, fn, _unresolved = lift.analyze(0x1000, _custom_cc(arch))
    assert len(fn.find_all(strider.pattern.function_arg(0))) == 1


def test_custom_cc_from_another_arch_is_rejected():
    """Without the check this analysed fine and bound zero arguments."""
    lift = strider.lift.lifter(SleighArch.x86_64(), _mem())
    foreign = _custom_cc(SleighArch.x86())
    with pytest.raises(strider.StriderError, match=r'"x86".*"x86_64"'):
        lift.analyze(0x1000, foreign)


def test_a_preset_cc_is_not_arch_locked():
    """Presets resolve their names at consumption time, so they carry no
    source arch to mismatch."""
    lift = strider.lift.lifter(SleighArch.x86_64(), _mem())
    _cfg, fn, _unresolved = lift.analyze(0x1000, CallingConvention.x86_64_systemv())
    assert fn.node_count() > 0


def test_custom_per_address_cc_from_another_arch_is_rejected():
    lift = strider.lift.lifter(SleighArch.x86_64(), _mem())
    opts = strider.lift.LifterOptions(
        per_address_ccs={0x1000: _custom_cc(SleighArch.x86())}
    )
    with pytest.raises(strider.StriderError, match=r"per-address"):
        lift.analyze(0x1000, CallingConvention.x86_64_systemv(), opts)


def test_custom_call_other_abi_from_another_arch_is_rejected():
    lift = strider.lift.lifter(SleighArch.x86_64(), _mem(RDTSC_RET))
    foreign = CallOtherAbi.custom(
        sleigh=Sleigh(SleighArch.x86(), _mem(RDTSC_RET)),
        implicit_writes=["EAX", "EDX"],
    )
    opts = strider.lift.LifterOptions(
        cfg=strider.cfg.CfgOptions(call_other_abis={"rdtsc": foreign})
    )
    with pytest.raises(strider.StriderError, match=r'"rdtsc".*"x86".*"x86_64"'):
        lift.analyze(0x1000, CallingConvention.x86_64_systemv(), opts)


def test_custom_call_other_abi_from_the_same_arch_is_accepted():
    lift = strider.lift.lifter(SleighArch.x86_64(), _mem(RDTSC_RET))
    abi = CallOtherAbi.custom(
        sleigh=Sleigh(SleighArch.x86_64(), _mem(RDTSC_RET)),
        implicit_writes=["EAX", "EDX"],
    )
    opts = strider.lift.LifterOptions(
        cfg=strider.cfg.CfgOptions(call_other_abis={"rdtsc": abi})
    )
    _cfg, fn, _unresolved = lift.analyze(
        0x1000, CallingConvention.x86_64_systemv(), opts
    )
    assert fn.node_count() > 0


def test_a_preset_call_other_abi_is_not_arch_locked():
    lift = strider.lift.lifter(SleighArch.x86_64(), _mem(RDTSC_RET))
    opts = strider.lift.LifterOptions(
        cfg=strider.cfg.CfgOptions(call_other_abis={"rdtsc": CallOtherAbi.pure()})
    )
    _cfg, fn, _unresolved = lift.analyze(
        0x1000, CallingConvention.x86_64_systemv(), opts
    )
    assert fn.node_count() > 0


def test_a_custom_cc_repr_names_its_arch():
    assert repr(_custom_cc(SleighArch.x86_64())) == "<CallingConvention custom for x86_64>"
    assert repr(CallingConvention.x86_64_systemv()) == "CallingConvention.x86_64_systemv()"


def test_no_return_as_the_main_cc_is_rejected():
    """`no_return` describes a CALLEE; `build_cc` silently dropped it while
    `per_address_ccs` honoured it."""
    lift = strider.lift.lifter(SleighArch.x86_64(), _mem())
    with pytest.raises(strider.StriderError, match=r"per_address_ccs"):
        lift.analyze(0x1000, CallingConvention.x86_64_systemv().no_return())


def test_no_return_still_works_as_a_per_address_override():
    lift = strider.lift.lifter(SleighArch.x86_64(), _mem())
    opts = strider.lift.LifterOptions(
        per_address_ccs={0x2000: CallingConvention.x86_64_systemv().no_return()}
    )
    _cfg, fn, _unresolved = lift.analyze(
        0x1000, CallingConvention.x86_64_systemv(), opts
    )
    assert fn.node_count() > 0
