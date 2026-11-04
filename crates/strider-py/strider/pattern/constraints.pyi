"""Type stubs for strider.pattern.constraints: relational join constraints.

A constraint is a relational predicate over the captures patterns bind,
evaluated after the join and passed as `find_all(..., constraints=[...])`.
"""

from __future__ import annotations

from typing import Sequence, Union

from . import Capture, Match

__all__: list[str]

class JoinConstraint:
    """A control-flow relation between captured entities. Build one with
    `dominates` or `phi_input_from_edge`, negate it with `negate`, and pass
    it as `Function.find_all([...], constraints=[...])`."""

class JoinPredicate:
    """Base class for a user-defined join constraint. Override
    `constraint(m)` to decide whether a joined match survives; optionally
    override `captures()` to declare the captures it correlates, so it can
    join otherwise independent patterns (the default `[]` is a pure filter).
    Pass an instance anywhere a `JoinConstraint` is accepted."""

    def __init__(self, *args, **kwargs) -> None:
        """Base initialiser; subclasses may take whatever arguments they like."""
        ...
    def captures(self) -> list[Capture]:
        """The captures this predicate correlates. Default: none."""
        ...
    def constraint(self, m: Match) -> bool:
        """Override to return whether the joined match `m` (spanning the
        whole join) survives, as a `bool`; anything else is a `TypeError`.
        An exception drops the row and is re-raised at the query boundary.
        The base raises `NotImplementedError`."""
        ...

#: A built-in relation or a user `JoinPredicate` instance.
ConstraintLike = Union[JoinConstraint, JoinPredicate]

def dominates(a: Capture, b: Capture) -> JoinConstraint:
    """`a` dominates `b`: every control-flow path from entry to `b` passes
    through `a`. Each is a captured node or an `If` branch-edge capture
    (`capture_true` / `capture_false`).

    Dominance, not reachability: no single incoming edge dominates a merge
    `Phi`; use `phi_input_from_edge` for that. A capture with no control
    position (a `load`, `store` or arithmetic node) drops the tuple, under
    `negate` too."""
def phi_input_from_edge(
    phi: Capture, edge: Capture, value: Capture
) -> JoinConstraint:
    """The value merged into `phi` (a `Phi` or `MemPhi`) from the branch
    `edge` is `value`. `edge` binds an `If`'s `capture_true` /
    `capture_false`; `value` is bound by another pattern in the same
    `find_all` list. The edge need not be the direct predecessor."""

def negate(c: ConstraintLike) -> JoinConstraint:
    """The negation of `c`: a match survives only if `c` does not hold. A
    tuple `c` cannot answer for (an unbound capture, a capture with no
    dominator-tree position) drops here too."""

def any_of(constraints: Sequence[ConstraintLike]) -> JoinConstraint:
    """A constraint that passes when any listed constraint passes. An empty
    list passes nothing."""

def all_of(constraints: Sequence[ConstraintLike]) -> JoinConstraint:
    """A constraint that passes only when every listed constraint passes. An
    empty list passes everything."""
