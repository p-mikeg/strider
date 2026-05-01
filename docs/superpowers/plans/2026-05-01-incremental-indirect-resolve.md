# Task 17 — Incremental indirect-branch resolution: deferred follow-up

> **Status:** plan only.  Not implemented.  Original follow-up to the
> 2026-05-01 scaling-bottlenecks plan; broken out into its own doc
> because it's a multi-day effort that warrants a dedicated branch and
> review pass.

## Why this is a separate effort

The 2026-05-01 scaling-bottlenecks plan landed Tasks 1–6, 10, 11,
14, 18–21, 23, and 24 on `feature/ai`.  Tasks 23, 10, and 14 are
caches that *speed up the rebuild path*.  Task 17 is structurally
different: it *replaces* the rebuild path with an incremental
delta-lift.  Done correctly, it makes Tasks 23, 10, and 14
redundant — they can be removed.  Done incorrectly, it silently
miscompiles.

This plan is dependably non-trivial (5–10 days) and requires a
strict equivalence gate.  Hence a separate branch + review.

## Goal

Replace `LoopState::rebuild()`'s "build a fresh `Cfg`, lift a fresh
`BuiltFunctionGraph`, run optimizer from scratch" with an in-place
mutation that splits affected regions, appends edges, lifts only
new instructions, and grows phi nodes at affected joins.  The
unaffected portion of the IR keeps its existing `NodeId`s.

## Phase 1 — `Cfg` post-build mutation API

**Where:** `crates/cfg/src/cfg/`

**What:**

1. Move (or expose) `start_addr_to_region_id` from `Builder` onto
   `Cfg` so post-build callers can query "is this address a region
   start?".
2. Promote `cfg/builder/split.rs::split_region` from a
   builder-private helper to a `Cfg`-level method.
3. Promote the per-region lift loop from `Builder::build` into a
   helper that can be invoked on a finalized `Cfg` (taking the
   existing `Cfg` as the target graph).
4. Add `Cfg::extend_with_targets(&mut self, dispatch_addr,
   new_targets) -> Result<Vec<EdgeAddedTo>>` returning per-target
   what happened:

   ```rust
   pub enum EdgeAddedTo {
       ExistingRegionStart(RegionId),
       SplitRegion { original: RegionId, new_tail: RegionId },
       NewRegion(RegionId),
   }
   ```

**Test plan:**

- `cfg_extend_existing_region_start`: target points at a region
  that already starts there → edge added, no split, no new region.
- `cfg_extend_mid_region`: target points inside a region → split,
  tail becomes new region, edge added to tail.
- `cfg_extend_unexplored`: target points at an address with no
  region → new region lifted, edge added.
- **Equivalence gate:** for every existing fixture, build the Cfg
  the legacy way (`Builder::with_known_targets(...).build()`) and
  via incremental extension (start with empty `known_targets`, then
  call `extend_with_targets` for each entry).  Assert the resulting
  `Cfg`s are structurally identical.

**Estimate:** 2–3 days.

## Phase 2 — IR delta lift in `Strider`

**Where:** `crates/strider/src/strider/pipeline.rs` and
`crates/strider/src/orchestrator.rs`

**What:**

1. Refactor `Strider::analyze_cfg` into a stateful builder
   `IrLifter` that survives across orchestrator iterations:

   ```rust
   pub struct IrLifter<'a> {
       strider: &'a Strider,
       graph: BuiltFunctionGraph,
       region_handles: HashMap<RegionId, RegionLiftHandles>,
       known_vns: FxHashSet<rsleigh::Vn>,
   }
   ```

   Initial construction does what `analyze_cfg` does today (lift
   the whole CFG once).  Subsequent mutations come through:

2. `IrLifter::extend_for_delta(&mut self, cfg, delta_outcome) -> Result<()>`
   that handles each `EdgeAddedTo` variant:

   - **`ExistingRegionStart(target_region)`**: append a control
     input to that CS, append phi inputs at the affected joins
     sourced from the dispatch site's `exit_vn_to_value`.
   - **`SplitRegion { original, new_tail }`**: lift the new_tail's
     instructions seeded with `original`'s exit values.  Rewire the
     original's terminator from "fallthrough" to a Branch edge.
   - **`NewRegion(target_region)`**: lift the new region from
     scratch using the dispatch site's exit values as initial state.

3. `apply_phi_growth_at_join(&mut graph, cs_id, new_pred_exit) ->
   Result<()>` helper.  Append one input slot to the CS, one value
   input per phi child of the CS sourced from `new_pred_exit`.

