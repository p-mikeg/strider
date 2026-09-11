"""Type stubs for strider.pattern.constraints: relational join constraints.

A pattern (`strider.pattern`) describes graph SHAPE and is passed as
`find_all(pats, ...)`. A constraint is a relational predicate over the
captures those patterns bind, evaluated after the join and passed as
`find_all(..., constraints=[...])`. They live in separate namespaces so the
two kinds cannot be mistaken for one another.
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
    """Base class for a user-defined join constraint. Subclass it, override
    `constraint(m)` to decide whether a joined match survives, and optionally
    override `captures()` to declare the captures it correlates. Pass an
    instance anywhere a `JoinConstraint` is accepted.

    Declaring captures makes the predicate a connector: it correlates the
    named captures across patterns and is range-checked like a built-in
    constraint, so it can join otherwise-independent patterns. The default
    `captures()` returns `[]`, making the predicate a pure filter."""

    def __init__(self, *args, **kwargs) -> None:
        """Base initialiser; subclasses may take whatever arguments they like."""
        ...
    def captures(self) -> list[Capture]:
        """The captures this predicate correlates. Default: none."""
        ...
    def constraint(self, m: Match) -> bool:
        """Override to return whether the joined match `m` survives. `m` spans
        the whole join, so `m.node(c)` / `m.uint(c)` see every pattern's
        captures. The base raises `NotImplementedError`."""
        ...

#: A built-in relation or a user `JoinPredicate` instance.
ConstraintLike = Union[JoinConstraint, JoinPredicate]

def dominates(a: Capture, b: Capture) -> JoinConstraint:
    """`a` dominates `b`: every control-flow path from entry to `b` passes
    through `a`. Both are captured nodes (any capture) or an `If` branch-edge
    capture (`capture_true` / `capture_false`); the meaningful pairs are
    node->node (`b` downstream of `a`), edge->node (`b` in the block that edge
    leads into), and edge->edge (nested branches).

    This is dominance, not "reachable from": a merge or loop-header `Phi` is
    reached from several predecessors, so no single incoming edge dominates it
    (`dominates(false_edge, phi)` is false and drops the tuple). Use
    `phi_input_from_edge` to say "the value `phi` merges from that edge".

    Only a capture carrying a control edge can be placed. A `load`, `store` or
    arithmetic capture has no position in the dominator tree, so the constraint
    drops the tuple and so does `negate(dominates(...))`."""
def phi_input_from_edge(
    phi: Capture, edge: Capture, value: Capture
) -> JoinConstraint:
    """The value merged into `phi` from the branch `edge` is `value`. `edge`
    binds an `If`'s `capture_true` / `capture_false` value, and `value` is
    bound by another pattern in the same `find_all` list."""

def negate(c: ConstraintLike) -> JoinConstraint:
    """The negation of `c`: a match survives only if `c` does not hold."""

def any_of(constraints: Sequence[ConstraintLike]) -> JoinConstraint:
    """A constraint that passes when ANY of the listed constraints passes.

    The top-level `constraints=[...]` list is already an AND, so `any_of` is
    how you express OR. An empty list passes nothing.
    """

def all_of(constraints: Sequence[ConstraintLike]) -> JoinConstraint:
    """A constraint that passes only when EVERY listed constraint passes.

    Use it to AND constraints inside an `any_of` (the top-level list does not
    nest). An empty list passes everything.
    """
