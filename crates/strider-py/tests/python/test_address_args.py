"""An address, node id, operand index, size or count outside its unsigned
range raises `StriderError`, and a `bool` is not taken for one."""

import pytest

import strider
from strider import cfg, lift, pattern as p, reader
from strider.sleigh import CallingConvention, SleighArch, Vn, VnSpace

from .conftest import fixture_path, lift_bytes

_CC = CallingConvention.x86_64_systemv()
_RET = bytes([0xC3])


def _lifter():
    return lift.lifter(SleighArch.x86_64(), reader.BufferReader(0x1000, _RET))


def _cfg():
    return _lifter().analyze(0x1000, _CC).cfg


def _elf():
    return lift.load_elf(str(fixture_path("x64", "memory")))


_TAKES = {
    "pcode_at addr": lambda n: _lifter().pcode_at(0x1000, n),
    "BufferReader base": lambda n: reader.BufferReader(n, b"x"),
    "BufferReader.read addr": lambda n: reader.BufferReader(0, b"ab").read(n, 1),
    "known_targets key": lambda n: cfg.CfgOptions(known_targets={n: [1]}),
    "known_targets target": lambda n: cfg.CfgOptions(known_targets={1: [n]}),
    "no_return_targets": lambda n: cfg.CfgOptions(no_return_targets=[n]),
    "data_ranges": lambda n: cfg.CfgOptions(data_ranges=[(n, 2)]),
    "per_address_ccs key": lambda n: lift.LifterOptions(per_address_ccs={n: _CC}),
    "noalias_allocators": lambda n: lift.AssumptionOptions(noalias_allocators=[n]),
    "symbol_at": lambda n: _elf().symbol_at(n),
    "add_symbols": lambda n: _elf().add_symbols({"x": n}),
    "ElfLifter.read": lambda n: _elf().read(n, 4),
    "ElfLifter.read size": lambda n: _elf().read(0x1000, n),
    "BufferReader.read size": lambda n: reader.BufferReader(0, b"ab").read(0, n),
    "bit_width": lambda n: p.load().bit_width(n),
    "of_width": lambda n: p.anything().of_width(n),
    "function_arg": lambda n: p.function_arg(n),
    "call().arg": lambda n: p.call().arg(n, p.anything()),
    "Function.node": lambda n: lift_bytes(_RET).node(n),
    "Function.walk": lambda n: lift_bytes(_RET).walk(n),
    "Function.neighborhood_dot": lambda n: lift_bytes(_RET).neighborhood_dot(n),
    "Cfg.pcode_at": lambda n: _cfg().pcode_at(n),
    "Cfg.region_at": lambda n: _cfg().region_at(n),
    "Cfg.neighborhood_dot": lambda n: _cfg().neighborhood_dot(n),
    "Function.neighborhood_dot depth": lambda n: lift_bytes(_RET).neighborhood_dot(0, n),
    "Function.neighborhood_dot hub_cap": lambda n: lift_bytes(_RET).neighborhood_dot(
        0, hub_cap=n
    ),
    "Function.neighborhood_dot max_nodes": lambda n: lift_bytes(
        _RET
    ).neighborhood_dot(0, max_nodes=n),
    "Cfg.neighborhood_dot depth": lambda n: _cfg().neighborhood_dot(0, n),
    "Cfg.neighborhood_dot max_nodes": lambda n: _cfg().neighborhood_dot(0, max_nodes=n),
    "function_max_size": lambda n: cfg.CfgOptions(function_max_size=n),
    "function_arg().index": lambda n: p.any_function_arg().index(n),
    "Vn off": lambda n: Vn(VnSpace.REGISTER, n, 8),
}


@pytest.mark.parametrize("take", _TAKES.values(), ids=_TAKES.keys())
@pytest.mark.parametrize("n", [-1, 2**64])
def test_out_of_range_raises_strider_error(take, n):
    with pytest.raises(strider.StriderError, match="out of range"):
        take(n)


@pytest.mark.parametrize("take", _TAKES.values(), ids=_TAKES.keys())
def test_bool_is_refused(take):
    with pytest.raises(TypeError, match="bool"):
        take(True)


_ENTRIES = {
    "analyze": lambda e: _lifter().analyze(e, _CC),
    "build_cfg": lambda e: _lifter().build_cfg(e),
    "ElfLifter.analyze": lambda e: _elf().analyze(e),
    "ElfLifter.build_cfg": lambda e: _elf().build_cfg(e),
}


@pytest.mark.parametrize("call", _ENTRIES.values(), ids=_ENTRIES.keys())
def test_bool_entry_is_refused(call):
    with pytest.raises(TypeError, match="bool"):
        call(True)
