"""The frozen value records compare by value: a fresh read-back of the same
arch, convention or symbol compares equal to it."""

from strider.lift import lifter, load_elf
from strider.reader import BufferReader
from strider.sleigh import CallingConvention, SleighArch

from .conftest import fixture_path


def _custom(arch, sp):
    return CallingConvention.custom(arch, ["RDI"], [], ["RAX"], [], sp, None, 8, 8)


def test_an_arch_equals_the_preset_it_came_from():
    lift = lifter(SleighArch.x86_64(), BufferReader(0, b"\xc3"))
    assert lift.arch == lift.arch == SleighArch.x86_64()
    assert lift.arch != SleighArch.x86()
    assert eval(repr(lift.arch), {"SleighArch": SleighArch}) == lift.arch
    assert len({SleighArch.arm(), SleighArch.arm(), SleighArch.arm_thumb()}) == 2


def test_a_convention_equals_the_calls_that_built_it():
    cc = CallingConvention.x86_64_systemv()
    assert cc == CallingConvention.x86_64_systemv()
    assert hash(cc) == hash(CallingConvention.x86_64_systemv())
    assert cc != CallingConvention.x86_cdecl()
    assert cc.preserves_all() == CallingConvention.x86_64_systemv().preserves_all()
    assert cc.preserves_all() != cc
    assert cc.preserves_all() != cc.preserves_regs()
    a = _custom(SleighArch.x86_64(), "RSP")
    assert a == _custom(SleighArch.x86_64(), "RSP")
    assert a != cc


def test_an_elf_lifter_reads_back_its_preset_convention():
    lift = load_elf(str(fixture_path("x86", "arithmetic")))
    assert lift.cc == CallingConvention.x86_cdecl()
    assert lift.arch == SleighArch.x86()


def test_a_symbol_equals_another_read_of_it():
    lift = load_elf(str(fixture_path("x86", "arithmetic")))
    sym = lift.symbol("add")
    assert sym == lift.symbol("add") == lift.symbol_at(sym.address)
    assert sym in set(lift.functions())
    assert sym != lift.symbol("main")
