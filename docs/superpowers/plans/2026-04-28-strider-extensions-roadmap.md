# Strider Extensions Roadmap — 7-Feature Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
>
> **Status:** AWAITING USER APPROVAL before any code lands.

**Goal:** Seven interrelated improvements to strider's analysis pipeline, organized so dependencies resolve cleanly and parallelizable work runs in parallel.

**Hard rules across every phase:**
- TDD: failing test FIRST, minimal impl, pass, commit.
- **Every new piece of logic ships with unit tests** — user's binding instruction.
- No `panic!` / `unwrap` / `expect` / `debug_assert!` / `unreachable!` in production code.
- Workspace stays GREEN at every commit.
- Every commit message: lowercase imperative + Why-body + `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>` trailer.

---

## The seven features

| # | Feature | Why | Dependency |
|---|---|---|---|
| **F1** | Fingerprints — each IR node tracks which pcode insns produced it | Pattern-match proof-of-work; surgical region splits; provenance debugging | None (touches every `create_node` site) |
| **F2** | Refactor `FunctionBuilder` not to consume itself on `build()` | Enables manual rewrite + re-optimize, opt-pass tier-2 integration | None (foundational) |
| **F3** | BUG-30 — cross-region `StackLoadForward` for computed-goto-via-stack-array | Closes 15 ignored per-arch fixture tests | None |
| **F4** | Orchestrator debug config — structured trace of fixed-point iteration | Diagnostics for soundness bugs; visibility into iteration behavior | None |
| **F5** | Move tier-2 resolver into `opt::Optimizer` pass | Cleaner architecture; resolver re-runs naturally inside fixed-point | **F2** (long-lived builder) |
| **F6** | Pattern-based rewriter (uses `pattern::rewrite_rule`) + re-optimize | "What if input X = 4" exploratory analysis; collapses jump tables thanks to F7 | **F2** (long-lived builder), **F7** (jump-table lifting) |
| **F7** | **Jump table support** — strider lifts `RegionTerminator::Switch` as If-ladder | Without this, tier-2-resolved jump tables produce CFG edges with no IR encoding — F6 can't collapse them, dot rendering breaks, downstream consumers see wrong shape | None (cfg already produces Switch terminators; this finishes the lifting) |

## Dependency graph

```
                       F2 (builder refactor)
                      /        |
                    F5         F6 ─── depends on ─── F7 (jump-table lifting)
                                                          |
          F3 (BUG-30) ── independent ──────────────────────|
          F4 (debug)  ── independent ──────────────────────|
                                                          |
                                                          F1 (fingerprints)
                                                       (touches everything;
                                                        runs LAST to absorb
                                                        all other create_node
                                                        additions in one sweep)
```

## Phased dispatch order

- **Phase 1 — F2 alone.** Foundation; must complete before F5/F6.
- **Phase 2 — F3 + F4 + F7 in parallel.** All three independent of each other and of F2. Land while F2 settles.
- **Phase 3 — F5 + F6 in parallel.** F5 depends on F2; F6 depends on F2 + F7.
- **Phase 4 — F1 alone.** Touches every `create_node` call across `ir`, `cfg`, `pcode-lift`, `opt`, `strider`. Done last so it absorbs every new `create_node` site introduced by F2-F7 in one sweep — no merge churn.

Estimated wall-time per phase (with subagent dispatch): Phase 1 ≈ 2h, Phase 2 ≈ 2.5h (3-way parallel), Phase 3 ≈ 3h (parallel), Phase 4 ≈ 3h. Total ≈ 10.5h with maximum parallelism.

---

# F2 — Refactor `FunctionBuilder` not to consume on `build()`

**Phase 1.** Foundation for F5 and F6.

## Current state

`FunctionBuilder::build(self) -> Result<BuiltFunctionGraph>` consumes self. Once called, you can't add more nodes to the underlying `Graph` without reconstructing the builder. This is what blocked the G1-COMPLETE physical IR persistence (the orchestrator falls back to per-iteration rebuild).

## Target state

```rust
impl FunctionBuilder {
    /// Existing: consume + produce final BuiltFunctionGraph.
    pub fn build(self) -> Result<BuiltFunctionGraph> { ... }

    /// New: snapshot the current state into a BuiltFunctionGraph
    /// without consuming self.  The builder remains usable for
    /// further node additions; subsequent calls return updated
    /// snapshots reflecting the new state.
    pub fn snapshot(&self) -> Result<BuiltFunctionGraphRef<'_>> { ... }

    /// New: extract the underlying Graph by reference for opt
    /// passes that need &mut Graph.
    pub fn graph_mut(&mut self) -> &mut Graph { ... }
}
```

`BuiltFunctionGraphRef<'a>` is a borrow-flavored sibling of `BuiltFunctionGraph` — same accessors, but doesn't take ownership of the underlying graph.

## Files

- Modify: `crates/ir/src/builder/mod.rs` — add `snapshot()`, expose `graph_mut()`.
- Modify: `crates/ir/src/function.rs` — add `BuiltFunctionGraphRef<'a>`.
- Modify: `crates/ir/src/lib.rs` — re-export.
- Modify: existing `build()` callers if they need the new shape (likely few: most callers build exactly once).
- Create: `crates/ir/tests/builder_snapshot.rs` — contract pinning.

