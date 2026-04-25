# strider-error Crate Review — Round 5 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fifth-pass review of `strider-error` after rounds 1–4. The crate is in good shape — rounds 3 and 4 closed every correctness item I could find. Round 5 addresses two small documentation/example consistency issues that became visible only *because* round 4 added the `# Caveats` section to `define_error!`, and adds one missing doc constraint on `bridge_error!`. No code-behavior changes; tests unchanged.

**Architecture:** Four-file crate (`lib.rs`, `fields.rs`, `define.rs`, `format.rs`), ~330 LoC. All three changes are inside doc comments in `define.rs`. No new public items, no signature changes.

**Tech Stack:** Rust 2024, `thiserror`, `std::backtrace::Backtrace`, `std::panic::Location`. No new dependencies; no MSRV change.

---

## Baseline (verified 2026-04-25 against HEAD `2bc7a86`)

- `cargo test -p strider-error` → 16 unit tests pass (3 in `tests/fields.rs` + 9 in `tests/macro_contract.rs` + 4 in `tests/format.rs`) + 3 doctests pass (1 ignored).
- `cargo clippy -p strider-error --all-targets --no-deps -- -D warnings` → clean.
- Round 4 commits present: `6b7158f docs(strider-error): document that .into() loses #[track_caller]…` and `a898833 refactor(strider-error): share chain+backtrace write loop…`.
- All in-tree consumers found via `grep`: 9 wrappers via `define_error!` (`opt`, `pattern`, `ir`, `analyzer`, `target`, `reader`, `cfg`), plus the hand-rolled generic `dot::error::Error<E>` that mirrors the macro. 5 cross-crate bridges via `bridge_error!`.

---

## Review Findings — Executive Summary

**Zero correctness bugs found this round.** Three small items, all in `define.rs`. None of them are behavioral regressions; the crate already does the right thing at runtime.

### Readability / Docs (R)

