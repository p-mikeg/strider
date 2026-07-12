# find_joined relational constraints (dominance + control reachability) — design

## Goal

Let `find_joined` correlate matches not only by shared-capture *equality* but by
CFG *relations* between captured entities: control dominance and forward control
reachability. This makes control-flow-sensitive queries expressible — the
motivating one being "this call happens on the true branch of `if(f_op->member)`,
not the false branch and not after the merge."

## Why (the motivating blocker)

Kernel dispatch like `file->f_op->read(...)` lowers to a call guarded by
`if(f_op->member)`. To assert the call is gated by (and on the true side of)
that guard, we need to relate two independently-located matches — the `If` and
the `Call` — by their CFG positions. Today the DSL can only descend a specific
edge (`if.true_branch(p)` matches the *immediate* consumer of the true edge),
which misses anything deeper in the branch and can't express "somewhere under
the true edge."

## Key design decisions (settled in discussion)

1. **CFG-only.** Constraints range over control-anchored nodes
   (`Entry/Region/If/Call/CallOther/Return/IndirectBranch`) — the exact set
   `ControlFlowView` presents. Floating pure values have no CFG position and are
   out of scope; a captured value is resolved to its producer node for
   dominance.

2. **Discriminate true vs false via the `If`'s control-output *value*, not the
   successor region.** A single-input successor region is collapsed by
   `RedundantPhis`/`RegionCollapse`, so it is not a stable anchor. The `If`'s two
   control outputs always exist (the `If` is a real branch). Reachability keyed
   off the true-output value isolates the true path and survives region collapse.

3. **True-exclusive falls out of reach + not-reach, no dominance needed.**
   Post-merge code is reachable from *both* control outputs, so
   `reaches(true_out, c) ∧ ¬reaches(false_out, c)` = "on the true path,
   exclusively." `dominates` is a separate, general primitive (strict "A gates
   B" ordering), not required for the true/false split.

4. **Relation, not descent → it lives on `find_joined`.** These are correlations
   between two independently-located matches filtered by a predicate — the
   `find_joined` execution model — not nested operand sub-patterns.

## Surface

### New builder primitive: capture the `If`'s control outputs

`IfPat` already builds both control-output vertices (`true_out` slot 0,
`false_out` slot 1) but exposes neither as a propagated capture (its
`with_true`/`with_false` inner captures are deliberately isolated). Add:

- Rust: `IfPat::capture_true(c: Capture)` / `capture_false(c: Capture)` — put a
  capture on the slot-0 / slot-1 control-output vertex.
- Python: `if_else(...).capture_true(c)` / `.capture_false(c)`.

The captured binding is `Binding::Value(<control-output ValueId>)` — a stable,
collapse-independent handle to the branch edge.

### Matcher: bind non-anchor output-vertex captures

The matcher currently binds only the anchor output capture, the node capture,
and input-producer captures — it never visits a node's *secondary* outputs. Add:
after a pat node matches its IR node, for every produced output vertex that
carries a capture and is not the anchor, bind that capture to the IR node's
output value at the vertex's slot (`node_outputs(ir_node)[slot]`). This is what
makes `capture_true`/`capture_false` bind.

### New: join constraints

```rust
pub enum JoinConstraint {
    /// node_of(a) dominates node_of(b) in the control subgraph.
    Dominates { a: Capture, b: Capture },
    /// node_of(to) is forward-control-reachable from the branch edge bound to
    /// `from` (a control-output value): start at the value's consumer, BFS
    /// `cfg_succs`.
    Reaches { from: Capture, to: Capture },
    /// The negation of `Reaches`.
    NotReaches { from: Capture, to: Capture },
}
```

- Rust: `Matcher::find_joined_constrained(&[&Pattern], &[JoinConstraint])`.
  `find_joined` stays as-is (delegates with an empty constraint slice).
- Python free constructors in `strider.pattern`: `dominates(a, b)`,
  `reaches(from, to)`, `not_reaches(from, to)`; `Function.find_joined(pats,
  constraints=[...])` grows an optional keyword.

### Constraint semantics

Applied as a **post-correlation filter** over the joined tuples: a tuple
survives iff every constraint holds on its bound captures.

- `Dominates { a, b }`: resolve both captures to nodes via `Binding::node_of`.
  If either is absent from the control subgraph, the constraint is **false**
  (not an error). Uses `control_dominators` (computed once per `find_joined`
  call, memoized) + `dominates`.
- `Reaches { from, to }`: `from` must be bound to a `Value` (a control-output
  value); resolve its single consumer node `C`; BFS `cfg_succs` from `C`
  (inclusive); true iff `node_of(to)` is in the reachable set. If `from` is not
  a value, or `to` resolves to no control node, the constraint is **false**.
  The reachable set per `from`-value is memoized within a `find_joined` call.
- `NotReaches { from, to }`: logical negation of `Reaches` (so a `from` that
  isn't a control value makes `Reaches` false ⇒ `NotReaches` true; document
  this — an ill-typed `from` makes `not_reaches` vacuously true).

## Worked example (the motivating query)

```python
from strider import pattern as p

fop, t, f, c = p.Capture(), p.Capture(), p.Capture(), p.Capture()

guard = p.if_else(
    cond=p.int_ne(p.load(p.add(p.var(fop), p.any_int_const())), p.int_const(0))
).capture_true(t).capture_false(f)

call = p.call().capture(c)  # or constrained further by target

hits = fn.find_joined([guard, call], constraints=[
    p.reaches(t, c),
    p.not_reaches(f, c),
])
```

Region-independent; survives the single-input-region collapse.

## Testing

- Rust matcher tests (mock diamond via `FunctionBuilder`): a call in the true
  arm, one in the false arm, one after the merge. Assert `reaches(t,·)` selects
  {true, merge}, `not_reaches(f,·)` removes {false, merge}, the pair selects
  {true} only; `dominates(if, ·)` selects {true, false, merge}; `dominates` on
  the true-edge's first control node selects the true arm.
- `capture_true`/`capture_false` bind a `Value`, and the binding survives
  running the default pipeline (region collapse) on the diamond.
- Python end-to-end: lift a real `x86_64` diamond, run the worked-example query,
  assert the true-arm call is selected and the false-arm call is not.
- Constraint edge cases: ill-typed `from` (a node capture) ⇒ `reaches` false /
  `not_reaches` true; a capture absent from the control subgraph ⇒ `dominates`
  false.

## Out of scope

- Floating-value "domination" (needs an invented region-of-a-value).
- Backward data-cone membership (`in_cone`) — the other half of the CFI-spill
  match; a separate future constraint, deliberately not bundled here.
- Scoping the sub-pattern search to the dominated cone (the "global-match then
  filter" model is what we build; up-front scoping is not).
