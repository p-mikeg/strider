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
    """`a` dominates `b` in the control subgraph.

    Each operand is a NODE or an EDGE, decided by what it captured: an
    `If`'s `capture_true` / `capture_false` value is a control edge, any
    other capture is a node. So one relation covers node-to-node (plain
    dominance), edge-to-node (`dominates(true_edge, c)`, meaning "`c` is in
    the true block", exclusively), and edge-to-edge
    (`dominates(outer_edge, inner_edge)`, the outer branch edge dominating
    the inner one).
    """
def phi_input_from_edge(
    phi: Capture, edge: Capture, value: Capture
) -> JoinConstraint:
    """`phi`'s data input on the predecessor fed by control edge `edge` is
    `value`: "the value merged from THIS branch is X". `edge` binds an
    `If`'s `capture_true` / `capture_false` value.

    An arm qualifies when its predecessor IS the edge, or is reached
    exclusively through it, so a merge across a call or any other
    intervening block still pins. It is exclusive: an arm reachable from
    both sides of the branch belongs to neither edge. A branch whose block
    splits and reaches the merge twice yields one match per qualifying arm.

    An empty result is AMBIGUOUS: either `edge` reaches no arm of `phi`, or
    it does and that arm merges a different value. Re-probe with `anything()`
    as the bound value to tell them apart. A wildcard cannot fail on value
    grounds, so an empty result from it proves the edge is not visible:

        v = Capture()
        probe = p.phi().any_input(p.anything().capture(v)).capture(ph)
        if not fn.find_all([g, probe], constraints=[
                p.phi_input_from_edge(ph, e, v)]):
            ...  # the edge does not reach this phi at all, not a mismatch

    `value` is a `Capture`, bound by another pattern in the same `find_all`
    list and compared by identity. Bind it ON THE PHI PATTERN with
    `.any_input(...)` rather than as an independent root: `any_input` is
    anchored at the phi's own inputs, so it costs one step per arm instead
    of ranging over the whole function, and it still enumerates one match
    per qualifying arm:

        v, ph = Capture(), Capture()
        fn.find_all([guard, p.phi().any_input(p.int_const(1).capture(v)).capture(ph)],
                    constraints=[p.phi_input_from_edge(ph, t, v)])
    """

def negate(c: JoinConstraint) -> JoinConstraint:
    """The negation of `c`: a result survives only if `c` does NOT hold.

    Every capture `c` mentions must be bound by a positive pattern in the
    same `find_all` list. An unbound capture makes `c` fail for want of a
    binding, which under negation would flip to a vacuous "true" and match
    everything, so `find_all` raises `StriderError` instead. This holds for
    every constraint, not just negated ones: an unbound capture in a
    positive constraint could never be satisfied and would silently return
    `[]`.

    Every constraint is a pure filter, so every constraint is negatable, and
    `negate(negate(c))` is allowed and is the identity.
    """

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