4. Replace `LoopState::rebuild()` with `LoopState::extend()`:

   ```rust
   fn extend(&mut self) -> Result<()> {
       let delta = self.compute_delta_since_last_iter();
       let cfg = self.cfg.as_mut().ok_or(...)?;
       let outcome = cfg.extend_with_targets(&delta)?;
       let lifter = self.lifter.as_mut().ok_or(...)?;
       lifter.extend_for_delta(cfg, &outcome)?;
       self.region_index.patch_from(&lifter.region_handles, &outcome);
       self.opts.strider.build_stable_optimizer_pipeline()
           .run_on_built(lifter.graph_mut())?;
       self.unresolved = lifter.collect_unresolved();
       Ok(())
   }
   ```

5. **Owner shifts**: `LoopState` now owns `cfg: Option<Cfg<...>>`
   and `lifter: Option<IrLifter>` across iterations.  Sleigh is
   harvested back from `cfg` only at finalization.

**Test plan:**

- **The critical equivalence test.**  For every existing fixture
  that triggers a non-trivial fixed-point loop
  (`x86/complex::complex_dispatch`, `x86/indirect_branch::main`,
  `x64/complex::complex_dispatch`):
  - Run the orchestrator twice — once via the legacy `rebuild`
    path (kept temporarily as a fallback during development), once
    via `extend`.
  - Assert the final `BuiltFunctionGraph`s are **structurally
    equivalent** (same `NodeKind` tree from each entry-reachable
    node, same edges, same phi shapes — `NodeId`s won't match, but
    everything else should).
  - Compare via a normalised dump or a structural-hash function.
- Per-phase unit tests:
  - `extend_existing_region_appends_phi_input`
  - `extend_split_region_preserves_original_exits`
  - `extend_new_region_threads_initial_vars_correctly`
  - `extend_handles_in_place_edits_concurrently_with_delta_lift`
    (in-place edits run first per `step()`; the delta lift runs
    against the post-edit graph)

**Estimate:** 5–7 days.  The phi-growth-at-join logic is the
riskiest part.

## Phase 3 — Optimizer worklist seeded by dirty set (optional)

Defer until Phase 1 + 2 are benched.  Most of the per-iteration
cost should now be the optimizer running on the whole graph;
Phase 3 makes that incremental too.  Depends on Tasks 7 + 8
(`KnownBits` cache, `RedundantPhis` cache) from the original plan.

## Plumbing risks

Ranked by impact:

1. **Phi input order.**  A `VarPhi(vn)` at a CS has value inputs in
   the same order as the CS's control inputs.  If we grow the CS's
   control inputs and the phi's value inputs in different orders,
   the IR is silently wrong.  Pin via a test that asserts
   `phi.input[i+1]` corresponds to `cs.input[i]` for every CS-phi
   pair after a delta.
2. **CSE cache invalidation.**  Mutating an existing cacheable
   node's inputs (via `add_node_input`) invalidates its cache key.
   The IR's `evict_cache_entry_if_cacheable` handles this on the
   normal path; the delta-extend path must exercise it.
3. **`RegionIndex::patch_from`.**  After Phase 2, `RegionIndex`
   entries for *unaffected* regions stay valid across iterations;
   only entries for *new* and *split-tail* regions need updating.
4. **In-place edits and delta lifts in the same iteration.**
   In-place edits run first (per `step()`), then if the edge set
   changed, the delta lift runs against the post-edit graph.  Pin
   with a test triggering both in the same iteration.

## Cleanup after Phase 1 + 2 land

Once Phase 2 is correct and the equivalence gate holds, these
caches from the rebuild path become redundant and should be
removed in the same branch:

- **Task 23** (`cfg::DecodeCache`): the cfg is no longer rebuilt,
  so per-address decodes are paid once at iter 0 and never again.
  The cache is dead.
- **Task 10** (`vn_cache`/`vn_cache_region_count` on `LoopState`):
  the IR lifter owns its own monotonically-growing `known_vns`
  set; the orchestrator-level vn cache duplicates that.
- **Task 14** (`Arc<HashMap>` for `exit_vn_to_value`): the
  `RegionIndex` is patched in place per Phase 2 instead of being
  rebuilt every iteration; cloning becomes per-affected-region
  instead of per-region, and the `Arc` wrapper's win evaporates.
  Keep the `Arc` only if benchmarks show contention from
  `RegionExitInfo` clones during in-place-edit handling.

**Task 11** (`largest_container` on `FunctionBuilder`) stays —
it's independent of the resolve loop and wins at IR construction
time regardless.

## Definition of done

- All existing tests pass.
- Equivalence test on every fixture that triggers Rebuild passes.
- End-to-end bench shows >5% speedup on at least three of the six
  existing fixtures (small fixtures may not see this; rerun after
  scaling Task 24's stress benches up to multi-thousand-region
  graphs).
- The three obsolete caches (Tasks 23, 10, 14) are removed.
- Full code review via `feature-dev:code-reviewer`.
