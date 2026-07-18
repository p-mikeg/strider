"""Type stubs for strider.pattern.constraints — join CONSTRAINTS.

A *pattern* (`strider.pattern`) describes graph SHAPE and is passed as
`find_all(pats, ...)`.  A *constraint* is a relational predicate over the
captures those patterns bind, evaluated after the join and passed as
`find_all(..., constraints=[...])`.  They live in separate namespaces so the
two kinds cannot be mistaken for one another.
"""

from __future__ import annotations

from typing import List

from . import Capture

class JoinConstraint:
    """A CFG relation between captured entities. Construct via
    `dominates` / `dominated_by_branch` / `phi_input_from_edge`, negate with
    `negate`; pass to `Function.find_all([...], constraints=[...])`."""

def dominates(a: Capture, b: Capture) -> JoinConstraint:
    """`a` dominates `b` in the control subgraph. Pass to
    `Function.find_all([...], constraints=[...])`."""
def dominated_by_branch(branch: Capture, node: Capture) -> JoinConstraint:
    """`node` is dominated by the target of branch edge `branch` (an `If`'s
    `capture_true`/`capture_false` value) — in that block exclusively, so
    `dominated_by_branch(true_edge, c)` means "`c` is in the true block"."""
def phi_input_from_edge(
    phi: Capture, edge: Capture, value: Capture
) -> JoinConstraint:
    """`phi`'s data input on the predecessor fed by control edge `edge` is
    `value` — "the value merged from THIS branch is X". `edge` binds an `If`'s
    `capture_true`/`capture_false` value.

    An arm qualifies when its predecessor IS the edge, or is reached exclusively
    through it — so a merge across a `call` or any other intervening block still
    pins. Exclusive: an arm reachable from both sides of the branch belongs to
    neither edge. A branch whose block splits and reaches the merge twice yields
    one match PER qualifying arm.

    An empty result is AMBIGUOUS: either `edge` reaches no arm of `phi`, or it
    does and the arm merges a different value. Re-probe with `anything()` as the
    bound value to tell them apart — a wildcard cannot fail on value grounds, so
    an empty result from it proves the edge is not visible:

        v = Capture()
        probe = p.phi().any_input(p.anything().capture(v)).capture(ph)
        if not fn.find_all([g, probe], constraints=[
                p.phi_input_from_edge(ph, e, v)]):
            ...  # the edge does not reach this phi at all — not a mismatch

    `value` is a `Capture`, bound by another pattern in the same `find_all` list
    and compared by identity. Bind it on the PHI PATTERN with `.any_input(...)`
    rather than as an independent root: `any_input` is anchored at the phi's own
    inputs, so it costs O(arity) and never ranges over the whole function, and it
    still enumerates one match per qualifying arm:

        v, ph = Capture(), Capture()
        fn.find_all([guard, p.phi().any_input(p.int_const(1).capture(v)).capture(ph)],
                    constraints=[p.phi_input_from_edge(ph, t, v)])"""

def negate(c: JoinConstraint) -> JoinConstraint:
    """The negation of `c`: a tuple survives iff `c` does NOT hold.

    RANGE RESTRICTION: every capture `c` mentions must be bound by a positive
    pattern in the same `find_all` list.  An unbound capture makes `c` fail for
    want of a binding, which under negation would flip to a vacuous "true" and
    match everything; `find_all` raises `StriderError` instead of matching
    blindly.  This holds for EVERY constraint, not just negated ones — an
    unbound capture in a positive constraint could never be satisfied and would
    silently return `[]`.

    Every constraint is a pure filter, so every constraint is negatable.
    `negate(negate(c))` is allowed and is the identity."""

def any_of(constraints: List[JoinConstraint]) -> JoinConstraint:
    """Disjunction: a tuple survives iff it passes ANY listed constraint. An
    empty list passes nothing.

    The top-level `constraints=[...]` list is already an implicit AND, so `any_of`
    is how you reach a disjunction — `any_of([rel_a, rel_b])`, "either holds".
    Evaluation is three-valued (Kleene): unknown (drops the row) when no listed
    constraint is true and at least one references a capture unbound in that
    row."""

def all_of(constraints: List[JoinConstraint]) -> JoinConstraint:
    """Conjunction: a tuple survives iff it passes EVERY listed constraint. An
    empty list passes everything. Use it to AND constraints INSIDE an `any_of`
    (the flat top-level list cannot nest). Evaluation is three-valued (Kleene):
    unknown (drops the row) when no listed constraint is false and at least one
    references a capture unbound in that row."""
