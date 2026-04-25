# Target Crate Review — Round 7 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Round 7 of the `target` crate review. After rounds 1–6 the crate is clippy-clean (`-D warnings`), all 6 unit + 8 smoke tests pass, ABI/register-set invariants are pinned (including SP sizing), and field-doc coverage is complete. This round closes one real correctness defect: a broken rustdoc intra-doc link in `arch.rs` that makes `cargo doc -p target` emit a warning and `RUSTDOCFLAGS="-D warnings" cargo doc -p target --no-deps` fail outright.

**Architecture:** A single one-line documentation fix plus a final workspace sanity sweep that now includes a strict `cargo doc` step so this class of defect cannot regress in `target` without being caught.

**Tech Stack:** Rust, `rsleigh`, `strider-error`, `thiserror`.

---

## What the review found (findings → tasks)

| # | Finding | Evidence | Task |
|---|---------|----------|------|
| F1 | The doc comment on `SleighArch` links to `[`crate::Analyzer::new`]`, but `Analyzer` lives in the `analyzer` crate, not in `target` (which depends only on `rsleigh`, `strider-error`, and `thiserror`). The link is unresolvable and produces `warning: unresolved link to \`crate::Analyzer::new\`` from `cargo doc -p target --no-deps`, and an outright `error: could not document \`target\`` under `RUSTDOCFLAGS="-D warnings"`. The sibling link three lines below (`[\`crate::CallingConvention::build\`]`) IS valid because `CallingConvention` is re-exported from this crate's `lib.rs`. The fix: drop the `crate::` prefix and demote the bracketed link to plain code formatting, since `target` does not depend on `analyzer` and therefore cannot intra-doc-link to it from this crate. | [arch.rs:13](crates/target/src/arch.rs#L13), confirmed by `cargo doc -p target --no-deps` (1 warning) and `RUSTDOCFLAGS="-D warnings" cargo doc -p target --no-deps` (errors out) | Task 1 |

### Considered and rejected

- **Adding `[workspace.lints.rustdoc]` with `broken_intra_doc_links = "deny"` to the workspace `Cargo.toml` to catch this class of bug everywhere in CI.** A workspace-wide `cargo doc --workspace --no-deps` currently emits dozens of `warning: unresolved link to ...` and `public documentation for X links to private item Y` warnings across `cfg`, `dot`, `ir`, `pattern`, and `analyzer`. Flipping the lint to deny without first cleaning each crate would break the workspace build and is far outside the scope of a `target`-crate review. If we want this enforcement, it has to be its own dedicated cleanup pass, one crate at a time. Flagged for the reviewer; not done here.
- **Making `target` depend on `analyzer` to "fix" the link properly.** This would be an upside-down dependency — `analyzer` depends on `target`, not the other way round, by design (CLAUDE.md: `analyzer → ir`, with `target` underneath). Plain code formatting is the correct rendering for "the thing in the analyzer crate that consumes this".
- **Linking to a hypothetical `analyzer::Analyzer::new` in HTML form (using a `[name]: url` shortcut).** Brittle — URLs depend on the deployed docs.rs version and the rendered path; a maintenance trap. Plain code is enough.
- All round 1–6 rejected items remain rejected (`Box<[Vn]>` vs `Vec<Vn>`, runtime validation of `stack_arg_offsets`, `#[non_exhaustive]` on `ErrorKind`, dropping `Hash` derives, const-ifying `cases()`, moving the unit tests to `tests/`, doctests on `build`, inlining `regs_to_vns`, sealing `BuiltCallingConvention` fields, reordering `BuiltCallingConvention` fields to match `CallingConvention`, additional ABI presets, comments on `presets_resolve_correct_register_sets` re. RDX overlap, comments on `assert_disjoint` re. one-directional iteration, fail-fast SP lookup ordering in `build()`, sharing `Sleigh::new(...).regs().unwrap()` between unit and integration tests).

---

## Task 1: Fix the broken `crate::Analyzer::new` intra-doc link in `arch.rs`

