# Indirect-branch resolution redesign — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the fragile per-region mini-graph indirect-branch resolver and the in-place IR editors with a dominator-scoped range analysis + a classify-only resolver driven by a single global resolution map, and collapse the stable/destructive pipeline split.

**Architecture:** A petgraph view over the IR control subgraph gives dominators (`strider-ir`); a dominator-scoped per-`(value, region)` integer-range pass (`strider-opt`) bounds switch indices; jump-table/stack-array classifiers consume ranges (stack-array via the shared mem-walker) and only *classify*; the orchestrator records classifications in the global `known_targets` map and rebuilds the CFG (the builder seats Return/Call+Return/edge terminators), eliminating in-place edits and the stable/destructive split.

**Tech Stack:** Rust workspace; `petgraph` 0.8.3 (`algo::dominators::simple_fast`, `visit::{GraphBase,IntoNeighbors,Visitable,IntoNodeIdentifiers,NodeCount}`); `cranelift-entity`; `cargo test`/`clippy`; PyO3 + `uv run pytest`.

**Working rules:** Branch `feature/indirect-branch-redesign` (off `develop`). One commit per task; `git push origin feature/indirect-branch-redesign` after each commit. End commit messages with the `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>` trailer. **Never** mention plan/task/phase identifiers in code or commit messages. **Prompt before merging** the branch to `develop`. Full-workspace gate (`cargo test --workspace` + `cargo clippy --workspace --all-targets` + `uv run pytest`) before the merge prompt.

Spec: `docs/superpowers/specs/2026-06-06-indirect-branch-resolution-redesign-design.md`. Key current-code anchors (from the architecture spike) are cited inline.

---

## File structure

**New:**
- `crates/strider-ir/src/control_flow_view.rs` — `ControlFlowView<'a>` (petgraph traits over the IR control subgraph) + `control_dominators(&Function)` + `dominates` helper.
- `crates/strider-opt/src/value_range/mod.rs` — the dominator-scoped range pass: `RangeMap`, `Interval`, `compute_value_ranges(function, &doms) -> RangeMap`.
- `crates/strider-opt/src/value_range/tests.rs`.

**Modified:**
- `crates/strider-ir/src/lib.rs` (export the view + helpers), `crates/strider-ir/Cargo.toml` (direct `petgraph` dep).
- `crates/strider-opt/src/indirect_branch_resolve/jump_table.rs` + `stack_array.rs` (bounding → `RangeMap`; stack-array → mem-walker), `classify.rs` (classify-only), `inplace.rs` (deleted), `mod.rs`.
- `crates/strider-orchestrator/src/orchestrator/mod.rs` (loop: classify→record→rebuild; delete in-place machinery), `crates/strider-orchestrator/src/indirect_resolver.rs` (deleted).
- `crates/strider-lift/src/cfg/builder/region_builder.rs` + `crates/strider-lift/src/cfg/builder/indirect_resolver.rs` (CFG seats terminators from the map; remove `with_indirect_resolver` if unused).
- `crates/strider-opt/src/lib.rs` + `crates/strider-orchestrator/src/strider/pipeline.rs` (merge stable/destructive).

---

## Phase 1 — IR control-flow petgraph view + dominators (strider-ir)

### Task 1.1: `ControlFlowView` petgraph traits + dominators

**Files:**
- Create: `crates/strider-ir/src/control_flow_view.rs`
- Modify: `crates/strider-ir/src/lib.rs`, `crates/strider-ir/Cargo.toml`

- [ ] **Step 1: Add the `petgraph` dependency to strider-ir.**

In `crates/strider-ir/Cargo.toml`, add under `[dependencies]` (use the workspace version):
```toml
petgraph = { workspace = true }
```
Verify the workspace root `Cargo.toml` pins `petgraph = "0.8"` (it does — `Cargo.toml:32`). Run `cargo build -p strider-ir` to confirm it resolves.