## Tests

Per "many tests including unit tests" rule, BOTH layers:

**Unit tests** (in `#[cfg(test)] mod tests {}` at bottom of `builder/mod.rs`):
- `snapshot_does_not_consume_builder` — call `snapshot()`, then call it again; both succeed.
- `snapshot_reflects_subsequent_node_additions` — snapshot, add node, snapshot again, second contains the new node.
- `graph_mut_returns_mutable_reference` — mutate via `graph_mut()`, verify mutation visible in snapshot.
- `build_after_snapshot_consumes_normally` — backward compat: existing `build()` still works after a `snapshot()`.

**Integration tests** (`crates/ir/tests/builder_snapshot.rs`):
- `lift_partial_function_snapshot_then_extend` — build a 2-region function, snapshot, build a 3rd region into the same builder, snapshot again, verify both snapshots are valid.
- `optimizer_runs_on_snapshot_then_more_nodes_added` — snapshot → run ConstantFold via `BuiltFunctionGraphRef` → add node → re-snapshot → re-run optimizer. Verify optimizer is idempotent on the extended graph.

## Acceptance

- 4 unit tests + 2 integration tests pass.
- All existing `build()` callers continue to work unchanged.
- Workspace test count: pre-F2 baseline + 6 new tests, 0 regressions.
- clippy clean.

---

# F3 — BUG-30 — Cross-region `StackLoadForward`

**Phase 2** (parallel with F4).

## Current state

15 per-arch tests in `crates/strider/tests/indirect_branch.rs` ignored under BUG-30. Cause: gcc/clang at -O0 lower `goto *targets[i]` to:
1. Function-entry region: store label addresses to a stack-local array.
2. Some later region: load from that array, jump to loaded value.

`StackLoadForward` currently only forwards within the same memory chain in the same region — it doesn't follow the chain across CFG edges.

## Target state

Extend the pass to chase the memory chain backward through **dominator-path predecessors** until it finds the matching `StackStore` or proves no aliasing store could intervene.

## Soundness rules — dominator-path forwarding (CRITICAL)

A node A **dominates** node B in the CFG when every execution path from the entry to B necessarily passes through A.  For load-store forwarding to be sound, the producing Store must dominate the consuming Load — otherwise some execution path reaches the Load without having executed the Store, and forwarding would substitute a value the program never wrote at that point.

The cross-region walk is therefore a **backward dominator-path search** from the Load:

### Walk algorithm

Starting at the Load's region, walk backward through the memory chain.  At each step, classify the producer of the memory chain's current input:

1. **`Store(addr, value)` with addr-aliasing the load (same SP-relative offset).**  Forward `value`.  Done.
2. **`Store` with a non-aliasing addr.**  Skip past it (the memory chain naturally chains through unrelated stores; `StackStoreDetect` tagged the SP-relative ones with explicit offsets, so address comparison is exact).
3. **`MemPhi` at a region entry.**  This is a join point.  Two cases:
    - **Single-predecessor join:** the one predecessor dominates the current region.  Recurse into the predecessor's exit memory chain.  Continue.
    - **Multi-predecessor join:** the predecessors do NOT individually dominate.  Recurse into ALL predecessors and verify they ALL agree on the same store (or absence thereof).  If any predecessor produces a different value or fails to find the store, bail (return unforwarded).  This is a **best-of-all-paths** check; only when every path has the same answer can we safely forward.
4. **`InitialMemory` at function entry.**  No store found; bail (return unforwarded).  We can't speculate about what the caller's stack looked like.

### Why dominator-path is the right framing

If a Store S dominates a Load L, then **every** execution path to L runs S first, so the value at the load address is whatever S wrote (modulo intervening aliasing stores, which step 2 handles).  This is the standard textbook condition for safe load-store forwarding.

If S does NOT dominate L (e.g., S is on one branch of an `If`, L is after the join), forwarding is unsound: the other branch reaches L without S, and the load's value is undefined or whatever the OTHER branch wrote.

The multi-predecessor case (step 3.b) generalizes: if all predecessors PROVE the same Store, then collectively they're equivalent to a Store that dominates the join.  We can safely forward.

### Concrete patterns the algorithm recognizes

- **Computed-goto in `goto *targets[idx]` shape (BUG-30):**
  - Function-entry region: `targets[0] = &&L0; targets[1] = &&L1; ...` — N stores at SP+0, SP+stride, SP+2*stride, ...
  - Some later region: `tmp = *(SP + idx*stride); jmp *tmp` — a load at SP-relative address.
  - Walk: load → MemPhi at the load's region → walks back through the single dominator-chain to the entry region → finds the matching store → forward.
- **Saved register `pop pc` (already handled intra-region):**
  - Entry: `push lr` — store of `lr` at SP-saved-offset.
  - Exit: `pop pc` — load from same offset, jmp.
  - Walk: load → … → entry's store of `InitialVar(lr)` → forward.

