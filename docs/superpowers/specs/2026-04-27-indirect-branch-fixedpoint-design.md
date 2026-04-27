# Indirect Branch Resolution — Fixed-Point Design

> **Supersedes:** [2026-04-27-indirect-branch-resolution-design.md](2026-04-27-indirect-branch-resolution-design.md).
>
> The earlier design routed `BranchIndirect` resolution through a **single-region mini IR graph** built lazily inside the cfg builder.  In practice that mini-graph couldn't see across regions and could not run stack-aware passes (`StackLoadForward`, `StackStoreDetect`), so it failed to resolve gcc-ARM `pop {pc}` and any jump-table shape.  This document replaces that approach with a **fixed-point iterative analysis** that runs the *full* optimizer on the *whole* function and feeds resolved targets back into a CFG rebuild until convergence.

## Goal

Resolve every `BranchIndirect` in the constructed function — including stack-popped return addresses (`pop pc`), tail calls (`mov reg, K; jmp *reg`), and jump tables — using the full power of the existing optimizer pipeline.  Reach a fixed point where every indirect branch is either classified or surfaced as `cfg::ErrorKind::UnresolvedIndirectBranch`.

## Motivation

The single-region mini-graph approach had three concrete failures:

1. **`pop pc`.**  gcc-ARM `pop {pc}` lifts to `tmp = load[sp]; sp += 4; BranchIndirect tmp`.  The mini-graph cannot prove `tmp == InitialVar(lr)` because it lacks `StackLoadForward`, which depends on the entry region's `push {lr}` being visible.  Phase 5 left 4 ARM tests ignored under BUG-5 for this reason.

2. **`push X; pop pc` tail call.**  A naïve "load address starts with `InitialVar(sp)`" heuristic would misclassify this as a return.  The user's binding constraint: the load must be from the **function-entry** sp, not the **region-entry** sp.  Distinguishing requires cross-region sp tracking.

3. **Jump tables.**  `jmp *load(table_base + idx * stride)` with a bounded `idx` requires reading the `.rodata` table contents (one Load per entry) and bounding `idx` (via `KnownBits` or a predecessor `If`-walk) — both impossible inside a stripped single-region mini-graph.

All three need the optimizer to run on the **constructed graph**.  The optimizer is what turns "untouched stack location" into "InitialVar(lr) value" (via `StackLoadForward`), what turns "constant table base + bounded index" into "list of load addresses" (via `LoadReadOnly` + `KnownBits`), and what proves cross-region sp invariants.

## Architecture

A two-tier resolver inside an outer fixed-point loop **that only runs when the CFG actually contains a `BranchIndirect`**.  For functions without indirect branches (the common case), the loop is skipped entirely and the analyze path is identical to today's:

```
                  ┌────────────────────────────────────────────────┐
                  │  Outer loop (orchestrated by strider::analyze) │
                  │                                                │
                  │   ┌────────────────────────────────────────┐   │
                  │   │ build CFG with known_targets           │   │
                  │   │  – tier 1 (cfg-time mini-graph):       │   │
                  │   │    resolves trivial cases inline       │   │
                  │   │  – on tier-1 failure: terminator =     │   │
                  │   │    UnresolvedIndirectBranch{vn,addr}   │   │
                  │   └────────────────────────────────────────┘   │
                  │                  │                             │
                  │                  ▼                             │
                  │   ┌────────────────────────────────────────┐   │
                  │   │ lift CFG → IR (strider)                │   │
                  │   │  – UnresolvedIndirectBranch regions    │   │
                  │   │    emit a placeholder Return that      │   │
                  │   │    consumes target_vn, anchoring it    │   │
                  │   │    in the IR for analysis              │   │
                  │   └────────────────────────────────────────┘   │
                  │                  │                             │
                  │                  ▼                             │
                  │   ┌────────────────────────────────────────┐   │
                  │   │ run full optimizer pipeline            │   │
                  │   │   ConstantFold KnownBits RedundantPhis │   │
                  │   │   StackStoreDetect StackLoadForward    │   │
                  │   │   LoadReadOnly DeadBranchElimination   │   │
                  │   │   CallStackArgCollect …                │   │
                  │   └────────────────────────────────────────┘   │
                  │                  │                             │
                  │                  ▼                             │
                  │   ┌────────────────────────────────────────┐   │
                  │   │ tier 2 resolver (post-IR pass)         │   │
                  │   │  for each UnresolvedIndirectBranch:    │   │
                  │   │    inspect target_vn's IR producer in  │   │
                  │   │    the optimised graph; classify into  │   │
                  │   │    ResolvedTargets or remain unresolved│   │
                  │   └────────────────────────────────────────┘   │
                  │                  │                             │
                  │                  ▼                             │
                  │     any new resolutions? ─── yes ──┐           │
                  │              │ no                  │           │
                  │              ▼                     │           │
                  │     fixed point reached            │           │
                  │              │                     │           │
                  │              ▼                     │           │
                  │     unresolved remain? ── yes ─→ Err           │
                  │              │ no                              │
                  │              ▼                                 │
                  │           return IR ←───────────────┘          │
                  └────────────────────────────────────────────────┘
```

