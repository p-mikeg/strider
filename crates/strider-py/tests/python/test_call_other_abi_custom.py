"""`CallOtherAbi` carries a full user-op footprint from Python.

The register names are resolved against the arch's register table at
construction, the way `CallingConvention.custom` resolves its own.
"""

import pytest

import strider
from strider.sleigh import CallOtherAbi, CallingConvention, SleighArch

# rdtsc; ret
RDTSC_RET = b"\x0f\x31\xc3"
# rdtsc; mov rax, rbx; ret
RDTSC_MOV_RET = b"\x0f\x31\x48\x89\xd8\xc3"


def _lifter(code=RDTSC_RET):
    mem = strider.reader.BufferReader(0x1000, code)
    return strider.lift.lifter(SleighArch.x86_64(), mem)


def _analyze(code, abis):
    lift = _lifter(code)
    opts = strider.lift.LifterOptions(cfg=strider.cfg.CfgOptions(call_other_abis=abis))
    _cfg, fn, _unresolved = lift.analyze(
        0x1000, CallingConvention.x86_64_systemv(), opts
    )
    return fn


def _call_other(fn, name):
    for nid in fn.node_ids():
        node = fn.node(nid)
        if node.call_other_name() == name:
            return node
    return None


def _initial_var_names(fn, sleigh):
    names = set()
    for nid in fn.node_ids():
        node = fn.node(nid)
        if node.kind().startswith("InitialVar"):
            vn = node.vn()
            if vn is not None:
                names.add(sleigh.reg_name(vn))
    return names


def test_custom_resolves_register_names():
    abi = CallOtherAbi.custom(
        SleighArch.x86_64(),
        implicit_reads=["RCX"],
        implicit_writes=["RBX"],
        clobbers_memory=True,
    )
    assert abi.implicit_reads == ["RCX"]
    assert abi.implicit_writes == ["RBX"]
    assert abi.clobbers_memory is True
    assert abi.is_no_return is False


def test_custom_unknown_register_raises_at_construction():
    with pytest.raises(
        strider.StriderError, match='implicit_reads: unknown sleigh register name "NOSUCHREG"'
    ):
        CallOtherAbi.custom(SleighArch.x86_64(), implicit_reads=["NOSUCHREG"])


def test_implicit_read_reaches_the_lifted_graph():
    """The override's read register must appear as an argument of the
    user-op node, not merely be accepted by the constructor."""
    sleigh = SleighArch.x86_64()
    plain = _analyze(RDTSC_RET, {})
    plain_node = _call_other(plain, "rdtsc")
    assert plain_node is not None
    assert "RCX" not in {
        sleigh.reg_name(vn)
        for i in plain_node.inputs()
        if (vn := i.vn()) is not None
    }

    abi = CallOtherAbi.custom(sleigh, implicit_reads=["RCX"])
    overridden = _analyze(RDTSC_RET, {"rdtsc": abi})
    node = _call_other(overridden, "rdtsc")
    assert node is not None
    assert "RCX" in {
        sleigh.reg_name(vn) for i in node.inputs() if (vn := i.vn()) is not None
    }


def test_implicit_write_replaces_the_entry_value():
    """`mov rax, rbx` reads RBX's entry value; declaring the user-op a
    writer of RBX makes it read the user-op's output instead, so the
    entry value goes dead."""
    sleigh = SleighArch.x86_64()
    plain = _analyze(RDTSC_MOV_RET, {})
    assert "RBX" in _initial_var_names(plain, sleigh)

    abi = CallOtherAbi.custom(sleigh, implicit_writes=["RBX"])
    overridden = _analyze(RDTSC_MOV_RET, {"rdtsc": abi})
    assert "RBX" not in _initial_var_names(overridden, sleigh)


def test_presets_match_the_former_string_classes():
    assert _call_other(_analyze(RDTSC_RET, {"rdtsc": CallOtherAbi.noop()}), "rdtsc") is None
    assert _call_other(_analyze(RDTSC_RET, {"rdtsc": CallOtherAbi.pure()}), "rdtsc") is not None
    assert (
        _call_other(_analyze(RDTSC_RET, {"rdtsc": CallOtherAbi.mem_clobber()}), "rdtsc")
        is not None
    )

    ended = _analyze(RDTSC_MOV_RET, {"rdtsc": CallOtherAbi.no_return()})
    kinds = {ended.node(nid).kind() for nid in ended.node_ids()}
    assert "Unreachable" in kinds
    assert "Return" not in kinds


