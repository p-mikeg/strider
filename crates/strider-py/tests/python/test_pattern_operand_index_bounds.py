"""An out-of-range operand index is an error, never wrapped arithmetic.

Every one of these setters shifts `idx` past the node's fixed head slots, so an
unbounded `idx` overflows: a panic in a debug build, and without
`overflow-checks` a wrap onto a real slot in a shipped wheel.
"""

import pytest

import strider
from strider import pattern as p

HUGE = 2**64 - 1


def _setters():
    return [
        ("call().arg", lambda i: p.call().arg(i, p.anything())),
        ("call_other().arg", lambda i: p.call_other().arg(i, p.anything())),
        ("ret().ret_val", lambda i: p.ret().ret_val(i, p.anything())),
        ("phi().phi_input", lambda i: p.phi().phi_input(i, p.anything())),
        ("mem_phi().phi_input", lambda i: p.mem_phi().phi_input(i, p.mem_phi())),
    ]


@pytest.mark.parametrize("name,build", _setters(), ids=[n for n, _ in _setters()])
def test_huge_operand_index_raises(name, build):
    with pytest.raises((strider.StriderError, ValueError)) as e:
        build(HUGE)
    assert "operand index" in str(e.value)


@pytest.mark.parametrize("name,build", _setters(), ids=[n for n, _ in _setters()])
def test_ordinary_operand_index_still_works(name, build):
    """The control for the refusal above: the magnitude is what is rejected,
    not the setter. Compiled AND run, since a wrapped index lands on a real
    slot and only shows up at match time."""
    fn = _ret_one()
    assert isinstance(fn.find_all(build(0).into_pat()), list)


def _lift(code: bytes, arch, cc):
    mem = strider.reader.BufferReader(0x1000, code)
    lifter = strider.lift.lifter(arch, mem)
    return lifter.analyze(0x1000, cc).function


def _ret_one():
    """`mov eax,1; ret`."""
    return _lift(
        bytes([0xB8, 0x01, 0, 0, 0, 0xC3]),
        strider.sleigh.SleighArch.x86_64(),
        strider.sleigh.CallingConvention.x86_64_systemv(),
    )


def test_an_ordinary_index_binds_the_slot_it_names():
    """`call().arg(0, ..)` and both `phi_input(0, ..)` are pinned against real
    IR in test_call_builder.py and test_pattern_mixins.py; these two were only
    ever compiled."""
    assert len(_ret_one().find_all(p.ret().ret_val(0, p.int_const(1)))) == 1
    # `int 0x80` lifts to CallOther(swi, 0x10, 0x80): two value arguments.
    syscall = _lift(
        bytes([0xCD, 0x80, 0xC3]),
        strider.sleigh.SleighArch.x86(),
        strider.sleigh.CallingConvention.x86_cdecl(),
    )
    assert len(syscall.find_all(p.call_other().arg(0, p.anything()))) == 1
    assert len(syscall.find_all(p.call_other().arg(1, p.anything()))) == 1


def test_nesting_cap_message_names_the_binding_limit():
    """The count ceiling is the looser of the two nesting bounds, and a
    pattern anywhere near it will not compile: the message has to say so
    rather than read as a usable depth."""
    pat = p.anything()
    with pytest.raises(strider.StriderError) as e:
        for _ in range(2000):
            pat = pat.of_width(32)
    assert "stack budget" in str(e.value)
