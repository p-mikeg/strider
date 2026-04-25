# strider-error Crate Review — Round 7 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Seventh-pass review of `strider-error` after rounds 1–6. Round 6 shipped (5 commits, all present in `git log`). Round 7 is again zero correctness bugs — three small readability/idiom edits in source plus one test-tightness gap on `bridge_error!`'s `#[track_caller]`. No public-API changes, no behavior changes for any in-tree consumer.

**Architecture:** Same four-file layout (`lib.rs`, `fields.rs`, `define.rs`, `format.rs`), ~330 LoC. R1 swaps an internal helper from `&Vec<…>` to `&[…]`. R2 replaces the source-walk while-let with `std::iter::successors`. R3 captures a format identifier. T1 adds one assertion to an existing bridge test.

**Tech Stack:** Rust 2024, `thiserror`, `std::backtrace::Backtrace`, `std::panic::Location`. No new dependencies; no MSRV change.

---

## Baseline (verified 2026-04-25 against HEAD `09066f0`)

- `cargo test -p strider-error` → 16 unit tests pass (3 in `tests/fields.rs` + 9 in `tests/macro_contract.rs` + 4 in `tests/format.rs`) + 3 doctests pass (1 ignored).
- `cargo clippy -p strider-error --all-targets --no-deps -- -D warnings` → clean.
- Round 6 commits present: `ec67006`, `4e920bd`, `44d1ee2`, `7a5a85a`, `09066f0`.
- 9 in-tree wrappers via `define_error!` and the hand-rolled generic `dot::error::Error<E>`. 5 cross-crate bridges via `bridge_error!`.

---

## Review Findings — Executive Summary

**Zero correctness bugs found.** Four small items: three in source, one in tests. None are behavioral regressions.

### Readability / Simplification (R)

