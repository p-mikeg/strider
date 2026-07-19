"""Type stubs for strider.pattern.constraints: relational join constraints.

A pattern (`strider.pattern`) describes graph SHAPE and is passed as
`find_all(pats, ...)`. A constraint is a relational predicate over the
captures those patterns bind, evaluated after the join and passed as
`find_all(..., constraints=[...])`. They live in separate namespaces so the
two kinds cannot be mistaken for one another.
"""

from __future__ import annotations

from typing import List

from . import Capture

class JoinConstraint:
    """A control-flow relation between captured entities. Build one with
    `dominates` or `phi_input_from_edge`, negate it with `negate`, and pass
    it as `Function.find_all([...], constraints=[...])`."""

def dominates(a: Capture, b: Capture) -> JoinConstraint:
    """`a` dominates `b`: every path from entry to `b` passes through `a`.
    Operands are captured nodes (or an `If` branch-edge capture)."""
def phi_input_from_edge(
    phi: Capture, edge: Capture, value: Capture
) -> JoinConstraint:
    """The value merged into `phi` from the branch `edge` is `value`. `edge`
    binds an `If`'s `capture_true` / `capture_false` value, and `value` is
    bound by another pattern in the same `find_all` list."""

def negate(c: JoinConstraint) -> JoinConstraint:
    """The negation of `c`: a match survives only if `c` does not hold."""

def any_of(constraints: List[JoinConstraint]) -> JoinConstraint:
    """A constraint that passes when ANY of the listed constraints passes.

    The top-level `constraints=[...]` list is already an AND, so `any_of` is
    how you express OR. An empty list passes nothing.
    """

def all_of(constraints: List[JoinConstraint]) -> JoinConstraint:
    """A constraint that passes only when EVERY listed constraint passes.

    Use it to AND constraints inside an `any_of` (the top-level list does not
    nest). An empty list passes everything.
    """
