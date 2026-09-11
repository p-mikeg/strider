"""Reverse varnode to register-name lookup: `Sleigh.reg_name(vn)`.

Without it a REGISTER-space `Vn` off a lifted function reprs as
`%[0x4]:4`, which reads like a stack slot and misleads.
"""

import strider
from strider import pattern as p

from .conftest import fixture_path


def _x86_64_sleigh():
    arch = strider.sleigh.SleighArch.x86_64()
    mem = strider.reader.BufferReader(0x1000, b"\x90")
    return strider.sleigh.Sleigh(arch, mem)


def test_reg_name_round_trips_forward_lookup():
    sleigh = _x86_64_sleigh()
    rsp = sleigh.reg("RSP")
    assert rsp is not None
    assert sleigh.reg_name(rsp) == "RSP"


def test_reg_name_none_for_non_register_space():
    sleigh = _x86_64_sleigh()
    # Non-register spaces yield None rather than raising.
    assert sleigh.reg_name(strider.sleigh.Vn(strider.sleigh.VnSpace.CONST, 0x42, 4)) is None
    assert sleigh.reg_name(strider.sleigh.Vn(strider.sleigh.VnSpace.RAM, 0x1000, 8)) is None
    # In REGISTER space, but no table entry at that offset.
    assert sleigh.reg_name(strider.sleigh.Vn(strider.sleigh.VnSpace.REGISTER, 0xDEAD00, 8)) is None


def test_reg_name_decodes_initial_var_varnodes_of_a_lifted_function():
    lifter = strider.lift.load_elf(str(fixture_path("x64", "arithmetic")))
    _cfg, fn, _unresolved = lifter.analyze("add")

    names = set()
    for m in fn.find_all(p.initial_var()):
        vn = fn.node(m.root).vn()
        assert vn is not None, "an InitialVar node always names a varnode"
        name = lifter.reg_name(vn)
        if name is not None:
            names.add(name)

    # `add` is a leaf taking its operands in the SystemV argument registers.
    # RDX motivated the method: it lifts as `%[0x10]:8`, whose small offset
    # reads like a stack slot until decoded.
    assert {"RDI", "RSI", "RDX"} <= names