- **R1 — `write_chain_and_backtrace` takes `&LocationChain` (a `&Vec<…>`) where `&[…]` would do.** [fields.rs:80-84](crates/strider-error/src/fields.rs#L80-L84):

  ```rust
  pub(crate) fn write_chain_and_backtrace<W: std::fmt::Write>(
      chain: &LocationChain,
      backtrace: &Backtrace,
      w: &mut W,
  ) -> std::fmt::Result {
  ```

  `LocationChain = Vec<&'static Location<'static>>`, so the parameter is `&Vec<…>`. The body only iterates (`for (i, loc) in chain.iter().enumerate()`), so a slice would work and is the standard idiom (`clippy::ptr_arg` flavor). The function is `pub(crate)`, used from the macro-generated `Debug` impls (via `ErrorFields::fmt_chain_and_backtrace`, which passes `&self.locations`) and from `format_traceback` (which passes `err.location_chain()`, a `&LocationChain`). Both call sites coerce `&Vec<…>` → `&[…]` automatically. The change is local and signals "this fn only reads".

  Note that `Traceback::location_chain` and the wrapper accessor `locations()` still return `&LocationChain` — those are public APIs and out-of-scope here. R1 only touches the internal helper.

- **R2 — Source-walk in `format_traceback` uses a hand-rolled `while let`+`mut cur`.** [format.rs:36-40](crates/strider-error/src/format.rs#L36-L40):

  ```rust
  let mut cur = Error::source(err);
  while let Some(e) = cur {
      let _ = writeln!(out, "  caused by: {e}");
      cur = e.source();
  }
  ```

  Equivalent shorter form with `std::iter::successors`:

  ```rust
  for e in std::iter::successors(Error::source(err), |e| e.source()) {
      let _ = writeln!(out, "  caused by: {e}");
  }
  ```

  Drops one `mut` local and the manual `cur = e.source()` re-bind. `std::iter::successors` is precisely the "build an iterator from a seed + step" combinator and matches the shape of the error walk one-to-one.

  **Caveat:** the explicit `while let` is the canonical form in the std `Error` docs and in much existing Rust code, so this is a borderline call. Treat as a discussion-only finding (Q1 below) — do not land unilaterally.

- **R3 — `write!(w, "{}", backtrace)` could use a captured identifier.** [fields.rs:95](crates/strider-error/src/fields.rs#L95):

  ```rust
  write!(w, "{}", backtrace)
  ```

  → `write!(w, "{backtrace}")`. Captured-identifier form has been stable since Rust 1.58. Trivial textual edit; identical codegen. (Same crate already uses captured idents in [fields.rs:88-93](crates/strider-error/src/fields.rs#L88-L93) for `i`, etc., though those mix in method calls so they're stuck on positional form.)

### Test-Tightness (T)

- **T1 — `bridge_error_macro_extends_chain_by_one` asserts chain length is 2 but does not pin where the second entry resolves to.** [tests/macro_contract.rs:129-141](crates/strider-error/tests/macro_contract.rs#L129-L141):

  ```rust
  #[test]
  fn bridge_error_macro_extends_chain_by_one() {
      fn inner() -> Result<(), MyError> { Err(MyKind::Boom.into()) }
      fn outer() -> Result<(), OuterError> { inner()?; Ok(()) }

      let err = outer().unwrap_err();
      assert_eq!(err.locations().len(), 2, "origin + one bridge push_caller = 2");
      assert!(matches!(err.kind(), OuterKind::Inner(MyKind::Boom)));
  }
  ```

  `bridge_error!` puts `#[track_caller]` on its emitted `From` impl ([define.rs:225-233](crates/strider-error/src/define.rs#L225-L233)) and calls `fields.push_caller()` ([fields.rs:53-58](crates/strider-error/src/fields.rs#L53-L58), also `#[track_caller]`). If either annotation got dropped in a refactor, `locations()[1]` would resolve into `core/src/convert/mod.rs` or into the strider-error source itself instead of `tests/macro_contract.rs` — the chain would still be length 2 and the existing assertion would pass.

  Pin via a file-suffix check on `locations()[1]` — same shape as the existing `track_caller_on_question_mark_points_at_question_mark_site` test at [tests/macro_contract.rs:60-76](crates/strider-error/tests/macro_contract.rs#L60-L76). One assertion in the existing test, no new test needed.

### Out of scope for this round

The following were considered and deliberately left alone — same rationale as in rounds 3, 4, 5, 6 unless noted:

- **Privatize `ErrorFields.{backtrace, locations}` fields.** Rounds 3-6 deferred. The macro's `locations()`/`backtrace()` accessors and `dot::Error<E>` both read these via field access from outside the strider-error crate, so the fields must stay `pub` unless we also add accessor methods on `ErrorFields`. Net benefit is encapsulation hygiene only — small. Re-flag if a third consumer ever needs the fields.
- **`Option<Box<Backtrace>>` to skip the alloc when backtraces are disabled.** Round 2 deferred. ~32 bytes saved per error construction; `Backtrace::capture()` itself is already cheap on disabled. Defer until profiling demands it.
- **`LocationChain` → `SmallVec<[&'static Location<'static>; 4]>`.** Same argument: no profiling demand.
- **`Traceback::location_chain` returning `&[&'static Location<'static>]` instead of `&LocationChain`.** Public-API change touching all wrapper macros and `dot::Error<E>`'s hand-rolled impl. R1 stays internal-only.
- **Generic-aware `define_error!` to subsume `dot::error::Error<E>`.** Round 4 noted. Threading a generic type parameter through every macro arm doubles the macro's size for one consumer. Defer indefinitely.
- **Blank-line separators between source-walk / locations / backtrace in `format_traceback`.** Cosmetic; no in-tree complaint.
- **Multi-line source-error display alignment in `format_traceback`'s `caused by:` walk.** None of the in-tree `thiserror` enums currently produce multi-line displays. Worth fixing if/when a real consumer hits it.
- **`compile_fail` doctest pinning the struct-variant restriction on `bridge_error!`.** The doc note added in round 5 is the pin.
- **Pinning `loc.line()` in `track_caller_on_question_mark_points_at_question_mark_site`.** Round 6's defer reasoning still applies — brittle to test-file edits.
- **Unit test for `ErrorFields::new()` resolving `Location::caller()` to the user's site.** This is already covered transitively via the macro-contract `track_caller_on_question_mark_points_at_question_mark_site` test, plus T1 for the bridge variant.

---

## Open Questions for the Reviewer

Each of these changes the shape of a task. Pick one per group. Assumed defaults are marked.

**Q1 — Land R2 (`iter::successors` source-walk) or skip?** — **User picked (B): land R2.**

**Q2 — Combine R1+R3 (and optionally R2) into one commit, or split?** — **User picked (B): one combined refactor commit for R1+R2+R3.** T1 stays its own commit (different scope).

---

## File Structure (after execution, assuming defaults)

```
crates/strider-error/
├── src/
│   ├── lib.rs        # unchanged
│   ├── fields.rs     # R1: write_chain_and_backtrace param &LocationChain → &[…]
│   │                 # R3: write!(w, "{}", backtrace) → write!(w, "{backtrace}")
│   ├── define.rs     # unchanged
│   └── format.rs     # unchanged (R2 deferred per Q1=A)
└── tests/
    ├── fields.rs        # unchanged
    ├── format.rs        # unchanged
    └── macro_contract.rs  # T1: pin locations()[1] file-suffix in bridge test
```

Downstream crates: zero changes. No public-API surface change, no signature change, no behavior change.

---

## Task 1: Slice param for `write_chain_and_backtrace` (R1)

**Files:**
- Modify: [crates/strider-error/src/fields.rs](crates/strider-error/src/fields.rs)

- [ ] **Step 1: Change the `chain` parameter type from `&LocationChain` to `&[…]`**

In [crates/strider-error/src/fields.rs](crates/strider-error/src/fields.rs), locate the helper at lines 80-96:

```rust
pub(crate) fn write_chain_and_backtrace<W: std::fmt::Write>(
    chain: &LocationChain,
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
    write!(w, "{}", backtrace)
}
```

Change the signature so `chain` is a slice:

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
    write!(w, "{}", backtrace)
}
```

Do not change the body. Do not change the doc comment block above the function — it still talks about a "location chain", which the parameter type now expresses as a slice rather than a Vec. (If the doc explicitly says "Vec", soften it; on inspection the current doc just says "writing a location chain" — fine as is.)

- [ ] **Step 2: Verify the two callers still compile**

`fmt_chain_and_backtrace` at [fields.rs:68-70](crates/strider-error/src/fields.rs#L68-L70) passes `&self.locations` (a `&Vec<…>`). Vec auto-derefs to `&[…]` — no edit needed.

`format_traceback` at [format.rs:42-46](crates/strider-error/src/format.rs#L42-L46) passes `err.location_chain()` (a `&LocationChain` = `&Vec<…>`). Same Deref coercion — no edit needed.

Run: `cargo check -p strider-error --all-targets`
Expected: PASS.

- [ ] **Step 3: Run the full suite to catch any subtle break**

Run: `cargo test -p strider-error`
Expected: 16 unit tests + 3 doctests pass (1 ignored).

Run: `cargo clippy -p strider-error --all-targets --no-deps -- -D warnings`
Expected: clean. (If `clippy::ptr_arg` was firing on the old form, this also clears that.)

- [ ] **Step 4: Verify in-tree consumers are unaffected**

Run: `cargo build --workspace`
Expected: PASS. The change is internal (`pub(crate)`), so workspace consumers can't see it — this is a belt-and-suspenders sanity check.

- [ ] **Step 5: Commit**

```bash
git add crates/strider-error/src/fields.rs
git commit -m "$(cat <<'EOF'
refactor(strider-error): take slice instead of &Vec in write_chain_and_backtrace

The internal helper only iterates over the chain, so &[T] is the
idiomatic parameter type. Callers — Debug impls (via ErrorFields::
fmt_chain_and_backtrace) and format_traceback — pass &Vec<…> and
get auto-coerced via Deref. No public-API change; signature stays
pub(crate).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Captured identifier in `write!` (R3)

**Files:**
- Modify: [crates/strider-error/src/fields.rs](crates/strider-error/src/fields.rs)

- [ ] **Step 1: Inline `backtrace` into the format string**

In [crates/strider-error/src/fields.rs](crates/strider-error/src/fields.rs), locate line 95:

```rust
    write!(w, "{}", backtrace)
```

Replace with:

```rust
    write!(w, "{backtrace}")
```

The argument is a plain identifier (not a method call or a field access), so the captured-identifier syntax (stable since Rust 1.58) applies cleanly. Identical codegen.

- [ ] **Step 2: Verify behavior is unchanged**

Run: `cargo test -p strider-error`
Expected: 16 unit tests + 3 doctests pass (1 ignored). The `format_traceback_includes_location_marker` test in particular asserts the backtrace section follows the location markers — would catch any accidental output change.

Run: `cargo clippy -p strider-error --all-targets --no-deps -- -D warnings`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add crates/strider-error/src/fields.rs
git commit -m "$(cat <<'EOF'
style(strider-error): use captured-identifier form in backtrace write!

write!(w, "{}", backtrace) → write!(w, "{backtrace}"). Captured
identifiers are stable since Rust 1.58 and the codegen is identical;
this just matches the captured form already used elsewhere in the
file.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Pin bridge_error! caller-site location (T1)

**Files:**
- Modify: [crates/strider-error/tests/macro_contract.rs](crates/strider-error/tests/macro_contract.rs)

- [ ] **Step 1: Augment `bridge_error_macro_extends_chain_by_one` with a file-suffix assertion on `locations()[1]`**

In [crates/strider-error/tests/macro_contract.rs](crates/strider-error/tests/macro_contract.rs), locate the test at lines 129-141:

```rust
#[test]
fn bridge_error_macro_extends_chain_by_one() {
    fn inner() -> Result<(), MyError> { Err(MyKind::Boom.into()) }
    fn outer() -> Result<(), OuterError> { inner()?; Ok(()) }

    let err = outer().unwrap_err();
    assert_eq!(
        err.locations().len(),
        2,
        "origin + one bridge push_caller = 2",
    );
    assert!(matches!(err.kind(), OuterKind::Inner(MyKind::Boom)));
}
```

Add a file-suffix check on `locations()[1]` — the bridge `push_caller` site — so that `locations[1]` is pinned to point at the `?` line in *this* test file, not into `core/src/convert/mod.rs` or into the strider-error source:

```rust
#[test]
fn bridge_error_macro_extends_chain_by_one() {
    fn inner() -> Result<(), MyError> { Err(MyKind::Boom.into()) }
    fn outer() -> Result<(), OuterError> { inner()?; Ok(()) }

    let err = outer().unwrap_err();
    assert_eq!(
        err.locations().len(),
        2,
        "origin + one bridge push_caller = 2",
    );
    assert!(matches!(err.kind(), OuterKind::Inner(MyKind::Boom)));
    let bridge_loc = err.locations()[1];
    assert!(
        bridge_loc.file().ends_with("tests/macro_contract.rs"),
        "bridge push_caller must point at caller's file, got {}",
        bridge_loc.file(),
    );
}
```

Rationale: the existing assertions catch a regression that drops the bridge entirely (chain length would be 1) or that wraps the wrong variant. They do not catch a regression that drops `#[track_caller]` from the `bridge_error!`-emitted `From` impl ([define.rs:225-233](crates/strider-error/src/define.rs#L225-L233)) or from `ErrorFields::push_caller` itself ([fields.rs:53-58](crates/strider-error/src/fields.rs#L53-L58)) — chain length stays 2 in both regressions, but `locations[1]` resolves into core::convert or into strider-error/src/fields.rs respectively.

The file-suffix check is the same pattern already used by `track_caller_on_question_mark_points_at_question_mark_site` at [tests/macro_contract.rs:60-76](crates/strider-error/tests/macro_contract.rs#L60-L76). Don't pin `loc.line()` — round 6 explicitly deferred line-pinning as too brittle to test-file edits.

Do not change the existing two assertions. Do not touch any other test in the file.

- [ ] **Step 2: Run the augmented test**

Run: `cargo test -p strider-error --test macro_contract bridge_error_macro_extends_chain_by_one`
Expected: 1 test passes.

- [ ] **Step 3: Run the full suite**

Run: `cargo test -p strider-error`
Expected: 16 unit tests + 3 doctests pass (1 ignored). Total test count unchanged.

- [ ] **Step 4: Commit**

```bash
git add crates/strider-error/tests/macro_contract.rs
git commit -m "$(cat <<'EOF'
test(strider-error): pin bridge_error! caller site to test file

The existing chain-length assertion stays at 2 even if a regression
drops #[track_caller] from the bridge_error!-generated From impl or
from ErrorFields::push_caller — the locations would just resolve
into core::convert or into strider-error itself. Pin the bridge
location to the test file via a file-suffix check, mirroring the
existing track_caller_on_question_mark_points_at_question_mark_site
pattern.

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

- Privatize `ErrorFields.{backtrace, locations}` fields (rounds 3-6 deferred).
- `Option<Box<Backtrace>>` to skip the alloc under disabled backtraces.
- `LocationChain` → `SmallVec` micro-optimization.
- Public-API change: `Traceback::location_chain` → `&[…]`.
- Generic-aware `define_error!` to subsume `dot::error::Error<E>`.
- Blank-line separators in `format_traceback` output.
- Multi-line source-error indentation in the `caused by:` walk.
- `compile_fail` doctest pinning the struct-variant restriction on `bridge_error!`.
- Tightening location-pinning tests with `loc.line()` (rounds 5/6 defer).
- (R2 was a marginal-readability call; default per Q1=A is to skip.)
