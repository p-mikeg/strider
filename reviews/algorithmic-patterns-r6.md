# Algorithmic-pattern analysis — round 6 (2026-04-30)

## Goal

Identify code that is algorithmically the same across the workspace but
expressed differently, and judge whether unification would actually pay
off. Surface duplication is out of scope (handled in
`reviews/cross-crate-r6.md`); per-crate findings already addressed by
the r6 reviews are not re-flagged.

## Summary

- Patterns surveyed: **9**
- Recommended for unification ("yes — clear win"): **3** (P-001, P-002, P-005)
- Worth more thought ("maybe"): **3** (P-003, P-006, P-009)
- Looks-similar-but-isn't ("no"): **3** (P-004, P-007, P-008)

## Headline recommendations

1. **`replace_output_uses` / inlined cursor loops should call `Graph::replace_all_uses`** (P-001, 2 active sites — `redundant_phis` and `call_other_elide`). The local copy was deleted in commit `6255d10` but was silently re-introduced by the anyhow-conversion commit `ebec8eb`; the second site (`call_other_elide`) inlined the same loop. F-013 is not actually applied in current code.
2. **The 3-way memory-chain walker (`mem_chain_is_dirty`, `probe`, `find_stack_stored_value_at_offset`) should share a generic backbone** (P-002, the still-deferred F-014). Each shape walks `StackStore` / `Store` / `MemPhi` with identical disjointness checks but different reductions; the per-site decision callback varies, the traversal does not.
3. **`preorder().filter(matches!(node_kind == X))` is the most common pattern in the codebase, ~15 sites.** Add `Graph::preorder_kind<P>(P)` / `BuiltFunctionGraph::preorder_kind` so the half-dozen kind-indexed scans in production passes (`call_other_elide`, `function_args`, `stack_store::call_args`, `redundant_phis`) and the seven `g.preorder().filter(...).count()` test helpers all collapse to one line (P-005).

The remaining six surveyed patterns either have meaningful per-site
divergence (P-003, P-006, P-009) or look like duplication but reflect
genuinely different problems (P-004, P-007, P-008). See the per-pattern
analyses for the trade-offs.

## Patterns

### P-001 Manual `output_use_cursor` loops re-implementing `replace_all_uses`

**Sites:**
- `crates/opt/src/redundant_phis/mod.rs:11-23` — `replace_output_uses` (called 3× in the same module).
- `crates/opt/src/call_other_elide/mod.rs:131-142` — two back-to-back inlined cursor loops on `ctrl_out` and `mem_out`.

**Pattern shape:** "Walk the use-list of `output` and rewrite every
consumer to `new_value`":

```rust
let mut cursor = graph.output_use_cursor(output);
while cursor.current().is_some() {
    cursor.replace_current_with(new_value)?;
}
```

That is exactly the body of `Graph::replace_all_uses`
(`crates/ir/src/ops/rewrite.rs:7-32`).

**Variations:**
- `redundant_phis::replace_output_uses` returns `bool` (whether any use
  was replaced); `Graph::replace_all_uses` already returns `bool` with
  the same semantics.
- `call_other_elide` inlines the loop twice and tracks `changed` itself
  with a local bool. Two sequential `replace_all_uses` calls would do
  the same with two boolean ORs.

**Honest assessment:** **Yes — clear win.**

This is not a judgment call. F-013 (opt-crate review) explicitly noted
this duplication in `redundant_phis`, the implementation pass *applied*
the fix in commit `6255d10`, and the anyhow-conversion commit `ebec8eb`
silently *reverted* it (likely a merge resolution artefact: the anyhow
diff was authored against a parent that pre-dated F-013). The "Applied"
entry in the opt-crate r6 outcomes table is therefore inaccurate as of
this branch. The `call_other_elide` site is a separate finding the opt
review didn't flag.

**Proposed fix:** Two trivial edits:
- `redundant_phis/mod.rs`: drop the local `replace_output_uses` (lines
  9-23) and replace its 3 call-sites with `function.replace_all_uses(...)`.
- `call_other_elide/mod.rs:131-142`: replace the two cursor loops with
  two `replace_all_uses` calls; recover `changed` by ORing their return
  values.

Both edits leave the per-site control flow unchanged; they just stop
duplicating a 5-line primitive that already lives in `ir`.

**Confidence:** high.

---

### P-002 Recursive memory-chain walks (`mem_chain_is_dirty` / `probe` / `find_stack_stored_value_at_offset`)