### Tier 1 — cfg-time mini-graph resolver (preserved from earlier work, semantics softened)

The Phase 4 mini-graph resolver from the superseded design **stays in place** for trivial CFG-time resolution.  It still builds a per-`BranchIndirect` mini-graph, runs `ConstantFold + KnownBits + RedundantPhis + LoadReadOnly`, and inspects the producer.  Two changes from the superseded design:

* **Failure no longer errors.**  When the mini-graph can't classify, the resolver returns `Ok(None)` (or an "unresolved" marker), and the CFG builder terminates the region with `RegionTerminator::UnresolvedIndirectBranch { target_vn, addr }`.  The cfg-build phase no longer issues `UnresolvedIndirectBranch` errors — that's now the outer loop's job at fixed point.
* **`known_targets: HashMap<PcodeInsnAddr, ResolvedTargets>`** is consulted before invoking the mini-graph.  If the outer loop has already classified this branch, the cfg builder uses the cached classification directly.

Tier 1's value is **speed**: trivial resolutions (constants, link-register VN match) close in O(region_size) without paying for the full opt pipeline.

### Tier 2 — post-IR resolver (new)

A new strider-level pass that runs **after** the full optimizer pipeline.  For each region whose terminator is `UnresolvedIndirectBranch { target_vn, addr }`:

1. Locate the placeholder `Return` node that anchors `target_vn` in the optimised IR (see `lifting strategy` below).
2. Inspect the producer of `target_vn`'s value-input slot:
   * `IntConst(k)` → `ResolvedTargets::Single(k as u64)`.
   * `InitialVar(vn)` where `vn == cc_link_register_vn` → `ResolvedTargets::LinkRegister`.  **Stack-popped return addresses** land here naturally — after `StackLoadForward` runs on the full graph, a properly-popped return address simplifies to `InitialVar(lr)`.  No special heuristic, no calling-convention stack arithmetic in the resolver.
   * `ValuePhi` whose every input folds to `IntConst(k_i)` → `ResolvedTargets::Multiple([k_1, …, k_n])`.  This is how an indirect branch with multiple constant predecessors resolves correctly across iterations: iteration N might see `Single(k_1)` (only one pred wired so far), iteration N+1 with both preds wired sees the Phi and upgrades to `Multiple([k_1, k_2])`.
   * **Jump-table shape** (round R4 only) — pattern-match `Load(IntAdd(IntConst(base), IntMul(idx, IntConst(stride))))` where `idx`'s range is bounded.  Bounds via `KnownBits.max()` first; predecessor `If`-walk fallback.  Read N entries from `.rodata` via the existing `MemReader`; produce `ResolvedTargets::Multiple(vec![target_0, …, target_{N-1}])`.
   * Anything else → still unresolved (no error; the outer loop decides at fixed point).

Tier 2's value is **power**: it sees the full IR after `StackLoadForward` + `LoadReadOnly` + cross-region phis.  This is where `pop pc` and jump tables resolve.

### IR caching across iterations — every instruction is lifted exactly once

The persistent state across the loop is the IR `Graph` itself, plus a `RegionIrCache: HashMap<PcodeInsnAddr, RegionIrEntry>` keyed by region start address.  The cache holds, for each region:

* the region's entry control / memory `NodeOutputId`s,
* the region's exit control / memory `NodeOutputId`s,
* the region's exit `vn_to_value` map (for consumers that read varnode values at the region boundary),
* the region's entry `ControlPhi` / `MemPhi` `NodeId`s, so new predecessors can be wired by adding inputs to the *existing* phi nodes,
* a back-pointer to the `RegionId` in the *current* CFG.

