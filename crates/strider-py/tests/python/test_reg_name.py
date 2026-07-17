"""Reverse varnode -> register-name lookup on `strider.Sleigh`.

`Sleigh.reg(name)` is the forward direction (name -> Vn).  Without a
reverse lookup, a REGISTER-space `Vn` reached from a lifted function
reprs as `%[0x4]:4` and has to be hand-decoded — which reads like a
stack slot and misleads.  `Sleigh.reg_name(vn)` closes the loop.
"""

import strider
from strider import pattern as p

from .conftest import fixture_path


def _x86_64_sleigh():
    arch = strider.SleighArch.x86_64()
    mem = strider.BufferReader(0x1000, b"\x90")
    return strider.Sleigh(arch, mem)


def test_reg_name_round_trips_forward_lookup():
    sleigh = _x86_64_sleigh()
    rsp = sleigh.reg("RSP")
    assert rsp is not None
    assert sleigh.reg_name(rsp) == "RSP"


def test_reg_name_none_for_non_register_space():
    sleigh = _x86_64_sleigh()
    # CONST-space varnodes are not registers — no name, and no error.
    assert sleigh.reg_name(strider.Vn(strider.VnSpace.const(), 0x42, 4)) is None
    # A RAM address (e.g. a real stack slot) likewise has no register name.
    assert sleigh.reg_name(strider.Vn(strider.VnSpace.ram(), 0x1000, 8)) is None
    # An unallocated REGISTER offset is still in REGISTER space but names
    # nothing in the table.
    assert sleigh.reg_name(strider.Vn(strider.VnSpace.register(), 0xDEAD00, 8)) is None


def test_reg_name_decodes_initial_var_varnodes_of_a_lifted_function():
    lifter = strider.load_elf(str(fixture_path("x64", "arithmetic")))
    _cfg, fn, _unresolved = lifter.analyze("add")

    names = set()
    for m in fn.find_all(p.initial_var()):
        vn = fn.node(m.root).vn()
        name = lifter.reg_name(vn)
        if name is not None:
            names.add(name)

    # `add` is a leaf taking its operands in the SystemV argument
    # registers, so its entry state reads them.  `RDX` is the varnode
    # that motivated this method: it lifts as `%[0x10]:8`, whose small
    # `0x10` offset reads like a stack slot until it is decoded.
    assert {"RDI", "RSI", "RDX"} <= names
