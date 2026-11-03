from __future__ import annotations

import pytest
import strider

_SA = strider.sleigh.SleighArch
_CC = strider.sleigh.CallingConvention
# `custom` and `preset` are constructors by other means, not presets.
_NOT_PRESETS = {"custom", "preset"}
_CC_NAMES = sorted(
    n
    for n in dir(_CC)
    if not n.startswith("_")
    and n not in _NOT_PRESETS
    and getattr(getattr(_CC, n), "__self__", None) is _CC
)
_ARCH_NAMES = sorted(strider.sleigh.ARCH_PRESETS)
_PAIRS = [(a, cc.name()) for a in _ARCH_NAMES for cc in _SA.preset(a).calling_conventions()]


def test_every_rust_cc_preset_is_bound():
    assert _CC_NAMES == sorted(strider.sleigh.CC_PRESETS)


@pytest.mark.parametrize("name", _CC_NAMES)
def test_cc_preset_by_name(name):
    assert _CC.preset(name) == getattr(_CC, name)()
    assert _CC.preset(name).name() == name


def test_cc_preset_unknown_name_raises():
    with pytest.raises(strider.StriderError, match="x86_64_systemv"):
        _CC.preset("no_such_cc")


def test_calling_conventions_x86_64():
    assert _SA.x86_64().calling_conventions() == (_CC.x86_64_systemv(),)


def test_calling_conventions_ppc64be_has_both_elf_abis():
    ccs = _SA.ppc64be().calling_conventions()
    assert _CC.powerpc64_elf_v1() in ccs
    assert _CC.powerpc64_elf_v2() in ccs


def test_every_cc_preset_belongs_to_some_arch():
    seen = {n for _, n in _PAIRS}
    assert seen == set(strider.sleigh.CC_PRESETS)


@pytest.mark.parametrize(("arch_name", "cc_name"), _PAIRS)
def test_to_dict_round_trips_through_custom(arch_name, cc_name):
    arch = _SA.preset(arch_name)
    cc = _CC.preset(cc_name)
    d = cc.to_dict()
    assert "sleigh" not in d
    rebuilt = _CC.custom(arch, **d)
    assert rebuilt.to_dict() == d


def test_custom_entry_address_round_trips_on_mips():
    arch = _SA.mipsle32()
    cc = _CC.custom(
        arch,
        arg_passing_regs=["a0"],
        callee_saved_regs=["s0", "ra"],
        ret_val_regs=["v0"],
        ret_val_regs_float=[],
        stack_pointer="sp",
        stack_arg_base=16,
        stack_arg_increment=4,
        ret_stack_pop=0,
        link_register="ra",
        entry_address="t9",
    )
    assert cc.to_dict()["entry_address"] == "t9"
    assert _CC.custom(arch, **cc.to_dict()).to_dict() == cc.to_dict()


def test_preserves_all_registers_is_not_expressible():
    with pytest.raises(strider.StriderError, match="preserves_all"):
        _CC.x86_64_systemv().preserves_all().to_dict()
    with pytest.raises(strider.StriderError, match="preserves_regs"):
        _CC.x86_64_systemv().preserves_regs().to_dict()
