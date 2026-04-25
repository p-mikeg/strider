# strider-error Crate Review — Round 6 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Sixth-pass review of `strider-error` after rounds 1–5. Round 5 was doc-only and shipped. The crate is in solid shape: zero correctness bugs found this round either. Round 6 covers three small readability / consistency wins inside the source files, and two test-tightness gaps where a real regression would slip past the current pins. No public-API changes, no behavior changes for any in-tree consumer.

**Architecture:** Same four-file layout as before (`lib.rs`, `fields.rs`, `define.rs`, `format.rs`), ~330 LoC. R1 simplifies one local in `format.rs`. R2 harmonizes a path prefix in `define.rs` (`::core::convert::From` → `::std::convert::From`). T1 and T2 add assertions to existing tests.

**Tech Stack:** Rust 2024, `thiserror`, `std::backtrace::Backtrace`, `std::panic::Location`. No new dependencies; no MSRV change.

---

## Baseline (verified 2026-04-25 against HEAD `4d1f916`)

- `cargo test -p strider-error` → 16 unit tests pass (3 in `tests/fields.rs` + 9 in `tests/macro_contract.rs` + 4 in `tests/format.rs`) + 3 doctests pass (1 ignored).
- `cargo clippy -p strider-error --all-targets --no-deps -- -D warnings` → clean.
- Round 5 commits present: `d250b95`, `13a67a3`, `4d1f916`.
- 9 in-tree wrappers via `define_error!` (`opt`, `pattern`, `ir`, `analyzer`, `target`, `reader`, `cfg`, plus the test wrappers in this crate) and the hand-rolled generic `dot::error::Error<E>` that mirrors the macro. 5 cross-crate bridges via `bridge_error!`.

---

## Review Findings — Executive Summary

**Zero correctness bugs found.** Five small items, three in source and two in tests. None are behavioral regressions.

### Readability / Simplification (R)

- **R1 — `format_traceback`'s explicit `&dyn Error` rebind is unnecessary.** [format.rs:39-41](crates/strider-error/src/format.rs#L39-L41) writes:

  ```rust
  let err_ref: &(dyn Error + 'static) = err;
  let mut cur = err_ref.source();
  ```

  with a two-line comment explaining trait upcasting. But `Traceback: std::error::Error` is already a supertrait bound, and `Error::source(err)` resolves directly via the supertrait — no upcast needed at the call site. Replacing the rebind with `let mut cur = Error::source(err);` removes one local, two lines of comment, and the implicit assumption that `Traceback` won't ever introduce a `source` method that shadows.

  Behavior is identical in both forms; this is a pure clarity edit. The supertrait bound is still pinned by the trait declaration in `fields.rs`, so the comment about "trait upcasting, stable since 1.86" is no longer carrying any pin work — the trait declaration does it.