- [ ] **Step 2: Write the failing tests** (`control_flow_view.rs` `#[cfg(test)] mod tests`).

Build a synthetic diamond with the IR builder: `Entry → Region A → If → {Region B, Region C} → Region D(join) → Return`. Assert:
```rust
// (1) the view's neighbors of the If node are exactly {B_region, C_region}
//     and exclude any data/Phi node;
// (2) the view's node set is exactly the control nodes (Entry/Region/If/Return),
//     no IntConst/Phi/etc.;
// (3) simple_fast over the view gives: idom(D) == A (the If's region),
//     idom(B) == If-region, D is NOT dominated solely by B or C.
```
Concretely (adapt node lookups to the builder API; use `IRViewer`/`IRWalker`):
```rust
#[test]
fn control_view_neighbors_are_control_successors_only() {
    let f = diamond_fixture(); // helper below
    let view = ControlFlowView::new(&f);
    let if_node = /* locate the If node */;
    let succ: BTreeSet<NodeId> = view.neighbors(if_node).collect();
    assert_eq!(succ.len(), 2, "If has exactly two control successors");
    // none of the successors is a data/phi node:
    for n in &succ { assert!(f.node_kind(*n).has_control_flow()); }
}

#[test]
fn simple_fast_dominators_over_control_view() {
    use petgraph::algo::dominators::simple_fast;
    let f = diamond_fixture();
    let view = ControlFlowView::new(&f);
    let doms = simple_fast(&view, f.entry().unwrap());
    // join region's idom is the branch region, not either arm
    let join = /* locate join Region */;
    let branch_region = /* region containing the If */;
    assert_eq!(doms.immediate_dominator(join), Some(branch_region));
}
```
Write a `diamond_fixture() -> Function` helper in the test module using `FunctionBuilder` (create regions via `create_region`, link with `build_if`/`build_branch`, return a const). Mirror existing builder-based fixtures in `crates/strider-ir/src/function/edit.rs` tests / `walk/mod.rs` tests.

- [ ] **Step 3: Run; verify FAIL** (`ControlFlowView` undefined).
Run: `cargo test -p strider-ir control_view` → FAIL (unresolved).

- [ ] **Step 4: Implement `ControlFlowView` + petgraph traits.**

```rust
//! A petgraph view over the IR's CONTROL subgraph: control nodes
//! (Entry/Region/If/Call/Return/IndirectBranch) connected by forward
//! control edges only (no data, no Phi back-edges). Lets
//! `petgraph::algo::dominators::simple_fast` compute dominators directly.

use crate::function::Function;
use crate::node::NodeId;
use crate::IRViewer;
use petgraph::visit::{GraphBase, IntoNeighbors, IntoNodeIdentifiers, NodeCount, Visitable};

/// Control-only view of a `Function` for petgraph algorithms.
#[derive(Clone, Copy)]
pub struct ControlFlowView<'a> {
    function: &'a Function,
}

impl<'a> ControlFlowView<'a> {
    pub fn new(function: &'a Function) -> Self { Self { function } }

    /// Forward control successors of `node`: for each Control-typed output of
    /// `node`, the consuming control node(s).
    fn control_successors(&self, node: NodeId) -> Vec<NodeId> {
        let g = self.function.graph();
        let mut out = Vec::new();
        for &val in g.node_outputs(node) {
            if g.value_kind(val).is_control() {
                for (consumer, _slot) in g.value_uses(val) {
                    out.push(consumer);
                }
            }
        }
        out
    }
}
```
Implement the petgraph traits. `GraphBase::NodeId = NodeId`, `EdgeId = ()`. `IntoNeighbors`'s `Neighbors = std::vec::IntoIter<NodeId>` returning `control_successors`. `Visitable::Map = SecondaryMap<NodeId, bool>` (cranelift), `reset_map` clears it, `visit_map` returns a fresh one sized to the arena. `IntoNodeIdentifiers` iterates `g.all_node_ids().filter(|n| node_kind(n).has_control_flow())`. `NodeCount` counts those. Follow `crates/strider-graph/src/petgraph_view.rs` for the exact trait-method shapes (it implements the same trait set over the bipartite graph) — adapt to nodes-only + control edges.

