# R3-extension — real generalizations

## Why this round exists

The previous R3 round was a wrapper-removal pass — useful, but
shallow.  The user pointed out two parallel implementations
`crates/opt/src/indirect_branch_resolve/jump_table.rs`
maintains alongside crates that already do the same work:

> "the walk back logic already exists in pattern for example, and
> the bit knowledge exists in known bit — so why redo it in jump
> table?"

This round addresses both, then sweeps the surrounding area for
other parallel-abstraction duplication.

## Consolidations landed

### 1. KnownBits read-only analyzer extracted; jump_table reuses it

Files: `crates/opt/src/known_bits/mod.rs`,
`crates/opt/src/indirect_branch_resolve/jump_table.rs`,
`crates/opt/src/indirect_branch_resolve/stack_array.rs`,
`crates/opt/src/lib.rs`,
`crates/opt/src/indirect_branch_resolve/jump_table_tests.rs`.
Commit: `d5a6bf3`.

**Before.**  `KnownBits::optimize_built` ran a private fixed-point
worklist over `node_known_bits` to populate a private
`FxHashMap<NodeOutputId, Kb>`, then performed a graph-rewriting
constant-replacement step.  Both `Kb` and `node_known_bits`
were private to the `known_bits` module.  The jump-table
classifier — needing only the analysis, not the rewrite — re-
implemented a stripped-down recurrence as `compute_max_mask`,
covering `IntConst`, `And`, `Truncate`, `ZeroExtend`, and
`ShiftRight`.  Each branch duplicated the corresponding rule
already in `node_known_bits`, plus a hand-rolled
`HashSet<NodeOutputId>` for cycle protection.

**After.**

* `Kb` and `node_known_bits` are now `pub`.
* New `pub fn opt::analyze_known_bits(&BuiltFunctionGraph) ->
  Result<FxHashMap<NodeOutputId, Kb>>` — the worklist phase
  factored out as a non-mutating analyzer.
* New `Kb::max_value(type_mask) -> u64` — the
  `(!zeros) & type_mask` upper-bound conversion the
  jump-table classifier needed.
* `KnownBits::optimize_built` calls `analyze_known_bits` then
  performs the rewrite step on top.
* `bound_via_known_bits` calls `analyze_known_bits` and reads
  the bound from the resulting `Kb` for `idx_output`.
* `compute_max_mask` and its `HashSet<NodeOutputId>`
  cycle-protection deleted (the worklist's `WorkSet` already
  handles cycles).

LOC delta: known_bits +56 −16; jump_table −96 (compute_max_mask
gone, simpler `bound_via_known_bits`); net **−56 LOC** with
the analyzer now reusable across the workspace.

**Behavior-equivalence evidence.**

* All 2860 workspace tests pass; all 26 ignored pre-existing.
* The fixed-point analyzer covers every node kind the local
  recurrence covered (`IntConst`, `And`, `Truncate`,
  `ZeroExtend`, `ShiftRight`) and several more (`Or`, `Xor`,
  `Not`, `Popcount`, `Lzcount`, `ShiftLeft`).  Any bound the
  previous code returned is still proved.
* The conversion `bound = max_value(type_mask) + 1` is
  algebraically equivalent to `compute_max_mask`'s
  return + 1: for each kind the local recurrence handled,
  `(!kb.zeros) & type_mask` reduces to the same number the
  recurrence returned.  Verified by hand for `IntConst`, `And`,
  `ZeroExtend`, and `ShiftRight`.
