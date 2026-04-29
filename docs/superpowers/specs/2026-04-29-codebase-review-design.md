# Codebase review & simplification — design

**Date:** 2026-04-29
**Branch:** `review/opt-crate-r6` (worktree `.worktrees/review-r6-opt`)
**Scope of this design:** all per-crate review sessions plus a final cross-crate session. This document covers the workflow; each session produces its own review report as a separate artifact.

## Goals

For every line of every `.rs` file (src + tests) in the workspace:
- Verify correctness against current code, not against comments or prior review docs.
- Flag dead code, duplication, missed dogfooding of shared helpers, over-engineering, and readability gaps.
- Flag performance concerns (redundant computation, hot-path bloat, missed parallelism, unbounded growth).
- Identify opportunities to generalise / unify code.

User gates every code change. No fix lands without explicit approval of the corresponding finding.

## Non-goals (explicitly out of scope)

- **Error-handling changes.** A parallel branch is migrating `strider-error` to `anyhow` per `2026-04-29-replace-strider-error-with-anyhow-design.md`. This work will not modify `crates/strider-error/`, the shape of `Error`/`Result` enums, or `ValidationErrors` / `opt::Error` payloads.
- **Public-API renames** that ripple outside the crate, unless individually flagged and approved.
- **Performance work** beyond removing obviously redundant computation. Performance findings are *flagged* in reports, not auto-implemented.

## Workflow per crate

Order: `opt` → then user picks the next, until all crates done. Final session is cross-crate.

### Step 1 — Read pass (1 subagent)

A single review subagent reads every `.rs` file in the crate (src + tests), front to back. The subagent is instructed to:
- Verify each claim against current code; treat doc comments and prior review reports as untrusted.
- Report **every** finding, not a confidence-filtered subset (overrides any default filtering).
- Categorise findings (see categories below).
- For each finding: cite `file:line`, quote enough context to judge, propose a concrete change, rate confidence (high/medium/low) and risk (low/medium/high).

The subagent writes its findings to a markdown report committed on the worktree branch (e.g. `reviews/opt-crate-r6.md`).

### Step 2 — Report review (user)

User reads the report and marks each finding:
- **Apply** — implement the proposed change.
- **Skip** — leave as-is (note reason if useful).
- **Defer** — file as a follow-up, do not block this session.

No code change happens before this gate.

### Step 3 — Implementation (main agent)

Apply approved findings in small thematic commits. After each commit:
- `cargo build --workspace` must pass.
- Crate-scoped tests must pass: `cargo test --workspace -p <crate>`.
- Targeted clippy on touched files.

If a build/test fails, revert the commit (do not amend) and re-examine. Never silence clippy with `#[allow(...)]` — fix or skip the change.

### Step 4 — End-of-crate verification

Before declaring the crate done:
- `cargo test --workspace`.
- `cargo clippy --workspace -- -D warnings` clean (or explicitly justify any leftover warnings to the user).
- Update the report file: each finding annotated with its outcome (Applied / Skipped / Deferred + commit SHA if applied).

## Finding categories

Reports group findings under these headings:

1. **Correctness** — real or suspected bugs, wrong logic, panic risks, unsoundness, missing checks at boundaries. *Highest priority. User decides per-finding whether to fix here or file separately.*
2. **Dead code** — unused fns/types/imports/branches, unreachable arms, `#[allow(dead_code)]` cruft.
3. **Duplication & unification** — copy-paste, near-duplicates, missed dogfooding of shared helpers (notably `pattern::*` constructors and `match_value!`), opportunities to lift to a shared crate.
4. **Simplification** — over-engineering, unneeded indirection, parameter sprawl, stringly-typed code where enums exist, comments that lie or narrate.
5. **Readability** — naming, file organisation, function size, nested-conditional flattening.
6. **Performance** — redundant computation, hot-path bloat, missed parallelism, N+1 patterns, unbounded data structures, overly broad reads. *Flagged only; not implemented unless explicitly approved.*

## Cross-crate session (final)

Same workflow, scoped to patterns spanning crates:
- Helpers duplicated across crates.
- Inconsistent naming for the same concept across crate boundaries.
- Opportunities to lift code into `entity-utils` / `graphwalk` / a new shared crate.
- Re-export hygiene.

## Verification gates (non-negotiable)

- No commit without `cargo build --workspace` passing.
- No commit without affected crate's tests passing.
- No `--no-verify`, no `#[allow]` to bypass clippy.
- No destructive git ops (force push, reset --hard, branch deletion) without explicit user approval.

## Risks accepted

- **Duplicate work vs prior `SIMPLIFICATION_REVIEW.md`.** That doc is ignored per user instruction. Cost: re-flagging things already addressed. Benefit: independent verification, no inherited blind spots.
- **Subagent context.** A single subagent reads the entire crate; if its context fills up before finishing, it must stop and report partial findings rather than truncate silently. The main agent then dispatches a second subagent for the remainder, picking up after the last completed file.
- **Anyhow merge conflicts.** Error-handling work is excluded, but cross-crate cleanups (e.g. helper extraction) may still touch files the anyhow branch edits. Bias toward small, isolated commits to make conflict resolution mechanical.

## Deliverables

Per crate:
- One markdown review report on the worktree branch (`reviews/<crate>-r6.md`).
- A series of small commits implementing approved findings.
- A final summary commit annotating the report with outcomes.

End of project:
- One cross-crate review report.
- Cross-crate fix commits.
- A short rollup commit summarising what shipped vs. what was deferred.