### Hazards and bail conditions

- **Aliasing intermediate Store.**  Any `Store` (or `StackStore` at the same offset) between the producing Store and the Load invalidates the forward.  Detected by step 2's filter.
- **Loops.**  If the walk re-enters a region already on the visit stack, bail.  Loop carries (e.g., `for (int i = 0; i < N; i++) targets[i] = ...`) genuinely require dataflow analysis we're not doing here; conservative bail is correct.
- **Depth limit.**  Cap the walk at 8 hops (configurable via `Options::stack_load_forward_max_depth`).  Pathological functions (very deep call-stack-style chains) bail at the cap; they get the same conservative-no-forward result they had pre-F3.

## Files

- Modify: `crates/opt/src/stack_load_forward/mod.rs` — add cross-region walk; gate behind a soundness depth limit.
- Modify: `crates/opt/src/stack_load_forward/tests.rs` — extend with cross-region cases.
- Possibly new: `crates/opt/src/stack_load_forward/cross_region.rs` if the cross-region logic warrants a separate file.
- Modify: `crates/strider/tests/indirect_branch.rs` — un-ignore the 15 BUG-30 tests.
- Modify: `docs/superpowers/plans/2026-04-25-analyzer-known-issues.md` — close BUG-30.

## Tests

**Unit tests** (in `stack_load_forward` source files):
- `cross_region_single_pred_chain_forwards`
- `cross_region_two_pred_join_with_matching_values_forwards` (both stores K → forward K)
- `cross_region_two_pred_join_with_differing_values_does_not_forward`
- `cross_region_aliasing_intermediate_store_blocks_forward`
- `cross_region_loop_back_edge_does_not_loop_forever`
- `cross_region_unbounded_depth_returns_unforwarded`

**Integration tests** (in `crates/strider/tests/indirect_branch.rs`):
- The 15 currently-ignored BUG-30 tests un-ignore and pass.

## Acceptance

- 15 BUG-30 tests un-ignore and pass.
- 6 unit tests for cross-region soundness rules.
- BUG-30 closed in tracker.
- Workspace baseline + 6 new unit tests + 15 un-ignored = +21 passing tests, -15 ignored.
- clippy clean.

---

# F4 — Orchestrator debug config

**Phase 2** (parallel with F3).

## Current state

`OrchestratorStats` exposes counters (iterations, stable_runs, destructive_runs, cfg_rebuilds, etc.) but no per-iteration trace. When something goes wrong, you only see the final state.

## Target state

```rust
pub struct OrchestratorConfig<...> {
    // existing fields ...
    pub debug: Option<OrchestratorDebugConfig>,
}

pub struct OrchestratorDebugConfig {
    /// Capture per-iteration snapshots into the returned trace.
    pub capture_iteration_snapshots: bool,
    /// Capture each tier-2 classification result with its anchor.
    pub capture_classifications: bool,
    /// Trace each in-place edit application.
    pub capture_edits: bool,
}

pub struct OrchestratorTrace {
    pub iterations: Vec<IterationSnapshot>,
}

pub struct IterationSnapshot {
    pub iteration_index: usize,
    pub unresolved_count_at_entry: usize,
    pub classifications: Vec<(PcodeInsnAddr, ClassificationOutcome)>,
    pub edits_applied: Vec<EditEvent>,
    pub cfg_rebuild_triggered: bool,
}

pub enum ClassificationOutcome {
    Resolved(ResolvedTargets),
    StillUnresolved,
}

pub enum EditEvent {
    LinkRegister { addr: PcodeInsnAddr },
    TailCall { addr: PcodeInsnAddr, target: u64 },
    KnownTargetUpdate { addr: PcodeInsnAddr, kind: ResolvedTargets },
}
```

The trace is OPT-IN via `OrchestratorDebugConfig`. When the config is `None`, no capture happens (zero overhead).

## Files

- Modify: `crates/strider/src/indirect_resolve_tier2/orchestrator.rs` — add `OrchestratorDebugConfig`, `OrchestratorTrace`, capture sites.
- Create: `crates/strider/src/indirect_resolve_tier2/debug.rs` — types + helpers.
- Create: `crates/strider/tests/tier2_debug_trace.rs` — tests.

## Tests

**Unit tests** (in `debug.rs`):
- `iteration_snapshot_default_is_empty`
- `classification_outcome_resolved_round_trips_via_debug_format`
- `edit_event_link_register_carries_addr`
- `edit_event_tail_call_carries_addr_and_target`

**Integration tests** (`crates/strider/tests/tier2_debug_trace.rs`):
- `trace_disabled_means_zero_overhead` — counter on capture call sites confirms zero captures when `debug = None`.
- `trace_captures_iteration_count` — multi-iteration scenario; trace has N IterationSnapshots.
- `trace_captures_classifications_per_iteration` — tier-2 produces M classifications per iteration; trace records all M.
- `trace_captures_in_place_edits` — link-register and tail-call edits both appear in the trace.
- `trace_captures_cfg_rebuild_triggers` — `cfg_rebuild_triggered` set true on rebuilding iterations.