**Sites:**
- `crates/opt/src/function_args/mod.rs:387-451` — `mem_chain_is_dirty`
  (returns `bool`, used by `FunctionArgDetect`).
- `crates/opt/src/stack_load_forward/mod.rs:163-241` — `probe`
  (returns `Option<ResolveShape>`, used by `StackLoadForward`).
- `crates/opt/src/stack_load_forward/mod.rs:402-463` —
  `find_stack_stored_value_at_offset` (returns `Option<NodeOutputId>`,
  used by tier-2 `stack_array` classifier and load-forward fallback).

**Pattern shape:** Each function walks the memory chain backward, doing
roughly the same per-node dispatch:

```rust
match graph.node_kind(node) {
    StackStore => /* range-disjoint? walk prev_mem : terminate */,
    Store(_)   => /* step_through_store: walk prev_mem : terminate */,
    MemPhi     => /* recurse on every value pred, combine */,
    InitialMemory | _ => terminate,
}
```

`step_through_store` and `step_through_stack_store{,_phi}` are *already*
deduped in `crates/opt/src/sp_expr.rs:108-189` (the F-015 outcome
applied), so the per-node alias logic isn't the duplication. What's
left is the *traversal scaffolding*: each walker carries a memo, a
visited-set cycle guard, and a recurse-on-prev-mem tail, with
slightly different per-site combiners.

**Variations:**
- Result type: `bool` (any-pred dirty) vs `Option<ResolveShape>` (all
  preds must yield) vs `Option<NodeOutputId>` (first matching store at
  offset).
- MemPhi handling: `mem_chain_is_dirty` recurses into all preds and
  ORs; `probe` recurses into all preds and packages the per-pred
  shapes into `ResolveShape::Phi`; `find_stack_stored_value_at_offset`
  bails on MemPhi entirely (documented as "future work").
- Call/CallOther handling: `mem_chain_is_dirty` walks through (a call
  doesn't dirty the stack-arg area — the post-call memory is what we
  care about); `probe` and `find_stack_stored_value_at_offset` don't
  match Call at all (so terminate via the catch-all).
- Memo shape: keyed on `(NodeOutputId, i64, i64)` for two of them,
  `(NodeOutputId, i64, NodeOutputType)` for the third.

**Honest assessment:** **Yes — clear win, but non-trivial.**