Closes F1. `Analyzer` is not in this crate, so the link is unresolvable. Change it to plain code formatting (matching the convention already used elsewhere in `target` — e.g. [`stack_ptr_reg_name`](crates/target/src/calling_convention.rs#L32) on `CallingConvention` says `\`Analyzer::new\`` in plain code, not as a link, for exactly this reason).

**Files:**
- Modify: [crates/target/src/arch.rs:13](crates/target/src/arch.rs#L13) (one line — the doc comment on `SleighArch`)

- [ ] **Step 1: Reproduce the failure**

Run: `RUSTDOCFLAGS="-D warnings" cargo doc -p target --no-deps`
Expected: FAIL with

```
error: unresolved link to `crate::Analyzer::new`
  --> crates/target/src/arch.rs:13:30
   |
13 | /// Pass a `SleighArch` to [`crate::Analyzer::new`] along with the calling
   |                              ^^^^^^^^^^^^^^^^^^^^ no item named `Analyzer` in module `target`
```

(Without `RUSTDOCFLAGS="-D warnings"`, the same condition surfaces as a `warning:` from plain `cargo doc -p target --no-deps`.)

- [ ] **Step 2: Apply the one-line fix**

In [crates/target/src/arch.rs](crates/target/src/arch.rs), replace:

```rust
/// Pass a `SleighArch` to [`crate::Analyzer::new`] along with the calling
/// convention to build an analyser for that target.  The calling convention
/// owns the stack-pointer register name (see
/// [`crate::CallingConvention::build`]) rather than the arch, so that
/// `CallingConvention::build` is self-contained and different ABIs on the
/// same arch can in principle declare different SP registers.
```

with:

```rust
/// Pass a `SleighArch` to `Analyzer::new` (in the `analyzer` crate) along
/// with the calling convention to build an analyser for that target.  The
/// calling convention owns the stack-pointer register name (see
/// [`crate::CallingConvention::build`]) rather than the arch, so that
/// `CallingConvention::build` is self-contained and different ABIs on the
/// same arch can in principle declare different SP registers.
```

The change has three parts:
1. `[\`crate::Analyzer::new\`]` → `\`Analyzer::new\`` (drop the link brackets and the `crate::` prefix; keep code formatting).
2. Add `(in the \`analyzer\` crate)` so the reader knows where to look — this was the only useful information the broken link was carrying.
3. Leave `[\`crate::CallingConvention::build\`]` alone — that link IS valid (`CallingConvention` is re-exported from `lib.rs`), and the round-3-era doc tightening would just be re-litigated by removing it.

- [ ] **Step 3: Verify rustdoc strict-mode now passes**

Run: `RUSTDOCFLAGS="-D warnings" cargo doc -p target --no-deps`
Expected: PASS, exit 0, "Generated /home/mike/Desktop/strider/target/doc/target/index.html".

- [ ] **Step 4: Verify no other doc warnings on `target`**

Run: `cargo doc -p target --no-deps 2>&1 | grep -E '(warning|error):'`
Expected: empty output (no doc warnings or errors on the `target` crate). If any new warnings appear, treat them as part of this task — fix or surface to the reviewer; do NOT commit a doc regression.

- [ ] **Step 5: Run the full target test suite — no behaviour change, but verify nothing tripped**

Run: `cargo test -p target`
Expected: 6 unit + 8 smoke = 14 PASS.

- [ ] **Step 6: Strict clippy sweep — confirms the doc-comment edit didn't introduce a new lint**

Run: `cargo clippy -p target --all-targets --no-deps -- -D warnings`
Expected: PASS with zero warnings.

- [ ] **Step 7: Commit**

```bash
git add crates/target/src/arch.rs
git commit -m "docs(target): fix broken \`crate::Analyzer::new\` intra-doc link

\`Analyzer\` lives in the analyzer crate, not target; the link was
unresolvable and made \`cargo doc -p target\` emit a warning (and
\`RUSTDOCFLAGS=\"-D warnings\" cargo doc -p target\` fail). Demote to
plain code formatting and name the analyzer crate explicitly so the
reader still knows where to look."
```

---

## Task 2: Final workspace sanity sweep (now with strict rustdoc on `target`)

**Files:**
- Run-only, no edits.

- [ ] **Step 1: Strict lint on the target crate**

Run: `cargo clippy -p target --all-targets --no-deps -- -D warnings`
Expected: PASS with zero warnings.

- [ ] **Step 2: Strict rustdoc on the target crate (regression guard for F1)**

Run: `RUSTDOCFLAGS="-D warnings" cargo doc -p target --no-deps`
Expected: PASS, exit 0.

- [ ] **Step 3: Full workspace build**

Run: `cargo build --workspace`
Expected: PASS.

- [ ] **Step 4: Full workspace tests**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 5: Smoke-run the example**

Run: `cargo run --example analyzer`
Expected: `cfg.html`, `graph.html`, `graph-opt.html` produced without error.

- [ ] **Step 6: Workspace lint (informational)**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS if no other crate has a latent lint; otherwise any remaining warnings are outside this review's scope — flag to the reviewer, do not fix here.

- [ ] **Step 7: Workspace rustdoc (informational only — DO NOT use `-D warnings` here)**

Run: `cargo doc --workspace --no-deps 2>&1 | grep -E '(warning|error):' | head -40`
Expected: zero warnings/errors *on the `target` crate*. Many warnings exist in other crates (`cfg`, `dot`, `ir`, `pattern`, `analyzer`) — those are out of scope for this review and are documented as such in the "Considered and rejected" section above. Do NOT attempt to fix them here.

---

## Out of scope (considered, rejected, or deferred)

All items from rounds 1–6's out-of-scope lists remain out of scope. Additionally:

- **Workspace-wide `[lints.rustdoc]` enforcement (`broken_intra_doc_links = "deny"`).** Would require a multi-crate cleanup pass to land cleanly. Tracked here as a known follow-up, not done in this round.
- **Cleaning up the broken intra-doc links in `cfg`, `dot`, `ir`, `pattern`, and `analyzer`.** Out of scope for a `target`-crate review. Each is its own cleanup task.
- **Switching `target`'s `pub fn` constructors (`SleighArch::x86_64()` etc.) to `pub const`s.** Would change the public API surface and ripple into every caller (`analyzer`, `cfg`, examples). The constructor style is the workspace's chosen idiom; not worth a one-crate divergence.
- **Adding a doctest that exercises `CallingConvention::build` against a Sleigh register table.** Doctests in `target` would require `rsleigh` heavy machinery in the doc snippet, which is brittle and slow; the unit tests already cover this surface. Already rejected in earlier rounds; reaffirmed.
