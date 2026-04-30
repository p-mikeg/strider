# Cross-crate review r6 — implementation plan

> **For agentic workers:** this is a review-and-apply project, not a feature build.  Tasks emerge from the read-pass findings; the plan defines the methodology, scope, and gates rather than a fixed task graph.

**Goal:** find code spanning multiple crates that can be simplified or generalized — including test infrastructure — and apply the cleanup.  The end state should have less duplication, sharper abstractions, and a single source of truth for common helpers.

**Architecture:** mirror the per-crate r6 workflow (worktree → read-pass subagent → findings report → user-gated apply pass → fast-forward merge), expanded to cover all 12 crates with a simplification/generalization focus.  Tests are first-class — duplication in test fixtures is in scope.

**Tech stack:** existing strider workspace (anyhow, cranelift-entity, rsleigh).  Single-subagent workflow (per user's no-OOM rule).

---

## Scope

### In scope
- **All 12 workspace crates:** `opt`, `pattern`, `strider`, `cfg`, `ir`, `pcode-lift`, `target`, `reader`, `dot`, `graphwalk`, `entity-utils`, `graphmock`.
- **Production code:** patterns spanning multiple crates that can be simplified / generalized; missed shared abstractions; helper duplication; trait shape mismatches; type leaks across boundaries.
- **Tests:** mock-IR builders, fixture helpers, common assertions duplicated across crates' test files.  **Possible deliverable: a new crate (e.g. `ir-test-utils`) or a `cfg(test)`-gated module exposing shared graph-construction helpers.**
- **Workspace-level:** `Cargo.toml` hygiene, dep workspace-pinning, feature-flag consistency.
- **Documentation:** `CLAUDE.md` vs per-crate `lib.rs` module docs, public API doc consistency.

### Out of scope
- **Deferred performance findings** from prior reviews (F-034/F-035 in ir, F-016/17/18/19/21 in opt followup) — those wait for a benches workstream.
- **New language features** or architectural redesigns beyond what the simplification work motivates.
- Re-reviewing already-merged findings.

---

## Workspace setup (Phase 0)

- **Worktree:** `.worktrees/review-r6-cross/` on new branch `review/cross-crate-r6` (forked from current `feature/ai`).
- **Fixtures:** symlink `fixtures/out/` from main worktree.
- **Baseline gate:** `cargo build --workspace` + `cargo test --workspace` must pass before the read pass.  If broken, stop and surface.

---

## Phase 1 — Read pass (1 subagent)

Dispatch one `general-purpose` subagent.  The brief includes:

### Required reading
1. All 5 prior review reports (`reviews/{opt,pattern,strider,cfg,ir}-crate-r6.md`).  Findings already addressed individually should NOT be re-flagged.
2. `CLAUDE.md` (refreshed twice this session — verify per-crate descriptions match current code).
3. Each crate's `lib.rs` + top-level module docs.
4. Each crate's `Cargo.toml`.
5. Sample of test files from every crate to map test-helper duplication.

### Focus areas

#### A. Simplification / generalization (production code)

1. **Helper duplication.**  Concrete examples to verify and look for more:
   - `1/2/4/8/16/32 → NodeOutputType` byte-size switch flagged in cfg + strider + ir reviews.  Verify which copies survived after the per-crate apply passes.  If multiple still exist, lift to one.
   - Sort-key `(space.shortcut_raw(), off, size)` was duplicated between cfg and strider; the cfg review fixed one site — verify no third site exists.
   - Other "two near-identical match arms" or "three near-identical functions" patterns.
2. **Trait shape mismatches.**  Several crates have trait-pair patterns:
   - opt's `OptimizerOnBuilt` + blanket `impl Optimizer`.
   - pattern's builder family.
   - cfg's `Builder` family.
   - Are these consistent in shape?  Could a workspace-level convention emerge?
3. **`Vec<Option<T>>` indexed by entity index** (strider's `RegionIndex` after the restructure).  Is this a pattern other crates would benefit from?  Could `entity-utils` provide a shared helper?
4. **Error message style consistency.**  Post-anyhow, the workspace mixes `bail!`, `Err(anyhow!(...))`, multi-line context chains.  Find the dominant style and document or normalize.
5. **`NodeId`/`NodeOutputId` stability invariants** — when each is valid across opt's destructive passes + strider's `LoopState::step`.  Documented?  Tested?
6. **Re-export hygiene.**  Each crate's `lib.rs` re-export list; workspace-wide check for unused or accidental leaks.
7. **Stale codename comments** (BUG-NN, F-NN, R-NN, Phase-NN) — each per-crate review stripped them locally; check for stragglers.

#### B. Simplification / generalization (tests)

This is a major focus area.  The strider review already lifted `make_strider_x86_64()`; multiple crates have mock-IR construction helpers:

- `crates/opt/src/test_support.rs` — `make_fn`, `make_fn_with_var`.
- `crates/strider/tests/common/` — strider-specific fixtures.
- `crates/pattern/tests/matching/support/{shapes,graph,assertions}.rs` — pattern test scaffolding.
- `crates/cfg/tests/common/{synthetic,real_binary,assertions}.rs` — cfg test scaffolding.
- `crates/ir/tests/common/mod.rs` — ir test scaffolding.

Specific tasks for the subagent:
1. **Inventory mock-IR construction helpers.**  For each crate's test files, list the helpers (function name, what it builds, how many sites use it).
2. **Find near-duplicates across crates.**  e.g., functions named `make_fn`, `simple_function`, `build_test_graph` that all do roughly the same thing with subtle differences.
3. **Propose a consolidation strategy.**  Three options to evaluate:
   - **(a) New `ir-test-utils` crate** — shared mock-IR builders, exposed to all crates' `[dev-dependencies]`.  Cleanest but adds a workspace member.
   - **(b) `cfg(test)` module inside `ir`** — `pub mod test_support` gated `#[cfg(test)]` won't work because `cfg(test)` is per-crate; would need a `test-utils` feature flag.  Trade-off: feature flag pollution vs new crate.
   - **(c) Per-pair extraction** — only consolidate where the duplication is exact + worth it.  Leave anything per-crate alone.
   The subagent picks one and justifies it.

#### C. Workspace hygiene

1. **`Cargo.toml`** — duplicated deps, candidates for workspace-pinning, unused deps, feature-flag inconsistency.
2. **Documentation drift** — `CLAUDE.md` vs each crate's `lib.rs` module docstring.

#### D. Per-crate light pass on the 7 unreviewed crates

For each of `pcode-lift`, `target`, `reader`, `dot`, `graphwalk`, `entity-utils`, `graphmock`:
- Read the crate's `lib.rs` + `Cargo.toml`.
- Read every public module (full coverage for these — they're small).
- Read every public test file.
- Flag findings under the same categories as the cross-crate ones (simplification / generalization / duplication).
- Each crate gets a finding section in the report.

### Hard rules for the subagent
- Verify every claim against current code (don't trust prior reviews or CLAUDE.md).
- Don't pre-filter by confidence; tag each finding with confidence + risk.
- Read-only — write only the report file.
- Out-of-scope: error-handling shapes, performance items.

### Output
- `reviews/cross-crate-r6.md` with:
  - Summary line (file count, finding count, breakdown).
  - TOC.
  - Findings grouped by:
    - **A. Production simplification / generalization.**
    - **B. Test infrastructure consolidation.**  Includes the consolidation-strategy proposal (a / b / c).
    - **C. Workspace hygiene.**
    - **D. Per-crate findings on the 7 small crates.**
  - Per-finding: ID, location, what, why, proposed change, confidence, risk.
  - Files reviewed table.

---

## Phase 2 — Findings digest + user gate

I read the cross-crate report and present a compact digest.  Buckets:

- **Generalization wins** (collapse N-way duplication into a shared helper or trait).
- **Test infrastructure** — including the consolidation-strategy decision.
- **Per-crate findings** — small-crate items the user can choose to apply now or defer.
- **Workspace hygiene** — `Cargo.toml`, doc drift.

User picks: apply all / per-bucket / specific findings.  The test-consolidation strategy decision (new crate vs feature flag vs leave alone) is gated explicitly because it has the largest blast radius.

---

## Phase 3 — Apply pass (1 subagent)

Same workflow as prior sessions:
- Single general-purpose subagent.
- Workflow per finding: read → apply → build → test → clippy → commit.
- Commit per-bucket (or per-finding for the larger ones — test consolidation likely warrants its own commit series).
- Annotate the cross-crate report with outcomes per finding.

If the user picks "(a) new `ir-test-utils` crate" the apply pass needs:
1. Create the new crate with `Cargo.toml`, `src/lib.rs`.
2. Move shared helpers from each consumer crate.
3. Add as `[dev-dependencies]` to each consumer.
4. Update import sites.
5. Verify `cargo test --workspace` still passes.

### Hard rules (per user's standing rules)
1. No `panic!` / `unwrap()` / `expect()` / `unreachable!()` / `todo!()` / `debug_assert!` in production code.  Tests have `cfg_attr(test, allow(...))`.
2. No silencing clippy warnings with `#[allow(...)]`.
3. Every commit must build + tests must pass.
4. Anyhow only.
5. Comments only when they add non-obvious WHY.

---

## Phase 4 — Merge

Fast-forward merge into `feature/ai` from the *correct pwd* (the cross-crate worktree, not the main worktree — to avoid the `git add -A` junk-files trap that struck the strider + ir merges).

---

## Verification gates (non-negotiable)

- After every commit on the cross-crate branch: `cargo build --workspace` clean.
- After every commit: affected crate tests pass.
- After Phase 3 done: `cargo test --workspace` shows ≥ 2755 passing / 0 failing across 92 suites.  `cargo clippy --workspace --all-targets` clean.
- Before Phase 4 merge: same.

---

## Risks accepted

- **A new test-utils crate adds a workspace member.**  Trade-off: cleanest dedup vs slightly heavier workspace structure.  The read-pass evaluates which option pays off given actual duplication volume.
- **Cross-crate fixes can have larger ripple than per-crate ones.**  Some "fixes" may turn out worse than the duplication they replace; the read-pass tags risk per-finding so the user can self-filter at the gate.
- **Stale CLAUDE.md / per-crate `lib.rs` docs.**  Drift was already partially addressed; remaining drift gets flagged in the doc-hygiene bucket.
- **Light pass on small crates is intentional.**  Each gets full coverage of public surface but nothing more — a future per-crate r6 may surface deeper findings.

---

## Deliverables

- `reviews/cross-crate-r6.md` — comprehensive findings report covering all 12 crates.
- Possibly `crates/ir-test-utils/` (new crate) OR a feature-gated test-support module — depending on Phase 2 decision.
- A series of small thematic commits on `review/cross-crate-r6`.
- Updated `reviews/cross-crate-r6.md` with per-finding outcomes table after Phase 3.
- A short rollup commit summarizing what shipped vs deferred.

---

## What this is *not*

- Not a performance pass.  F-034/F-035 (ir cache-key alloc) and the opt-followup perf items remain deferred until benches exist.
- Not an architecture redesign.  We're looking for patterns that emerged organically and need consolidation, not redesigning the layering.