## Acceptance

- 4 unit + 5 integration tests pass.
- Default behavior unchanged (no trace capture by default).
- clippy clean.

---

# F5 — Move tier-2 resolver into `opt::Optimizer` pass

**Phase 3** (parallel with F6). Depends on **F2**.

## Current state

Tier-2 lives at the strider layer (`crates/strider/src/indirect_resolve_tier2/`). The orchestrator calls `classify_anchor` for each unresolved branch and applies in-place edits via direct graph mutation. The opt pipeline doesn't know about tier-2.

## Target state

```rust
// crates/opt/src/indirect_branch_resolve/mod.rs (new)
pub struct IndirectBranchResolve {
    pub link_register_vn: Option<rsleigh::Vn>,
    pub rom: Option<Arc<dyn ReadOnlyMemory>>,
    pub unresolved_anchors: Vec<(PcodeInsnAddr, NodeOutputId)>,
}

impl Optimizer for IndirectBranchResolve {
    fn optimize(&self, function: &mut BuiltFunctionGraph) -> Result<OptimizationResult> {
        // For each anchor:
        //   - classify_anchor(graph, anchor, lr) -> Option<ResolvedTargets>
        //   - LinkRegister or tail-call Single → apply in-place edit, mark Changed
        //   - Other → leave for orchestrator (CFG rebuild needed)
    }
}
```

The orchestrator becomes thinner: it builds the pipeline (with the resolve pass added), runs it in the fixed-point loop, and only handles the CFG-rebuild side (the part opt can't do because it doesn't own CFG).

## Soundness preserved

Tier-2 classification rules don't change. The pass is a thin wrapper around the existing `classify_anchor` / `apply_*` machinery — just relocated into the opt crate's namespace.

## Files

- Create: `crates/opt/src/indirect_branch_resolve/mod.rs` — `IndirectBranchResolve` pass.
- Create: `crates/opt/src/indirect_branch_resolve/tests.rs` — pass-level tests.
- Move: `crates/strider/src/indirect_resolve_tier2/classify.rs` → `crates/opt/src/indirect_branch_resolve/classify.rs` (with re-export shim in strider for backward compat).
- Move: `crates/strider/src/indirect_resolve_tier2/inplace.rs` → `crates/opt/src/indirect_branch_resolve/inplace.rs`.
- Modify: `crates/strider/src/indirect_resolve_tier2/orchestrator.rs` — replace direct `classify_anchor` calls with `IndirectBranchResolve` pass invocation.
- Modify: `crates/opt/src/lib.rs` — export new pass; possibly add to `default_pipeline()` (gated by config — only relevant when there are unresolved branches).

## Tests

**Unit tests** (in `indirect_branch_resolve/mod.rs`):
- `pass_does_nothing_when_no_anchors`
- `pass_returns_no_change_when_no_anchor_classifies`
- `pass_returns_changed_when_link_register_anchor_resolves`
- `pass_returns_changed_when_tail_call_anchor_resolves`
- `pass_does_not_apply_in_place_for_intra_fn_single` (orchestrator handles those)

**Integration tests** (in `crates/opt/src/indirect_branch_resolve/tests.rs`):
- `pass_runs_inside_optimizer_pipeline` — add to a pipeline, run it, verify edits applied.
- `pass_round_trips_through_existing_orchestrator` — orchestrator using the new pass produces identical IR to the old direct-call path.
- All existing tier-2 tests still pass via the re-export shims.

## Acceptance

- 5 unit + 2 integration tests pass.
- Existing tier-2 tests pass via shims (no breakage).
- The orchestrator's `run_with_stats` shrinks (fewer direct classifier calls).
- clippy clean.

---

# F7 — Jump table support (Switch terminator → If-ladder lifting)

**Phase 2** (parallel with F3 + F4). Independent of all other features.

## Why this is its own feature

