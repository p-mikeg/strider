"""A `custom(...)` convention or user-op ABI freezes varnodes resolved against
one arch's register table.

The same register name denotes a different varnode on another arch (x86-64
`EDI` is `%[0x38]:4`, x86 `EDI` is `%[0x1c]:4`), so consuming one on the wrong
`Lifter` has to raise rather than lift a function whose arguments silently
are not there.
"""

import pytest

import strider
from strider.sleigh import CallOtherAbi, CallingConvention, SleighArch

# mov eax, edi ; ret
MOV_EAX_EDI_RET = b"\x89\xf8\xc3"
# rdtsc ; ret
RDTSC_RET = b"\x0f\x31\xc3"

X86_64_ARGS = ["EDI", "ESI", "EDX", "ECX"]


def _mem(code=MOV_EAX_EDI_RET):
    return strider.reader.BufferReader(0x1000, code)


def _custom_cc(arch):
    return CallingConvention.custom(
        sleigh=arch,
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


def test_a_custom_cc_crosses_presets_with_one_register_table():
    """`arm_be_kernel` has its own sla, for its big-endian data transfers,
    but the same registers as `arm`, so a convention frozen on either
    serves the other."""
    # mov r0, r1 ; bx lr
    code = b"\x01\x00\xa0\xe1\x1e\xff\x2f\xe1"
    for built_on, lifted_by in (
        (SleighArch.arm(), SleighArch.arm_be_kernel()),
        (SleighArch.arm_be_kernel(), SleighArch.arm()),
    ):
        cc = CallingConvention.custom(
            sleigh=built_on,
            arg_passing_regs=["r0", "r1"],
            callee_saved_regs=["lr"],
            ret_val_regs=["r0"],
            ret_val_regs_float=[],
            stack_pointer="sp",
            stack_arg_base=None,
            stack_arg_increment=4,
            ret_stack_pop=0,
            link_register="lr",
            preserves_memory=False,
        )
        lift = strider.lift.lifter(lifted_by, strider.reader.BufferReader(0x1000, code))
        _cfg, fn, _unresolved = lift.analyze(0x1000, cc)
        assert len(fn.find_all(strider.pattern.function_arg(1))) == 1


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
        sleigh=SleighArch.x86(),
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
        sleigh=SleighArch.x86_64(),
        implicit_writes=["EAX", "EDX"],
    )
    opts = strider.lift.LifterOptions(
        cfg=strider.cfg.CfgOptions(call_other_abis={"rdtsc": abi})
    )
    _cfg, fn, _unresolved = lift.analyze(
        0x1000, CallingConvention.x86_64_systemv(), opts
    )
    assert fn.node_count() > 0


def test_a_call_other_abi_accepted_on_one_arch_is_checked_again_on_another():
    """The resolved table is kept per arch, so one options object reused on a
    second `Lifter` is checked against that one's register table."""
    abi = CallOtherAbi.custom(
        sleigh=SleighArch.x86_64(),
        implicit_writes=["EAX", "EDX"],
    )
    opts = strider.lift.LifterOptions(
        cfg=strider.cfg.CfgOptions(call_other_abis={"rdtsc": abi})
    )
    lift = strider.lift.lifter(SleighArch.x86_64(), _mem(RDTSC_RET))
    lift.analyze(0x1000, CallingConvention.x86_64_systemv(), opts)
    other = strider.lift.lifter(SleighArch.x86(), _mem(RDTSC_RET))
    with pytest.raises(strider.StriderError, match=r'"rdtsc".*"x86_64".*"x86"'):
        other.analyze(0x1000, CallingConvention.x86_cdecl(), opts)


def test_a_preset_call_other_abi_is_not_arch_locked():
    lift = strider.lift.lifter(SleighArch.x86_64(), _mem(RDTSC_RET))
    opts = strider.lift.LifterOptions(
        cfg=strider.cfg.CfgOptions(call_other_abis={"rdtsc": CallOtherAbi.pure()})
    )
    _cfg, fn, _unresolved = lift.analyze(
        0x1000, CallingConvention.x86_64_systemv(), opts
    )
    assert fn.node_count() > 0


def test_an_empty_custom_abi_does_not_repr_as_a_preset():
    """`__repr__` is "the constructor call that produces this ABI", and
    `__eq__` keys on `source_arch`, so an empty `custom` must not print as
    the preset it is unequal to."""
    empty = CallOtherAbi.custom(sleigh=SleighArch.x86_64())
    assert empty != CallOtherAbi.pure()
    assert repr(empty) != repr(CallOtherAbi.pure())
    assert repr(empty).startswith("CallOtherAbi.custom(")
    assert repr(CallOtherAbi.pure()) == "CallOtherAbi.pure()"


def test_a_custom_cc_repr_names_its_arch():
    assert repr(_custom_cc(SleighArch.x86_64())) == "<CallingConvention custom for x86_64>"
    assert repr(CallingConvention.x86_64_systemv()) == "CallingConvention.x86_64_systemv()"


def test_custom_abis_equal_and_hash_alike_on_one_preset_only():
    """x86 and x86-64 `EAX` are one varnode, and `arm` / `arm_thumb` share an
    sla, so only the arch tells these apart."""
    x64 = [CallOtherAbi.custom(SleighArch.x86_64(), implicit_writes=["EAX"]) for _ in range(2)]
    x86 = CallOtherAbi.custom(SleighArch.x86(), implicit_writes=["EAX"])
    arm, thumb = (
        CallOtherAbi.custom(arch, implicit_writes=["r0"])
        for arch in (SleighArch.arm(), SleighArch.arm_thumb())
    )
    assert x64[0] == x64[1]
    assert hash(x64[0]) == hash(x64[1])
    assert x64[0] != x86
    assert arm != thumb
    assert len({x64[0], x64[1], x86, arm, thumb}) == 4


def test_one_per_address_table_resolves_against_each_lifters_arch():
    opts = strider.lift.LifterOptions(
        per_address_ccs={0x2000: CallingConvention.x86_64_systemv()}
    )
    x64 = strider.lift.lifter(SleighArch.x86_64(), _mem(b"\xc3"))
    x86 = strider.lift.lifter(SleighArch.x86(), _mem(b"\xc3"))
    x64.analyze(0x1000, CallingConvention.x86_64_systemv(), opts)
    with pytest.raises(strider.StriderError, match='unknown sleigh register name "RDI"'):
        x86.analyze(0x1000, CallingConvention.x86_cdecl(), opts)
    x64.analyze(0x1000, CallingConvention.x86_64_systemv(), opts)


def test_a_preserves_variant_reprs_as_the_call_that_made_it():
    """A `preserves_*` override is what `per_address_ccs` mostly holds, so its
    repr must not read as the plain preset."""
    cc = CallingConvention.x86_64_systemv()
    assert repr(cc.preserves_all()) == "CallingConvention.x86_64_systemv().preserves_all()"
    assert repr(cc.preserves_regs()) == "CallingConvention.x86_64_systemv().preserves_regs()"
    custom = _custom_cc(SleighArch.x86_64()).preserves_all()
    assert repr(custom) == "<CallingConvention custom.preserves_all() for x86_64>"