**Why reusing the body's IR is correct.**  The body of a region depends on (i) earlier-in-region nodes wired at lift time, and (ii) the region's *entry-boundary* phi nodes for control / memory / per-variable reads.  Adding a new predecessor adds an *input* to those existing phi nodes — it does not move them or create new ones.  The body still reads the same `NodeOutputId`s.  The pcode-to-IR translation is therefore deterministic from the region's pcode and is reusable across iterations.

**Lifting protocol on every iteration:**

1. Build (or rebuild) the CFG with `known_targets`.
2. For each `Region` in the CFG:
   * If `RegionIrCache` already has an entry for that region's start address → **reuse** it.  No pcode-to-IR work.
   * Otherwise → lift the region's pcode into the persistent `Graph` via `FunctionBuilder`, populate the cache.
3. Stitch edges between regions using the cached entry/exit handles.  When an existing region gains a new predecessor, append a new input to its entry `ControlPhi` / `MemPhi` / per-var phi nodes (which the cache pinned at lift time).
4. Run the **stable optimizer subset** (see below).

**Stable vs destructive optimizer passes.**  A pass is *stable across iterations* if its rewrites can survive the addition of new phi inputs.  The optimizer pipeline splits into two tiers:

| Pass | Stable? | Reason |
|---|---|---|
| `ConstantFold` | ✓ | Rewrites operands; old nodes become dead but stay in the arena; phi inputs widen without disturbing folded successors. |
| `KnownBits` | ✓ | Annotation-driven rewrites; recomputes from current phi inputs on each run. |
| `LoadReadOnly` | ✓ | Rewrites a `Load` to an `IntConst`; the new `IntConst` becomes a stable consumer. |
| `StackStoreDetect`, `StackLoadForward` | ✓ | Rewrite Store/Load nodes in place; no removal of dependents. |
| `RedundantPhis` | ✗ | Removes phi / ControlState nodes by detaching inputs and rewiring consumers.  When a later iteration adds a new predecessor, the consumers now point past the phi and cannot be restored. |
| `DeadBranchElimination` | ✗ | Removes If-true/false edges based on a transient `BoolConst` condition.  A later iteration may make the condition Phi-dependent again, but the branch is gone. |

**Pipeline split.**  Intermediate iterations run only the stable subset.  The final iteration — once the fixed point is reached and tier 2 has produced no new resolutions — runs the **full** pipeline including `RedundantPhis` and `DeadBranchElimination`, producing the optimized IR that downstream consumers expect.

Tier 2's classification is robust to whether the destructive subset has run:

* `ValuePhi` whose every input folds to `IntConst(k_i)` → `Multiple([k_i])` (after dedup).
* If `RedundantPhis` *did* run and folded the phi to `IntConst(k)`, tier 2 sees `IntConst(k)` → `Single(k)`.

Both classifications produce the same induced edge set.  Convergence is unaffected.

**Stale cache invalidation:**

* In-place IR edits (Return → Call+Return) mutate nodes inside a single region's subgraph.  The cached entry's *control* / *memory* boundary handles don't change (the placeholder Return becomes a Call+Return; control still exits via a Return-shaped tail), so the cache stays valid.
* CFG split (`split_region`): when a region splits because a new branch lands in its interior, the cache must be updated.  The first half keeps its cache entry; the second half gets a fresh one.  This is mechanical — splits already exist in cfg, we add the cache update at split time.
* Stale predecessor edges: handled in protocol step 3 above.

**Practical guarantee:**

> Each pcode instruction in the function is lifted to IR **at most once** across the entire fixed-point analysis, regardless of how many CFG rebuilds occur.

The cost across N iterations is `O(initial_lift_cost + N * stable_optimizer_cost + final_full_optimizer_cost)`.  The stable optimizer subset is ~70% of today's pipeline by pass count and the bulk of its work is amortised — it finishes quickly on already-folded subgraphs.

### In-place IR edits vs CFG rebuild

A tier-2 resolution falls into one of two buckets that determine whether we can edit the IR in place or must rebuild the CFG:

