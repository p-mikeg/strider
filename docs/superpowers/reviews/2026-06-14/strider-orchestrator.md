# Deep audit — `strider-orchestrator`

Date: 2026-06-14
Scope: `crates/strider-orchestrator/src/lib.rs` (the only source file, 466 LOC),
plus `examples/` and `benches/scaling.rs`. The crate is the `Strider::analyze`
top-level driver, the indirect-branch resolution loop, `AnalyzeResult`, and the
re-exported lift surface.

Audit anchored to the real call paths into `strider-lift` (`build_ir_with` /
`LiftOutcome.unresolved_branches`), `strider-opt`
(`OptimizerPipeline::run` / `IndirectBranchClassify` / `OptCtx`), and
`strider-cfg` (`region_builder::process_branch_indirect` / `known_targets`).

---

## Findings

### OR-1 — Dead / unreachable indirect branches are reported as "unresolved"
- **Dimension:** SOUNDNESS (code vs itself; contract discrepancy)
- **Severity:** MED
- **Confidence:** HIGH
- **Location:** `src/lib.rs:241-244` (result assembly) + `src/lib.rs:285-289`
  (`unresolved` sourced from the lift) vs
  `crates/strider-opt/src/post_opt/indirect_branch_resolve/mod.rs:182-189,206`
  (classifier doc + `function.walk()`).
- **What & why (verified):** `unresolved` is `LiftOutcome.unresolved_branches`
  — the **complete** lift-time placeholder list, with no reachability filter.
  `apply_resolutions` only clears entries the classifier produced, and the
  classifier (`IndirectBranchClassify::apply`) walks `function.walk()`
  (entry-reachable nodes only), so a placeholder that the node-removing passes
  proved unreachable is **never visited** → never resolved. At finalize,
  `analyze` maps every remaining `unresolved` addr into
  `unresolved_indirect_branches` (lines 241-244) with no liveness check. So a
  *dead* indirect branch is reported as unresolved. This directly contradicts
  the classifier's own contract comment ("a dead indirect branch needs no
  resolution and is silently dropped rather than reported unresolved",
  mod.rs:188-189). A caller that asserts `unresolved_indirect_branches.is_empty()`
  for "full resolution" will spuriously fail on functions containing dead
  indirect jumps.
- **Proposed fix:** Before building `unresolved_indirect_branches`, intersect
  `unresolved` against the live `IndirectBranch` placeholders still present in
  the final `function` (e.g. `function.walk()` filtered to `IndirectBranch`, or
  the set of nodes the last classifier pass actually visited). Reporting should
  be "live-and-unclassified", matching the classifier's "silently dropped"
  contract. Add a regression test (see EC-1).

### OR-2 — `Multiple` resolution with any OOB target is permanently unresolvable but classifier keeps re-firing
- **Dimension:** SOUNDNESS / RUNTIME (loop interaction with cfg policy)
- **Severity:** MED
- **Confidence:** HIGH
- **Location:** `src/lib.rs:227-236` (loop) + `apply_resolutions` (333-351) vs
  `crates/strider-cfg/src/builder/region_builder.rs:459-468`.
- **What & why (verified):** When `known_targets[addr] = Multiple([…OOB…])`,
  the CFG builder does **not** seat a `Switch`; it re-emits
  `UnresolvedIndirectBranch` (region_builder.rs:462-467) because a Switch has no
  per-target tail-call escape. Consequence in the loop: the placeholder for
  `addr` reappears on the next rebuild, the classifier reclassifies it to the
  *same* `Multiple`, `apply_resolutions` re-inserts an identical value →
  `edge_set_of` is unchanged → returns `false` → loop terminates (good, no
  infinite loop). But the branch is then reported in
  `unresolved_indirect_branches` even though it *was* classified — the
  classification is silently dropped by the cfg layer. The orchestrator has no
  visibility into "classified but un-seatable", so the user sees a resolvable
  table as unresolved with no diagnostic.
  - **Termination caveat:** if the *re-lifted* graph yields a **different**
    `Multiple` set each iteration (e.g. value-range widening as more edges are
    seated), the edge set changes every iteration → the loop runs up to
    `MAX_RESOLUTION_ITERATIONS = 256` before the backstop fires. Bounded, but
    256 full CFG rebuild + optimize passes on a pathological input is a real
    cost cliff, and the doc's "loop normally stops far sooner ... strictly grows
    the bounded map" (lib.rs:315-321) overstates the monotonicity guarantee —
    `apply_resolutions` *overwrites* entries, it does not only grow them.