def test_preset_footprints_are_empty():
    for abi in (
        CallOtherAbi.noop(),
        CallOtherAbi.pure(),
        CallOtherAbi.mem_clobber(),
        CallOtherAbi.no_return(),
    ):
        assert abi.implicit_reads == []
        assert abi.implicit_writes == []
    assert CallOtherAbi.mem_clobber().clobbers_memory is True
    assert CallOtherAbi.pure().clobbers_memory is False
    assert CallOtherAbi.no_return().is_no_return is True
    assert CallOtherAbi.noop().is_noop is True
    assert CallOtherAbi.pure().is_noop is False


def test_user_op_names_are_enumerable():
    names = _lifter().user_op_names()
    assert isinstance(names, list)
    assert "rdtsc" in names


def test_classification_reads_back():
    lift = _lifter()
    builtin = lift.call_other_abi("rdtsc")
    assert builtin is not None
    assert builtin.clobbers_memory is False
    assert builtin.is_no_return is False
    assert lift.call_other_abi("no_such_user_op") is None

    custom = CallOtherAbi.custom(SleighArch.x86_64(), implicit_reads=["RCX"])
    opts = strider.cfg.CfgOptions(call_other_abis={"rdtsc": custom})
    assert lift.call_other_abi("rdtsc", opts) == custom
    assert lift.call_other_abi("rdtsc", opts) != builtin


def test_classification_refuses_a_foreign_entry_as_build_cfg_does():
    lift = _lifter()
    foreign = CallOtherAbi.custom(SleighArch.aarch64(), implicit_reads=["x8"])
    opts = strider.cfg.CfgOptions(call_other_abis={"rdtsc": foreign})
    with pytest.raises(strider.StriderError, match="built against a \"aarch64\" Sleigh") as built:
        lift.build_cfg(0x1000, opts)
    with pytest.raises(strider.StriderError) as classified:
        lift.call_other_abi("rdtsc", opts)
    assert str(classified.value) == str(built.value)


# syscall; ret
SYSCALL_RET = b"\x0f\x05\xc3"


def test_syscall_lifts_with_its_full_implicit_footprint():
    """`syscall` names no register in its p-code, so only its ABI row puts
    R10 (read) and R11 (write) in the tracked universe: neither is a SysV
    argument register nor callee-saved."""
    sleigh = SleighArch.x86_64()
    fn = _analyze(SYSCALL_RET, {})
    node = _call_other(fn, "syscall")
    assert node is not None
    args = [
        sleigh.reg_name(vn) for i in node.inputs() if (vn := i.vn()) is not None
    ]
    assert args == ["RAX", "RDI", "RSI", "RDX", "R10", "R8", "R9"]
    result_vn = node.vn()
    assert result_vn is not None
    assert sleigh.reg_name(result_vn) == "RAX"


def test_custom_reads_a_register_with_no_calling_convention_role():
    """Under x86 cdecl, ECX is neither an argument, a return, nor a
    callee-saved register, and `rdtsc; ret` never names it: the declared
    footprint is the only thing that can track it."""
    mem = strider.reader.BufferReader(0x1000, RDTSC_RET)
    lift = strider.lift.lifter(SleighArch.x86(), mem)
    sleigh = SleighArch.x86()
    abi = CallOtherAbi.custom(sleigh, implicit_reads=["ECX"], implicit_writes=["EDI"])
    opts = strider.lift.LifterOptions(
        cfg=strider.cfg.CfgOptions(call_other_abis={"rdtsc": abi})
    )
    _cfg, fn, _unresolved = lift.analyze(0x1000, CallingConvention.x86_cdecl(), opts)
    node = _call_other(fn, "rdtsc")
    assert node is not None
    assert [
        sleigh.reg_name(vn) for i in node.inputs() if (vn := i.vn()) is not None
    ] == ["ECX"]


@pytest.mark.parametrize(
    "custom",
    [
        lambda arg: CallOtherAbi.custom(arg, implicit_reads=["EAX"]),
        lambda arg: CallingConvention.custom(
            arg, ["EAX"], [], ["EAX"], [], "ESP", None, 4, 4
        ),
    ],
    ids=["CallOtherAbi", "CallingConvention"],
)
def test_custom_names_what_it_takes_for_a_wrong_type(custom):
    with pytest.raises(
        TypeError,
        match="argument 'sleigh': 'str' object cannot be converted to 'SleighArch'",
    ):
        custom("x86")


def test_a_custom_abi_repr_reads_back_and_a_builtin_one_does_not_pose_as_custom():
    arch = SleighArch.x86_64()
    abi = CallOtherAbi.custom(arch, implicit_reads=["ECX"], clobbers_memory=True)
    assert eval(repr(abi), {"CallOtherAbi": CallOtherAbi, "SleighArch": SleighArch}) == abi
    lifter = strider.lift.lifter(arch, strider.reader.BufferReader(0x1000, b"\xc3"))
    builtin = lifter.call_other_abi("cpuid")
    assert builtin is not None
    assert repr(builtin).startswith("<CallOtherAbi ") and "ECX" in repr(builtin)