* One existing characterization test
  (`bound_via_known_bits_handles_zero_extend`) constructed an
  Extend node *post-`build()`* that wasn't routed through the
  Return — i.e. unreachable from `entry`.  The new
  `analyze_known_bits` scopes its worklist to `preorder()`
  (entry-reachable, matching the validator's Layer-A scope), so
  the test now rewires the Return through the Extend before
  asserting the bound.  The fix is a test artefact; production
  callers always pass `idx_output` from a reachable shape.

Confidence: **high**.  Tests + clippy clean, semantics traced
end-to-end.

### 2. `bound_from_if_condition` uses `pattern::int_cmp_any`

Files: `crates/opt/src/indirect_branch_resolve/jump_table.rs`,
`crates/opt/src/indirect_branch_resolve/jump_table_tests.rs`,
`crates/opt/src/indirect_branch_resolve/stack_array.rs`.
Commit: `1e90e9c`.

**Before.**  Hand-rolled three steps:

```rust
let NodeKind::IntCmpOp(op) = *graph.node_kind(cmp_node) else { return None };
let [lhs, rhs] = graph.node_inputs_exact::<2>(cmp_node).ok()?;
let (idx_side, const_side, swapped) = if same_value(graph, lhs, idx_output) {
    (lhs, rhs, false)
} else if same_value(graph, rhs, idx_output) {
    (rhs, lhs, true)
} else { return None };
let n = graph.int_const_val(const_side)?;
match op {
    IntCmpOp::Less | IntCmpOp::Sless if !swapped => Some(n),
    IntCmpOp::LessEqual | IntCmpOp::SlessEqual if !swapped => n.checked_add(1),
    _ => None,
}
```

**After.**

```rust
let pat = int_cmp_any(op_var, var(idx_var), any_int_const(n_var));
let m = Matcher::new(fg).match_at(cmp_node, &pat)?;
let lhs = m.get(idx_var)?;
if !same_value(graph, lhs, idx_output) { return None; }
let n = u64::try_from(m.get_int(n_var)?).ok()?;
let op = m.get_int_cmp_op(op_var)?;
match op {
    IntCmpOp::Less | IntCmpOp::Sless => Some(n),
    IntCmpOp::LessEqual | IntCmpOp::SlessEqual => n.checked_add(1),
    _ => None,
}
```

The kind-match, arity-check, and operand-disambiguation collapse
into a single pattern.  `int_cmp_any` honours each op's
commutativity automatically: non-commutative ops (`Less`,
`LessEqual`, `Sless`, `SlessEqual`) only bind when `idx` is on
the LHS — exactly the orientation the previous `if !swapped`
filter selected for.  Asymmetric-op-with-`swapped` always
returned `None` in the old code, and the pattern simply fails
to match in that case, so behaviour is preserved.

`same_value` stays — it walks through trivial single-input phis
(intermediate orchestrator iterations leave them in place when
RedundantPhis hasn't run yet), and that's not expressible as a
tree pattern.

LOC delta: jump_table −9 (cmp shape); +call-site updates.
Net **−6 LOC** with cleaner intent.

**Behavior-equivalence evidence.**

* All 2860 tests pass; six existing
  `bound_from_if_condition_*` and `bound_via_predecessor_if_*`
  characterization tests cover the asymmetric-op true/false
  branches, the unrelated-idx case, and the multi-region
  `If(idx<N) → dispatch` walk.  All green post-refactor.
* `int_cmp_any` for Equal / Carry / Scarry would match in both
  orderings; the catch-all `_ => None` arm handles those
  identically to the old code.

Confidence: **high**.

## Consolidations attempted but reverted

None — both attempted consolidations landed.  Two earlier mid-
work missteps were corrected before commit:

* The first `analyze_known_bits` draft seeded the worklist with
  `function.all_node_ids()` (to include detached nodes the test
  relied on); that triggered `node_inputs_exact` errors on
  detached zombies left behind by `RedundantPhis`.  Fixed by
  reverting to `preorder()` and updating the artefactual test
  to route its Extend through the Return.

## Found-but-not-consolidated

### `graph.get_node_from_output(out); graph.node_kind(node);` two-step

103 call sites across `opt`, `pattern`, and `ir`.  An
`ir::graph::access::kind_of_output(out) -> &NodeKind` helper
would tighten every one of them.  Not done in this round
because the touch-count is too large for a single-commit
behaviour-preserving refactor and the win per site is small;
warrants its own pass.

### `eval_int_binary` cross-crate reuse

`crates/opt/src/constant_fold/eval_int.rs::eval_int_binary` is
the canonical "evaluate `IntBinaryOp` on two constants"
function.  Searched `pattern::guards`, `jump_table`,
`stack_array`, `known_bits` for reimplementations of constant
arithmetic; no duplicates found.  The arithmetic in
`stack_array` and `jump_table` (`checked_add`, `checked_mul`)
is scalar address-arithmetic, not constant evaluation of IR
nodes — different scope.

### Predecessor / control-flow walks

`walk_control_for_if_bound` is the only backward-control walker
in the workspace.  `redundant_phis` and `dead_branch` operate
forward via the standard `preorder()` traversal; no shared
backward-walk helper would match more than one site.

### Manual `Matcher::new(fg).match_at(...)` boilerplate

Four sites in
`crates/opt/src/indirect_branch_resolve/{jump_table,stack_array}.rs`.
Below the threshold where factoring helps; each site uses a
distinct pattern shape.

## Honest scope notes

* **Backward dominator-walk in `bound_via_predecessor_if`**:
  the user's brief mentioned that "walk back logic already
  exists in pattern".  This is *partially* true.  Pattern's
  `try_walk_through_control_state` (in
  `crates/pattern/src/matcher/walk_through.rs`) is single-hop:
  it lets a value-pattern transparently see through one
  `ControlState` join.  Jump-table's walk is **transitive
  backward control flow** — through arbitrary chains of
  `If` / `ControlState` / other Control producers, with cycle
  detection and per-predecessor visited-set cloning at
  `ControlState` joins.  No equivalent exists in pattern; the
  walk's recursion shape can't be expressed in the matcher's
  forward-rooted tree-pattern model.  The walk stays in
  `jump_table.rs` as-is.
* **`same_value` is single-purpose, not duplicated**: the
  trivial-phi-chasing helper is unique to this file.  It walks
  *output identity* through single-input phis bidirectionally
  — also not expressible as a tree pattern (patterns capture
  one side; verifying equality with a runtime `NodeOutputId`
  would require a `.when(...)` predicate that itself walks
  phis).  Left in place.

## Open questions for R4 / R5

* Is the `kind_of_output(out) -> &NodeKind` accessor worth
  adding?  Affects ~100 sites; small per-site win but
  consistent reduction in noise.  Risk: very low.  R5
  candidate.
* Should `same_value` move to a shared utility (`ir::graph` or
  a new `opt::value_identity` module)?  Currently it's only
  used in `jump_table.rs`, but other passes (e.g. future
  alias-analysis or load-forwarding extensions) might need the
  same trivial-phi-chasing primitive.  Defer until a second
  caller appears.
