# strider-error Crate Review — Round 8 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eighth-pass review of `strider-error` after rounds 1–7. Round 7 shipped (commits `931080e` "tighten format helpers" + `427b2c8` "pin bridge_error! caller site"). Round 8 finds **zero correctness bugs**. Two small simplification edits in source plus one marginal test annotation cleanup. No public-API changes, no behavior changes.

**Architecture:** Same four-file layout (`lib.rs`, `fields.rs`, `define.rs`, `format.rs`), ~330 LoC across `src/`. S1 collapses the per-location format into `Location`'s built-in Display impl. S2 swaps fully-qualified `Error::source(_)` for method-call syntax in the source-walk closure. R3 removes a redundant `#[track_caller]` on a test helper.

**Tech Stack:** Rust 2024, `thiserror`, `std::backtrace::Backtrace`, `std::panic::Location`. No new dependencies; no MSRV change.

---

## Baseline (verified 2026-04-25 against HEAD `0de8094`)

- `cargo test -p strider-error` → 16 unit tests pass (3 in `tests/fields.rs` + 9 in `tests/macro_contract.rs` + 4 in `tests/format.rs`) + 3 doctests pass (1 ignored).
- `cargo clippy -p strider-error --all-targets --no-deps -- -D warnings` → clean.
- Round 7 commits present: `931080e`, `427b2c8`.
- 9 in-tree wrappers via `define_error!` and the hand-rolled generic `dot::error::Error<E>`. 5 cross-crate bridges via `bridge_error!`.
- Verified `std::panic::Location` has a `Display` impl that produces exactly `file:line:column` (smoke test, see Findings → S1).
- Verified `write_chain_and_backtrace` is only called from `fields.rs:69` and `format.rs:40`. `dot::error::Error<E>` (the only out-of-crate consumer of the helper machinery) calls the public `fmt_chain_and_backtrace` method, not the `pub(crate)` helper — so changes to the helper body don't ripple.

---

## Review Findings — Executive Summary

**Zero correctness bugs found.** Three small items: two in source, one in tests.

### Simplification (S)

- **S1 — `write_chain_and_backtrace` open-codes Location's Display format.** [crates/strider-error/src/fields.rs:85-93](crates/strider-error/src/fields.rs#L85-L93):

  ```rust
  for (i, loc) in chain.iter().enumerate() {
      writeln!(
          w,
          "  at [{}] {}:{}:{}",
          i,
          loc.file(),
          loc.line(),
          loc.column(),
      )?;
  }
  ```

  `std::panic::Location` implements `Display` as `write!(f, "{}:{}:{}", self.file(), self.line(), self.column())` (verified: a smoke test prints `[/tmp/loc_display_check.rs:3:13]`, exactly the format the current code is hand-rolling). The whole inner format-args list collapses to:

  ```rust
  for (i, loc) in chain.iter().enumerate() {
      writeln!(w, "  at [{i}] {loc}")?;
  }
  ```

  Output is byte-identical. Saves three method calls and four format positionals; pulls in the captured-identifier idiom that round 7 introduced for `backtrace`. Pinned by every existing test that asserts on `"  at [N] "` and the column-pin test (`debug_prints_location_markers` checks `l.matches(':').count() >= 2`).

