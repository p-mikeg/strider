"""`strider.pattern.constraints`, the join constraints' own namespace.

A pattern describes graph SHAPE (`find_all(pats, ...)`); a constraint is a
relational predicate over captures (`find_all(..., constraints=[...])`). The
split is namespaced so the two cannot be confused for one another.
"""

import pytest

import strider
import strider.pattern
import strider.pattern.constraints
from strider import pattern as p
from strider.pattern import constraints as cons

_MOVED = ("dominates", "phi_input_from_edge", "JoinConstraint")


def test_import_strider_pattern_constraints_dotted():
    assert strider.pattern.constraints.dominates is not None


def test_from_strider_pattern_import_constraints():
    from strider.pattern import constraints

    assert constraints.dominates is not None


def test_sys_modules_registers_the_full_dotted_path():
    import sys

    assert "strider.pattern.constraints" in sys.modules


@pytest.mark.parametrize("name", _MOVED)
def test_moved_names_are_present_on_constraints(name):
    assert hasattr(cons, name), f"{name} missing from strider.pattern.constraints"


@pytest.mark.parametrize("name", _MOVED)
def test_moved_names_are_gone_from_pattern(name):
    # A name that still resolved in the old namespace would defeat the
    # separation the move makes.
    assert not hasattr(p, name), f"{name} still on strider.pattern"


def test_negate_is_exported():
    assert callable(cons.negate)