| `ResolvedTargets` variant | Adds new edges? | In-place update? | Cost |
|---|---|---|---|
| `LinkRegister` | No (terminal) | **Yes** — placeholder `Return` already has the right shape; just promote the side-table entry to "resolved". | One side-table write. |
| `Single(target)` where `is_branch_tail_call(target)` | No (terminal) | **Yes** — replace placeholder `Return(target_vn)` with `Call(IntConst(target)) → Return(ret_vars)`.  Local IR rewrite. | A few node mutations. |
| `Single(target)` intra-fn | **Yes** — needs a new `Branch` edge from this region to a region at `target`. | No — must rebuild CFG with `target` in `known_targets`. | Full iteration. |
| `Multiple(targets)` | **Yes** — N new edges. | No — must rebuild CFG. | Full iteration. |

The first two cases — `LinkRegister` and tail-call `Single` — are by far the most common in real code (`bx lr`, `pop pc`, gcc-emitted indirect tail calls).  They cover the entire 4-ARM-test regression.  Handling them as in-place edits means **most functions with at least one `BranchIndirect` resolve in ZERO additional iterations** — one tier-2 pass classifies, one batch of local IR edits, done.

CFG rebuilds are reserved for the genuinely structural cases: intra-fn `Single` and `Multiple` jump tables.  Those are where new code becomes reachable and the CFG must be re-explored.

### Outer loop — fixed-point orchestration

Strider's `analyze` entry orchestrates the iteration.  Note that
`graph` and `region_ir_cache` are persistent across iterations —
they're constructed before the loop and only **extended** thereafter,
never rebuilt.

```rust
// Persistent state — survives every iteration; only ever grows.
let mut graph = Graph::new();
let mut function_builder = FunctionBuilder::new(&mut graph, ...);
let mut region_ir_cache: HashMap<PcodeInsnAddr, RegionIrEntry> = HashMap::new();

// First pass — same shape as today's analyze entry.
let mut cfg = Builder::new(sleigh, start_addr, opts).build()?;
lift_new_regions_into(&mut function_builder, &mut region_ir_cache, &cfg)?;
run_stable_optimizer_subset(&mut graph)?;

// Fast-path: most functions have NO BranchIndirect at all.  In that
// case there are no UnresolvedIndirectBranch regions, no tier-2 work,
// no iteration.  We pay zero overhead beyond a one-line check, then
// run the full optimizer (incl. RedundantPhis / DeadBranchElim) and
// return.
if !cfg.has_unresolved_indirect_branches() {
    run_destructive_optimizer_subset(&mut graph)?;
    return Ok(graph);
}

// Function has at least one BranchIndirect.  Enter the loop.
let mut known_targets: HashMap<PcodeInsnAddr, ResolvedTargets> = HashMap::new();
let pending_at_iter_0 = cfg.unresolved_indirect_branch_count();

for _iteration in 0..=2 * pending_at_iter_0 + 4 {
    let latest = run_tier_2_resolver(&graph, &region_ir_cache)?;

    // Apply in-place edits for terminal classifications (LinkRegister
    // and tail-call Single).  These mutate `graph` directly — no
    // rebuild of the IR or CFG.  The cached region entries' boundary
    // handles stay valid.
    let needs_rebuild = apply_in_place_edits(
        &mut graph, &mut region_ir_cache, &latest, &cfg,
    );
    // `needs_rebuild` returns the subset of `latest` whose targets
    // require new CFG edges (intra-fn Single, Multiple).  If empty,
    // we don't touch the CFG.

    if needs_rebuild.is_empty()
        && edge_set_of(&latest) == edge_set_of(&known_targets)
    {
        // Fixed point reached without needing a rebuild.
        if cfg.has_unresolved_indirect_branches() {
            return Err(ErrorKind::UnresolvedIndirectBranch(
                first_unresolved_addr(&cfg),
            ).into());
        }
        // Run the destructive subset only at the fixed point — the
        // graph shape is now final, so RedundantPhis / DeadBranchElim
        // can safely remove nodes without violating the cache.
        run_destructive_optimizer_subset(&mut graph)?;
        return Ok(graph);
    }

    known_targets = latest;
    if !needs_rebuild.is_empty() {
        // Structural change: rebuild the CFG only.  The IR Graph is
        // PRESERVED; lift_new_regions_into walks the new CFG and
        // skips any region already present in `region_ir_cache`,
        // honouring the "every instruction is lifted exactly once"
        // contract.  Newly-discovered regions are appended to the
        // existing graph and stitched in via the cache's edge handles.
        cfg = Builder::new(sleigh, start_addr, opts)
            .with_known_targets(&known_targets)
            .build()?;
        lift_new_regions_into(
            &mut function_builder, &mut region_ir_cache, &cfg,
        )?;
        run_stable_optimizer_subset(&mut graph)?;
    }
}
return Err(ErrorKind::IndirectResolutionDidNotConverge);
```