- **Proposed fix:** Either (a) have the cfg layer report `Multiple`-with-OOB
  back as a distinct resolved-but-unseatable state so the orchestrator can stop
  reclassifying it (don't re-defer a site whose `known_targets` entry already
  matches), or (b) in `apply_resolutions`, skip re-inserting a resolution that
  is byte-identical to the existing `known_targets[addr]` and treat a site
  already present in `known_targets` as terminal (don't count its re-deferral as
  "unresolved"). Tighten the lib.rs doc to say the map is **overwritten** and
  convergence relies on the cfg seating placeholders away, not on monotone
  growth.

### OR-3 — Iteration-cap exhaustion is silent (no error, no signal)
- **Dimension:** SOUNDNESS (error propagation) / RUNTIME
- **Severity:** LOW
- **Confidence:** HIGH
- **Location:** `src/lib.rs:227-236`.
- **What & why (verified):** `for _ in 0..MAX_RESOLUTION_ITERATIONS { … }` simply
  falls through when the cap is hit; the function then returns `Ok(...)` with
  whatever partial `function`/`unresolved` exist at that point. A pathological
  classifier/cfg oscillation (see OR-2) that never converges produces a
  silently-truncated, possibly-stale result indistinguishable from a clean
  "everything unresolvable" stop. There is no `log`/`debug_assert`/error to flag
  that the backstop fired.
- **Proposed fix:** On cap exhaustion, either return an `Err` (genuine
  non-convergence is a bug, not an unresolvable branch) or at minimum emit a
  `debug_assert!`/trace. Cheap insurance given the loop is the crate's core
  invariant.

### OR-4 — Same-address indirect branches collide in `known_targets`; last write wins
- **Dimension:** SOUNDNESS (correctness of folding)
- **Severity:** LOW
- **Confidence:** MED
- **Location:** `apply_resolutions` `src/lib.rs:343-349` (`known_targets.insert`)
  and the `node_to_addr` build (338-341).
- **What & why (verified):** `known_targets` is keyed by `PcodeInsnAddr`, but
  `unresolved` is keyed by `NodeId`. If two distinct `IndirectBranch`
  placeholders ever share one `PcodeInsnAddr` (the same machine address
  terminating two regions — possible with overlapping/duplicated code paths),
  they classify independently into two `resolutions` entries that both fold onto
  the *same* `known_targets[addr]`; the second `insert` overwrites the first
  with no merge or conflict check. The cfg builder then seats one terminator for
  both. In practice region terminators are 1:1 with addresses for normal code so
  this is unlikely to bite, hence MED confidence / LOW severity, but the code
  has no guard or assertion documenting the assumption.
- **Proposed fix:** Either assert single-placeholder-per-addr at fold time, or
  if collisions are genuinely possible, merge same-addr resolutions (e.g. union
  the `Multiple` target sets) rather than silently overwriting. At minimum add a
  comment pinning the one-placeholder-per-addr assumption.

### OR-5 — The crate's flagship example bypasses the orchestrator entirely
- **Dimension:** EDGE CASES / docs-vs-code
- **Severity:** LOW
- **Confidence:** HIGH
- **Location:** `examples/orchestrator_demo.rs:24-57`,
  `examples/memory_demo.rs`, `benches/scaling.rs:79-108`.
- **What & why (verified):** CLAUDE.md and the crate docs frame
  `orchestrator_demo` as the main example, but it constructs a
  `strider_orchestrator::Lifter` (the re-exported lift engine), names the
  variable `strider` (misleading — it is a `Lifter`, not a `Strider`), calls
  `build_cfg` / `build_ir` / `pipeline.run` by hand, and **never** calls
  `Strider::analyze` nor exercises indirect-branch resolution. The
  `benches/scaling.rs` "analyze_case" helper likewise drives `Lifter` directly,
  so the resolution loop — the crate's actual reason to exist — has **no**
  benchmark coverage and is shown nowhere as an example. A reader copying the
  example gets a single-pass lift with unresolved indirect branches left in.