The cfg builder already produces `RegionTerminator::Switch { targets }` when tier-2 resolves a jump table to `Multiple` ([region_builder.rs:436](crates/cfg/src/cfg/builder/region_builder.rs#L436)).  But **strider has no Switch-lifting logic** — verified by grep: no `NodeKind::Switch`, no Switch arm in strider's terminator dispatch.  When tier-2 fires today, the orchestrator wires CFG edges but the IR has no encoding of the dispatch shape.  Downstream consumers (pattern queries, dot rendering, F6's rewriter) can't see the dispatch.

This is a load-bearing gap.  Closing it is a discrete chunk of work that doesn't depend on any other feature in this roadmap.

## Architecture choice — If-ladder for now, `NodeKind::Switch` later

From the spec's existing future-work section:

- **(a) If-ladder.** For N targets, emit `If(IntCmpOp::Equal(idx_value, IntConst(K_0)))` taking the K_0 branch on true, falling through to `If(... K_1 ...)` on false, and so on. Final case's false-branch is the "default" or unreachable (optimizer prunes if all cases are exhaustive).
- (b) New `NodeKind::Switch { cases: Vec<u64> }` — cleaner pattern target but requires validator entries, new fold rules, dot-label code, and pattern-crate updates.

**F7 picks (a) — explicitly as a temporary choice.**  Strictly additive — no new node kind, no new validator code. ConstantFold + DeadBranchElim + RedundantPhis already collapse the ladder when `idx_value` becomes a constant (which is exactly what F6 needs).  The If-ladder will look ugly in the dot rendering for jump tables with many cases (each case adds an If node + a CS join), and pattern queries that want to recognize "any switch" will have to walk the ladder.

**Future work** (added to the spec's Future-work section as part of F7):

- **`NodeKind::Switch { cases: Vec<u64> }` migration.**  Replace the If-ladder with a dedicated IR node that takes (control, memory, index) → N control outputs.  Add validator entries (Layer A signature + Layer C arity), a ConstantFold rule (constant index → propagate the matching control output, leave others dead), pattern-crate builders + matcher arms, and dot-label code.  Migration path: keep the If-ladder lowering as a fallback while a feature flag controls which lowering strider uses; flip the default once the new node kind is fully supported; remove the ladder lowering.

## Files

- Modify: `crates/strider/src/strider/insn/control.rs` — add `handle_switch(targets: &[u64], idx_value: NodeOutputId, region_lookup: ...)` that emits the If-ladder, threading control through each comparison and wiring true-branches to the per-target region.
- Modify: `crates/strider/src/strider/pipeline.rs` (or wherever the terminator dispatch lives) — replace the placeholder for `RegionTerminator::Switch` with a call to `handle_switch`.
- Modify: `crates/strider/src/strider/insn/control.rs` — read `target_vn` from the Switch terminator's saved state (cfg already has the targets; strider needs the original target VN for the comparisons — this likely means `RegionTerminator::Switch` carries `{ target_vn, targets }`, OR the orchestrator records it in a side-table).
- Possibly modify: `crates/cfg/src/cfg/types.rs` — `RegionTerminator::Switch { target_vn: rsleigh::Vn, targets: Vec<u64> }` (carry the index VN, not just the targets, so strider knows what to compare).

## Subtle: where does `target_vn` for the Switch's comparisons come from?

The original `BranchIndirect` had `target_vn` as its operand — the VN holding the runtime jump target. After tier-2 resolves to `Multiple`, the cfg builder discards target_vn (it only carries `targets: Vec<u64>` today). For the If-ladder, we need to compare a VALUE against each constant. Two options:

- **(i)** Make `RegionTerminator::Switch` carry `target_vn: rsleigh::Vn` alongside `targets`. The cfg builder propagates it from the original `BranchIndirect`.
- **(ii)** Strider's lift remembers it via the `unresolved_branches: Vec<(PcodeInsnAddr, NodeOutputId)>` side-table that already pins the placeholder Return's target value. After tier 2 produces `Multiple` and the orchestrator rebuilds CFG, the strider re-lift has access to the same side-table and can recover the target value.

**F7 picks (i)** — adds the field to the cfg variant. Cleaner: cfg fully describes its terminator. Cost: one field addition + downstream pattern matches updated. The variant payload was `Vec<u64>` only; becomes `{ target_vn: rsleigh::Vn, targets: Vec<u64> }`.

## Tests

**Unit tests** (in `crates/strider/src/strider/insn/control.rs::tests`):
- `handle_switch_with_one_target_emits_single_if_then_branch_to_target`
- `handle_switch_with_two_targets_emits_if_ladder_with_correct_polarity`
- `handle_switch_with_three_targets_chains_falses_correctly`
- `handle_switch_threads_control_chain_through_each_if`
- `handle_switch_with_zero_targets_errors` (defensive — should not reach here)

**Unit tests** (in `crates/cfg/src/cfg/types.rs::tests` if not already there):
- `region_terminator_switch_carries_target_vn_and_targets`

**Integration tests** (`crates/strider/tests/jump_table_lifting.rs` — new):
- `switch_terminator_lifts_to_if_ladder_for_one_target`
- `switch_terminator_lifts_to_if_ladder_for_three_targets`
- `switch_with_const_index_collapses_via_default_pipeline_to_single_branch` — verifies F7's lifted IR + existing optimizer compose correctly. Replace `idx_value` with `IntConst(K)`, run `default_pipeline()`, assert only K's branch survives.
- `tier_2_multiple_resolution_end_to_end_produces_lifted_switch_in_ir` — run the orchestrator on a synthetic CFG that produces a `Multiple` resolution; assert the resulting IR contains the If-ladder corresponding to the targets.

## Acceptance

- 5 (control.rs unit) + 1 (cfg types unit) + 4 (integration) = **10 new tests**.
- The 15 BUG-30 tests stay closed (R5 already un-ignored them; F7 must not regress).
- Tier-2 jump-table resolutions now produce visible IR structure that downstream consumers can pattern-match against.
- clippy clean.

---

# F6 — Pattern-based rewriter + re-optimize

**Phase 3** (parallel with F5). Depends on **F2** (long-lived builder) and **F7** (jump tables lifted into IR).

## Use case

> "I want to replace this Switch's selector with `IntConst(4)` and re-run the optimizer to see the graph collapse to just case 4." — including jump tables.

Without F7, jump tables resolved via tier-2 `Multiple` would have CFG edges but no IR encoding to fold. F6 leans on F7's If-ladder lifting so the same `IntCmpOp::Equal` fold rules collapse both ordinary switches and jump tables.

## Architecture — leverage `pattern::rewrite_rule`

Per user direction, the rewriter API uses the existing `pattern::rewrite_rule` infrastructure rather than rolling its own substitution logic.  `pattern::rewrite_rule(lhs_pat, rhs_pat) -> impl Fn(&mut BuiltFunctionGraph, NodeId) -> Result<bool>` ([crates/pattern/src/rewrite.rs:38](crates/pattern/src/rewrite.rs#L38)) already handles:
- Pattern matching the LHS at a candidate root.
- Materializing the RHS via the pattern crate's `BuildCtx`.
- Calling `replace_all_uses` with use-list integrity preserved.
- Skip-via-error sentinel (`pattern::Error::skip()`).

F6 layers a thin **rewriter façade** on top:

```rust
// crates/strider/src/rewrite.rs (new)
pub struct GraphRewriter<'a> {
    fg: &'a mut BuiltFunctionGraph,
}

impl<'a> GraphRewriter<'a> {
    /// Apply a single rule at every candidate root in the graph until
    /// it stops firing.  Internally walks matchable roots and calls
    /// the closure returned by `pattern::rewrite_rule(lhs, rhs)`.
    pub fn apply_rule<F>(&mut self, rule: F) -> Result<usize>
    where
        F: Fn(&mut BuiltFunctionGraph, NodeId) -> Result<bool>;

    /// Apply N rules round-robin until a fixed point.  Internally
    /// uses `pattern::apply_rules_in_order`.
    pub fn apply_rules(&mut self, rules: &[pattern::BoxedRule]) -> Result<usize>;

    /// Re-run the standard optimizer pipeline (stable + destructive)
    /// on the current graph state.  Idempotent — running twice in a
    /// row produces the same result as running once.
    pub fn re_optimize(&mut self) -> Result<()>;
}

impl FunctionBuilder {
    /// Open a rewriter.  Requires F2's non-consuming builder API.
    pub fn rewriter(&mut self) -> Result<GraphRewriter<'_>>;
}
```

The user's "replace selector with 4" case becomes:

```rust
let switch_selector_pat = /* pattern capturing the switch's selector */;
let rule = pattern::rewrite_rule(switch_selector_pat, pattern::int_const(4));
let mut rewriter = function_builder.rewriter()?;
rewriter.apply_rule(rule)?;
rewriter.re_optimize()?;                  // pipeline collapses dead branches
```

For jump tables, the same flow works because F7 lifted the dispatch to an If-ladder whose comparisons fold via ConstantFold when the user replaces the index.

## Files

- Create: `crates/strider/src/rewrite.rs` — `GraphRewriter` thin façade over `pattern::rewrite_rule`.
- Modify: `crates/strider/src/lib.rs` — re-export.
- Create: `crates/strider/tests/manual_rewrite.rs` — use-case validation.

## Tests

**Unit tests** (in `rewrite.rs`):
- `apply_rule_with_no_match_returns_zero_applications`
- `apply_rule_with_one_match_returns_one_application`
- `apply_rules_round_robin_reaches_fixed_point`
- `re_optimize_is_idempotent`
- `apply_rule_preserves_use_list_integrity` (call `validate` after)

**Integration tests** (`crates/strider/tests/manual_rewrite.rs`):
- `replace_switch_selector_with_const_collapses_to_one_branch` — simple switch, replace selector, re-optimize, only case-K's branch survives.
- `replace_jump_table_index_with_const_collapses_to_one_target` — **the headline new case** (depends on F7): tier-2-resolved jump table lifted via F7's If-ladder; user replaces the index input with `IntConst(I)`; re-optimize; assert only target_I's branch survives.
- `replace_input_then_reoptimize_then_replace_again_works` — multi-edit.
- `re_optimize_without_changes_is_no_op`.
- `manual_rewrite_does_not_break_validate` — after each rewrite, `ir::validate::validate(&graph, entry)` passes.
- `apply_rule_using_pattern_var_capture` — demonstrate the `pattern::rewrite_rule(lhs, rhs)` flow via a non-trivial pattern (e.g., `add(var(x), int_const(0)) -> var(x)`).

## Acceptance

- 5 unit + 6 integration = **11 new tests**.
- Both simple-switch AND jump-table collapse cases produce the expected IR.
- clippy clean.

---

# F1 — Fingerprints (proof-of-work / provenance tracking)

**Phase 4** (last). Touches every `create_node` site across `ir`, `cfg`, `pcode-lift`, `opt`, `strider`. Done last to absorb F2-F6 additions in one sweep.

## Target shape

```rust
// crates/ir/src/node/fingerprint.rs (new)

/// Identifies a single pcode instruction by its (machine_addr, insn_index)
/// pair.  Re-uses cfg's existing PcodeInsnAddr.
pub use cfg::test_api::types::PcodeInsnAddr as FingerprintEntry;

/// The set of pcode instructions that contributed to a node.
///
/// CONTRACT: every value-producing IR node has a fingerprint that is
/// the union of:
///   - The pcode instruction that DIRECTLY constructed it (lift time), AND
///   - The fingerprints of all input nodes whose values flowed into it.
///
/// Optimizer rewrites that fold N input nodes into one output node
/// MUST union the inputs' fingerprints into the output's fingerprint.
/// This is what makes `fingerprint` a sound "proof of work" — every
/// pcode instruction reachable through the value-flow chain to this
/// node is recorded.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Fingerprint {
    addrs: SmallVec<[PcodeInsnAddr; 4]>,
}

impl Fingerprint {
    pub fn from_single(addr: PcodeInsnAddr) -> Self;
    pub fn merge(a: &Fingerprint, b: &Fingerprint) -> Self;
    pub fn merge_many<'a>(fps: impl IntoIterator<Item = &'a Fingerprint>) -> Self;
    pub fn contains(&self, addr: PcodeInsnAddr) -> bool;
    pub fn iter(&self) -> impl Iterator<Item = PcodeInsnAddr> + '_;
    pub fn len(&self) -> usize;
}
```

Storage: `Graph::fingerprints: HashMap<NodeId, Fingerprint>` — side-table, same pattern as `stack_phi_offsets` and `call_other_names`. Deliberately external to keep `Node` Copy/small.

## Lift-site population

Every `create_node` call inside `pcode-lift::ValueLifter` needs to attribute the new node to the source pcode insn. The cleanest way: thread a `current_pcode_addr: Option<PcodeInsnAddr>` through `ValueLifter`'s state, and `create_node` consults it.

```rust
impl<'a, 'b, R: rsleigh::MemReader> ValueLifter<'a, 'b, R> {
    pub fn lift(&mut self, region_insn: &RegionInstruction) -> Result<bool> {
        self.current_pcode_addr = Some(region_insn.addr);
        let result = self.lift_inner(&region_insn.insn)?;
        self.current_pcode_addr = None;
        Ok(result)
    }
}
```

`Graph::create_node_with_fingerprint` is the new entry point. The plain `create_node` becomes a thin wrapper that uses an empty fingerprint (used by tests / synthetic constructors).

## Optimizer-fold population

Every fold rule in `opt` that creates a replacement node MUST union the input fingerprints. Example for ConstantFold's `IntAdd(IntConst(a), IntConst(b))` → `IntConst(a+b)`:

```rust
let new_fp = Fingerprint::merge(
    graph.fingerprint_of(left_input_node),
    graph.fingerprint_of(right_input_node),
);
let new_const = graph.create_node_with_fingerprint(
    NodeKind::IntConst(a + b), [], [...], new_fp,
);
```

This is mechanical but touches every fold rule. Worth a sweep.

## Use cases enabled

1. **Pattern-match proof-of-work:** when a `pattern::Matcher` matches a node, the user can call `graph.fingerprint_of(matched_node)` to see exactly which pcode instructions contributed. Confirms the match is grounded in the right disassembly.

2. **Surgical region split (future):** when the cache wants to split a region's IR body at pcode insn K, it asks: "which cached nodes have insn K-or-later in their fingerprint?" Those go to the second half; the rest stay in the first half. Avoids re-lifting (closes the surgical-split future-work item).

3. **Provenance debugging:** when a downstream pattern query fails unexpectedly, the user can ask "what did this node come from?" and trace back to source.

## Files

- Create: `crates/ir/src/node/fingerprint.rs` — `Fingerprint` type + ops.
- Modify: `crates/ir/src/graph/mod.rs` — `fingerprints: HashMap<NodeId, Fingerprint>` field.
- Modify: `crates/ir/src/graph/store.rs` — `create_node_with_fingerprint`, `fingerprint_of`, `set_fingerprint`.
- Modify: `crates/ir/src/lib.rs` — re-exports.
- Modify: `crates/pcode-lift/src/lib.rs` — `ValueLifter::current_pcode_addr` + use it on every `create_node` call.
- Modify: every per-opcode handler in `crates/pcode-lift/src/value/` — propagate the addr.
- Modify: every fold rule in `crates/opt/src/constant_fold/`, `known_bits/`, `redundant_phis/`, `dead_branch_elim/`, etc. — merge fingerprints on rewrite.
- Modify: `crates/strider/src/strider/insn/control.rs` and other lift sites — pass addr through.
- Create: `crates/ir/tests/fingerprint.rs` — contract tests.

## Tests

This is the largest tier of tests because the contract spans every site.

**Unit tests** (in `fingerprint.rs`):
- `fingerprint_default_is_empty`
- `fingerprint_from_single_contains_addr`
- `fingerprint_merge_unions_two`
- `fingerprint_merge_dedupes_overlap`
- `fingerprint_merge_many_handles_empty_iter`
- `fingerprint_merge_many_handles_single`
- `fingerprint_iter_yields_unique_addrs`
- `fingerprint_len_matches_iter_count`

**Unit tests** (in `crates/ir/src/graph/store.rs::tests`):
- `create_node_with_fingerprint_stores_in_side_table`
- `create_node_without_fingerprint_uses_default`
- `set_fingerprint_overwrites_previous`
- `fingerprint_of_unknown_node_returns_default`

**Integration tests** (per-pass; in each fold-rule test file):
- ConstantFold: `int_add_fold_unions_input_fingerprints`
- KnownBits: `known_bits_fold_unions_input_fingerprints`
- RedundantPhis: `single_input_phi_collapse_carries_input_fingerprint`
- DeadBranchElim: `dead_branch_removal_preserves_kept_branch_fingerprints`
- LoadReadOnly: `load_to_const_carries_load_addr_fingerprint`

**Integration tests** (per-lift-site; new file `crates/strider/tests/fingerprint_e2e.rs`):
- `every_lifted_value_node_has_fingerprint_with_at_least_one_addr`
- `int_const_constructor_records_constructing_pcode_addr`
- `lifted_int_add_records_addr_of_add_insn`
- `optimizer_run_preserves_fingerprint_provenance`
- `pattern_match_fingerprint_matches_disassembled_insns` — the proof-of-work use case end-to-end.

## Acceptance

- 8 fingerprint unit + 4 graph store unit + 5 fold-rule integration + 5 e2e integration = **22 new tests**.
- Every `create_node` call site in `pcode-lift` and `strider` populates the fingerprint.
- Every fold rule in `opt` merges input fingerprints.
- The proof-of-work use case demonstrates: pattern match → query fingerprint → see correct disassembly addresses.
- clippy clean.

---

## Acceptance criteria across all 7 features

- Phase 1 (F2): +6 tests.
- Phase 2 (F3 + F4 + F7): +40 tests (F3: 21 incl. -15 ignored; F4: 9; F7: 10), -15 ignored.
- Phase 3 (F5 + F6): +18 tests (F5: 7; F6: 11).
- Phase 4 (F1): +22 tests.
- **Total: ~86 new passing tests, -15 ignored**, no regressions.
- Workspace state at end: ≈2884 passed / 0 failed / 18 ignored, clippy clean across all 7 phases.
- BUG-30 closed.
- Jump tables encoded in IR (via If-ladder) and pattern-rewritable.
- All 7 features have unit tests AND integration tests per the user's binding rule.

## Risks

| Risk | Phase | Mitigation |
|---|---|---|
| F2's `BuiltFunctionGraphRef` lifetime story conflicts with opt-pass `&mut Graph` API | Phase 1 | Explicitly design ref-vs-owned both work; integration test pins it. |
| F3 cross-region walk over-forwards across an unsound path | Phase 2 | Soundness rules above + 6 dedicated unit tests; add a depth limit + cycle detection. |
| F4 trace capture introduces non-zero overhead even when disabled | Phase 2 | Counter-instrumented test asserts zero-capture when `debug = None`. |
| F5 move-to-opt breaks the orchestrator's CFG-rebuild path | Phase 3 | Round-trip integration test asserts identical IR pre vs post move. |
| F6 manual rewrite produces graph that fails `validate` | Phase 3 | Every test calls `validate` after the rewrite. |
| F1 fingerprint storage cost dominates large functions | Phase 4 | `SmallVec<[PcodeInsnAddr; 4]>` keeps common case (≤ 4 ancestors) inline; benchmark on the largest fixture; if memory matters, switch to a bitset of region-relative IDs later. |
| F1 missed `create_node` site silently leaves a node fingerprint-less | Phase 4 | A workspace-level integration test enumerates every node in a representative function and asserts non-empty fingerprint. |

## Decisions (Q1–Q7) — locked

- **Q1 — F2 backward compat:** **both APIs** kept. `build(self)` stays for one-shot final use; `snapshot()` added for in-flight inspection.  No deprecation.
- **Q2 — F3 depth limit:** **8 hops**, configurable via `Options::stack_load_forward_max_depth` (default 8).
- **Q3 — F4 trace capture format:** **pure data structures.** No `tracing` crate adapter.
- **Q4 — F5 pass placement:** **after `StackLoadForward`, before `RedundantPhis`** in the stable subset.  Implementer free to override if a different order proves necessary during integration.
- **Q5 — F6 rewriter API:** **constants + input replacement only.**  No general `build_int_add` / `build_load` exposed in the rewriter façade; users compose via `pattern::rewrite_rule(lhs, rhs)` + the pattern crate's existing builder constructors.
- **Q6 — F1 fingerprint storage:** **side-table** `HashMap<NodeId, Fingerprint>` on `Graph`.  Matches `stack_phi_offsets` precedent; keeps `Node` Copy/small.
- **Q7 — Phase parallelism:** **parallel.**  Phase 2 dispatches F3 + F4 + F7 as 3 simultaneous subagents.  Phase 3 dispatches F5 + F6 as 2 simultaneous subagents.

---

Approve to proceed.  I'll dispatch Phase 1 (F2) first; subsequent phases follow per the dependency graph.