**Common-case cost: zero.**  Functions with no `BranchIndirect` skip the loop.

**`Return` / tail-call-only case: one extra tier-2 pass + one batch of in-place edits.**  No CFG rebuild, no extra optimizer run.  The 4-ARM-regression workload, gcc tail-call workloads, and `pop pc` all land here.

**Jump-table / intra-fn-target case: one rebuild per resolution wave** (typically 1, occasionally 2).

**No panics anywhere.**  Both the iteration cap and the final-state-unresolved arm return typed errors.

The iteration cap `2 * pending_at_iter_0 + 4` is a soundness-bug guard.  Hitting it means the resolver violated monotonicity — surfaced as `Err(IndirectResolutionDidNotConverge)`.

### Lifting strategy for `UnresolvedIndirectBranch` regions

Strider lifts a region whose terminator is `UnresolvedIndirectBranch { target_vn, addr }` by:

1. Reading `target_vn` to obtain its current `NodeOutputId`.
2. Emitting a synthetic `Return(target_value)` (single-input return).  This anchors `target_value` in the IR's reachable graph so the optimizer simplifies it like any other return-bound expression.
3. Recording the mapping `addr → (region_id, target_value)` on a side-table that tier 2 walks.

The synthetic Return is a stable placeholder — it preserves the *current* misclassification semantics (BranchIndirect-as-Return) for downstream code while letting the optimizer chew on the value.  When tier 2 resolves the branch, the *next iteration's* CFG build wires the real terminator (`Branch` / `TailCall` / `Return-via-LR`) and the synthetic Return goes away.

### Resolution feedback semantics

`known_targets: HashMap<PcodeInsnAddr, ResolvedTargets>` is keyed by the offending `BranchIndirect`'s p-code address — stable across iterations because the same machine instruction lifts to the same pcode address regardless of which iteration we're in.

Each iteration **fully re-classifies** every `BranchIndirect` from the current optimised IR; the new map *replaces* the old one in the outer loop.  We do not merge / accumulate deltas, because tier 2's classification can legitimately *upgrade* across iterations (see below) — discarding the old map and trusting the latest classification keeps the cache consistent with the current IR.

The outer loop's convergence test compares the **induced edge set** of consecutive iterations' maps:

```
edge_set(map) = ⋃ over (addr, targets) in map of {(addr → t) for t in targets}
```

Fixed point ↔ same edge set as the previous iteration.

### Soundness and termination argument

**Tier-2 classification rules** (the conservative set):

* `IntConst(k)` → `Single(k)` (sound: producer is a literal constant).
* `InitialVar(vn)` where `vn == cc_link_register_vn` → `LinkRegister`.
* `ValuePhi` whose every input folds to `IntConst(k_i)` → `Multiple([k_1, …, k_n])`.
* Jump-table shape (R4) where bounds are provable → `Multiple([k_1, …, k_n])`.
* anything else → still unresolved.