The opt-crate r6 review flagged this as F-014 and explicitly deferred
it ("multi-file refactor with subtle per-site bail semantics; size
warrants its own PR"). The deferral is honest, but the duplication is
real and the proposed shape — a generic walker with a per-site visitor
trait — is genuine simplification, not refactoring for its own sake.

The minimal viable abstraction:

```rust
// In sp_expr.rs (already houses step_through_*)
trait MemChainVisitor {
    type Output;
    fn on_stack_store(...) -> Step<Self::Output>;
    fn on_store(...)       -> Step<Self::Output>;
    fn on_mem_phi(...)     -> Step<Self::Output>;
    fn on_initial(...)     -> Self::Output;
    fn on_other(...)       -> Self::Output; // catch-all bail
}

enum Step<O> { Walk(NodeOutputId), Done(O) }

fn walk_mem_chain<V: MemChainVisitor>(
    graph: &Graph, mem: NodeOutputId, visitor: &mut V,
) -> V::Output { ... }
```

The three callers each define a tiny visitor type; the recursive
backbone moves to one place. Memos remain caller-owned.

The reason this is "clear win, non-trivial" rather than "yes, easy" is
testing: each walker has its own bug-history (BUG-19, BUG-21, BUG-28,
BUG-30 codenames stripped from comments per F-037), so the merge
needs systematic testing to confirm none of those edge cases regresses.
That's a refactor PR, not a doc-comment fix.

**Confidence:** high (in the duplication); medium (in the abstraction's
ergonomics — the `MemChainVisitor` trait pulls a chunk of the
caller's local state into associated data).

---

### P-003 Cycle-guarded recursive walks (`decompose_sp` family, `walk_control_for_if_bound`)

**Sites:**
- `crates/opt/src/sp_expr.rs:208-322` — `decompose_sp` /
  `decompose_sp_inner` / `decompose_sp_phi` (3 functions threading
  `visiting: &mut FxHashSet<NodeId>`).
- `crates/opt/src/indirect_branch_resolve/jump_table.rs:337-423` —
  `walk_control_for_if_bound` (threads
  `visited: &mut HashSet<NodeId>` + `trail: &mut Vec<NodeId>`).
- `crates/opt/src/indirect_branch_resolve/stack_array.rs:343-382` —
  `flatten_add_tree` (threads `budget: &mut usize` instead of a
  visited set, but same shape).

**Pattern shape:** All three are recursive walks where a callee mutates
shared state on entry, recurses, and unwinds the mutation on the way
back:

```rust
fn walk(g, node, visiting: &mut HashSet<NodeId>) -> R {
    if !visiting.insert(node) { return bail; }
    let result = recurse(...);
    visiting.remove(&node);
    result
}
```

The `decompose_sp` family does this through three mutually-recursive
functions; `walk_control_for_if_bound` does it inline, with an
additional `trail: &mut Vec<NodeId>` for save-restore on each
predecessor (the F-016 deferred performance item).

**Variations:**
- `visiting` semantics: in `decompose_sp` it's a "current path" set
  (insert on entry, remove on exit, so a different call path can still
  resolve the same node); in `walk_control_for_if_bound` it's the
  same semantics but with an explicit `trail` to allow per-predecessor
  rollback at the ControlState join arm.
- `flatten_add_tree` doesn't need a visited set at all — it walks a
  data tree (Adds and Subs of constants) where cycles don't exist; it
  uses `budget` to defend against pathological lifter output.

**Honest assessment:** **Maybe — needs more thought.**

A `PathGuard` RAII helper would look clean:

```rust
struct PathGuard<'a> { set: &'a mut HashSet<NodeId>, node: NodeId }
impl Drop for PathGuard<'_> { fn drop(&mut self) { self.set.remove(&self.node); } }

fn enter(set: &mut HashSet<NodeId>, node: NodeId) -> Option<PathGuard<'_>> {
    if set.insert(node) { Some(PathGuard { set, node }) } else { None }
}
```

But:

1. **It can't help `walk_control_for_if_bound`'s ControlState arm**.
   That arm needs save-restore on each predecessor *across* a `for`
   loop body, not RAII at the function entry. The deferred F-016 fix
   restructures this DFS to use shared visited semantics — that
   restructure obviates the `PathGuard` helper for this site.
2. **It barely helps `decompose_sp`**. The three functions are tightly
   coupled (they call each other). The 4-line save-restore pattern is
   already isolated to one place per function; converting to RAII
   reduces that to 2 lines per function but the `&mut visiting`
   parameter still threads through every recursive call (it's also
   passed *into* the recursion to detect cycles, not just held during
   it).
3. **The error rate of bugs in this pattern is low.** Across 5 reviews
   of the 5 main crates, no finding flagged a missed `visiting.remove`.
   The Rust borrow-checker doesn't catch the bug, but the pattern is
   short enough that visual review does.

The decision tipper would be: if F-016 lands (the
`walk_control_for_if_bound` restructure to share visited state across
predecessors), the `decompose_sp` family ends up as the *only* site
needing the cycle-current-path semantics. At that point, the helper
becomes a 4-line file in `sp_expr.rs` for one consumer — not worth
extracting.

**Confidence:** medium.

---

### P-004 Two-phase walks ("probe then realize")

**Sites:**
- `crates/opt/src/stack_load_forward/mod.rs:163-241` (`probe`) +
  `:252-380` (`realize`) — read-only shape discovery, then materialise.
- `crates/strider/src/orchestrator.rs:357-399` (`classify_and_partition`)
  + `:401-415` (`apply_in_place_edits`) — read-only classification of
  every unresolved anchor, then apply edits.

**Pattern shape:** "Phase 1: walk the graph and collect a structural
description of what could change, returning a value type. Phase 2:
take that value and commit IR mutations, possibly creating new nodes."

`probe` returns `Option<ResolveShape>` (an enum of `Existing`,
`Narrow`, `Phi`); `realize` consumes it and emits `Truncate` /
`ShiftRight` / `ValuePhi` nodes.

`classify_and_partition` returns `(HashMap, Vec)` of resolved targets;
`apply_in_place_edits` consumes the `Vec` and rewires the IR.

**Variations:**
- `probe`/`realize` are pure functions over IR data: probe is a single
  recursive walk; realize is a single recursive consumption of the
  shape tree.
- `classify_and_partition`/`apply_in_place_edits` are methods on
  `LoopState` that read/write `self`-owned state (`unresolved`,
  `known_targets`, `region_index`, `graph`).

**Honest assessment:** **No — looks like duplication but isn't.**

The split-phase pattern is a *good practice* both sites independently
adopted, not a mechanical duplication. The shapes diverge sharply:

- `probe` walks one input chain to depth and produces a tree-like
  shape value; `realize` recursively consumes that tree. The shape
  type (`ResolveShape`) is intrinsic to the pass's algorithm.
- `classify_and_partition` produces a flat `Vec` of independent
  resolved anchors; `apply_in_place_edits` iterates that vec and
  applies an entirely separate `apply_link_register` /
  `apply_tail_call` to each. No tree consumption, no per-edit
  generic logic.

A "generic two-phase walker" abstraction would force both sites to
adopt a single intermediate shape type (`Vec` vs custom tree), giving
up local clarity for no shared code. The pattern is a *style* — "don't
mutate while walking" — which is already the convention in the
codebase (`apply_rule` in `strider::rewrite` does the same with its
pre-collected `candidates: Vec<NodeId>`). It's not a missing
abstraction.

**Confidence:** high.

---

### P-005 `preorder().filter(matches!(NodeKind == ...))`

**Sites (production):**
- `crates/opt/src/call_other_elide/mod.rs:62-63` — `CallOther { .. }`.
- `crates/opt/src/stack_store/call_args.rs:225-227` — `Call`.
- `crates/opt/src/redundant_phis/mod.rs:178-187` —
  `ControlPhi(_) | MemPhi | ControlState`.
- `crates/opt/src/function_args/mod.rs:144-147` — `InitialVar(_)`.
- `crates/opt/src/indirect_branch_resolve/mod.rs:411-413` — `Return`.
- `crates/opt/src/indirect_branch_resolve/inplace.rs:191-194` —
  `Return` (with uniqueness check).
- `crates/cfg/src/cfg/builder/indirect_resolve.rs:306-321`
  (`find_unique_return`) — `Return` (with uniqueness check).

**Sites (tests):**
- `crates/strider/src/strider/insn/control.rs:340-368` — `If`,
  `IntCmpOp(Equal)`, `IntConst(c)` (counts).
- `crates/opt/src/dead_branch/tests.rs:172-173`,
  `crates/opt/src/stack_store/tests.rs:62-66`,
  `crates/opt/src/load_readonly/tests.rs:153-154`,
  `crates/opt/src/call_other_elide/tests.rs:18`, `:211`,
  `crates/opt/src/known_bits/tests.rs:11`, `:234`,
  `crates/opt/src/indirect_branch_resolve/jump_table_tests.rs:669-1042`
  (5 occurrences).
- `crates/strider/tests/common/mod.rs:232` — already abstracted as
  `count_with(g, pred)` taking a `Fn(&NodeKind) -> bool`.

That's at least **15 production+test sites** all written as
`g.preorder().filter(|&n| matches!(g.graph.node_kind(n), <Pat>))`.

**Pattern shape:** "Walk the reachable graph and visit every node
whose discriminant matches a predicate." The result is either iterated
in place, collected into a `Vec`, counted, or scanned with `.find` /
`.any`.

**Variations:**
- Some collect into a `Vec` (so the graph can be mutated in the body);
  others iterate directly.
- Some need post-filter logic (e.g. `find_unique_return` enforces
  uniqueness; `inplace.rs:191` does the same).
- Some do `.count()` (test invariant checks); others do `.find()` or
  `.collect()`.

**Honest assessment:** **Yes — clear win.**

Two helper methods on `BuiltFunctionGraph` (and one on `Graph`) close
~80% of these sites:

```rust
impl BuiltFunctionGraph {
    pub fn preorder_kind<F>(&self, pred: F) -> impl Iterator<Item = NodeId> + '_
    where F: Fn(&NodeKind) -> bool + 'a;
}
```

Call site rewrites:

```rust
// before
function.preorder().filter(|&n| matches!(function.graph.node_kind(n), NodeKind::Call))

// after
function.preorder_kind(|k| matches!(k, NodeKind::Call))
```

That's the production case. For test counting:

```rust
// strider/tests/common/mod.rs:232 already does this — promote to a
// pub method on BuiltFunctionGraph itself.
fn count_kind<F: Fn(&NodeKind) -> bool>(g: &BuiltFunctionGraph, pred: F) -> usize {
    g.preorder_kind(pred).count()
}
```

The find-unique pattern (`find_placeholder_return_for_anchor` /
`find_unique_return` / the `inplace.rs:191` defensive uniqueness
assert) wraps this with an `at_most_one()` adapter; that's a separate
finding.

The reason this is a clear win and not "maybe" is that the predicate
form is *already* the common factor in 15 sites: every caller types
`matches!(..., NodeKind::X)` against the same shape. There's no
divergence in the predicate semantics — only in what's done with the
filtered iterator, which is exactly what `Iterator` adapters handle.

**Confidence:** high.

---

### P-006 Find-unique-by-kind patterns

**Sites:**
- `crates/cfg/src/cfg/builder/indirect_resolve.rs:306-321` —
  `find_unique_return` (returns `Result`, errors on 0 or >1 Returns).
- `crates/opt/src/indirect_branch_resolve/mod.rs:325-339` —
  `find_placeholder_return_for_anchor` (returns `Option<NodeId>`;
  walks `output_uses(anchor)`, not the whole graph; checks 3-input
  shape).
- `crates/opt/src/indirect_branch_resolve/jump_table.rs:321-331` —
  `find_anchor_consumer_return` (returns `Option<NodeId>`; walks
  `output_uses(anchor)`, no shape check).
- `crates/opt/src/indirect_branch_resolve/inplace.rs:191-203` (in
  `cfg(test)` block) — `Return` uniqueness assertion via
  `preorder().for-loop`.

**Pattern shape:** "Find the (unique) node of kind X" — three forms:

1. Whole-graph scan with uniqueness assertion (`find_unique_return`,
   the test in `inplace.rs`).
2. Use-list scan starting from a specific output (the two
   indirect-branch-resolve helpers).

**Variations:**
- Source: whole graph (`preorder`) vs `output_uses(anchor)` of a
  specific value.
- Result: `Result<NodeId>` (errors on duplicates) vs
  `Option<NodeId>` (first match) vs first-of-N with shape check.
- Shape check: `find_placeholder_return_for_anchor` requires
  `inputs.len() == 3 && inputs[2] == anchor_output`;
  `find_anchor_consumer_return` doesn't.

**Honest assessment:** **Maybe — needs more thought.**

The two whole-graph variants (`find_unique_return` and the
`inplace.rs` test assertion) collapse cleanly with P-005's
`preorder_kind` plus an `at_most_one()` adapter or a tiny
`find_unique_kind` helper.

The two use-list-based variants (`find_placeholder_return_for_anchor`
and `find_anchor_consumer_return`) have **subtly different semantics**:

- `find_placeholder_return_for_anchor` is the canonical "is this
  output anchored by a placeholder Return?" check. It validates the
  3-input shape so a tier-2-edited Return (now 4+ inputs) doesn't
  spuriously match.
- `find_anchor_consumer_return` doesn't validate the shape because
  its caller (`bound_via_predecessor_if`) is a defensive helper
  in the jump-table classifier path that would have been gated out
  earlier if the shape were wrong.

Merging these two would require either (a) accepting the more strict
3-input check in the jump-table site (small risk: the comment claims
"the producer-shape match should have gated us out before reaching
this point", which suggests the check would always pass anyway), or
(b) adding a parameter to opt-in/out of the check (which buys
nothing).

**The real problem is naming**: `find_placeholder_return_for_anchor`
and `find_anchor_consumer_return` are both in the same module
hierarchy, both check "is the consumer of this output a Return?",
but they don't share an implementation. A reader has to infer the
distinction from the body. Renaming one to clarify that it's the
shape-checking variant would help.

The decision tipper: combine them only if `find_anchor_consumer_return`'s
sole caller is updated to also depend on the shape check (verified
sound) — at which point one helper covers both.

**Confidence:** medium (in both the duplication and whether merging
is the right answer).

---

### P-007 Fixed-point loop scaffolding (`OptimizerPipeline::run`,
`strider::run`, `KnownBits::analyze`, `cfg::Builder::build`)

**Sites:**
- `crates/opt/src/pipeline.rs:258-285` — `OptimizerPipeline::run`,
  `MAX_ITERS = 1024`, accumulates `Changed` per iter, bail on cap.
- `crates/strider/src/orchestrator.rs:143-161` — `strider::run`,
  `LoopState::step` returns `Decision::{FixedPoint, StableOnly,
  Rebuild}`, dynamic cap (`2 * pending_at_iter_0 + 4`), bail on cap.
- `crates/opt/src/known_bits/mod.rs:322-349` — `analyze`, drains
  `WorkSet`, no explicit cap (worklist convergence is bounded by the
  number of distinct `Kb` values per output, finite by construction).
- `crates/opt/src/constant_fold/mod.rs:60-87`,
  `crates/opt/src/dead_branch/mod.rs:125-139` — same `WorkSet` drain
  pattern, no cap.
- `crates/cfg/src/cfg/builder/mod.rs:212-215` —
  `while let Some((parent, addr)) = work_queue.pop() { explore(...) }`,
  no cap.

**Pattern shape:** Several distinct shapes:

1. **Capped fixed-point of opaque pass calls** (`OptimizerPipeline::run`):
   each pass returns `Changed`/`NoChange`; loop continues if any
   reported `Changed`. Externally-visible iteration cap.
2. **Capped fixed-point with multi-way decision** (`strider::run`):
   each step returns `Decision`; only `FixedPoint` exits, the other
   variants run pass-specific recovery. Dynamic cap based on
   placeholder count.
3. **Worklist drain to convergence** (`KnownBits::analyze`,
   `ConstantFold::optimize_built`, `DeadBranchElimination`,
   `cfg::Builder::build`): explicit pop loop, re-enqueue on dependency
   change, no iteration cap (convergence is structural).

**Variations:**
- Iteration cap: hardcoded constant vs dynamic vs none.
- Progress signal: `bool` (`Changed`) vs enum (`Decision`) vs implicit
  (worklist not empty).
- Per-iteration side effects: validate at end (`OptimizerPipeline`),
  rebuild CFG (`strider`), append regions (`cfg::Builder`), update
  per-output `Kb` (`KnownBits`).
- Cap-exceeded handler: `bail!("did not converge after N")` for the
  capped variants.

**Honest assessment:** **No — looks like duplication but isn't.**

These are **three distinct algorithms** with shared *vocabulary*
(while-loop, progress check, optional cap), not a shared shape:

- The pipeline-of-passes loop runs N opaque calls per iteration; the
  unit of progress is a pass.
- The orchestrator loop has a per-iteration *plan*: it classifies, it
  partitions, it edits, it rebuilds; the unit of progress is an
  edge-set delta or an in-place edit. The `Decision` enum is intrinsic
  to the per-iteration plan, not boilerplate that could be hidden.
- The worklist drain is a different style of fixed-point — items
  re-enqueue themselves until quiescence, with no notion of "an
  iteration" at all. `WorkSet` (`crates/opt/src/worklist.rs`) is
  *already* the shared abstraction here, used by 3 of 4 worklist
  sites. (The fourth is `cfg::Builder::build`, which uses a `Vec` —
  see notes below.)

The MAX_ITERS-and-bail shell is too small (5 lines) and the body is
too case-specific (the post-pass calls in `OptimizerPipeline::run`,
the `Decision` match in `strider::run`) to extract usefully. A generic
`fixpoint(max_iters, fn)` function would have signatures like
`fn step() -> bool` for one caller and `fn step() -> Result<Decision>`
for the other, immediately bifurcating into two impls — meaning the
"shared" abstraction has zero shared code.

**Side observation:** `cfg::Builder::build` uses a plain
`Vec::push`/`Vec::pop` rather than `WorkSet`. It doesn't need dedup
(every queued address is unique by construction — see the
`work_queue` invariant in `cfg/builder/mod.rs:70-110`), so the choice
is correct. The fact that it's structurally similar to a worklist
drain doesn't make it the same algorithm.

**Confidence:** high.

---

### P-008 Phi-predecessor iteration (`inputs[1..]`)

**Sites:**
- `crates/opt/src/sp_expr.rs:302-316` — `decompose_sp_phi`: walks
  every `ControlPhi(sp)` predecessor, decomposing each.
- `crates/opt/src/function_args/mod.rs:427-431` —
  `mem_chain_is_dirty`: walks every `MemPhi` value pred, ORs.
- `crates/opt/src/stack_load_forward/mod.rs:233-237` — `probe`: walks
  every `MemPhi` pred, packages into `ResolveShape::Phi`.
- `crates/opt/src/indirect_branch_resolve/classify.rs:142-153` —
  `classify_anchor_with_rom_and_sp`: walks every `ValuePhi` pred,
  collects targets.

**Pattern shape:** Phi nodes (ControlPhi, MemPhi, StackStorePhi,
ValuePhi) all have the convention `inputs[0] = phi_token` and
`inputs[1..] = per-predecessor values`. Each site iterates the
predecessor slice via `inputs.into_iter().skip(1)` (or
`inputs.iter().skip(1)`).

**Variations:**
- Pred reduction: collect all (sp_expr), OR-fold (function_args),
  All-must-succeed (stack_load_forward, classify).
- Bail condition: any-pred-non-Terminal (sp_expr), any-pred-dirty
  (function_args), any-pred-fails-probe (stack_load_forward),
  any-pred-not-IntConst (classify).

**Honest assessment:** **No — looks like duplication but isn't.**

The "skip(1) iterate preds" idiom is shared, but it's a 1-line idiom
that already encodes the IR convention (slot 0 is the dispatch
token). Wrapping it in a helper:

```rust
impl Graph {
    pub fn phi_pred_inputs(&self, node: NodeId) -> impl Iterator<Item = NodeOutputId> + '_ {
        self.node_inputs(node).into_iter().skip(1)
    }
}
```

…doesn't help readers, because:

1. The 4 sites already document at the `skip(1)` site why slot 0 is
   skipped (the comments differ but say the same thing). Replacing
   the comment with `phi_pred_inputs` is a local readability win, but
   the convention doesn't *change* — the helper just renames the
   abstraction.
2. The actual algorithmic content of each site is the per-pred
   reduction, which differs sharply. A `try_for_each_phi_pred(node, F)`
   helper that tries to share both the iteration and the bail logic
   would force an unwieldy generic signature, since
   `decompose_sp_phi` builds a `Vec<i64>` of offsets (and
   `Vec<NodeOutputId>` of bases) while `mem_chain_is_dirty` ORs
   bools.

The `phi_pred_inputs` helper is fine as a 2-line method to add (the
P-005 reviewer has it on a maybe-do list), but it's not algorithmic
unification — it's API consistency. Out of scope for this pass.

**Confidence:** high.

---

### P-009 Per-region "stand up a sub-IR, run an opt pipeline" pattern

**Sites:**
- `crates/cfg/src/cfg/builder/indirect_resolve.rs:117-244` —
  `resolve_indirect_target` builds a fresh single-region IR via
  `ir::FunctionBuilder::new_raw`, lifts pcode via
  `pcode_lift::ValueLifter`, runs `make_resolver_pipeline()`
  (`ConstantFold + KnownBits + RedundantPhis`), inspects the unique
  Return.
- `crates/strider/src/strider/pipeline.rs:118-160` (top-level
  `analyze_cfg`) — full per-function lift; the orchestrator runs
  `build_stable_optimizer_pipeline` and
  `build_destructive_optimizer_pipeline` against it.
- `crates/strider/src/orchestrator.rs:357-399` — uses
  `classify_anchor_with_rom_and_sp` (an opt-side helper), no
  sub-IR rebuild.
- `crates/cfg/src/cfg/builder/indirect_resolve.rs:233-244` —
  `make_resolver_pipeline()` is the cfg-side pinned 3-pass pipeline.

**Pattern shape:** "Build a small / large IR, hand it to a configured
optimizer pipeline, then introspect."

**Variations:**
- Sub-IR scope: `cfg::indirect_resolve::resolve_indirect_target`
  builds a *single-region* mini-IR per indirect-branch site (cheap,
  per-callsite); `strider::run` builds the full function IR (the
  whole-function lift is amortised across many indirect resolves).
- Pipeline content: cfg pins `ConstantFold + KnownBits +
  RedundantPhis`; strider's stable pipeline adds
  `StackStoreDetect + StackLoadForward + FunctionArgDetect`;
  strider's destructive pipeline adds
  `RedundantPhis + DeadBranchElimination + CallOtherElide` again.

**Honest assessment:** **Maybe — needs more thought.**

The cfg-side `make_resolver_pipeline` and strider's stable/destructive
pipelines are *not* the same construction. They serve different
purposes:

- `make_resolver_pipeline` is a tiny pipeline used at *cfg-build
  time* on a one-region mini-IR to fold the target value of an
  indirect branch into a constant if possible. It's a tier-1 resolver.
- The strider pipelines are the full per-function optimization; they
  run on the real function IR after cfg-build is done.

There's no real duplication in the pipeline configurations — they
contain different passes for genuinely different reasons. The
tier-1-resolver vs tier-2-resolver distinction is documented in the
tier-2 module (`crates/strider/src/indirect_resolve_tier2/`).

What *is* shared between the two is **the boilerplate of building a
fresh IR around a single region, lifting pcode, running a pipeline,
and reading back a Return value-input**. `cfg::indirect_resolve`
builds it from scratch; nothing in `opt` or `strider` does the same
thing today (the strider orchestrator works on the already-lifted
function IR).

So the "drive opt against a sub-graph" pattern is **a single
production site** (the cfg one). The opt-side
`tier-2 indirect_resolve_tier2` helpers don't build a sub-IR — they
edit the existing one. The handful of test helpers that do build
empty IRs (`FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)`)
are documented as `F-022` cross-crate boilerplate; they don't run a
pipeline against the result.

The decision: **don't extract an abstraction for one site.** The
boilerplate in `resolve_indirect_target` is genuinely
indirect-branch-specific (it picks varnodes from the region's pcode,
sorts them deterministically, lifts only value-producing opcodes,
emits a `Return` of the target VN, then classifies the producer).
Extracting an `OptOnSubIR` helper would either parameterise on every
one of those steps (ending up with a callback-stuffed signature) or
be so coarse that it doesn't actually save lines.

**Confidence:** medium.

---

## Cross-pattern observations

**P-001 ↔ P-005** are the workspace's two biggest "the right helper
exists but wasn't used" findings. P-001 is one helper that already
exists and is being shadowed by inlined copies (in 2 sites); P-005 is
one helper that *should* exist and currently shows up as inlined
filter expressions in 15 sites. Both are the kind of duplication
where the abstraction is obvious in hindsight; both are low-risk to
apply.

**P-002 and P-003 share an algorithmic family** (recursive walks over
IR data with cycle-or-budget guards). P-002's resolution doesn't help
P-003 (the `MemChainVisitor` trait would be too narrow for
`decompose_sp` and too wide for `walk_control_for_if_bound`). They're
correctly separate findings.

**The opt-crate r6 outcomes table shows F-013 as "Applied" but the
fix was reverted by the anyhow conversion** (P-001). This suggests
the cross-crate r6 review's **outcomes-table verification** discipline
should include re-checking applied findings after the anyhow merge:
several F-numbered outcomes from earlier reviews could have similar
silent regressions. The specific fix here is a single-commit
re-application of the F-013 patch.

**`WorkSet` is already shared** (P-007 notes it). The codebase has
done the worklist abstraction correctly: 3 of 4 worklist-style
fixed-point sites use it; the fourth (`cfg::Builder`) genuinely
doesn't need dedup. This is a positive finding — the abstraction
works at the right granularity. There's no ask here.

**The pattern crate's `Matcher::find_all` already encodes P-005's
"preorder + kind filter" pattern** (`crates/pattern/src/matcher/mod.rs:172-196`).
That implementation does `preorder().filter(|n|
kind.accepts_discriminant(...))` exactly as the proposed
`Graph::preorder_kind` would. Worth checking in P-005's
implementation pass whether the pattern crate could route through a
shared helper too — though the `KindSpec` machinery there carries
extra structure beyond a raw `Fn(&NodeKind) -> bool`, so it might
end up as a separate code path.

## Out-of-scope items observed

- **Performance items**: many of the deferred-perf items (F-016
  visited-set save-restore, F-017 same_value memo, F-018
  bound_via_known_bits per-anchor analyze) are perf-deferrals, not
  duplication. They're outside this pass's scope.
- **The `find_largest_fitting_register` walk** in
  `pcode-lift::vn_io.rs:129` is a per-arch register-aliasing search;
  it doesn't repeat anywhere else in the workspace and isn't a
  "graph traversal" in the IR sense. Not flagged.
- **The 50+ test sites that call `FunctionBuilder::new_raw(vec![],
  &[], &[], &[], None, 0)`** are F-022 from the cross-crate review —
  surface duplication, not algorithmic, and correctly the test-utils
  / test-DSL track's territory.
- **F-014 / F-015 (the deferred mem-chain walker dedup)** is P-002
  here, surfaced in this report's terms. The cross-crate review's
  finding-by-finding outcomes covers the alias-disambiguation half
  (F-015) as already-applied via `step_through_store`. The walker
  backbone half (F-014) is what this pass argues remains a clear win.

## Outcomes (2026-04-29 — apply pass on `review/cross-crate-r6`)

| ID | Outcome | Notes |
| --- | --- | --- |
| P-001 | Applied | Silent opt-r6 F-013 regression caught by P-001's read pass; `replace_output_uses` removed, all 5 sites delegate to `Graph::replace_all_uses`. |
| P-002 | Skipped | Per-site reductions are too divergent to share without a tedious callback API. |
| P-003 | Skipped | Only one actual `visiting.insert/remove` pair in sp_expr (the other recursive helpers forward the set). RAII overkill. |
| P-004 | Skipped | Per the user-confirmed skip list. |
| P-005 | Applied | `BuiltFunctionGraph::preorder_kind<P>(P)` added; 4 production call sites converted. |
| P-006 | Partial | Subsumed by `preorder_kind`; `find_unique_return` rewritten as `iter.next() / iter.next()`. |
| P-007 | Skipped | Per the user-confirmed skip list. |
| P-008 | Skipped | Per the user-confirmed skip list. |
| P-009 | Skipped | Per the user-confirmed skip list. |