Add the dominators helper:
```rust
/// Immediate-dominator tree over the function's control subgraph, rooted at
/// `Function::entry()`. Panics only if entry is unset (built Function invariant).
pub fn control_dominators(
    function: &Function,
) -> petgraph::algo::dominators::Dominators<NodeId> {
    let entry = function.entry().expect("control_dominators: entry must be set");
    petgraph::algo::dominators::simple_fast(&ControlFlowView::new(function), entry)
}

/// True if `a` dominates `b` (reflexive). `a == b` ⇒ true.
pub fn dominates(
    doms: &petgraph::algo::dominators::Dominators<NodeId>,
    a: NodeId,
    b: NodeId,
) -> bool {
    if a == b { return true; }
    doms.dominators(b).is_some_and(|mut it| it.any(|d| d == a))
}
```
(`is_control()` is on `ValueKind`; `value_uses` is on `Graph`; confirm exact names via the graph API.)

In `crates/strider-ir/src/lib.rs`: `mod control_flow_view; pub use control_flow_view::{ControlFlowView, control_dominators, dominates};`.

- [ ] **Step 5: Run tests; verify PASS.**
Run: `cargo test -p strider-ir control_view simple_fast` → PASS.
Run: `cargo clippy -p strider-ir` → zero warnings.

- [ ] **Step 6: Commit + push.**
```bash
git add crates/strider-ir/
git commit -m "feat(strider-ir): petgraph control-flow view + dominators over the IR control subgraph

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
git push origin feature/indirect-branch-redesign
```

---

## Phase 2 — Dominator-scoped range analysis (strider-opt)

### Task 2.1: `Interval` + `RangeMap` + `compute_value_ranges`

**Files:**
- Create: `crates/strider-opt/src/value_range/mod.rs`, `crates/strider-opt/src/value_range/tests.rs`
- Modify: `crates/strider-opt/src/lib.rs` (add `pub mod value_range;` + re-exports)

- [ ] **Step 1: Write the failing tests** (`value_range/tests.rs`).

```rust
// (1) After `If(IntCmp(Less, idx, IntConst(8)))`, on the TRUE successor region
//     and every region it dominates, range_of(idx, region) == [0, 7].
// (2) On the FALSE side (or a region not dominated by the true edge),
//     range_of(idx, region) is the full type range (top).
// (3) A value with KnownBits upper bound (idx & 7) has range [0,7] everywhere.
// (4) At a join dominated by two guarded edges with bounds 8 and 16, the meet
//     gives [0,15] (max upper bound across reaching guards — actually the
//     join's range is the UNION over predecessors = [0,15]); document the
//     join semantics in the test (see Step 4).
```
Use builder fixtures (a guarded dispatch shape). Assert exact intervals via `RangeMap::range_of(value, region)`.

- [ ] **Step 2: Run; verify FAIL.**
Run: `cargo test -p strider-opt value_range` → FAIL.

- [ ] **Step 3: Define the types.**

