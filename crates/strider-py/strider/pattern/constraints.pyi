"""Type stubs for strider.pattern.constraints — join CONSTRAINTS.

A *pattern* (`strider.pattern`) describes graph SHAPE and is passed as
`find_all(pats, ...)`.  A *constraint* is a relational predicate over the
captures those patterns bind, evaluated after the join and passed as
`find_all(..., constraints=[...])`.  They live in separate namespaces so the
two kinds cannot be mistaken for one another.
"""

from __future__ import annotations

from . import Capture, Pat

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
    phi: Capture, edge: Capture, value: Capture | Pat
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
    value to tell them apart — a wildcard cannot fail on value grounds, so an
    empty result from it proves the edge is not visible:

        if not fn.find_all([g, ph_p], constraints=[
                p.phi_input_from_edge(ph, e, p.anything())]):
            ...  # the edge does not reach this phi at all — not a mismatch

    `value` is either a `Capture` (bound by another pattern in the same
    `find_all` list; compared by identity) or a **pattern** matched inline at the
    arm value. The inline form states the fact locally — no independent root
    ranging over the whole function and no cartesian product against it — and it
    binds: captures inside it read back off the match, unifying with (never
    overwriting) whatever the rest of the join already bound."""

def negate(c: JoinConstraint) -> JoinConstraint:
    """The negation of `c`: a tuple survives iff `c` does NOT hold.

    RANGE RESTRICTION: every capture `c` mentions must be bound by a positive
    pattern in the same `find_all` list.  An unbound capture makes `c` fail for
    want of a binding, which under negation would flip to a vacuous "true" and
    match everything; `find_all` raises `StriderError` instead of matching
    blindly.

    `negate` of a `phi_input_from_edge` with an inline value *pattern* is
    rejected — that form binds captures rather than deciding a predicate, so
    there is nothing to bind on the false branch; use a `Capture` value instead.
    `negate(negate(c))` is allowed and is the identity."""