- **R2 — Macro path inconsistency: `define_error!` uses `::std::convert::From` but `bridge_error!` uses `::core::convert::From`.** [define.rs:140](crates/strider-error/src/define.rs#L140) and [define.rs:152](crates/strider-error/src/define.rs#L152) both write `::std::convert::From`; [define.rs:225](crates/strider-error/src/define.rs#L225) writes `::core::convert::From`. Both resolve to the same trait (`core` is re-exported from `std`), so this is harmless at runtime, but the inconsistency is awkward when reading the two macros side-by-side. Every other absolute path in both macros uses `::std::` (`::std::boxed::Box`, `::std::fmt::*`, `::std::error::Error`, `::std::option::Option`, `::std::backtrace::Backtrace`). Harmonize `bridge_error!`'s `::core::convert::From` to `::std::convert::From` to match.

- **R3 — Optional: silence-comment inconsistency in `format_traceback`.** [format.rs:34-37](crates/strider-error/src/format.rs#L34-L37) and [format.rs:48-54](crates/strider-error/src/format.rs#L48-L54) carry comments explaining the `let _ = writeln!(...)` swallow ("Writing into a String is infallible"); the middle hop at [format.rs:43](crates/strider-error/src/format.rs#L43) silently uses the same pattern with no comment. Three options:
  - Drop the comments on the other two — the pattern is idiomatic and the helper at the bottom of `fields.rs` already explains the invariant via its `W: Write` signature.
  - Add a third short comment at the middle hop for parity.
  - Leave it.

  Marginal; only pulls weight if R1 lands and you're already touching the file. **Default: drop the now-redundant comments since R1's edit removes the upcast comment and this leaves one structural form.**

### Test-Tightness (T)

- **T1 — `format_traceback_prints_wrapper_display_exactly_once` doesn't assert that `caused by:` is *absent* when there is no source.** [tests/format.rs:33-42](crates/strider-error/tests/format.rs#L33-L42) constructs `MyKind::Boom` (no `#[from]`, no source) and asserts the Display line appears once and the location marker is present, but a regression where the source-walk accidentally re-emits the wrapper itself ("error: foo\n  caused by: foo") would still pass this test, because the Display marker appears in `error: …` not in `caused by: …`.

  Add one line: `assert!(!s.contains("caused by:"), "source-less error must not emit caused-by line; got:\n{s}");`. One assertion in an existing test, no new test needed.

- **T2 — No test pins the column field in the `at [N] file:line:col` format.** Both [tests/format.rs:41,80](crates/strider-error/tests/format.rs#L41) and [tests/macro_contract.rs:100](crates/strider-error/tests/macro_contract.rs#L100) check `s.contains("  at [0] ")`; if a refactor accidentally dropped `loc.column()` from the format string in [fields.rs:91](crates/strider-error/src/fields.rs#L91), the substring `"  at [0] "` would still match (rendered output `"  at [0] foo.rs:42\n"` contains the prefix). Pin via a colon-count check on the matching line (no `regex` dev-dep needed): one assertion in the existing `debug_prints_location_markers` test that asserts the `at [0] …` line contains at least two colons in the tail.

### Out of scope for this round

The following were considered and deliberately left alone — same rationale as in rounds 3, 4, and 5 unless noted:

- **Privatize `ErrorFields.{backtrace, locations}` fields and add accessor methods.** Rounds 3, 4, and 5 deferred. The macro-generated wrappers and `dot::Error<E>` both read these fields directly via field access; privatizing requires adding accessor methods on `ErrorFields` and updating ~5 reference sites across two files. Net benefit is encapsulation hygiene — small, since the fields are read-only in practice (mutation goes through `push_caller(self) -> Self`). Still not pulling its weight versus the API churn. Re-flag if a third consumer ever needs the fields.
- **`Option<Box<Backtrace>>` to skip the heap allocation when `RUST_BACKTRACE` is unset.** Round 2 deferred. `Backtrace::capture()` itself returns a `Disabled` variant cheaply; the cost we'd save is the ~32-byte `Box` allocation per error construction. Defer until profiling demands it.
- **`LocationChain` → `SmallVec<[&'static Location<'static>; 4]>`.** Round 3 deferred. Same argument: no profiling demand.
- **Generic-aware `define_error!` to subsume `dot::error::Error<E>`.** Round 4 noted the macro is "monomorphic" and `dot::Error<E>` is hand-rolled to mirror it. Threading a generic type parameter through every macro arm would more than double the macro's size for one consumer. Defer indefinitely.
- **Blank-line separators between source-walk / locations / backtrace in `format_traceback`.** Cosmetic; no in-tree consumer complaining.
- **Multi-line source-error display alignment in `format_traceback`'s `caused by:` walk.** If a source error's `Display` is multi-line, only the first line gets the `"  caused by: "` prefix; subsequent lines lack indentation. None of the in-tree `thiserror` enums currently produce multi-line displays. Worth fixing if/when a real consumer hits it.
- **`compile_fail` doctest pinning the struct-variant restriction on `bridge_error!`.** Round 5 added a doc note for this; the doc is the pin. An executable negative test would over-engineer it.
- **Second-arm `bridge_error!` consuming `ir::ValidationErrors` directly.** One call site, not worth a second macro form. Round 2's out-of-scope list.
- **Tighten the `track_caller_on_question_mark_points_at_question_mark_site` assertion to also pin `loc.line()` to the `?` line.** Brittle to test-file edits — small offset changes cascade into failures unrelated to the macro contract. The file-suffix check is enough.

---

## Open Questions for the Reviewer

Each of these changes the shape of a task. Pick one per group. Assumed defaults are marked.

**Q1 — R3 silence-comment treatment: drop, add, or leave?**

- **(A)** Drop the existing comments at [format.rs:34-37](crates/strider-error/src/format.rs#L34-L37) and [format.rs:48-54](crates/strider-error/src/format.rs#L48-L54). Rationale: the `let _ = writeln!()` pattern is idiomatic Rust for swallowing a `fmt::Result` from a `String` sink; comments restate the obvious. The `W: fmt::Write` signature on `write_chain_and_backtrace` already documents the broader contract. **Default.**
- **(B)** Add a parity comment at the middle hop ([format.rs:43](crates/strider-error/src/format.rs#L43)). Heavier; preserves explanatory style.
- **(C)** Leave it. Skip R3 entirely.

Assume **(A)**.

**Q2 — Include T2 (location-format-column pin)?** **User picked (B): include T2.**

- **(B)** Include T2 with the colon-count heuristic.

**Q3 — Combine R1+R2+R3 into one commit, or split?**

- **(A)** Three commits, one per finding. Easier to revert any single change without affecting others. **Default.**
- **(B)** One combined commit "refactor(strider-error): small readability tidy". Easier to skim in `git log`.

Assume **(A)**.

---

## File Structure (after execution, assuming defaults)

```
crates/strider-error/
├── src/
│   ├── lib.rs        # unchanged
│   ├── fields.rs     # unchanged
│   ├── define.rs     # R2: ::core::convert::From → ::std::convert::From
│   │                 #     in bridge_error! macro body
│   └── format.rs     # R1: drop &dyn Error rebind + upcast comment
│                     # R3: drop the remaining "infallible String" comments
└── tests/
    ├── fields.rs        # unchanged
    ├── format.rs        # T1: assert no "caused by:" in source-less case
    └── macro_contract.rs  # T2: pin column field in location format
```

Downstream crates: zero changes. No public-API surface change, no signature change, no behavior change.

---

## Task 1: Simplify the source-walk in `format_traceback` (R1 / drops upcast rebind)

**Files:**
- Modify: [crates/strider-error/src/format.rs](crates/strider-error/src/format.rs)

- [ ] **Step 1: Remove the explicit `&dyn Error` rebind**

In [crates/strider-error/src/format.rs](crates/strider-error/src/format.rs), locate the source-walk block at lines 38-45:

```rust
    // Source-chain walk via the Error supertrait. `&dyn Traceback` upcasts
    // to `&dyn Error` implicitly (trait upcasting, stable since 1.86).
    let err_ref: &(dyn Error + 'static) = err;
    let mut cur = err_ref.source();
    while let Some(e) = cur {
        let _ = writeln!(out, "  caused by: {e}");
        cur = e.source();
    }
```

Replace with:

```rust
    let mut cur = Error::source(err);
    while let Some(e) = cur {
        let _ = writeln!(out, "  caused by: {e}");
        cur = e.source();
    }
```

`Error::source(err)` resolves via the `Traceback: std::error::Error` supertrait bound declared in [fields.rs:108](crates/strider-error/src/fields.rs#L108); no upcast or rebind is required. Calling the method as a fully-qualified path makes it explicit which trait we're using and is robust against any future inherent or trait method on `Traceback` named `source`.

Do not change the writeln line, the loop body, or anything outside this block.

- [ ] **Step 2: Verify behavior is unchanged**

Run: `cargo test -p strider-error --test format`
Expected: 4 tests pass. The `format_traceback_walks_source_chain_top_to_bottom` test in particular pins outer-then-caused-by ordering and would fail if the walk no longer entered the loop.

- [ ] **Step 3: Verify nothing else broke**

Run: `cargo test -p strider-error`
Expected: 16 unit tests + 3 doctests pass (1 ignored).

Run: `cargo clippy -p strider-error --all-targets --no-deps -- -D warnings`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/strider-error/src/format.rs
git commit -m "$(cat <<'EOF'
refactor(strider-error): drop unneeded &dyn Error rebind in format_traceback

The two-line upcast rebind plus its explanatory comment were defensive
against a hypothetical future `Traceback::source` shadowing the
supertrait method. Calling `Error::source(err)` as a fully-qualified
path makes the trait choice explicit at the call site and removes the
need for both the rebind and the comment. Behavior is identical: the
`Traceback: std::error::Error` supertrait bound (declared in fields.rs)
already guarantees the resolution.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Harmonize macro path prefix on `From` (R2)

**Files:**
- Modify: [crates/strider-error/src/define.rs](crates/strider-error/src/define.rs)

- [ ] **Step 1: Update `bridge_error!`'s `From` impl path**

In [crates/strider-error/src/define.rs](crates/strider-error/src/define.rs), locate the `bridge_error!` macro arm at lines 223-235. Find:

```rust
    ($inner:ty => $outer:ident, $outer_kind:ident :: $variant:ident) => {
        impl ::core::convert::From<$inner> for $outer {
            #[track_caller]
            fn from(e: $inner) -> Self {
```

Replace `::core::convert::From` with `::std::convert::From`:

```rust
    ($inner:ty => $outer:ident, $outer_kind:ident :: $variant:ident) => {
        impl ::std::convert::From<$inner> for $outer {
            #[track_caller]
            fn from(e: $inner) -> Self {
```

This matches the path used by `define_error!`'s two `From` impls at [define.rs:140](crates/strider-error/src/define.rs#L140) and [define.rs:152](crates/strider-error/src/define.rs#L152), and the `::std::` prefix used everywhere else in both macros. Both `core::convert::From` and `std::convert::From` are the same trait — `From` is in `core` and re-exported from `std` — so the change is a pure source-style harmonization.

Also update the macro-expansion sketch in the doc comment if it shows the path. Inspect [define.rs:208-220](crates/strider-error/src/define.rs#L208-L220) (the `Expands to:` block) and confirm:

```rust
/// impl ::core::convert::From<InnerError> for OuterError {
```

Replace with:

```rust
/// impl ::std::convert::From<InnerError> for OuterError {
```

If the existing doc shows a different path (e.g. unqualified `From`), keep it consistent with whatever's there — but if it uses `::core::convert::From`, swap it to `::std::convert::From` so the doc matches the macro body.

- [ ] **Step 2: Verify the macro still compiles**

Run: `cargo check -p strider-error --all-targets`
Expected: PASS.

Run: `cargo test -p strider-error --test macro_contract bridge_error_macro_extends_chain_by_one`
Expected: 1 test passes. The bridge test exercises `bridge_error!` end-to-end.

- [ ] **Step 3: Verify the doctest still parses and runs**

Run: `cargo test -p strider-error --doc`
Expected: 3 doctests pass + 1 ignored. The `bridge_error` doctest at line 181 in particular still goes through `?` propagation across the bridge.

- [ ] **Step 4: Verify all in-tree consumers still build**

Run: `cargo build -p opt -p analyzer -p pattern`
Expected: PASS. These are the three consumers of `bridge_error!` that resolve `From` against the path the macro emits. (Same trait, so this is belt-and-suspenders — if the change broke compilation, `cargo check -p strider-error --all-targets` would already fail.)

- [ ] **Step 5: Commit**

```bash
git add crates/strider-error/src/define.rs
git commit -m "$(cat <<'EOF'
style(strider-error): harmonize bridge_error! From path on ::std::convert

Every other absolute path in both define_error! and bridge_error! uses
::std:: (Box, fmt, error, option, backtrace). bridge_error!'s
::core::convert::From was an isolated outlier; both paths resolve to
the same trait, so this is a pure source-style change.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Drop redundant `let _ = writeln!` comments in `format_traceback` (R3 / Q1=A)

**Files:**
- Modify: [crates/strider-error/src/format.rs](crates/strider-error/src/format.rs)

This task assumes Task 1 has landed (so the upcast comment is already gone). The remaining two comments are at the first writeln (lines 34-37 in the *original* file, now ~35-37 after Task 1) and at the bottom-of-function call (lines 48-50 in the original).

- [ ] **Step 1: Remove the comment above the first `writeln!`**

In [crates/strider-error/src/format.rs](crates/strider-error/src/format.rs), find the body of `format_traceback`. After Task 1 the top of the function body looks like:

```rust
    let mut out = String::new();
    // Writing into a String is infallible; the `let _` silences the
    // Result produced by the fmt::Write trait.
    let _ = writeln!(out, "error: {err}");
```

Replace with (drop the two-line comment):

```rust
    let mut out = String::new();
    let _ = writeln!(out, "error: {err}");
```

- [ ] **Step 2: Remove the comment above the bottom `write_chain_and_backtrace` call**

Same file, find:

```rust
    // Reuse the same chain+backtrace formatting that Debug impls use
    // (ErrorFields::fmt_chain_and_backtrace). Writing into a String is
    // infallible; the `let _` silences the fmt::Result.
    let _ = crate::fields::write_chain_and_backtrace(
        err.location_chain(),
        err.origin_backtrace(),
        &mut out,
    );
    out
```

Replace with (drop the three-line comment):

```rust
    let _ = crate::fields::write_chain_and_backtrace(
        err.location_chain(),
        err.origin_backtrace(),
        &mut out,
    );
    out
```

The reuse story is now told only by the helper's own docstring at [fields.rs:73-79](crates/strider-error/src/fields.rs#L73-L79), which is the right place for it (the helper is the one shared piece, not the call site).

Do not touch the loop body, the function signature, or any code outside these two comment-only deletions.

- [ ] **Step 3: Verify behavior is unchanged**

Run: `cargo test -p strider-error`
Expected: 16 unit tests + 3 doctests pass (1 ignored).

Run: `cargo clippy -p strider-error --all-targets --no-deps -- -D warnings`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/strider-error/src/format.rs
git commit -m "$(cat <<'EOF'
refactor(strider-error): drop redundant fmt::Write infallibility comments

The `let _ = writeln!()` pattern is idiomatic for the case where the
sink can't fail. The cross-module reuse story for write_chain_and_back-
trace lives on the helper's own docstring; the call sites no longer
need to repeat it.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: Pin absence of `caused by:` for source-less errors (T1)

**Files:**
- Modify: [crates/strider-error/tests/format.rs](crates/strider-error/tests/format.rs)

- [ ] **Step 1: Add the absence assertion to `format_traceback_prints_wrapper_display_exactly_once`**

In [crates/strider-error/tests/format.rs](crates/strider-error/tests/format.rs), locate the test at lines 32-42:

```rust
#[test]
fn format_traceback_prints_wrapper_display_exactly_once() {
    let err: MyError = MyKind::Boom.into();
    let s = strider_error::format_traceback(&err);
    let count = s.matches("unique-display-marker-7a3f").count();
    assert_eq!(
        count, 1,
        "expected the Display line once; got {count} occurrences in:\n{s}",
    );
    assert!(s.contains("  at [0] "), "locations dropped; got:\n{s}");
}
```

Add one more assertion immediately after the location check, so the body becomes:

```rust
#[test]
fn format_traceback_prints_wrapper_display_exactly_once() {
    let err: MyError = MyKind::Boom.into();
    let s = strider_error::format_traceback(&err);
    let count = s.matches("unique-display-marker-7a3f").count();
    assert_eq!(
        count, 1,
        "expected the Display line once; got {count} occurrences in:\n{s}",
    );
    assert!(s.contains("  at [0] "), "locations dropped; got:\n{s}");
    assert!(
        !s.contains("caused by:"),
        "source-less error must not emit a caused-by line; got:\n{s}",
    );
}
```

`MyKind::Boom` has no `#[from]` and no `#[source]` field — `Error::source(err)` returns `None` and the source-walk loop body never executes. A regression that re-printed the wrapper as its own source ("error: foo\n  caused by: foo") would still pass the existing two assertions because the marker would appear in the `error:` line.

Do not change the `MyKind::Boom` variant or any of the existing assertions.

- [ ] **Step 2: Run the augmented test**

Run: `cargo test -p strider-error --test format format_traceback_prints_wrapper_display_exactly_once`
Expected: 1 test passes.

- [ ] **Step 3: Run the full suite**

Run: `cargo test -p strider-error`
Expected: 16 unit tests + 3 doctests pass (1 ignored). Total test count unchanged (same test, one more assertion).

- [ ] **Step 4: Commit**

```bash
git add crates/strider-error/tests/format.rs
git commit -m "$(cat <<'EOF'
test(strider-error): pin absence of caused-by line for source-less errors

The existing assertions catch a regression where the Display line is
duplicated, but not one where the source-walk re-emits the wrapper
itself as its own source. Add a single assert! that pins the missing
"caused by:" line for the no-source case.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: Pin column field in location format (T2)

**Files:**
- Modify: [crates/strider-error/tests/macro_contract.rs](crates/strider-error/tests/macro_contract.rs)

- [ ] **Step 1: Augment `debug_prints_location_markers` with a colon-count check**

In [crates/strider-error/tests/macro_contract.rs](crates/strider-error/tests/macro_contract.rs), locate the test at lines 95-101:

```rust
#[test]
fn debug_prints_location_markers() {
    let err: MyError = MyKind::Boom.into();
    let dbg = format!("{err:?}");
    assert!(dbg.contains("boom"), "Debug should start with Display line; got {dbg:?}");
    assert!(dbg.contains("  at [0] "), "Debug should include location[0]; got {dbg:?}");
}
```

Add one more assertion immediately after the existing `at [0]` check:

```rust
#[test]
fn debug_prints_location_markers() {
    let err: MyError = MyKind::Boom.into();
    let dbg = format!("{err:?}");
    assert!(dbg.contains("boom"), "Debug should start with Display line; got {dbg:?}");
    assert!(dbg.contains("  at [0] "), "Debug should include location[0]; got {dbg:?}");
    assert!(
        dbg.lines().any(|l| l.starts_with("  at [0] ") && l.matches(':').count() >= 2),
        "location format must include both line and column (`file:line:col`); got {dbg:?}",
    );
}
```

Rationale: a regression that drops `loc.column()` from the format string in [crates/strider-error/src/fields.rs:91](crates/strider-error/src/fields.rs#L91) would render `  at [0] foo.rs:42` (one colon), failing the new assertion. The current `:line:col` form has two colons in the tail and passes. A unix-only test environment means we don't need to worry about `C:\…` Windows paths inflating the count.

Do not change the existing two assertions and do not touch any other test in the file.

- [ ] **Step 2: Run the augmented test**

Run: `cargo test -p strider-error --test macro_contract debug_prints_location_markers`
Expected: 1 test passes.

- [ ] **Step 3: Run the full suite**

Run: `cargo test -p strider-error`
Expected: 16 unit tests + 3 doctests pass (1 ignored). Total test count unchanged (same test, one more assertion).

- [ ] **Step 4: Commit**

```bash
git add crates/strider-error/tests/macro_contract.rs
git commit -m "$(cat <<'EOF'
test(strider-error): pin column field in location render format

The existing `assert!(dbg.contains("  at [0] "))` check still passes
even if a regression drops `loc.column()` from the format string,
because `"  at [0] foo.rs:42"` still contains the prefix. Add a
colon-count assertion on the matching line: `:line:col` has two
colons; a regression to `:line` only has one.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: Workspace sanity sweep

**Files:** Run-only, no edits.

- [ ] **Step 1: Full workspace build**

Run: `cargo build --workspace`
Expected: PASS.

- [ ] **Step 2: Full workspace tests**

Run: `cargo test --workspace`
Expected: PASS. Specifically:
- `cargo test -p strider-error` — 16 unit tests + 3 doctests (1 ignored).
- `cargo test -p dot --test error` — unaffected (no signature change touched).
- `cargo test -p analyzer --test error_chain` — unaffected.
- `cargo test -p reader --test error` — unaffected.

- [ ] **Step 3: Strict lint on the touched crate**

Run: `cargo clippy -p strider-error --all-targets --no-deps -- -D warnings`
Expected: clean.

- [ ] **Step 4: Workspace lint**

Run: `cargo clippy --workspace -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Smoke-run the example**

Run: `cargo run --example analyzer`
Expected: `cfg.html`, `graph.html`, `graph-opt.html` produced — confirms no runtime regression via the error path. (Should be impossible to break given this round is doc/comment/style-only plus one assertion, but the sweep is cheap.)

---

## Out of Scope (considered, rejected or deferred)

Items already listed inline under **Out of scope for this round** above. Restated here for completeness:

- Privatize `ErrorFields.backtrace` / `ErrorFields.locations` fields (rounds 3, 4, 5 deferred; unchanged).
- `Option<Box<Backtrace>>` to skip the alloc under disabled backtraces.
- `LocationChain` → `SmallVec` micro-optimization.
- Generic-aware `define_error!` to subsume `dot::error::Error<E>`.
- Blank-line separators in `format_traceback` output.
- Multi-line source-error indentation in the `caused by:` walk.
- `compile_fail` doctest pinning the struct-variant restriction on `bridge_error!`.
- Second-arm `bridge_error!` consuming `ir::ValidationErrors` directly.
- Tightening `track_caller_on_question_mark_points_at_question_mark_site` to also pin `loc.line()`.
- (T2 was added to this round — see Task 5.)
