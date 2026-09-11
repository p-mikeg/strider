"""Signed borrow is the one flag test in the family that is not symmetric.

`SBORROW(a, b)` asks whether `a - b` overflows, so swapping the operands
asks a different question, while `SCARRY` and `CARRY` are additions and do
commute. `NodeKind::is_commutative` lists the latter two and omits
`Sborrow`; a query is what that omission is for, so it is pinned here from
the outside, on both the matching and the building side.
"""

from __future__ import annotations

import strider
from strider import template as tpl
from strider.pattern import (
    Capture,
    anything,
    int_const,
    int_sborrow,
    int_scarry,
    var,
)

# add rdi, 5 / seto al / sub rsi, 7 / seto ah / ret: one SCARRY against a
# literal 5 and one SBORROW against a literal 7, both kept live by the setcc.
FLAG_PAIR = bytes.fromhex("4883c705" "0f90c0" "4883ee07" "0f90c4" "c3")
BASE = 0x1000


def _flag_graph():
    mem = strider.reader.BufferReader(BASE, FLAG_PAIR)
    lift = strider.lift.lifter(strider.sleigh.SleighArch.x86_64(), mem)
    _cfg, function, _unresolved = lift.analyze(
        BASE,
        strider.sleigh.CallingConvention.x86_64_systemv(),
        opts=strider.lift.LifterOptions(pipeline=strider.opt.OptimizerPipeline.empty()),
    )
    return function


def test_a_signed_borrow_matches_in_one_operand_order_only():
    g = _flag_graph()
    x = Capture()
    assert len(g.find_all(int_sborrow(var(x), int_const(7)))) == 1
    assert not g.find_all(int_sborrow(int_const(7), var(x)))


def test_the_signed_carry_beside_it_matches_in_either_order():
    g = _flag_graph()
    x = Capture()
    assert len(g.find_all(int_scarry(var(x), int_const(5)))) == 1
    assert len(g.find_all(int_scarry(int_const(5), var(x)))) == 1


def test_a_built_signed_borrow_keeps_the_operand_order_it_was_given():
    g = _flag_graph()
    value, konst = Capture(), Capture()
    assert g.rewrite(
        find=int_sborrow(var(value), int_const(konst)),
        replace=tpl.int_sborrow(tpl.var(konst), tpl.var(value)),
    )
    assert g.validate() is None
    assert len(g.find_all(int_sborrow(int_const(7), anything()))) == 1
    assert not g.find_all(int_sborrow(anything(), int_const(7)))
