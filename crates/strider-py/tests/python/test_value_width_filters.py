"""Width and boolean filters against IR that holds both a matching and a
non-matching shape, so each one is shown to reject as well as accept.

`aarch64/arithmetic.elf::shl` lifts, unoptimized, to a graph carrying an
`I1` `Xor` over `I1` operands next to an `I1` comparison over `I64` ones:
the pair that separates "one bit out" from "one bit in".
"""

from __future__ import annotations

import strider
from strider.pattern import (
    Capture,
    any_bool_binary,
    any_int_binary,
    any_int_cmp,
    anything,
    bool_inputs,
    int_const,
    inputs_of_width,
    value_of_width,
)

from .conftest import fixture_path


def _mixed_width_graph():
    prog = strider.lift.load_elf(str(fixture_path("aarch64", "arithmetic")))
    return prog.analyze(
        "shl",
        opts=strider.lift.LifterOptions(pipeline=strider.opt.OptimizerPipeline.empty()),
    ).function


def _roots(function, pat):
    return {m.root for m in function.find_all(pat)}


def test_value_of_width_partitions_the_graph_by_output_width():
    g = _mixed_width_graph()
    one_bit = _roots(g, value_of_width(1))
    sixty_four = _roots(g, value_of_width(64))
    assert one_bit and sixty_four
    assert not one_bit & sixty_four
    assert {g.node(i).value_type() for i in one_bit} == {"I1"}
    assert {g.node(i).value_type() for i in sixty_four} == {"I64"}


def test_a_comparison_is_one_bit_wide_on_its_output_but_not_on_its_inputs():
    g = _mixed_width_graph()
    c = Capture()
    cmps = g.find_all(any_int_cmp(c, anything(), anything()))
    assert len(cmps) == 1
    cmp_root = cmps[0].root
    assert cmp_root in _roots(g, value_of_width(1))
    assert cmp_root in _roots(g, inputs_of_width(64, any_int_cmp(c, anything(), anything())))
    assert not g.find_all(bool_inputs(any_int_cmp(c, anything(), anything())))


def test_inputs_of_width_keeps_only_the_operand_width_it_names():
    g = _mixed_width_graph()
    c = Capture()
    inner = any_int_binary(c, anything(), anything())
    every = _roots(g, inner)
    one_bit = _roots(g, inputs_of_width(1, inner))
    assert one_bit
    assert one_bit < every
    assert not _roots(g, inputs_of_width(64, inner))


def test_bool_inputs_is_the_one_bit_case_of_inputs_of_width():
    g = _mixed_width_graph()
    c = Capture()
    inner = any_int_binary(c, anything(), anything())
    assert _roots(g, bool_inputs(inner)) == _roots(g, inputs_of_width(1, inner))


def test_a_constant_has_no_value_inputs_so_no_input_width_matches_it():
    g = _mixed_width_graph()
    assert g.find_all(int_const())
    assert not g.find_all(bool_inputs(int_const()))
    assert not g.find_all(inputs_of_width(64, int_const()))


def test_any_bool_binary_keeps_only_the_one_bit_integer_binaries():
    g = _mixed_width_graph()
    c = Capture()
    every = g.find_all(any_int_binary(c, anything(), anything()))
    booleans = g.find_all(any_bool_binary(c, anything(), anything()))
    assert {m.value_type(c) for m in every} == {"I1", "I32"}
    assert {m.value_type(c) for m in booleans} == {"I1"}
    assert {m.root for m in booleans} < {m.root for m in every}