- **S2 — `format_traceback` source-walk uses fully-qualified `Error::source` where method syntax suffices.** [crates/strider-error/src/format.rs:36-38](crates/strider-error/src/format.rs#L36-L38):

  ```rust
  for e in std::iter::successors(Error::source(err), |e| Error::source(*e)) {
      let _ = writeln!(out, "  caused by: {e}");
  }
  ```

  `err` is `&(dyn Traceback + 'static)`. Since `Traceback: Error`, method resolution on `err.source()` finds `<dyn Error>::source` via the supertrait — same dispatch, same return type. Inside the closure, `e: &&(dyn Error + 'static)`; `e.source()` auto-derefs to `<dyn Error>::source(&**e)`, identical to `Error::source(*e)`. Drop the qualification:

  ```rust
  for e in std::iter::successors(err.source(), |e| e.source()) {
      let _ = writeln!(out, "  caused by: {e}");
  }
  ```

  No need to keep `use std::error::Error;` purely for the qualifier (it stays needed elsewhere — `format_traceback`'s param type doesn't import it but we use `Error` as a path; see Step 1 below for the call check).

  **Caveat:** the explicit `Error::source(*e)` form was the *result* of round 4's `ec67006` cleanup ("drop unneeded &dyn Error rebind"). The fully-qualified path was deliberate disambiguation against any inherent `source` method on the wrapper. The wrappers don't define an inherent `source`, so dispatch is unambiguous. Treat as a discussion-only finding (Q1) — borderline.

### Marginal Readability (R)

- **R3 — Redundant `#[track_caller]` on `probe()` test helper.** [crates/strider-error/tests/macro_contract.rs:64-68](crates/strider-error/tests/macro_contract.rs#L64-L68):

  ```rust
  #[track_caller]
  fn probe() -> Result<(), MyError> {
      let _ = std::fs::File::open("/definitely/not/a/real/path")?; // << expected loc line
      Ok(())
  }
  ```

  The annotation is dead weight. The test asserts that `Location::caller()` inside `ErrorFields::new` resolves to the `?` site — the chain runs `ErrorFields::new` (track_caller) ← `From::from` (track_caller) ← `?` desugaring at the call site. Track-caller bubbles up *through* track-callered callees back to the user's `?` line. Adding `#[track_caller]` to `probe()` itself would only matter if `probe()` were *called* from somewhere using `Location::caller()` — it isn't. Removing the annotation does not change `loc.file().ends_with("tests/macro_contract.rs")` because the `?` is inside `probe`, which lives in this same file.

  This is a true no-op cleanup; the test still pins the same invariant. Treat as a discussion-only finding (Q2) — could be argued it's "defensive documentation" that propagation is intended.

### Out of scope for this round

The following were considered and deliberately left alone — same rationale as in rounds 3–7 unless noted:

- **Privatize `ErrorFields.{backtrace, locations}` fields.** Rounds 3–7 deferred. The macro-generated `locations()` / `backtrace()` accessors and `dot::Error<E>` both read these via field access from outside the strider-error crate. Privatization is feasible if we add `ErrorFields::locations(&self) -> &LocationChain` and `::backtrace(&self) -> &Backtrace` accessor methods and update both the `define_error!` macro and `dot::error::Error<E>` to use methods. Net benefit is encapsulation hygiene only; touch surface is moderate (one macro arm, one hand-written impl). Re-flag if a third consumer of the fields ever appears.
- **`Option<Box<Backtrace>>` to skip the alloc when backtraces are disabled.** Round 2 deferred; no profiling demand.
- **`LocationChain` → `SmallVec<[&'static Location<'static>; 4]>`.** No profiling demand.
- **`Traceback::location_chain` returning `&[&'static Location<'static>]` instead of `&LocationChain`.** Public-API change. R1 (round 7) was internal-only.
- **Generic-aware `define_error!` to subsume `dot::error::Error<E>`.** Round 4 noted; threading a generic doubles macro size for one consumer.
- **Combine `kind: Box<Kind>` + `fields: ErrorFields` into a single boxed inner struct (anyhow-style).** Would shrink wrapper from 5 words to 1 at the cost of one indirection per accessor. Touches every wrapper macro arm and `dot::Error<E>`. No correctness motivation.
- **Drop `+ 'static` from `format_traceback`'s `&(dyn Traceback + 'static)` parameter.** Currently all in-tree wrappers are `'static`, so nothing is gained or lost; the bound matches the `Error::source` return type's `'static` and reads as deliberate. Would be a (compatibly) loosened public API.
- **DRY-ing `Traceback` impl bodies vs. inherent `locations()` / `backtrace()` in `define_error!`.** Both pairs read the same fields; the duplication is in the macro body, compiled once per wrapper. Eliminating one pair forces users to import `Traceback` for the trait methods (worse ergonomics) or removes the abstraction `format_traceback` relies on.
- **Blank-line separators between source-walk / locations / backtrace in `format_traceback`.** Cosmetic.
- **Multi-line source-error display alignment in `format_traceback`'s `caused by:` walk.** No in-tree complaint.
- **`compile_fail` doctest pinning the struct-variant restriction on `bridge_error!`.** The doc note added in round 5 is the pin.
- **Pinning `loc.line()` in the `track_caller_*` test.** Rounds 5/6 defer — brittle to test-file edits.

---

## Open Questions for the Reviewer

Each of these changes the shape of a task. Pick one per group. Assumed defaults are marked.

**Q1 — Land S2 (`Error::source(_)` → `_.source()`) or skip?**
- (A) **Default.** Skip. The fully-qualified form is the explicit result of round 4's `ec67006` cleanup; reverting it without a strong reason is churn.
- (B) Land S2. `err.source()` / `e.source()` are unambiguous (no inherent `source` on the wrapper), and method syntax matches the rest of the file.

**Q2 — Land R3 (remove `#[track_caller]` from `probe()`) or skip?**
- (A) **Default.** Skip. The annotation is harmless and reads as "this fn participates in caller tracking", which is at least documentation-adjacent.
- (B) Land R3. Annotation is dead weight; future readers may copy-paste it onto helpers where it won't propagate the way they expect.

**Q3 — Combine S1 with anything?**
- (A) **Default.** Land S1 alone (one commit). It's a clean simplification of the helper body.
- (B) If both S2 and S1 land, combine into a single "tighten format helpers" commit (mirrors round 7's `931080e`).

---

## File Structure (after execution, assuming all defaults: Q1=A, Q2=A, Q3=A)

```
crates/strider-error/
├── src/
│   ├── lib.rs        # unchanged
│   ├── fields.rs     # S1: writeln!(w, "  at [{i}] {loc}") via Location's Display
│   ├── define.rs     # unchanged
│   └── format.rs     # unchanged (S2 deferred per Q1=A)
└── tests/
    ├── fields.rs        # unchanged
    ├── format.rs        # unchanged
    └── macro_contract.rs  # unchanged (R3 deferred per Q2=A)
```

Downstream crates: zero changes. No public-API surface change, no signature change, no behavior change.

If the reviewer flips Q1 and/or Q2, add the corresponding sections from Tasks 2 / 3 below.

---

## Task 1: Use Location's Display impl in `write_chain_and_backtrace` (S1)

**Files:**
- Modify: [crates/strider-error/src/fields.rs](crates/strider-error/src/fields.rs)

- [ ] **Step 1: Collapse the per-location format into `{loc}`**

In [crates/strider-error/src/fields.rs](crates/strider-error/src/fields.rs), locate the helper at lines 80–96:

```rust
pub(crate) fn write_chain_and_backtrace<W: std::fmt::Write>(
    chain: &[&'static Location<'static>],
    backtrace: &Backtrace,
    w: &mut W,
) -> std::fmt::Result {
    for (i, loc) in chain.iter().enumerate() {
        writeln!(
            w,
            "  at [{}] {}:{}:{}",
            i,
            loc.file(),
            loc.line(),
            loc.column(),
        )?;
    }
    write!(w, "{backtrace}")
}
```

Replace the `for` body with the captured-identifier form using Location's `Display`:

```rust
pub(crate) fn write_chain_and_backtrace<W: std::fmt::Write>(
    chain: &[&'static Location<'static>],
    backtrace: &Backtrace,
    w: &mut W,
) -> std::fmt::Result {
    for (i, loc) in chain.iter().enumerate() {
        writeln!(w, "  at [{i}] {loc}")?;
    }
    write!(w, "{backtrace}")
}
```

Do not change the signature, the doc comment block above the function, or the trailing `write!` line.

Reason it's correct: `std::panic::Location<'_>` has a `Display` impl that emits `<file>:<line>:<column>` — verified by a one-off `rustc` smoke test that printed `[/tmp/loc_display_check.rs:3:13]`. The previous code emitted `"<file>:<line>:<column>"` for the same triple. Output is byte-identical.

- [ ] **Step 2: Verify behavior is unchanged**

Run: `cargo test -p strider-error`
Expected: 16 unit tests + 3 doctests pass (1 ignored). In particular:
- `tests/macro_contract.rs::debug_prints_location_markers` asserts `l.starts_with("  at [0] ") && l.matches(':').count() >= 2` — pins both the prefix and that file:line:col survives round-trip.
- `tests/format.rs::format_traceback_includes_location_marker` asserts `s.contains("  at [0] ")`.
- `tests/format.rs::format_traceback_prints_wrapper_display_exactly_once` asserts `s.contains("  at [0] ")` and the absence of duplication.

Run: `cargo clippy -p strider-error --all-targets --no-deps -- -D warnings`
Expected: clean.

- [ ] **Step 3: Workspace-wide sanity check**

Run: `cargo build --workspace`
Expected: PASS. The change is internal (`pub(crate)` helper body), so workspace consumers can't see it — belt-and-suspenders.

Run: `cargo test --workspace`
Expected: PASS. Catches anything in `dot::error::Error<E>` (which goes through `fmt_chain_and_backtrace` → `write_chain_and_backtrace`) that might somehow regress.

- [ ] **Step 4: Commit**

```bash
git add crates/strider-error/src/fields.rs
git commit -m "$(cat <<'EOF'
refactor(strider-error): use Location's Display in chain render

std::panic::Location implements Display as "file:line:column",
which is exactly what the per-location render in
write_chain_and_backtrace was open-coding with four positional
format args. Collapse to "  at [{i}] {loc}" via the captured-
identifier form. Output is byte-identical (verified by the
existing macro_contract debug_prints_location_markers assertion
on l.matches(':').count() >= 2).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2 (only if Q1=B): Drop fully-qualified `Error::source` in `format_traceback` (S2)

**Files:**
- Modify: [crates/strider-error/src/format.rs](crates/strider-error/src/format.rs)

- [ ] **Step 1: Switch both `Error::source(_)` calls to method syntax**

In [crates/strider-error/src/format.rs](crates/strider-error/src/format.rs), locate the source-walk at lines 36–38:

```rust
for e in std::iter::successors(Error::source(err), |e| Error::source(*e)) {
    let _ = writeln!(out, "  caused by: {e}");
}
```

Replace with:

```rust
for e in std::iter::successors(err.source(), |e| e.source()) {
    let _ = writeln!(out, "  caused by: {e}");
}
```

Method dispatch:
- `err: &(dyn Traceback + 'static)`. `Traceback: Error`, so `err.source()` resolves via the supertrait Error::source — exact same dispatch as `Error::source(err)`.
- `e: &&(dyn Error + 'static)` (the closure's argument is `&T` where `T = &(dyn Error + 'static)`). `e.source()` auto-derefs through the outer `&` and the `&dyn Error` to call `<dyn Error>::source(&**e)` — exact same dispatch as `Error::source(*e)`.

Check whether `use std::error::Error;` is still needed in [crates/strider-error/src/format.rs:3](crates/strider-error/src/format.rs#L3):

```bash
grep -n "Error" crates/strider-error/src/format.rs
```

After the edit, `Error` is no longer named anywhere in the file. Remove the import at line 3:

```rust
// Before
use std::error::Error;
use std::fmt::Write;

// After
use std::fmt::Write;
```

- [ ] **Step 2: Verify behavior**

Run: `cargo test -p strider-error`
Expected: 16 unit tests + 3 doctests pass (1 ignored). In particular:
- `tests/format.rs::format_traceback_walks_source_chain_top_to_bottom` asserts the outer Display precedes the `caused by:` line — directly exercises the modified loop.
- `tests/format.rs::format_traceback_prints_wrapper_display_exactly_once` asserts `!s.contains("caused by:")` for source-less errors — exercises the closure with `None` initial.

Run: `cargo clippy -p strider-error --all-targets --no-deps -- -D warnings`
Expected: clean. (If `Error` is left unused in scope after the edit, `unused_imports` would fire.)

Run: `cargo build --workspace`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/strider-error/src/format.rs
git commit -m "$(cat <<'EOF'
style(strider-error): use method syntax for Error::source walk

err.source() / e.source() resolve via the same Error supertrait
dispatch as the fully-qualified Error::source(err) /
Error::source(*e) — there's no inherent source method on the
wrapper to disambiguate against. Drop the qualifier and the
now-unused use std::error::Error import.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3 (only if Q2=B): Drop redundant `#[track_caller]` on `probe()` (R3)

**Files:**
- Modify: [crates/strider-error/tests/macro_contract.rs](crates/strider-error/tests/macro_contract.rs)

- [ ] **Step 1: Remove the annotation**

In [crates/strider-error/tests/macro_contract.rs](crates/strider-error/tests/macro_contract.rs), locate the test at lines 59–76:

```rust
#[test]
fn track_caller_on_question_mark_points_at_question_mark_site() {
    // This test pins that `?` on a `Result<_, $src>` -> Result<_, $wrapper>
    // places the location at the `?` line in *this* function, not inside
    // the generated From impl.
    #[track_caller]
    fn probe() -> Result<(), MyError> {
        let _ = std::fs::File::open("/definitely/not/a/real/path")?; // << expected loc line
        Ok(())
    }
    let err = probe().unwrap_err();
    let loc = err.locations()[0];
    assert!(
        loc.file().ends_with("tests/macro_contract.rs"),
        "location must point at the caller's file, got {}",
        loc.file(),
    );
}
```

Drop the `#[track_caller]` line on the inner `probe`:

```rust
#[test]
fn track_caller_on_question_mark_points_at_question_mark_site() {
    // This test pins that `?` on a `Result<_, $src>` -> Result<_, $wrapper>
    // places the location at the `?` line in *this* function, not inside
    // the generated From impl.
    fn probe() -> Result<(), MyError> {
        let _ = std::fs::File::open("/definitely/not/a/real/path")?; // << expected loc line
        Ok(())
    }
    let err = probe().unwrap_err();
    let loc = err.locations()[0];
    assert!(
        loc.file().ends_with("tests/macro_contract.rs"),
        "location must point at the caller's file, got {}",
        loc.file(),
    );
}
```

The annotation never participated in the chain `ErrorFields::new (track_caller) ← From::from (track_caller) ← ?`. The `?` site is inside `probe`, so `loc.file()` ends with `tests/macro_contract.rs` regardless of whether `probe` itself is `track_caller`-annotated.

- [ ] **Step 2: Verify the test still pins the original invariant**

Run: `cargo test -p strider-error --test macro_contract track_caller_on_question_mark_points_at_question_mark_site`
Expected: 1 test passes.

Run: `cargo test -p strider-error`
Expected: 16 unit tests + 3 doctests pass (1 ignored). Total test count unchanged.

- [ ] **Step 3: Commit**

```bash
git add crates/strider-error/tests/macro_contract.rs
git commit -m "$(cat <<'EOF'
test(strider-error): drop redundant #[track_caller] on probe helper

The track_caller chain that this test pins runs ErrorFields::new
← From::from ← ?, all inside probe() — annotating probe() itself
contributes nothing because probe is never called via a
Location::caller() consumer. Drop the annotation; the
file-suffix assertion still pins the same invariant.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: Workspace sanity sweep

**Files:** Run-only, no edits.

- [ ] **Step 1: Full workspace build**

Run: `cargo build --workspace`
Expected: PASS.

- [ ] **Step 2: Full workspace tests**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 3: Strict lint on the touched crate**

Run: `cargo clippy -p strider-error --all-targets --no-deps -- -D warnings`
Expected: clean.

- [ ] **Step 4: Workspace lint**

Run: `cargo clippy --workspace -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Smoke-run the example**

Run: `cargo run --example analyzer`
Expected: `cfg.html`, `graph.html`, `graph-opt.html` produced.

---

## Out of Scope (considered, rejected or deferred)

Items already listed inline under **Out of scope for this round** above. Restated here for completeness:

- Privatize `ErrorFields.{backtrace, locations}` fields (rounds 3–7 deferred; would touch `define_error!` macro arm + `dot::error::Error<E>`).
- `Option<Box<Backtrace>>` to skip alloc under disabled backtraces.
- `LocationChain` → `SmallVec` micro-optimization.
- Public-API change: `Traceback::location_chain` → `&[…]`.
- Generic-aware `define_error!` to subsume `dot::error::Error<E>`.
- Boxed-inner footprint refactor (anyhow-style).
- Loosening `+ 'static` on `format_traceback`'s parameter.
- DRY-ing `Traceback` impl bodies against inherent `locations()` / `backtrace()`.
- Blank-line separators in `format_traceback` output.
- Multi-line source-error indentation in the `caused by:` walk.
- `compile_fail` doctest pinning the struct-variant restriction on `bridge_error!`.
- Tightening location-pinning tests with `loc.line()` (rounds 5/6 defer).
- (Round-7 R2's `iter::successors` form is the new canonical, established in `931080e`.)