- **R1 — The `define_error!` example demonstrates the broken pattern that the new `# Caveats` section warns against.** Round 4 added a `# Caveats` block that says: don't use `.into()` if you want the chain's origin entry to point at your own code; use `Wrapper::from(kind)` instead. The example at [define.rs:41](crates/strider-error/src/define.rs#L41) immediately above that block does:

  ```rust
  let err: Error = ErrorKind::NotMapped(0xdead_beef).into();
  ```

  A reader who skims top-to-bottom sees the `.into()` form first, copies it, and then reads — possibly later, possibly never — the caveat below. Worse: the example *also* asserts `err.locations().len() == 1`, which still holds (the chain has one entry; it's just pointing at stdlib instead of the user's line). So the example "works", and the contradictory teaching slips by silently.

  Fix: rewrite the example to use the form the caveat recommends (`Error::from(...)`). The `.len() == 1` assertion still passes — the only thing that changes is pedagogy. The `# Caveats` section's own `ignore`d snippet at lines 61-65 already uses `MyError::from(MyKind::Boom)` for "good" and `.into()` for "bad", so the main example aligning with the "good" form is internally consistent.

- **R2 — `bridge_error!` doc doesn't state the tuple-variant requirement.** The macro at [define.rs:217-230](crates/strider-error/src/define.rs#L217-L230) emits `$outer_kind::$variant(*kind)` — positional tuple-variant call syntax. If a user writes:

  ```rust
  pub enum OuterKind {
      Inner { kind: InnerKind },   // struct variant
  }
  strider_error::bridge_error!(InnerError => OuterError, OuterKind::Inner);
  ```

  it fails to compile with a confusing "expected struct variant" error from the macro expansion site, not the user's code. Today every in-tree bridge target is a tuple variant (`opt::ErrorKind::IrError(...)`, `analyzer::ErrorKind::CfgError(...)`, …) so nothing trips on this. A one-line doc note prevents future surprise.

- **R3 — Test name `decompose_and_reconstruct_preserves_chain_length_and_backtrace_status` doesn't reconstruct.** [tests/macro_contract.rs:79-87](crates/strider-error/tests/macro_contract.rs#L79-L87) only decomposes and asserts on the parts; there's no reassembly. Mostly cosmetic — but trivial to rename in the same round. Optional; pin includes both options below.

### Out of scope for this round

The following were considered and deliberately left alone — same rationale as in rounds 3 and 4 unless noted:

- **Privatize `ErrorFields.{backtrace, locations}` fields and add accessor methods.** Rounds 3 and 4 deferred. The macro-generated wrappers and `dot::Error<E>` both read these fields directly via field access; privatizing requires adding accessor methods on `ErrorFields` and updating ~5 reference sites across two files. Net benefit is encapsulation hygiene — small, since the fields are read-only in practice (mutation goes through `push_caller(self) -> Self`). Still not pulling its weight versus the API churn. Re-flag if a third consumer ever needs the fields.
- **`Option<Box<Backtrace>>` to skip the heap allocation when `RUST_BACKTRACE` is unset.** Round 2 deferred. `Backtrace::capture()` itself returns a `Disabled` variant cheaply; the cost we'd save is the ~32-byte `Box` allocation per error construction. Defer until profiling demands it.
- **`LocationChain` → `SmallVec<[&'static Location<'static>; 4]>`.** Round 3 deferred. Same argument: no profiling demand.
- **Generic-aware `define_error!` to subsume `dot::error::Error<E>`.** Round 4 noted the macro is "monomorphic" and `dot::Error<E>` is hand-rolled to mirror it. Threading a generic type parameter through every macro arm would more than double the macro's size for one consumer. Defer indefinitely.
- **Blank-line separators between source-walk / locations / backtrace in `format_traceback`.** Cosmetic; no in-tree consumer complaining.
- **Multi-line source-error display alignment in `format_traceback`'s `caused by:` walk.** If a source error's `Display` is multi-line, only the first line gets the `"  caused by: "` prefix; subsequent lines lack indentation. None of the in-tree `thiserror` enums currently produce multi-line displays, and the wrapper-level multi-line case is already pinned by `format_traceback_does_not_duplicate_multiline_display`. Worth fixing if/when a real consumer hits it.
- **Doctest for `bridge_error!` with a struct variant.** The pin in R2 below is doc-only; adding a deliberately-failing struct-variant doctest with `compile_fail` would over-engineer the pin. The doc note plus existing tuple-variant doctest is enough.
- **Second-arm `bridge_error!` that consumes `ir::ValidationErrors` directly.** One call site, not worth a second macro form. Round 2's out-of-scope list.

---

## Open Questions for the Reviewer

Each of these changes the shape of a task. Pick one per group. Assumed defaults are marked.

**Q1 — R1 example body: which `From::from` form to demonstrate?**

- **(A)** `let err = Error::from(ErrorKind::NotMapped(0xdead_beef));` — turbofish-free, mirrors the "good" form in the existing `# Caveats` snippet at [define.rs:62](crates/strider-error/src/define.rs#L62) (`MyError::from(MyKind::Boom)`). **Default.**
- **(B)** `let err: Error = Error::from(ErrorKind::NotMapped(0xdead_beef));` — keeps the explicit type annotation that the current example has. Slightly redundant since `Error::from` already nails the type. Skip.
- **(C)** Add a second line showing the `?` form too:
  ```rust
  fn fallible() -> Result<(), Error> {
      let _ = std::fs::File::open("/x")?; // also #[track_caller]-correct
      Ok(())
  }
  ```
  Useful, but the doctest grows by 4-5 lines for marginal benefit (the `# Caveats` snippet already names `?` as an option). Skip.

Assume **(A)**.

**Q2 — R2 doc placement: where does the tuple-variant note live?**

- **(A)** Add one short paragraph between the existing two paragraphs of the `bridge_error!` summary (currently [define.rs:165-172](crates/strider-error/src/define.rs#L165-L172)), before the `# Example`. Reads as part of the contract. **Default.**
- **(B)** New `# Caveats` section after `# Example`. Mirrors the structure of `define_error!`. Heavier shape for one constraint. Skip.

Assume **(A)**.

**Q3 — Include R3 (test rename) in this round, or skip?**

- **(A)** Include. Rename `decompose_and_reconstruct_preserves_chain_length_and_backtrace_status` → `decompose_preserves_chain_length_and_backtrace_status`. Trivial; one ident change in [tests/macro_contract.rs:79](crates/strider-error/tests/macro_contract.rs#L79). **Default.**
- **(B)** Skip — leave test names alone in a doc-only round.

Assume **(A)**.

---

## File Structure (after execution, assuming defaults)

```
crates/strider-error/
├── src/
│   ├── lib.rs        # unchanged
│   ├── fields.rs     # unchanged
│   ├── define.rs     # R1: example uses Error::from(...)
│   │                 # R2: one-paragraph note on bridge_error!
│   │                 #     about tuple-variant requirement
│   └── format.rs     # unchanged
└── tests/
    ├── fields.rs        # unchanged
    ├── format.rs        # unchanged
    └── macro_contract.rs  # R3: one test rename
```

Downstream: zero changes. No public-API surface change, no signature change, no behavior change.

---

## Task 1: Align `define_error!` example with the `# Caveats` guidance (R1 / Q1)

**Files:**
- Modify: [crates/strider-error/src/define.rs](crates/strider-error/src/define.rs)

- [ ] **Step 1: Update the example doctest body**

In [crates/strider-error/src/define.rs](crates/strider-error/src/define.rs), locate the `# Example` block at lines 25-44. Replace the construction line at line 41:

Current (line 41):

```rust
/// let err: Error = ErrorKind::NotMapped(0xdead_beef).into();
```

Replace with:

```rust
/// let err = Error::from(ErrorKind::NotMapped(0xdead_beef));
```

Do not change lines 42 or 43 (the two `assert_eq!` lines). The chain length is still 1 — the only difference is *where* `err.locations()[0]` points (now: the new `Error::from(...)` call site in the doctest; before: a line inside `core/src/convert/mod.rs`). The assertion `err.locations().len() == 1` is unchanged.

Do not change anything else in the macro body or in the `# Caveats` / `# Cross-crate bridges` blocks.

- [ ] **Step 2: Verify the doctest still compiles and runs**

Run: `cargo test -p strider-error --doc`
Expected: 3 doctests pass + 1 ignored. Specifically the test labeled `crates/strider-error/src/define.rs - define::define_error (line 27)` runs and passes. (If the line number drifts because of the edit, the count and pass status are what matters.)

- [ ] **Step 3: Verify nothing else broke**

Run: `cargo test -p strider-error`
Expected: 16 unit tests + 3 doctests pass (1 ignored).

Run: `cargo clippy -p strider-error --all-targets --no-deps -- -D warnings`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/strider-error/src/define.rs
git commit -m "$(cat <<'EOF'
docs(strider-error): align define_error! example with the Caveats section

Round 4 added a Caveats block warning against `.into()` because the std
blanket `Into::into` is not `#[track_caller]`, so `Wrapper::from(kind)`
is the form that actually pins the location chain's origin to the
user's code. The macro's own example one screen above the caveat used
the broken form, contradicting the very guidance below it. Switch the
example to `Error::from(kind)`. The `err.locations().len() == 1`
assertion still holds; only the chain entry's pointer changes.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Document `bridge_error!` tuple-variant requirement (R2 / Q2)

**Files:**
- Modify: [crates/strider-error/src/define.rs](crates/strider-error/src/define.rs)

- [ ] **Step 1: Add a constraint paragraph to the `bridge_error!` summary**

In [crates/strider-error/src/define.rs](crates/strider-error/src/define.rs), locate the doc comment for `bridge_error!`. Currently the head reads (lines 165-173, line numbers approximate after Task 1's edit):

```rust
/// Generates a `#[track_caller] impl From<$inner> for $outer` that decomposes
/// the inner wrapper, appends the caller's site, and re-assembles as the outer
/// wrapper with kind wrapped in `$outer_kind::$variant`.
///
/// The inner type must expose `.decompose() -> (Box<InnerKind>, ErrorFields)`
/// — any type produced by [`define_error!`](crate::define_error) does, as does
/// the hand-rolled `dot::error::Error<E>` via its manual `decompose` method.
///
/// # Example
```

Insert one new paragraph between the second paragraph (`The inner type must expose…`) and the `# Example` heading. The result should read:

```rust
/// Generates a `#[track_caller] impl From<$inner> for $outer` that decomposes
/// the inner wrapper, appends the caller's site, and re-assembles as the outer
/// wrapper with kind wrapped in `$outer_kind::$variant`.
///
/// The inner type must expose `.decompose() -> (Box<InnerKind>, ErrorFields)`
/// — any type produced by [`define_error!`](crate::define_error) does, as does
/// the hand-rolled `dot::error::Error<E>` via its manual `decompose` method.
///
/// `$outer_kind::$variant` must be a **tuple variant** that takes the inner
/// kind as its single positional field (e.g. `Inner(InnerKind)`). The
/// expansion calls `$outer_kind::$variant(*kind)`, so struct variants like
/// `Inner { kind: InnerKind }` will not compile. If you need a struct
/// variant, write the bridge `impl From<$inner> for $outer` by hand.
///
/// # Example
```

Do not change the `# Example` body or the expansion sketch at the end.

- [ ] **Step 2: Verify the doc-comment block still parses**

Run: `cargo check -p strider-error`
Expected: PASS. A malformed doc attribute fails at parse time.

- [ ] **Step 3: Verify the existing doctests still pass (the new paragraph is plain prose with no fences)**

Run: `cargo test -p strider-error --doc`
Expected: 3 doctests pass + 1 ignored. Same set as before — the new paragraph adds no executable code.

- [ ] **Step 4: Commit**

```bash
git add crates/strider-error/src/define.rs
git commit -m "docs(strider-error): document that bridge_error! requires a tuple variant"
```

---

## Task 3: Rename misleading test (R3 / Q3)

**Files:**
- Modify: [crates/strider-error/tests/macro_contract.rs](crates/strider-error/tests/macro_contract.rs)

- [ ] **Step 1: Rename the test**

In [crates/strider-error/tests/macro_contract.rs](crates/strider-error/tests/macro_contract.rs), at line 78-79, the test reads:

```rust
#[test]
fn decompose_and_reconstruct_preserves_chain_length_and_backtrace_status() {
```

Rename to:

```rust
#[test]
fn decompose_preserves_chain_length_and_backtrace_status() {
```

The body asserts on the parts produced by `decompose()`; nothing reassembles. Drop `_and_reconstruct` from the name. No body changes.

- [ ] **Step 2: Run the renamed test by name to confirm it picks up**

Run: `cargo test -p strider-error --test macro_contract decompose_preserves_chain_length_and_backtrace_status`
Expected: 1 test passes ("test result: ok. 1 passed; 0 failed; 0 ignored").

- [ ] **Step 3: Run the full suite**

Run: `cargo test -p strider-error`
Expected: 16 unit tests + 3 doctests pass (1 ignored). Total test count unchanged — this is a pure rename.

- [ ] **Step 4: Commit**

```bash
git add crates/strider-error/tests/macro_contract.rs
git commit -m "test(strider-error): rename decompose test to drop misleading 'reconstruct'"
```

---

## Task 4: Workspace sanity sweep

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
Expected: `cfg.html`, `graph.html`, `graph-opt.html` produced — confirms no runtime regression via the error path. (Should be impossible to break given this round is doc-only, but the sweep is cheap.)

---

## Out of Scope (considered, rejected or deferred)

Items already listed inline under **Out of scope for this round** above. Restated here for completeness:

- Privatize `ErrorFields.backtrace` / `ErrorFields.locations` fields (rounds 3 and 4 deferred; unchanged).
- `Option<Box<Backtrace>>` to skip the alloc under disabled backtraces.
- `LocationChain` → `SmallVec` micro-optimization.
- Generic-aware `define_error!` to subsume `dot::error::Error<E>`.
- Blank-line separators in `format_traceback` output.
- Multi-line source-error indentation in the `caused by:` walk.
- `compile_fail` doctest pinning the struct-variant restriction on `bridge_error!` (the doc note is the pin; an executable negative test would over-engineer it).
- Second-arm `bridge_error!` consuming `ir::ValidationErrors` directly.