```rust
//! Dominator-scoped per-(value, region) integer ranges. Minimal interval
//! lattice seeded from `If(IntCmp(v, const))` guards (edge-sensitive, propagated
//! through the control dominator tree) and from KnownBits upper bounds. Used to
//! bound jump-table / stack-array switch indices. NOT a general abstract
//! interpreter.

/// Inclusive unsigned integer interval `[lo, hi]` over a value's bit width.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Interval { pub lo: u128, pub hi: u128 }

impl Interval {
    pub fn top(width_mask: u128) -> Self { Self { lo: 0, hi: width_mask } }
    /// Tight upper bound as an exclusive count, or None if top.
    pub fn upper_exclusive(&self, width_mask: u128) -> Option<u64> {
        if self.hi >= width_mask { None } else { u64::try_from(self.hi + 1).ok() }
    }
    fn intersect(self, other: Self) -> Self {
        Self { lo: self.lo.max(other.lo), hi: self.hi.min(other.hi) }
    }
    fn union(self, other: Self) -> Self {
        Self { lo: self.lo.min(other.lo), hi: self.hi.max(other.hi) }
    }
}

/// Per-(value, region) range table.
pub struct RangeMap { /* FxHashMap<(ValueId, NodeId /*region*/), Interval> + KnownBits-derived flow-insensitive map */ }
impl RangeMap {
    /// Range of `value` valid within `region` (the Region node id). Falls back
    /// to the KnownBits-derived flow-insensitive range, else top.
    pub fn range_of(&self, value: ValueId, region: NodeId) -> Interval { /* ... */ }
}
```

- [ ] **Step 4: Implement `compute_value_ranges(function, &doms, &known_bits) -> RangeMap`.**