**Why this is sound across iterations.**  Adding edges to the CFG (as a result of last iteration's tier-2 output) can only:
1. Make more code reachable.
2. Add inputs to existing `Phi` nodes (where new predecessors merge).
3. Create new `Phi` nodes at regions that previously had a single predecessor.

It cannot:
* Remove pcode from a region (regions are the same; their pcode is fixed).
* Cause `IntConst` producers to disappear (constants are tied to pcode; same pcode → same constant).
* Invalidate an `InitialVar(lr)` classification (function-entry initial-value is a global property of the function, not a per-iteration artefact).

The two ways tier 2's classification of a single branch can change across iterations:

* **`Single(k)` → `Multiple([k, …])`** when a new predecessor adds a different constant to the now-Phi target.  Edge set strictly grows.
* **Unresolved → resolved** when the optimiser can newly fold a value because more reachable code reveals a constant.  Edge set strictly grows.

In every legal transition, the **edge set is monotone non-decreasing**.  The set is bounded above by `unresolved_count_at_iter_0 × max_phi_inputs_per_branch`, which is finite.  Therefore the loop terminates in finitely many iterations.

The iteration cap (`2 * pending + 4`) is a **soundness guard** for resolver bugs that violate the monotonicity invariant; it returns `Err(IndirectResolutionDidNotConverge)`.  No panic.

## Components and boundaries

| Component | Crate | Responsibility |
|---|---|---|
| `RegionTerminator::UnresolvedIndirectBranch{vn,addr}` | `cfg` | Placeholder terminator marking deferred regions. |
| `Builder::with_known_targets` | `cfg` | Threads tier-2 results back into the next CFG build. |
| `RegionIrCache` | `strider` | Persistent map `RegionStartAddr → RegionIrEntry`.  Guarantees each pcode insn is lifted to IR at most once across all iterations. |
| `lift_new_regions_into` | `strider` | Walks a CFG; for cached regions it reuses the existing IR subgraph; for new ones it lifts pcode→IR and populates the cache.  Stitches edges via cached boundary handles. |
| Tier-1 resolver (existing, softened) | `cfg::indirect_resolve` | Mini-graph; failure now returns Unresolved instead of erroring. |
| `IrStrider` lifting of `UnresolvedIndirectBranch` | `strider` | Emits placeholder `Return(target_vn)`; records `(addr, region, target_value)` for tier 2. |
| Tier-2 resolver | `strider::indirect_resolve_tier2` (new) | Pattern-match producers in the optimised IR; produce `ResolvedTargets`. |
| Outer fixed-point loop | `strider::analyze` | Orchestrate iterate/resolve/feed-back; final-state strict-failure (typed error, no panic). |
| `IndirectResolutionDidNotConverge` error | `cfg::ErrorKind` (or `strider`) | Typed error returned when the iteration cap is hit; catches soundness bugs in the resolver without panicking. |
| `UnresolvedIndirectBranch` error | `cfg::ErrorKind` (existing, repurposed) | Returned at fixed point if any branch remains unresolved. |

Each unit has a clear API surface: tier 1 takes `(insns, target_vn, sleigh, …) → Option<ResolvedTargets>`; tier 2 takes `&BuiltFunctionGraph → HashMap<PcodeInsnAddr, ResolvedTargets>`.

## Test strategy

> **The caching layer is fragile.**  A bug in cache-reuse, phi-extension, or pipeline-tier-selection produces silently-wrong IR — wrong constants, wrong reachability, wrong call semantics — that downstream consumers won't notice until pattern queries return wrong answers.  Every fragile invariant gets multiple tests, and every implementation site gets an inline comment explaining *why* the operation is correct.

### Unit tests by concern

#### Tier 1 (existing, semantics softened)

1. `tier_1_unresolved_now_produces_placeholder_terminator` — what previously errored now produces `RegionTerminator::UnresolvedIndirectBranch{vn,addr}`.
2. `tier_1_resolved_cases_unchanged` — `LinkRegister` / `Single(K)` paths still work as in Phase 5.

#### `cfg::Builder::with_known_targets`

3. `known_target_overrides_tier_1_mini_graph` — when `known_targets` has an entry for a `BranchIndirect`, the cached classification is used and tier 1 is not invoked.
4. `unknown_branch_falls_through_to_tier_1` — entries not in `known_targets` get tier 1 treatment.
5. `multiple_targets_produces_multiple_edges` — `Multiple([K1,K2,K3])` produces 3 successor edges.
6. `tail_call_target_produces_no_edge` — `TailCall { target }` is terminal; no successor edge.

#### Tier 2 resolver — one positive per variant + negatives

7. `tier_2_int_const_to_single` — target VN's producer is `IntConst(k)` after `ConstantFold` → `Single(k)`.
8. `tier_2_initial_var_lr_to_link_register` — producer is `InitialVar(lr_vn)` → `LinkRegister`.
9. `tier_2_pop_pc_resolves_via_stack_load_forward` — pcode-level `tmp = load[sp]; sp += 4; bx tmp` after the stable optimizer subset (incl. `StackLoadForward`) produces `InitialVar(lr_vn)` for the target → `LinkRegister`.  This is the test that proves the design closes the 4 ARM regressions.
10. `tier_2_push_target_pop_pc_does_not_resolve_to_link_register` — `push 0x1000; pop pc` produces `Load(IntSub(InitialVar(sp),4))`-shaped target after the stable subset → NOT classified as LinkRegister.  Negative test for the soundness gap that killed the prior heuristic.
11. `tier_2_phi_of_int_consts_to_multiple` — `ValuePhi(IntConst(K1), IntConst(K2))` → `Multiple([K1,K2])`.
12. `tier_2_phi_with_non_const_input_unresolved` — `ValuePhi(IntConst(K), InitialVar(r0))` → still unresolved.
13. `tier_2_jump_table_with_known_bits_bound` (R4) — pattern `Load(IntAdd(IntConst(base), IntMul(idx, IntConst(stride))))` with `KnownBits.max(idx) = N-1` → `Multiple([table[0], …, table[N-1]])`.
14. `tier_2_jump_table_with_predecessor_if_bound` (R4) — same shape with bound from a predecessor `If(idx < N)`.
15. `tier_2_no_classification_returns_unresolved` — opaque target produces `None`/Unresolved (no error inside tier 2).

#### Outer loop and convergence

16. `outer_loop_zero_iter_when_no_branch_indirect` — function without `BranchIndirect` skips the loop entirely; full pipeline runs once; returns IR.
17. `outer_loop_one_iter_for_link_register_only` — function with one `bx lr` resolves in iteration 0 via in-place edit; no rebuild; one full-pipeline run at the end.
18. `outer_loop_one_iter_for_tail_call_only` — same shape, target-const resolution, in-place edit.
19. `outer_loop_rebuilds_for_intra_fn_single` — intra-fn `Single(K)` triggers a CFG rebuild; second iteration converges.
20. `outer_loop_rebuilds_for_jump_table` (R4) — `Multiple` triggers a rebuild; second iteration converges.
21. `outer_loop_cascading_resolution` — `Multiple([A,B])` resolves; the freshly-explored A's region contains another `BranchIndirect` that resolves only after A is built.  Verifies multi-iteration convergence.
22. `outer_loop_fixed_point_with_unresolved_returns_error` — fixed point reached with one branch still unresolved → `Err(UnresolvedIndirectBranch(addr))`.
23. `outer_loop_iteration_cap_returns_typed_error` — synthetic resolver that violates monotonicity; loop hits cap; returns `Err(IndirectResolutionDidNotConverge)`.  No panic.

#### Cache invariants — the fragile core

24. `cache_lifts_each_instruction_exactly_once` — instrument `lift_new_regions_into` with a counter; run a 2-rebuild scenario; assert counter equals the function's total pcode insn count.
25. `cache_reuse_preserves_body_node_outputs` — snapshot the `NodeOutputId`s of a region's body nodes after iter 0; run iter 1 with a CFG rebuild that doesn't touch this region; assert the same `NodeOutputId`s point to byte-identical node kinds + inputs.
26. `cache_phi_extension_adds_input_not_node` — region X has 1 predecessor in iter 0; iter 1 wires a 2nd predecessor for X; assert (a) X's `ControlPhi` for each var is the SAME `NodeId` as iter 0, (b) it has 2 inputs after iter 1, (c) body nodes still reference the same phi `NodeOutputId`.
27. `cache_split_updates_correctly` — region Y splits because a new branch lands in its interior; the first half retains its cache entry; second half gets a fresh entry; no re-lift of either half's pcode.
28. `cache_in_place_edit_does_not_invalidate_cache` — apply an in-place Return-to-Call+Return edit; verify the cache entry for that region is still considered valid for the next iteration; no re-lift.
29. `cache_key_stability_across_rebuilds` — PcodeInsnAddr keys map to the same cache entries across multiple CFG rebuilds.

#### Optimizer-tier separation — the other fragile core

30. `intermediate_iter_does_not_run_redundant_phis` — inspect graph after a single intermediate iteration; assert no `ControlPhi` / `MemPhi` was eliminated (every phi node from lift time is still alive in the arena, with an alive output id).
31. `intermediate_iter_does_not_run_dead_branch_elim` — similarly for `If` nodes with constant-folded conditions: the dead branch is still wired (its successor `ControlState` still has the dead predecessor as an input).
32. `final_iter_runs_destructive_subset` — at fixed point, assert `RedundantPhis` and `DeadBranchElim` HAVE run (single-input phis are gone; constant-condition Ifs are gone).
33. `tier_2_classification_robust_to_redundant_phis` — same input function with destructive subset enabled vs disabled; tier 2's induced edge set is identical.
34. `stable_subset_is_idempotent` — run stable subset twice in a row; second run produces zero diff.

#### In-place IR edit semantics

35. `in_place_link_register_promotes_placeholder_to_return` — placeholder `Return(target_vn)` stays a Return; the side-table marks it resolved; control / memory exits are unchanged.
36. `in_place_single_tail_call_rewrites_to_call_then_return` — placeholder `Return(target_vn)` is replaced with `Call(IntConst(target)) → Return(ret_vars)`.  Verify control chain: pre-call control feeds the Call's control input; Call's control output feeds the Return's control input.
37. `in_place_edits_preserve_use_lists` — after both edit types, IR `validate` passes (use-lists are consistent, no zombie inputs).

#### Regression: the four ARM tests

38–41. The 4 currently-ignored ARM tests (`test_tail_caller`, `test_nested_loops`, `test_escape_via_ptr`, `test_bit_test_zero`) un-ignore and pass after R2 lands, via the natural `pop pc` → `InitialVar(lr)` resolution path.

### Comment discipline at implementation time

Every site that mutates the cache, extends a phi, applies an in-place edit, or branches on the optimizer tier MUST carry an inline comment explaining the correctness invariant.  The reviewer's question "why is this safe across the next iteration?" must have its answer in the line above the operation, not in a separate doc.  Specifically:

* `lift_new_regions_into` — comment at the cache-hit branch explaining why reuse is correct (body refs are stable; predecessor adds touch only entry phis).
* `apply_in_place_edits` — comment at each variant explaining why the edit doesn't invalidate the cached boundary handles.
* `run_stable_optimizer_subset` — comment explaining the omitted passes and *why each one* is destructive across iterations.
* `run_destructive_optimizer_subset` — comment explaining why this is only safe at fixed point.
* Phi-extension call site — comment explaining that we're adding to an existing node, not creating one, and why this matches the cached body refs.

Reviewers should reject any commit in this area lacking these comments.
* **Integration**: the 4 ARM tests currently ignored under BUG-5 must un-ignore and pass.  R4 adds a jump-table fixture per `fixtures/cases/`.

## Out-of-scope confirmations

* **Cross-region tier-1 chasing** — unchanged stance: out of scope.  Tier 2 supersedes it.
* **`fn_max_size` default** — unchanged: no default, caller must set it explicitly.
* **Strict-failure semantic** — preserved, but **moved** from cfg-build time to outer-loop fixed-point time.
* **CallIndirect** — unchanged: still untouched.  `blx lr` lifts to `Call(unknown)`.
* **Incremental CFG/IR rebuild** — out of scope.  Round 1 rebuilds from scratch each iteration.

## Future work (not part of this round)

* **Incremental rebuild.**  Avoid full CFG/IR teardown each iteration; only re-explore newly-discovered targets.  Only worth doing if profiling shows the rebuild dominates analyze time.
* **Pattern-driven tier 2.**  Today's tier 2 hand-codes the producer-shape match.  Reformulating as `pattern` crate queries would let downstream consumers extend the resolver without modifying strider.
* **Cycle-detection diagnostics.**  When `MAX_ITERATIONS` is hit, dump the unresolved set and the candidate resolutions per iteration to help debug a runaway resolver.

## Phasing summary

R1 (`UnresolvedIndirectBranch` variant + softened tier 1 + IR placeholder) — wired but no behaviour change visible to tests.
R2 (tier-2 resolver: stack-popped + IntConst + InitialVar(lr)) — closes the 4 ARM ignores.
R3 (outer loop + iteration cap + final-state strict-failure) — restores end-to-end strict semantics.
R4 (jump-table extension: `Multiple(Vec<u64>)` + `RegionTerminator::Switch`) — closes BUG-5 jump-table case.
R5 (fixture + per-arch tests + close BUG-5) — landing rituals.

Detailed step-by-step plan goes into `docs/superpowers/plans/2026-04-27-indirect-branch-fixedpoint.md` after this spec is approved.
