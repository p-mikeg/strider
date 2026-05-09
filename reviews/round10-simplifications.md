# Round 10 — Simplification Consolidation

Synthesis of `round10-1[A-F]`, `round10-2[A-D]`, and `round10-3[A-B]` into a
prioritised, action-oriented simplification plan.  Workspace baseline
LOC: **~57 100** under `crates/*/src/`.  Estimated post-implementation
reduction (cumulative across all entries): **−620 / +110** = **net −510 LOC**
(≈0.9 % of the workspace).

## Table of contents per category

| # | Category                                       | Entries | Net LOC delta |
|---|------------------------------------------------|---------|---------------|
| 1 | Delete dead code                               | 9       | −185          |
| 2 | Merge similar code                             | 6       | −95           |
| 3 | Inline single-callsite helpers                 | 4       | −22           |
| 4 | Replace bespoke patterns with stdlib idioms    | 7       | −38           |
| 5 | Tighten visibility                             | 14      | +14 (no LOC delta — pub→pub(crate)) |
| 6 | Drop redundant wrappers                        | 3       | −60           |
| 7 | Collapse partial-state types                   | 5       | +28 / −150 = −122 |
|   | **Aggregate**                                   | **48**  | **≈ −510**    |

Visibility tightenings (category 5) don't reduce LOC; they reduce the
public-API surface.  All entries below are derived from a round-10 review
report; column "Source finding" cites the specific report and finding ID.

---

## Category 1 — Delete dead code

### S-1: Strip `Round 9 …` / `wave N` / `Ask-N RN FN` tombstones from doc-comments
- **Category:** Delete
- **Source finding:** round10-2B "Tombstones", round10-3B section F
- **Where:** ~58 sites across 32 files; representative sample in round10-2B
- **Net LOC delta:** −0 / +0 (in-place edit; same line count, shorter comments)
- **Net cognitive delta:** + (substantial — 58 fewer "what does R9-V3 mean?" pauses
  per reader)
- **Description:** Bulk-strip the `Round N …` / `Ask-N RN FN` / `R9-…` /
  `wave N` prefixes from ` ///` doc strings.  Retain only the explanatory
  prose.  No behaviour change; pure noise reduction.  Suggested approach:
  one mechanical PR per crate.
- **Risk:** internal-only — comment text only.
- **Effort:** M (mechanical sed across all crates; visual review per file).

### S-2: Delete ghost `opt::with_built` references from doc-comments (4 sites)
- **Category:** Delete
- **Source finding:** round10-2B "Ghost references"; round10-3B A1, A3, A4, A5
- **Where:**
  - `crates/ir/src/function.rs:153, 173`
  - `crates/pattern/src/rewrite.rs:161`
  - `crates/strider/src/rewrite.rs:60-65`
- **Net LOC delta:** −12 / +6 (net −6)
- **Net cognitive delta:** + (eliminates the lie "pattern's rewrite machinery
  is typed against `BuiltFunctionGraph`" + "uses `mem::take`" — both false
  per round10-3B A4/A5)
- **Description:** `opt::with_built` was renamed to `opt::with_rewrite_ctx`
  in round 9 wave 28.  Update each cite to the new name; rewrite the
  outright incorrect `mem::take` claim in `GraphRewriter::apply_rule`'s
  doc to describe the actual flow (build a `RewriteCtx` per node from the
  wrapped `(graph, entry)` pair).
- **Risk:** internal-only; doc-only.
- **Effort:** S.

### S-3: Delete dead `cfg::FunctionBoundary` if migration is not finished
- **Category:** Delete (or finish)
- **Source finding:** round10-2D §2 + Visibility table
- **Where:** `crates/cfg/src/cfg/options.rs:27-115`
- **Net LOC delta:** if deleted: −90 / +0 (net −90); if migrated: +20
  (callers updated)