Algorithm (documented precisely — the executor implements it):
1. **KnownBits seed (flow-insensitive):** for every value with KnownBits facts, record `[0, (!zeros)&mask]` in a flow-insensitive sub-map (reuse `KnownBitsMap` from `crates/strider-opt/src/known_bits/`).
2. **Guard seeds (edge-sensitive):** for each `If` node whose condition is `IntCmpOp(Less|Sless, v, IntConst(N))` (and the lowered `<=` shape `Xor(Less(N, v), 1):I1`), determine the true-successor region (the Region consuming the If's output slot 0). Compute the guard interval for `v`: `Less ⇒ [0, N-1]`; `<=` ⇒ `[0, N]`; `Sless` only when `v`'s sign bit is known-zero (via KnownBits). For every region `R` such that `dominates(&doms, true_succ_region, R)`, intersect `v`'s interval in `R` with the guard interval. (Use the dominator tree: walk regions dominated by `true_succ_region`.)
3. **Phi handling:** the index value read at the dispatch may be a `Phi` of the guarded value. When `range_of` is queried for a `Phi` output in region `R`, take the UNION of its incoming values' ranges over their respective predecessor regions (so a phi merging a `[0,7]` arm and an unguarded arm is top — fail-closed). Implement this in `range_of` (lazy) or precompute. Keep it bounded: do not iterate to a fixed point across loops — cap phi-chasing depth and treat cycles as top.
4. Return the `RangeMap`.

`compute_value_ranges` signature:
```rust
pub fn compute_value_ranges(
    function: &Function,
    doms: &petgraph::algo::dominators::Dominators<NodeId>,
    known: &KnownBitsMap,
) -> RangeMap
```
Region-of-a-value: a value's "region" is the Region node of the basic block it's used in; for the dispatch index, that's the region containing the `IndirectBranch`. The classifier passes the dispatch region to `range_of`.

- [ ] **Step 5: Run tests; verify PASS.** Run: `cargo test -p strider-opt value_range` → PASS. Run: `cargo clippy -p strider-opt` → zero warnings.

- [ ] **Step 6: Commit + push.**
```bash
git add crates/strider-opt/
git commit -m "feat(strider-opt): dominator-scoped per-value integer range analysis

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
git push origin feature/indirect-branch-redesign
```

---

## Phase 3 — Classifiers consume the range pass; stack-array via mem-walker

### Task 3.1: Jump-table + stack-array bound via `RangeMap`; delete the ad-hoc walk

**Files:**
- Modify: `crates/strider-opt/src/indirect_branch_resolve/jump_table.rs` (`bound_via_predecessor_if`, `bound_via_known_bits` → `RangeMap`), `stack_array.rs`, `classify.rs` (thread `RangeMap`), `mod.rs`.

- [ ] **Step 1: Thread `RangeMap` into the classifiers.**

`classify_anchor` currently receives a `KnownBitsMap` (`classify.rs:69`). Add a `&RangeMap` parameter (computed once per iteration by the caller alongside KnownBits + dominators). Replace the bounding call in `classify_jump_table` (`jump_table.rs:83-84`) and `classify_stack_array` (`stack_array.rs:96-97`):
```rust
// was: bound_via_known_bits(...).or_else(|| bound_via_predecessor_if(...))
let dispatch_region = /* the Region node of the IndirectBranch's control */;
let bound = ranges
    .range_of(shape.idx_value, dispatch_region)
    .upper_exclusive(idx_type_mask)?;
```
Determine `dispatch_region` from the `IndirectBranch`'s control input (walk to its owning `Region`). Keep the `MAX_TABLE_ENTRIES` cap.

- [ ] **Step 2: Delete `bound_via_predecessor_if` and `bound_via_known_bits`** from `jump_table.rs` (and the `same_value` phi-walk helper if only used by them). The range pass subsumes both.

- [ ] **Step 3: Update the existing tests.**

The jump-table/stack-array resolution tests build a guarded dispatch and assert resolution. They should still pass (the range pass derives the same bound from the `If(idx<N)` guard). Where a test relied on `bound_via_predecessor_if` internals directly, retarget it to assert the resolved targets (the observable behavior). Fix only what the run breaks.

- [ ] **Step 4: Run; verify.** Run: `cargo test -p strider-opt indirect_branch_resolve jump_table stack_array` → PASS (adapt failing tests per Step 3). Run: `cargo clippy -p strider-opt`.

- [ ] **Step 5: Commit + push.**
```bash
git add crates/strider-opt/
git commit -m "refactor(strider-opt): bound jump-table/stack-array indices via the range analysis

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
git push origin feature/indirect-branch-redesign
```

### Task 3.2: Stack-array stored-value lookup via the mem-walker

**Files:**
- Modify: `crates/strider-opt/src/indirect_branch_resolve/stack_array.rs` (`find_stack_stored_value_at_offset` → `may_clobber`).

- [ ] **Step 1: Write a test** asserting stack-array resolution where a `Call` clobbers memory between the stores and the dispatch load: the post-Call slots must NOT be read as their pre-Call stored values (the mem-walker forks/stops at the Call, unlike the old loose scan). Build a fixture: store consts into a stack array, then a `Call`, then the dispatch load. Assert the table is NOT resolved from the stale stores (or resolved only for slots provably unclobbered).

- [ ] **Step 2: Run; verify FAIL** (the old loose scan would mis-resolve).

- [ ] **Step 3: Replace `find_stack_stored_value_at_offset`'s raw backward scan with `memory_ssa::may_clobber` + `SpAliasOracle`.** For each element offset, build an `SpAliasOracle { load_class: SpRooted{base, offset}, load_size, ... }` and call `may_clobber` to find the nearest clobbering def; accept the value only if that def is an exact-match `Store` of an `IntConst` (peeling `Truncate`/`Extend`), else fail that element (→ whole table unresolved, conservative). Reuse the exact pattern from `crates/strider-opt/src/function_args/mod.rs` `mem_chain_is_dirty`.

- [ ] **Step 4: Run; verify PASS** + existing stack-array tests still pass. Run: `cargo test -p strider-opt stack_array` → PASS. Clippy clean.

- [ ] **Step 5: Commit + push.**
```bash
git add crates/strider-opt/
git commit -m "refactor(strider-opt): stack-array stored values via the shared memory-SSA walker

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
git push origin feature/indirect-branch-redesign
```

---

## Phase 4 — Resolution cutover: classify-only + global-map-driven rebuild

This is the big behavioral change. Sub-tasks build the new path, then delete the old.

### Task 4.1: CFG builder seats terminators from the resolution map (incl. Return)

**Files:**
- Modify: `crates/strider-lift/src/cfg/builder/region_builder.rs` (`process_branch_indirect`), `crates/strider-lift/src/cfg/builder/indirect_resolver.rs` (`ResolvedTargets`).

- [ ] **Step 1: Extend `ResolvedTargets` consumption at CFG build.**

Today `process_branch_indirect` (`region_builder.rs:433`) checks `known_targets` and, on a hit, sets the terminator. Confirm/extend it so that for a hit it sets:
- `ResolvedTargets::LinkRegister` → `RegionTerminator::Return` (no successors).
- `ResolvedTargets::Single(k)` in-function → an edge to k's region (existing); out-of-function → a tail-call terminator (new `RegionTerminator` arm, or reuse the Call-then-Return seam) that the lift driver materializes as `Call`+`Return`.
- `ResolvedTargets::Multiple(targets)` → edges to each target region (existing).
Add a `RegionTerminator::Return`-from-indirect and a tail-call arm if not already expressible. Write a focused test in the cfg builder tests: a CFG built with a `known_targets` entry `addr → LinkRegister` produces a region whose terminator is `Return`.

- [ ] **Step 2: Run; verify** the new terminator seating test passes; existing cfg tests pass. Commit + push.

### Task 4.2: Orchestrator loop becomes classify→record→rebuild

**Files:**
- Modify: `crates/strider-orchestrator/src/orchestrator/mod.rs` (`step`, `classify_and_partition`, `apply_in_place_edits` removal).

- [ ] **Step 1: Replace `classify_and_partition` + `apply_in_place_edits` with a record-only step.**

New `step()`:
1. Compute dominators (`strider_ir::control_dominators(&function)`), KnownBits, and `compute_value_ranges`.
2. For each unresolved `IndirectBranch`, call the classify-only `classify_anchor` → `Option<ResolvedTargets>` (now also returning `LinkRegister`/`Single`/`Multiple` uniformly).
3. Record each `Some(_)` into `known_targets` keyed by the branch's `PcodeInsnAddr`.
4. Convergence: if `known_targets` did not grow this iteration → `Decision::FixedPoint` (any remaining unresolved → error). Else → `Decision::Rebuild`.
There is no `StableOnly`, no `in_place_edits`, no `RegionIndex` lookup for CC context. Delete `apply_in_place_edits`, `anchor_calling_context_for`, and the `RegionIndex` persistence if now unused (grep to confirm).

- [ ] **Step 2: Run** the orchestrator's indirect-resolution tests (`crates/strider-orchestrator/tests/`): jump tables, tail calls, link-register returns. They must still resolve (now via rebuild). Fix fallout. Commit + push.

### Task 4.3: Delete the mini-graph resolver and the in-place editors

**Files:**
- Delete: `crates/strider-orchestrator/src/indirect_resolver.rs`, `crates/strider-opt/src/indirect_branch_resolve/inplace.rs`.
- Modify: `crates/strider-orchestrator/src/orchestrator/mod.rs` (remove `with_indirect_resolver` install at `mod.rs:1293`), `crates/strider-lift/src/cfg/builder/indirect_resolver.rs` (remove `IndirectResolverFn` + `with_indirect_resolver` if no other caller), `crates/strider-opt/src/indirect_branch_resolve/mod.rs` (drop `apply_link_register`/`apply_tail_call` re-exports), `strider-py` (drop any `PyCfgDetach`-adjacent exposure of the editors if present).

- [ ] **Step 1: Remove the mini-graph resolver + its wiring.** Delete `indirect_resolver.rs`; remove the `resolver` closure + `.with_indirect_resolver(resolver)` from `build_cfg` (`mod.rs:1293`). The CFG builder now relies solely on `known_targets` (a branch not in the map → `UnresolvedIndirectBranch`, classified later at IR level).
- [ ] **Step 2: Remove the in-place editors.** Delete `inplace.rs`; remove its re-exports + any callers (Task 4.2 already removed the orchestrator call sites). `grep -rn "apply_link_register\|apply_tail_call\|resolve_indirect_target\|with_indirect_resolver\|IndirectResolverFn" crates/` → only deletions remain (empty).
- [ ] **Step 3: Run** `cargo build --workspace` (iterate on removed-symbol fallout) → `cargo test --workspace` → no new failures. Clippy clean. Commit + push.

---

## Phase 5 — Merge the stable/destructive pipelines

**Files:**
- Modify: `crates/strider-opt/src/lib.rs` (`default_pipeline`/`destructive_default_pipeline`), `crates/strider-orchestrator/src/strider/pipeline.rs` (`build_stable_optimizer_pipeline`/`build_destructive_optimizer_pipeline`), `crates/strider-orchestrator/src/orchestrator/mod.rs` (call one pipeline per iteration).

- [ ] **Step 1: Collapse to one pipeline builder.**

Merge the stable + destructive pass lists into a single `OptimizerPipeline` (all passes — fold, known-bits, load-forward, dead-branch, cfg-detach, phi-collapse, region-collapse, + the post-passes). The orchestrator runs this one pipeline each iteration (it no longer needs a stable-only subset, since there are no in-place edits / `RegionIndex` to protect across iterations — see the `Decision::StableOnly` removal in Task 4.2). Keep `build_*_optimizer_pipeline` as one method or collapse the trio; update `strider-py`'s `reoptimize(destructive=...)` if it referenced the split (make `destructive` a no-op alias or remove the flag, per the simplest correct change — check `crates/strider-py/src/function.rs`).

- [ ] **Step 2: Run** `cargo test --workspace` → no new failures (the v2-baseline note: judge by "no NEW failures" vs the pre-branch baseline). Clippy clean.
- [ ] **Step 3: Commit + push.**
```bash
git add crates/
git commit -m "refactor: single optimizer pipeline; drop the stable/destructive split

The split existed only to protect in-place-edit RegionIndex state across
StableOnly iterations; resolution is now rebuild-driven, so one pipeline suffices.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
git push origin feature/indirect-branch-redesign
```

---

## Final verification gate (before merge prompt)

- [ ] `cargo test --workspace` → no NEW failures vs the pre-branch baseline.
- [ ] `cargo clippy --workspace --all-targets` → zero warnings.
- [ ] `cd crates/strider-py && uv run maturin develop && uv run pytest` → all pass.
- [ ] Spot-check the orchestrator fixture suite resolves the same-or-more indirect branches than before (no resolution regressions on real binaries).
- [ ] **Prompt the user** to merge `feature/indirect-branch-redesign` → `develop` (do not merge unprompted).

---

## Self-review notes

- **Spec coverage:** A (control view + dominators) → Task 1.1; B (range pass) → Task 2.1; C (classify-only) → Tasks 3.1 + 4.2; D (global map + rebuild + CFG seating) → Tasks 4.1, 4.2; E (deletions: mini-graph, in-place editors, pipeline split) → Tasks 4.3, 5; F (stack-array mem-walker) → Task 3.2. All covered.
- **Sequencing:** Phases 1–3 are additive/behavior-preserving (each compiles + green). Phase 4 is the atomic behavioral cutover (build new terminator path → switch the loop → delete the old). Phase 5 is the consequent simplification.
- **Risk:** Phase 4 is cross-crate (lift + opt + orchestrator) and is where resolution behavior changes; the orchestrator fixture suite is the safety net. Phase 2's range/phi handling must fail closed (cycles/unguarded arms → top) to preserve the "never over-read the ROM" soundness invariant.
- **Type consistency:** `ControlFlowView`/`control_dominators`/`dominates` (strider-ir); `Interval`/`RangeMap`/`compute_value_ranges`/`range_of`/`upper_exclusive` (strider-opt value_range); `ResolvedTargets::{LinkRegister,Single,Multiple}` (unchanged, strider-lift). The classifier signature gains `&RangeMap` consistently across `classify_anchor`/`classify_jump_table`/`classify_stack_array`.