- **Proposed fix:** Add an `analyze`-driven path to (or a new) example/bench so
  the orchestrator's headline API is demonstrated and perf-tracked; rename the
  `strider` local in `orchestrator_demo` to `lifter` to stop implying it is a
  `Strider`.

### OR-6 — `compact` is carried in `working` LiftOptions but deliberately unused; live trap for future edits
- **Dimension:** SOUNDNESS (code vs itself) / simplification
- **Severity:** LOW
- **Confidence:** HIGH
- **Location:** `src/lib.rs:215-218, 238-240`.
- **What & why (verified):** `working.compact` is copied from `lift_opts.compact`
  with a comment "Carried for completeness; not used during lifting", while the
  finalize step reads `lift_opts.compact` directly (line 238). So `working`
  carries a field that is intentionally dead within the loop. It is harmless
  today but is a latent hazard: any future code that reads `working.compact`
  (the natural-looking field) would behave correctly only by coincidence, and a
  reviewer must cross-reference two read sites to confirm intent.
- **Proposed fix:** Drop `compact` from the in-loop `working` clone (set it to a
  fixed `false`/`Default` or restructure so `working` only holds loop-relevant
  knobs), keeping the single authoritative read at line 238. Removes a
  per-construction field and the explanatory comment.

---

## Things checked and found SOUND (no finding)

- **Loop termination on real input:** once `known_targets[addr]` is set, the cfg
  builder seats a concrete terminator and the placeholder for that addr does
  **not** reappear (region_builder.rs:431-444), so on normal inputs the
  `unresolved` set strictly shrinks and `apply_resolutions`' edge-set diff
  reliably detects progress. The `Multiple`-OOB case (OR-2) is the one
  exception, and even there the backstop bounds it.
- **Pipeline reuse:** `OptimizerPipeline::run` takes `&self`
  (pipeline.rs:502-506); `add_post_pass` is called once before the loop;
  `OptCtx` (with `indirect_resolutions` + `sp_memo`) is rebuilt fresh per
  `build_lift` via `opt_ctx_for_run` (lib.rs:88-95) and the classifier's output
  is `std::mem::take`-drained (lib.rs:293). No cross-iteration state leak.
- **`OptOptions` clone cost:** `OptOptions` is `#[derive(… Copy)]`
  (pipeline.rs:6), so `opt_opts.clone()` per iteration is trivial — no perf
  concern.
- **`apply_resolutions` complexity:** O(|unresolved| + |resolutions| +
  |known_targets|) per iteration via the `node_to_addr` map; no
  branches×nodes blowup.
- **`edge_set_of` convergence test:** the `BTreeSet<(addr, Option<u64>)>`
  encoding (LinkRegister→None) gives order-independent, dedup'd comparison; unit
  tests (lib.rs:410-465) cover empty/single/multiple/order/dedup.
- **`apply_resolutions` defensive `ok_or_else`:** no live pass mints new
  `IndirectBranch` nodes, so the "classified node has no recorded pcode address"
  error path is correctly defensive and shouldn't fire on real input.

---

## Missing edge-case tests (names + scenarios; not written)

- **EC-1 `analyze_dead_indirect_branch_not_reported_unresolved`** — lift a
  function whose only `IndirectBranch` is in a block the optimizer proves dead
  (e.g. `If(const-false)` guarding the indirect jump). Assert
  `unresolved_indirect_branches.is_empty()`. Currently would fail (OR-1).
- **EC-2 `analyze_multiple_with_oob_target_is_classified_not_oscillating`** — a
  jump table where one entry points outside the function range. Assert the loop
  terminates well under the cap and document the reported state (OR-2).
- **EC-3 `analyze_does_not_hit_iteration_cap_on_real_table`** — instrument /
  expose iteration count for a normal multi-table function and assert it
  converges in a small number of iterations (guards against a future regression
  silently relying on the 256 backstop; OR-3).
- **EC-4 `analyze_two_placeholders_same_pcode_addr`** — synthesize overlapping
  regions terminating at the same machine address (or document why impossible)
  to pin the one-placeholder-per-addr assumption (OR-4).
- **EC-5 `analyze_resolution_via_loop_matches_manual_two_pass`** — an
  end-to-end test asserting `Strider::analyze`'s resolution result equals a hand
  two-pass lift, covering the headline API that examples/benches skip (OR-5).