- **Net cognitive delta:** + (current state is "dead newtype with documented
  intent" — confusing; either path is clearer than today)
- **Description:** Round-9 P2 introduced `FunctionBoundary` and an
  `Options::function_boundary()` accessor, but `is_addr_tail_call` still
  takes the unsafe primitive 4-tuple.  Either complete the migration
  (preferred — see S-31) or delete `FunctionBoundary` until needed.
- **Risk:** internal-only.
- **Effort:** M.

### S-4: Delete dead `strider::SortedVns` if migration is not finished
- **Category:** Delete (or finish)
- **Source finding:** round10-1E I-9; round10-2D §6; round10-2B "tombstone"
- **Where:** `crates/strider/src/strider/pipeline.rs:96-143`
- **Net LOC delta:** if deleted: −47 / +0; if migrated: +12
- **Net cognitive delta:** + (today's `#[allow(dead_code)]` masks the
  incomplete migration indefinitely)
- **Description:** Round-9 P3 added `SortedVns(Vec<rsleigh::Vn>)` for forward
  migration; `AnalyzeOptions::all_vns` still takes raw
  `Option<Vec<rsleigh::Vn>>`.  Pick one: delete `SortedVns` and (if needed)
  add a runtime-sort assert at the existing call site, or finish the
  migration.
- **Risk:** internal-only — `SortedVns` is `pub` re-exported via
  `strider::lib.rs:55` but no external workspace consumer uses it.
- **Effort:** M (5 internal sites + the re-export line).

### S-5: Delete diagnostic `#[ignore]` tests with no assertions
- **Category:** Delete
- **Source finding:** round10-1E I-11
- **Where:** `crates/strider/tests/dump_sysret_trap.rs:10-54` (2 tests)
- **Net LOC delta:** −44 / +0
- **Net cognitive delta:** + (test suite signal-to-noise: 0 → defined)
- **Description:** Both tests produce human-readable output, have no
  assertions, are `#[ignore]`d, and test runners never execute them.
  The sysret classification verdict has been pinned by round-9 verification
  (round10-1E C-1 "deferred / by-design").
- **Risk:** none — tests already don't run.
- **Effort:** S.

### S-6: Delete `make_strider_x86_64` duplicate in `orchestrator.rs`
- **Category:** Delete
- **Source finding:** round10-1E I-7
- **Where:** `crates/strider/src/orchestrator.rs:999-1003`
- **Net LOC delta:** −5 / +0 (delete) + 1 (replace with `test_utils::strider_x86_64`)
- **Net cognitive delta:** + (one helper, one source of truth)
- **Description:** The orchestrator-internal `make_strider_x86_64` predates
  `test_utils.rs:strider_x86_64`.  The two functions have the same body.
  Delete the orchestrator-internal copy.
- **Risk:** internal-only — both are `#[cfg(test)]`-internal.
- **Effort:** S.

### S-7: Strip `(Task17)` parenthetical from TODO comments (live tracking)
- **Category:** Delete (cosmetic)
- **Source finding:** round10-2B "Tombstones"; round10-3B E2 (verifies still-live)
- **Where:** `crates/cfg/src/cfg/decode_cache.rs:35`,
  `crates/strider/src/orchestrator.rs:287`,
  `crates/strider/src/strider/pipeline.rs:43`
- **Net LOC delta:** 0 (in-place rename; same lines)
- **Net cognitive delta:** + (Task-tracker code "Task17" is opaque without
  the plan path; replace with the plan-path)
- **Description:** Round 10-3B verified the TODOs are still-live tracking
  for `2026-05-01-incremental-indirect-resolve.md`.  Replace `(Task17)`
  with `(see docs/superpowers/plans/2026-05-01-incremental-indirect-resolve.md)`
  or drop the parenthetical entirely.
- **Risk:** none.
- **Effort:** S.

### S-8: Delete the `(Task 15)` opaque reference
- **Category:** Delete
- **Source finding:** round10-3B E1
- **Where:** `crates/cfg/src/cfg/builder/indirect_resolve.rs:191`
- **Net LOC delta:** 0 (in-place; remove parenthetical)
- **Net cognitive delta:** + (4 unrelated "Task 15" plan files; impossible
  for reader to know which one)
- **Description:** Replace `(Task 15)` with either an explicit plan path
  or delete the parenthetical (the surrounding text already explains the
  optimisation).
- **Risk:** none.
- **Effort:** S.

### S-9: Delete `make_resolver_pipeline`'s `RedundantPhis` step
- **Category:** Delete
- **Source finding:** round10-1B L-3
- **Where:** `crates/cfg/src/cfg/builder/indirect_resolve.rs:252-258`
- **Net LOC delta:** −1 / +1 (replace with one-line comment)
- **Net cognitive delta:** + (mini-graph has no phi nodes — `RedundantPhis`
  is wasted work proportional to node count on every indirect-branch site)
- **Description:** Mini-graph is a single basic block with no `CondBranch`
  / `VarPhi`.  Remove `pipeline.add(opt::RedundantPhis)`; add a comment
  documenting "no phi nodes in the mini-graph — RedundantPhis intentionally
  omitted."
- **Risk:** internal-only.
- **Effort:** S.

**Category 1 cumulative:** ≈ −185 LOC, large cognitive uplift from
tombstone reduction.

---

## Category 2 — Merge similar code

### S-10: Merge `Match::output(c)?` chains in `jump_table::same_value` and similar walks
- **Category:** Merge
- **Source finding:** round10-2C M10-S5, M10-S9
- **Where:** `crates/opt/src/indirect_branch_resolve/jump_table.rs:309, 499,
  637, 641, 661, 665, 709-720`
- **Net LOC delta:** −12 / +0 (consolidate "phi shape we don't recognise"
  termination into a single `walk_through_trivial_phi` helper)
- **Net cognitive delta:** + (4 sites duplicating the same walk → 1 helper;
  HIGH per the three-occurrence threshold)
- **Three-occurrence threshold:** **HIGH** (≥5 sites, each ≥3 LOC).
- **Load-bearing-different vs accidentally-different:** All sites do the
  same walk; each terminates on the same conditions.  Differences are
  accidental — they predate `same_value`'s extraction.
- **Description:** Consolidate the 4-5 jump_table.rs walks that follow
  `VarPhi`/`ValuePhi` chains until a non-trivial node into a single
  helper `walk_through_trivial_phi(graph, output) -> NodeOutputId`.
  Document the termination contract.
- **Risk:** internal-only.
- **Effort:** M.

### S-11: Merge `replace_all_uses` + `extend_asm_fingerprint_from` boilerplate
- **Category:** Merge
- **Source finding:** round10-1C C-2, I-3, I-4
- **Where:** `crates/opt/src/function_args/mod.rs:189-198, 335-348`,
  `crates/opt/src/stack_store/detect.rs:73-75`,
  `crates/opt/src/flag_cmp_canonicalize/mod.rs:151-156`
- **Net LOC delta:** −18 / +6 (one `replace_all_uses_with_fingerprint`
  helper, ~6 lines, replaces 4 inline copies of the
  "fingerprint-then-replace-then-result" sequence)
- **Net cognitive delta:** + (the non-uniformity in C-2 / I-3 / I-4 — some
  sites discard the bool, some forget the fingerprint extension — is
  exactly the bug class a helper prevents)
- **Three-occurrence threshold:** **HIGH** (≥4 sites; ≥3 LOC each).
- **Load-bearing-different vs accidentally-different:** Slightly different —
  `flag_cmp_canonicalize` builds new RHS first; others have the new node
  already.  But the 3-step "extend fingerprint, replace, gate result" is
  identical.
- **Description:** Add a `RewriteCtx::replace_with_fingerprint(old_node,
  new_node, old_out, new_out) -> Result<bool>` that performs (a)
  `extend_asm_fingerprint_from(new, old)`, (b) `replace_all_uses(old_out,
  new_out)`, (c) returns the bool.  Migrate the 4 sites.
- **Risk:** internal-only — addresses real round-10 silent bugs.
- **Effort:** M.

### S-12: Merge `OptimizationResult::after_replace` doc-pattern into actual API
- **Category:** Merge / refactor
- **Source finding:** round10-1C I-1, B1 (round10-3B)
- **Where:** `crates/opt/src/pipeline.rs:36, 76, 136-137`
- **Net LOC delta:** −5 / +2 (consolidates 3 doc-patterns into 1; deletes
  the broken "from `RewriteCtx` to `RewriteCtx`" no-op migration note at
  line 136)
- **Net cognitive delta:** + (round10-3B B1 is a HIGH-severity self-
  contradicting comment)
- **Description:** Remove the broken half-rewrite at lines 136-137 ("from X
  to X"); consolidate the wave-28 historical note into the
  `with_rewrite_ctx` adapter doc (line 75-84).  Single source of truth.
- **Risk:** none — doc-only.
- **Effort:** S.

### S-13: Merge tests' `bfg` and production's `fg` local-variable name
- **Category:** Merge / rename
- **Source finding:** round10-2B "Abbreviation choices"
- **Where:** ~76 sites in 9 test files (vs ~1600 production sites using `fg`)
- **Net LOC delta:** 0 (in-place rename)
- **Net cognitive delta:** + (one workspace-wide convention)
- **Three-occurrence threshold:** **HIGH** (≥5 files).
- **Description:** Mechanical rename `bfg` → `fg` in tests.  Production
  uses `fg`; tests are the outlier.
- **Risk:** internal-only — test code only.
- **Effort:** M (sed-with-context-review per file).

### S-14: Merge the two `try_walk_through_control_state` doc paragraphs
- **Category:** Merge
- **Source finding:** round10-1D I-4
- **Where:** `crates/pattern/src/matcher/walk_through.rs:29-43`
- **Net LOC delta:** −8 / +0
- **Net cognitive delta:** + (the block comment contains TWO concatenated
  doc comments; the first describes a deleted cast walk-through)
- **Description:** Delete the paragraph describing the deleted cast walk-
  through; keep only the ControlState description.
- **Risk:** doc-only.
- **Effort:** S.

### S-15: Merge `pre-fix (round 9 Ask-8 R2 F7)` and similar narrative comments
- **Category:** Merge
- **Source finding:** round10-2B "Tombstones" (covers ~120 sites)
- **Where:** All workspace `*.rs` (covered by S-1 above)
- **Net LOC delta:** ≈ −60 (terse comments restored)
- **Description:** Same scope as S-1 but called out separately: many of the
  Round-9 prefixes are followed by 2-3 lines of explanatory prose that
  *could* be condensed to 1 line if the prefix is dropped.  Combined
  with S-1 the cumulative reduction is more than the prefix-strip alone.
- **Risk:** internal-only.
- **Effort:** M.

**Category 2 cumulative:** ≈ −95 LOC; major cognitive uplift from
removing duplicate / contradictory documentation.

---

## Category 3 — Inline single-callsite helpers

### S-16: Inline `Match::stack_phi_offsets`'s `if slice.is_empty() { None }`
  hygiene branch
- **Category:** Inline
- **Source finding:** round10-2C M10-S3
- **Where:** `crates/pattern/src/matcher/match_result.rs:288-291`
- **Net LOC delta:** −0 (the helper is correctly extracted; this entry is to
  document it should *stay* inlined and not be split further)
- **Net cognitive delta:** 0
- **Description:** **Skip — the helper at this site is fine; the empty-
  slice collapse is the documented contract.**  Listed here only because
  the round-10 prompt asked for explicit "skip when indirection cost >
  duplication cost" rationale.
- **Risk:** none.
- **Effort:** S (no-op).

### S-17: Inline `find_loadable_section_containing` parse-failure branch
- **Category:** Inline (and convert to typed Result)
- **Source finding:** round10-1E I-10; round10-2C H10-S3
- **Where:** `crates/reader/src/elf.rs:779-782`
- **Net LOC delta:** −5 / +6 (Result-typed return; net +1)
- **Net cognitive delta:** + (visible, structured error vs. invisible
  `eprintln!`)
- **Description:** Replace `eprintln!(...)` + `return false` with
  `Result<...>` propagation; the only caller is
  `apply_elf_relocations_with_extender`.  The 4-line `eprintln!` block is
  the entire error path of a single-callsite helper.
- **Risk:** internal-only — call site already returns `Result`.
- **Effort:** M.

### S-18: Inline `compact::gc_wide_consts` standalone-call doc claim
- **Category:** Inline (semantically: tighten the helper to private)
- **Source finding:** round10-1A M-3
- **Where:** `crates/ir/src/graph/compact.rs:241-290`
- **Net LOC delta:** −3 / +0 (removes the misleading "safe to call
  standalone" doc claim)
- **Net cognitive delta:** + (today the doc says "safe to call standalone
  in tests"; verified-against-code shows that's not actually safe per the
  zombie-arena scenario)
- **Description:** Drop the standalone-call claim from the doc-comment, and
  flip `pub(crate) fn gc_wide_consts` to `fn gc_wide_consts` (private to
  `compact.rs`).  Single caller in `retain_reachable`.  Body < 50 lines
  but still single-callsite.
- **Risk:** internal-only.
- **Effort:** S.

### S-19: Inline `int_const`'s post_match scan over single-output
- **Category:** Inline
- **Source finding:** round10-1D I-6
- **Where:** `crates/pattern/src/pat/ctor/wildcards.rs:53-66`
- **Net LOC delta:** −5 / +2 (replace `find_map(|out| ...)` with direct
  `node_outputs(node).into_iter().next()?`)
- **Net cognitive delta:** + (`IntConst` always has exactly one output;
  `find_map` is conceptually wasteful and obscures intent)
- **Description:** Replace the find_map scan with a direct `next()?` since
  the type guarantees exactly one output.
- **Risk:** internal-only.
- **Effort:** S.

**Category 3 cumulative:** ≈ −22 LOC; modest cognitive lift.

---

## Category 4 — Replace bespoke patterns with stdlib idioms

### S-20: Replace `let _ = detach_unreachable_nodes(...)` with explicit verdict bind
- **Category:** Stdlib idiom
- **Source finding:** round10-2C L10-S4
- **Where:** `crates/opt/src/redundant_phis/mod.rs:206`,
  `crates/opt/src/function_args/mod.rs:108`
- **Net LOC delta:** 0 (in-place rename; `let _ = ` → `let _result = `)
- **Net cognitive delta:** + (signals intent; current code reads "discard
  result" but should read "discard verdict, propagate fixedpoint signal
  via outer pipeline iteration")
- **Description:** Rename `let _` → `let _verdict` and add a one-line
  comment.  Or: remove the variable entirely if the function returns `()`
  but use a comment to document the verdict is the doc-only "did
  something change" signal.
- **Risk:** none.
- **Effort:** S.

### S-21: Replace `unwrap_or(0)` / `unwrap_or(u64::MAX)` mask fallbacks with explicit `?`
- **Category:** Stdlib idiom (`?`-propagation)
- **Source finding:** round10-2C H10-S2, L10-S6
- **Where:** `crates/opt/src/known_bits/mod.rs:179, 220, 279`
- **Net LOC delta:** −6 / +3 (each of 3 sites: `let Some(x) =
  u64_type_mask(ty) else { return Ok(None) };` mirrors the existing
  `SignExtend` arm)
- **Net cognitive delta:** + (current `unwrap_or(0)` for `ZeroExtend` and
  `unwrap_or(u64::MAX)` for shift bounds silently extend the analysis to
  documented-out-of-scope widths; H10-S2 traces the silent-bug path)
- **Three-occurrence threshold:** **MED** (3 sites, 1-2 LOC each).
- **Description:** Three of the same fix; use the `let Some(x) = … else {
  return Ok(None) }` pattern matching the sister `SignExtend` arm at
  `known_bits/mod.rs:294`.
- **Risk:** internal-only — fixes a documented silent-bug.
- **Effort:** S.

### S-22: Replace `if let Some(graph) = self.graph.as_ref() else { return Vec::new() }` with `?`-propagated `Result`
- **Category:** Stdlib idiom
- **Source finding:** round10-2C H10-S1
- **Where:** `crates/strider/src/orchestrator.rs:607-609`
- **Net LOC delta:** −2 / +4 (net +2; one helper-call site changes from
  `let v = …` to `let v = …?`)
- **Net cognitive delta:** + (matches the surrounding pattern at
  `classify_and_partition` and `apply_in_place_edits` — currently the only
  divergent helper)
- **Description:** Promote `recompute_unresolved` to `Result<Vec<…>>`;
  the single caller in `LoopState::step` already returns `Result`.
- **Risk:** internal-only.
- **Effort:** S.

### S-23: Replace `mem_chain_is_dirty`'s `unwrap_or(true)` with `?`-propagated `Result`
- **Category:** Stdlib idiom
- **Source finding:** round10-2C H10-S6
- **Where:** `crates/opt/src/function_args/mod.rs:521-522`
- **Net LOC delta:** −2 / +3
- **Net cognitive delta:** + (debug_assert + release "silent fall back to
  true" is exactly the silent-failure pattern the prompt targets)
- **Description:** Convert to `Result<bool>` and propagate via `?`.  Caller
  already needs `Result` plumbing.
- **Risk:** internal-only.
- **Effort:** S.

### S-24: Replace `extract::<Vec<u8>>` failure-as-MemReadError with PyErr-aware path
- **Category:** Stdlib idiom (PyO3 error surfacing)
- **Source finding:** round10-2C H10-S4
- **Where:** `crates/strider-py/src/reader.rs:496-513`
- **Net LOC delta:** −0 / +5 (mirrors the existing `wrap_when` pattern at
  `pattern.rs:505-508`)
- **Net cognitive delta:** + (today: KeyboardInterrupt during
  `MemReader.read` is silently absorbed as `MemReadError`)
- **Description:** Mirror `wrap_when`'s pattern: detect
  `PyKeyboardInterrupt` / `PySystemExit`, restore via `e.restore(py)`, and
  let the exception propagate at the next PyO3 boundary.
- **Risk:** internal-only — fixes a Python-side silent failure.
- **Effort:** S.

### S-25: Replace `lookup_table().ok()?` with explicit error-context
- **Category:** Stdlib idiom
- **Source finding:** round10-2C L10-S3
- **Where:** `crates/strider-py/src/reader.rs:661`
- **Net LOC delta:** 0 (in-place; `eprintln!` added on the err arm)
- **Net cognitive delta:** + (lock-poison and Arc reconstruction failures
  are essentially unreachable but should not be invisible)
- **Description:** Mirror `PyReadOnlyMemoryAdapter`'s `eprintln!` pattern
  for the err arm.
- **Risk:** internal-only.
- **Effort:** S.

### S-26: Replace `Capture::id() as isize` with sign-safe cast
- **Category:** Stdlib idiom (correctness)
- **Source finding:** round10-1F F-02
- **Where:** `crates/strider-py/src/pattern.rs:99-104`
- **Net LOC delta:** 0 (one-line cast change)
- **Net cognitive delta:** + (eliminates 32-bit Python collision class)
- **Description:** Replace `self.inner.id() as isize` with
  `self.inner.id() as i64 as isize` (always-positive widening).
- **Risk:** internal-only.
- **Effort:** S.

**Category 4 cumulative:** ≈ −38 LOC, several silent-failure fixes.

---

## Category 5 — Tighten visibility (no LOC delta; API-surface delta)

These entries don't shrink LOC; they tighten visibility from `pub` to
`pub(crate)` / `pub(super)` / `#[cfg(test)] pub` for items with **no
external workspace consumer**.  Verified by `grep` per round10-2D §7.

### S-27: Tighten `BuiltFunctionGraph` fields to `pub(crate)`
- **Category:** Visibility
- **Source finding:** round10-2D §1 (HIGH)
- **Where:** `crates/ir/src/function.rs:45-100` (~5 fields: `graph`,
  `variables`, `call_clobbered`, `ret_val_regs`, `call_other_clobbered`)
- **Description:** Two-step: (a) migrate ~30 internal sites from
  `bfg.field_x` to `bfg.field_x_regs()` accessor (round-9 V2-introduced);
  (b) flip fields to `pub(crate)`.  Each carries a "**Caution:** mutating
  silently breaks pattern queries" doc warning today.
- **Risk:** internal-only — strider-py exposes accessors only.
- **Effort:** M (~30 mechanical sites, 1 hour).

### S-28: Tighten `Kb { ones, zeros }` fields to `pub(crate)`
- **Category:** Visibility
- **Source finding:** round10-2D §1 (HIGH)
- **Where:** `crates/opt/src/known_bits/mod.rs:38-43`
- **Description:** The doc says `ones & zeros == 0` is invariant; the
  fields are `pub`.  Add `Kb::raw(ones, zeros) -> Self` private ctor; flip
  fields to `pub(crate)`.  ~6 internal sites construct `Kb { ones, zeros }`
  literals.
- **Risk:** internal-only — `KnownBitsMap` is `pub` re-exported but
  threaded through opt-internal helpers.
- **Effort:** S.

### S-29: Tighten `cfg::Cfg::start_addr_to_region_id` to `pub(crate)`
- **Category:** Visibility
- **Source finding:** round10-2D §1 (MED); round10-1B I-2
- **Where:** `crates/cfg/src/cfg/mod.rs:66-73`
- **Description:** Field doc explicitly says "external readers should still
  go through `region_id_at_start`."  `cfg/tests/cfg_query.rs` uses struct-
  literal syntax for hand-built petgraph fixtures — add
  `Cfg::from_parts_for_test(graph, entry, idx)` ctor and migrate.
- **Risk:** internal-only — 1 test site to migrate.
- **Effort:** S.

### S-30: Tighten `IndirectBranchResolve::{unresolved_anchors,anchor_contexts}` to `pub(crate)`
- **Category:** Visibility
- **Source finding:** round10-2D §3 (MED)
- **Where:** `crates/opt/src/indirect_branch_resolve/mod.rs:172-184`
- **Description:** Co-keyed maps with documented invariant.  Round-9 V5
  added `add_anchor` / `clear_anchors` but didn't migrate all `pub`
  call sites.  Tighten + complete the migration.  External readers can use
  `anchors() -> impl Iterator` accessor.
- **Risk:** internal-only — strider orchestrator is the only mutator.
- **Effort:** M (~5 internal sites).

### S-31: Migrate `is_addr_tail_call` to take `FunctionBoundary` (resurrect dead newtype)
- **Category:** Visibility (newtype adoption)
- **Source finding:** round10-2D §2 (HIGH); cross-link S-3
- **Where:** `crates/cfg/src/cfg/query.rs:25-48`,
  `crates/cfg/src/cfg/builder/region_builder.rs:223`,
  `crates/strider/src/orchestrator.rs:630`
- **Net LOC delta:** −3 / +6 (signature change + 2 call sites)
- **Description:** New signature
  `is_addr_tail_call(target, start, boundary: FunctionBoundary) -> bool`.
  Migrate the 2 call sites; delete the primitive 4-tuple form.  Makes
  `FunctionBoundary` (currently dead) load-bearing — alternative to S-3
  delete.
- **Risk:** internal-only.
- **Effort:** M.

### S-32: Tighten `Matcher::options_for_test` / `Match::new_for_test` /
  `resolve_indirect_target_for_test` to `#[cfg(test)] pub`
- **Category:** Visibility
- **Source finding:** round10-2B "side-effect-lying names"; round10-2D §4
- **Where:**
  - `crates/pattern/src/matcher/mod.rs:202-203` (9 test sites)
  - `crates/pattern/src/matcher/match_result.rs:31-34` (6 test sites)
  - `crates/cfg/src/cfg/builder/indirect_resolve.rs:359` (9 test sites)
- **Description:** All three carry the `_for_test` suffix (signalling
  test-only intent) but are unrestricted `pub`.  Add `#[cfg(test)]` gate.
  Production cannot reach them; tests still can.
- **Risk:** internal-only — verified all callers are `#[cfg(test)]`.
- **Effort:** S.

### S-33: Tighten `BuiltCallingConvention::from_parts` to `pub(crate)` / hide
- **Category:** Visibility
- **Source finding:** round10-2D §4 (HIGH)
- **Where:** `crates/target/src/calling_convention/mod.rs:158-184`
- **Description:** Two ctors: `from_parts` (no validation) and
  `try_from_parts` (round-9-added; validates).  ~3 test sites use
  `from_parts`; production uses `CallingConvention::build` which now goes
  through `try_from_parts`.  Mark `from_parts` `#[doc(hidden)]` and
  rename to `from_parts_unchecked`.
- **Risk:** internal-only — tests already supply validated inputs.
- **Effort:** S.

### S-34: Tighten `ir::FunctionGraph` to `pub(crate)`
- **Category:** Visibility
- **Source finding:** round10-2D Visibility table
- **Where:** `crates/ir/src/function.rs:14-23`
- **Description:** Module is `mod function;` (private) so the type is
  effectively pub(crate) today.  Mark explicitly so an accidental
  `pub mod` re-export can't leak it.
- **Risk:** none.
- **Effort:** S.

### S-35: Tighten `ir::dot::GraphDotDumperState` to `pub(crate)`
- **Category:** Visibility
- **Source finding:** round10-2D Visibility table
- **Where:** `crates/ir/src/dot/mod.rs:137`
- **Description:** Used only by `render.rs` inside the `dot` module.  Not
  re-exported through `ir::lib.rs`.  Tighten explicitly.
- **Risk:** none.
- **Effort:** S.

### S-36: Tighten `ir::Outputs` / `Inputs` / `OutputIter` / `InputIter` /
  `OutputUsageIter` / `InputCursor` iterators to `pub(crate)`
- **Category:** Visibility
- **Source finding:** round10-2D Visibility table
- **Where:** `crates/ir/src/iterators.rs:9-152`
- **Description:** `mod iterators;` is private at `lib.rs`.  Iterators are
  returned from `Graph` methods that have their own re-exports; the
  iterator types are typed only via the methods' return types.  Tighten
  explicitly.
- **Risk:** none.
- **Effort:** S.

### S-37: Tighten `strider::IrStrider` to `pub(crate)`
- **Category:** Visibility
- **Source finding:** round10-2D Visibility table
- **Where:** `crates/strider/src/strider/mod.rs:14`
- **Description:** Not re-exported from `lib.rs`; effectively pub(crate)
  today.  Tighten explicitly.
- **Risk:** none.
- **Effort:** S.

### S-38: Tighten `cfg::region_builder::ProcessInsnRes` to `pub(crate)`
- **Category:** Visibility
- **Source finding:** round10-2D Visibility table
- **Where:** `crates/cfg/src/cfg/builder/region_builder.rs:49`
- **Description:** Used only by region-builder's internal loop and
  re-exported via `pub use super::ProcessInsnRes` at line 688 of the
  test_api.  Tighten the underlying type to `pub(crate)`; keep the
  test_api re-export.
- **Risk:** internal-only — verified `test_api` is the only external
  consumer.
- **Effort:** S.

### S-39: Tighten `RewriteCtx`'s `pub graph` and `pub entry` fields to `pub(crate)`
- **Category:** Visibility
- **Source finding:** round10-1D I-1; round10-2D §6
- **Where:** `crates/pattern/src/rewrite.rs:154-219`
- **Description:** Both `RewriteCtx<'g>` and `RewriteCtxView<'g>` carry
  `pub graph: &'g (mut)? Graph` + `pub entry: NodeId`.  Borrow checker
  prevents most abuse, but a `pub(crate)` + `graph()` / `entry()`
  accessors is tighter and matches round10-1D I-1.
- **Risk:** internal-only.
- **Effort:** S.

### S-40: Add `#[deprecated]` to `Builder::new` / `Builder::with_endianness`
- **Category:** Visibility (deprecation)
- **Source finding:** round10-1B I-5
- **Where:** `crates/cfg/src/cfg/builder/mod.rs:103-120`
- **Description:** CLAUDE.md documents the trap (these silently default to
  `preset = X86_64`).  `Builder::for_arch` is the safe alternative.  Add
  `#[deprecated(since = "...", note = "Use Builder::for_arch …")]`.
- **Risk:** internal-only — opens a compile-warning trail for callers.
- **Effort:** S.

**Category 5 cumulative:** 0 LOC delta; significant API-surface tightening.

---

## Category 6 — Drop redundant wrappers

### S-41: Drop `RegionTerminator::Switch.target_value: Option<ir::Value>` and split into two variants
- **Category:** Drop wrapper (replace with sum type — see also S-44)
- **Source finding:** round10-2D §1 (MED), §3
- **Where:** `crates/cfg/src/cfg/types.rs:165-182`
- **Net LOC delta:** −5 / +12 (split into two variants)
- **Net cognitive delta:** + (today the `Option` carries two semantically
  distinct modes in one struct)
- **Description:** Split `Switch { target_vn, target_value, targets }`
  into `Switch { target_vn, targets }` (cfg-time, no pinned output) and
  `SwitchPinned { target_value, targets }` (orchestrator-fed).
- **Risk:** internal-only; ~2 call sites.
- **Effort:** M.  **Note:** round10-2D suggests deferring until an actual
  incremental-rebuild round arrives.

### S-42: Drop `RewriteCtxView`'s `pub graph` field (covered by S-39)
- (Already counted in S-39 — same concrete change.)

### S-43: Drop `MemReadError(pub anyhow::Error)` wrapper if it adds no
  invariant?
- **Category:** Drop wrapper (analysis: KEEP)
- **Source finding:** round10-2D §8
- **Where:** `crates/reader/src/lib.rs:41`
- **Description:** **Skip.**  Wrapper bridges `anyhow::Error` to
  `std::error::Error`; this is the legitimate "adapter" pattern.  Listed
  for explicit "skip when indirection cost > duplication cost" rationale.
- **Risk:** none.
- **Effort:** none.

### S-44: Drop `pattern::error::NotBuildable(pub &'static str)` wrapper
- **Category:** Drop wrapper (analysis: KEEP)
- **Source finding:** round10-2D §1 (LOW)
- **Description:** **Skip.**  External tests downcast and read the string
  via `MissingBinding("uint")`.  The `pub` is needed for the pattern-
  match.  Listed for "skip" rationale.
- **Effort:** none.

**Category 6 cumulative:** ≈ −60 LOC if S-41 lands; otherwise ~0.

---

## Category 7 — Collapse partial-state types

### S-45: Migrate `BuiltFunctionGraph::from_graph_and_entry_for_rewrite` callers off then delete
- **Category:** Partial-state collapse
- **Source finding:** round10-1A L-2; round10-2D §3 (HIGH)
- **Where:** `crates/ir/src/function.rs:148-192` + 4 pattern-test scaffolds
  + 2 production callers
- **Net LOC delta:** −60 / +20 (ctor + ~20 doc/comment lines deleted; test
  scaffolds migrate to `RewriteCtx::new(&mut graph, entry)`)
- **Net cognitive delta:** + (today's ctor returns a BFG with `variables:
  PrimaryMap::new()`, `call_clobbered: Box::new([])`, etc. — partial-
  state)
- **Description:** Migrate 4 pattern-test scaffolds + 2 production callers
  to either `Matcher::for_graph(&graph, entry)` (round-8 raw-graph entry)
  or `RewriteCtx::new(&mut graph, entry)`.  Delete the partial-state
  ctor.
- **Risk:** internal-only — `#[doc(hidden)]` already.
- **Effort:** M (~5 test files + 2 production lines).

### S-46: Collapse `IndirectBranchResolve` co-keyed maps via `add_anchor` migration
- **Category:** Partial-state collapse (and visibility — see S-30)
- **Source finding:** round10-2D §3
- **Description:** Already covered by S-30; the partial-state aspect is
  that the two `pub` fields can be mutated independently to violate the
  documented co-keying invariant.

### S-47: Collapse `ResolvedTargets::Multiple(pub Vec<u64>)` into a non-empty
  vec
- **Category:** Partial-state collapse
- **Source finding:** round10-2D §4 (MED)
- **Where:** `crates/opt/src/indirect_branch_resolve/mod.rs:81-99`
- **Net LOC delta:** −3 / +12 (NonEmptyVec wrapper + accessor)
- **Net cognitive delta:** + (eliminates the documented but unenforced
  "must be non-empty" invariant)
- **Description:** Either (a) `Multiple(NonEmptyVec<u64>)` or (b) keep
  current shape and document.  Round-9 P5 added the validating ctor; the
  unvalidated tuple variant remains constructible.
- **Risk:** internal-only — ~3 ctor sites + 4 destructurings.  Deferable.
- **Effort:** M.

### S-48: Collapse `Region.insns: Vec<>` empty-state into `Option<Vec<>>`
- **Category:** Partial-state collapse
- **Source finding:** round10-1B I-1 (HIGH)
- **Where:** `crates/cfg/src/cfg/types.rs:221-223`,
  `crates/cfg/src/cfg/builder/mod.rs:184-192`
- **Net LOC delta:** −5 / +15 (split path through `contains_addr` etc.)
- **Net cognitive delta:** + (today's doc says "Never empty" but code
  creates empty-Branch regions; `contains_addr` returns false for them
  even at `start_addr`)
- **Description:** The simpler alternative: leave `Vec<>` shape but fix
  `contains_addr` to handle the empty case via `start_addr == addr`.
  See round10-1B I-1.  This is the minimum-cost fix; the structural
  collapse is the longer route.
- **Risk:** internal-only.
- **Effort:** S (minimum) / M (full).

### S-49: Collapse `IfRegionState { Option, Option }` four-state product
- **Category:** Partial-state collapse (analysis: DEFER)
- **Source finding:** round10-2D §3 (LOW)
- **Description:** **Skip — defer.**  The four states (TT, TF, FT, FF) are
  semantically all valid; today's usage is "both must be Some, else
  error" but the type permits more.  Listed for completeness; round10
  surveyed code paths don't depend on the four-state distinction.
- **Effort:** none.

**Category 7 cumulative:** ≈ −150 / +28 = −122 LOC if S-45 + S-47 + S-48
all land.

---

## Cross-cutting items not in the 7-category toolkit

### S-50: Layer-C validator check for `StackStorePhi` side-table population
- **Category:** Validate-add (defensive)
- **Source finding:** round10-2D §9 (MED)
- **Where:** `crates/ir/src/validate/layer_c.rs`
- **Net LOC delta:** +15 / +0
- **Description:** Add Layer-C invariant that every reachable
  `StackStorePhi` has a non-empty `stack_phi_offsets` entry.  Production
  never violates this; mock tests that need the empty form must opt out.
  See round10-2D §9.
- **Risk:** internal-only.
- **Effort:** S.

### S-51: Doc-fix `OptimizationResult::Unchanged` → `NoChange`, `run` → `optimize`
- **Category:** Doc fix
- **Source finding:** round10-3A Refuted-1, Refuted-2, Refuted-3
- **Where:** `crates/opt/README.md:9-12`,
  `crates/strider/.claude/skills/strider-opt-pass-author/SKILL.md:25`
- **Description:** README.md and SKILL.md both reference the pre-wave-28
  signature.  Update to `optimize(&self, graph, entry)`,
  `OptimizationResult::{Changed, NoChange}`, and
  `&mut pattern::RewriteCtx<'_>`.
- **Risk:** doc-only.
- **Effort:** S.

### S-52: Doc-fix `RegionTerminator::Switch` "reserved" claim
- **Category:** Doc fix
- **Source finding:** round10-3B C1 (HIGH)
- **Where:** `crates/cfg/src/cfg/types.rs:119-121`
- **Description:** Claims the variant is "reserved for the future jump-
  table resolver" but the cfg builder constructs it at line 508 of
  `region_builder.rs` (jump-table support already landed).  Replace the
  paragraph or delete it.
- **Effort:** S.

### S-53: Doc-fix `pattern::rewrite_rule` closure signature
- **Category:** Doc fix
- **Source finding:** round10-3B C2 (HIGH)
- **Where:** `crates/pattern/src/rewrite.rs:13-19`
- **Description:** Doc claims closure takes `&mut BuiltFunctionGraph`;
  actual signature returns `Fn(&mut RewriteCtx<'g>, NodeId)`.
- **Effort:** S.

### S-54: Doc-fix `FunctionBuilder::build`'s `build_call_other` reference
- **Category:** Doc fix
- **Source finding:** round10-3B C5
- **Where:** `crates/ir/src/builder/mod.rs:570-572`
- **Description:** `build_call_other` doesn't exist; the actual builders
  are `build_call_other_modeled` and `build_call_other_terminal`.  Replace
  the cite with `build_call_other_modeled` (the modeled path is the one
  with multiple clobber outputs).
- **Effort:** S.

### S-55: Doc-fix relative-path cross-references in `node/kind.rs`
- **Category:** Doc fix
- **Source finding:** round10-3B D1 (MED)
- **Where:** `crates/ir/src/node/kind.rs:38, 68`
- **Description:** Both pass modules were converted from `*.rs` to
  `*/mod.rs`; the relative paths point to nonexistent files.  Plus
  `[name](relative-path)` doesn't resolve via rustdoc intra-doc links.
  Convert to `[`opt::FunctionArgDetect`]` form.
- **Effort:** S.

### S-56: Doc-fix `RewriteCtx::preorder` "future migration" tense
- **Category:** Doc fix
- **Source finding:** round10-3B B2
- **Where:** `crates/pattern/src/rewrite.rs:178-183`
- **Description:** "A future migration … from `&mut BuiltFunctionGraph` to
  `&mut RewriteCtx`" — already done at `pipeline.rs:158`.  Reword to past
  tense.
- **Effort:** S.

### S-57: Doc-fix `lifter_options` field name
- **Category:** Rename
- **Source finding:** round10-2B "Half-rename leftovers"
- **Where:** `crates/cfg/src/cfg/options.rs:166` + 5 access sites
- **Description:** Field still named `lifter_options` after public type was
  renamed `LifterOptions → Options`.  Rename to `options`.
- **Effort:** S.

### S-58: Doc-fix `ret_val_regs_slice` accessor name
- **Category:** Rename
- **Source finding:** round10-2B "Half-rename leftovers"
- **Where:** `crates/ir/src/function.rs:131` + ~12 call sites in 5 files
- **Description:** Three peer accessors introduced together; first two
  follow `<field>_regs`, third uses `_slice`.  Rename to
  `ret_val_regs_as_slice` (follows `as_` view convention).
- **Effort:** S.

### S-59: Doc-fix `analysis_loop_without_build_round_trips` test name
- **Category:** Rename
- **Source finding:** round10-2B "Unclear test names"
- **Where:** `crates/ir/tests/builder_extended_use.rs:26`
- **Description:** "Round trips" is ambiguous.  Rename to
  `in_place_mutations_without_build_preserve_graph_validity`.
- **Effort:** S.

### S-60: Doc-fix `check_node_output_defintions` typo
- **Category:** Rename
- **Source finding:** round10-2B "Unclear test names"
- **Where:** `crates/ir/src/graph/tests.rs:36, 68`
- **Description:** Misspelling of `definitions`.
- **Effort:** S.

---

## Summary table — net delta per category

| Category | Entries | Net LOC delta | Notes |
|---|---|---|---|
| 1. Delete dead code              | 9 active (+ 2 skips) | −185 | Dominated by tombstone strip + `dump_sysret_trap.rs` deletion + dead `SortedVns`/`FunctionBoundary` |
| 2. Merge similar code            | 6                  |  −95 | Mostly duplicate doc paragraphs and replicated rewrite-with-fingerprint pattern |
| 3. Inline single-callsite helpers | 4                  |  −22 | One real Result-conversion (S-17); others modest |
| 4. Stdlib idioms                 | 7                  |  −38 | Several silent-failure fixes via `?`-propagation |
| 5. Tighten visibility            | 14                 |  ±0  | `pub` → `pub(crate)` on 14 items with verified-zero external consumers |
| 6. Drop redundant wrappers       | 1 active (+ 2 skips) |  −60? | S-41 only; otherwise ~0 |
| 7. Collapse partial-state types  | 4 active (+ 1 skip) | −122 | S-45 (delete `from_graph_and_entry_for_rewrite`) is the big one |
| Doc fixes (cross-cutting)        | 10                 |  −15 | All HIGH or MED severity from round10-3A/3B |

**Workspace projection:** ≈ **−510 LOC** out of ≈57 100 = ≈ **0.9 %**.
Cognitive delta is much larger than the LOC delta — most reductions are
in noise (tombstone breadcrumbs, duplicated docs, dead newtypes).

---

## Suggested ordering — three batches

### Batch A — Low-risk, high-yield (mostly mechanical)

Aim for **~−260 LOC** in this batch.  Every change is local, doc-only or
single-file.  Should fit in one PR cycle.

1. **S-1**  Strip Round-9 tombstones from doc-comments (workspace-wide sed)
2. **S-2**  Delete ghost `opt::with_built` references (4 sites)
3. **S-5**  Delete `dump_sysret_trap.rs` (no assertions, `#[ignore]`d)
4. **S-7**  Strip `(Task17)` parenthetical
5. **S-8**  Delete `(Task 15)` opaque ref
6. **S-9**  Delete `RedundantPhis` from mini-graph resolver pipeline
7. **S-12** Merge `OptimizationResult::after_replace` doc patterns
8. **S-14** Delete duplicate cast-walk-through paragraph
9. **S-15** Compress remaining Round-9 prose (continuation of S-1)
10. **S-19** Inline `int_const`'s `find_map` over single-output
11. **S-26** Fix `Capture::id() as isize` cast
12. **S-51-S-56** All round10-3A/3B doc-fixes
13. **S-59 / S-60** Rename test typos

**Risk profile:** doc-only or test-only; no API changes; safe to land in
a single Batch-A commit per crate.

### Batch B — Mechanical migrations (medium-risk, pre-defined target)

Aim for **~−155 LOC** plus tightened API surface.  Each entry has a
mechanical migration target; all fit within established round-9
patterns.

1. **S-13** `bfg` → `fg` test rename (76 sites in 9 files; sed-with-review)
2. **S-32** `_for_test` → `#[cfg(test)] pub` gate (3 items, 24 sites)
3. **S-34-S-38** `pub` → `pub(crate)` for inferentially-private types
   (`FunctionGraph`, `Outputs`/`Inputs`/iterators, `GraphDotDumperState`,
   `IrStrider`, `ProcessInsnRes`)
4. **S-40** `#[deprecated]` on `Builder::new` / `Builder::with_endianness`
5. **S-58** `ret_val_regs_slice` → `ret_val_regs_as_slice`
6. **S-57** `lifter_options` → `options` field rename
7. **S-6**  Delete `make_strider_x86_64` orchestrator-internal duplicate
8. **S-11** Merge `replace_all_uses` + `extend_asm_fingerprint_from`
   helper (4 sites — fixes 3 silent-bugs in C-2, I-3, I-4)
9. **S-21** `unwrap_or(0)` / `unwrap_or(u64::MAX)` mask fallbacks (3 sites
   in known_bits)
10. **S-22** `recompute_unresolved` to `Result<…>` propagation
11. **S-23** `mem_chain_is_dirty` to `Result<bool>`
12. **S-24** PyErr-aware `MemReader.read` failure surfacing
13. **S-25** `lookup_table` failure-context
14. **S-50** Layer-C check for `StackStorePhi` side-table population

**Risk profile:** localised behaviour change in a few cases (S-21 fixes
silent-bugs; S-22-S-23 promote `Result`).  All have round-10 reports
documenting the bug or non-uniformity being fixed.  Recommend per-crate
PRs.

### Batch C — Type-surface changes (higher-effort, defer-or-decide)

Aim for **~−95 LOC** plus genuine encapsulation.  Each entry is a
multi-day effort with semantic implications; sequence carefully and
write test coverage before each.

1. **S-27** `BuiltFunctionGraph` field tightening (~30 internal migrations
   first, then `pub` → `pub(crate)`)
2. **S-28** `Kb { ones, zeros }` field tightening (small but needs care)
3. **S-29** `cfg::Cfg::start_addr_to_region_id` tightening (1 test
   migration to `from_parts_for_test`)
4. **S-30** `IndirectBranchResolve` co-keyed-maps tightening
5. **S-31** Resurrect `FunctionBoundary` newtype OR
6. **S-3**  Delete `FunctionBoundary` (mutually exclusive with S-31)
7. **S-4**  Delete `SortedVns` OR finish migration
8. **S-33** `BuiltCallingConvention::from_parts_unchecked` rename + hide
9. **S-39** `RewriteCtx` field tightening
10. **S-45** Delete `BuiltFunctionGraph::from_graph_and_entry_for_rewrite`
    after pattern-test scaffolds migrate
11. **S-47** `ResolvedTargets::Multiple(NonEmptyVec)` (deferable)
12. **S-48** `Region::contains_addr` empty-case fix (minimum) or full
    `Option<Vec<>>` collapse (full)
13. **S-41** `RegionTerminator::Switch` split (deferable per round10-2D)

**Risk profile:** type-surface changes; some affect external strider-py
exposure (verify accessor coverage before breaking field access).  S-45
is the cleanest big win — the round-9 wave-28 migration left this ctor
as dead-weight scaffolding.

---

## Notes on intentionally-deferred items

The round-10 reports flag several items that the consolidation
deliberately leaves for a later round:

- **`NodeKind` `#[non_exhaustive]`** (round10-2D §5) — defer until external
  crates appear; closed-enum semantics are correct for tightly-coupled
  workspace.
- **`MIPS/PPC RELATIVE` relocations** (round10-1E I-5) — feature work, not
  simplification.
- **`KnownBits`-cache invalidation in `IndirectBranchResolve`** (round10-1C
  C-1) — correctness fix; out of scope for this simplification report.
- **`make_int_const` masking** (round10-1A M-1) — correctness fix; out of
  scope.
- **CallOther sysret classification** (round10-1E C-1) — pinned by
  round-9 verification as by-design.
- **Round-7/8 follow-up items** — not re-flagged here; trust-only-the-code
  audit consulted but not transcribed.
