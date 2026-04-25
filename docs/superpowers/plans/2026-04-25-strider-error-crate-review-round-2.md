# strider-error Crate Review — Round 2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Second-pass review of `strider-error` (round 1 is committed as of 2026-04-24). Remove a latent correctness bug + ~60 lines of drift-prone duplication between the `define_error!` macro and the hand-rolled generic `dot::error::Error<E>`, and pin the generic wrapper with its own contract tests.

**Architecture:** Round 1 already landed: split `wrapper.rs` → `fields.rs` + `define.rs`, `Arc`→`Box`, `bridge_error!`, tests, `format_traceback` dedup. This round targets the remaining rough edges:

1. `dot::Error<E>::Debug` uses `{:?}` for kind where the macro uses `{}` (Display). Two consequences: (a) the two wrappers have inconsistent Debug shapes; (b) `format_traceback` strips the "first line of Debug" assuming it equals Display, which is true for the macro and false for dot — so dot errors passed through `format_traceback` silently lose a line of useful text.
2. The Debug body — iterate locations, write backtrace — is duplicated byte-for-byte between [define.rs:104-119](crates/strider-error/src/define.rs#L104-L119) and [dot/src/error.rs:66-81](crates/dot/src/error.rs#L66-L81). Extracting a shared helper stops the drift.
3. `dot::Error<E>` has no direct contract tests. After (1) it needs them.
4. `bridge_error!` lives in `fields.rs`; cosmetic, but co-locating the two `#[macro_export]` macros in `define.rs` improves grep/discoverability.

**Tech Stack:** Rust 2024, `thiserror`, `std::backtrace::Backtrace`, `std::panic::Location`.

---

## Baseline (verified 2026-04-25)

- `cargo test -p strider-error` → 14 passed (3 unit in `tests/fields.rs` + 9 in `tests/macro_contract.rs` + 2 in `tests/format.rs` + 3 doctests).
- `cargo clippy -p strider-error --all-targets --no-deps -- -D warnings` → clean.
- `crates/dot/tests/` → does not exist.

---

## Open Questions (defaults assumed)

**Q1 — Should `dot::Error<E>::Debug` converge to Display-of-kind (matching the macro) or the other way round?**
- **(A)** Change dot to Display-of-kind. Adds `E: Display` bound on the Debug impl (already required on the Display impl at [dot/src/error.rs:60](crates/dot/src/error.rs#L60)). Consistent with the macro; `format_traceback`'s strip logic becomes correct for dot. **Default.**
- (B) Change the macro to Debug-of-kind. Breaks `tests/macro_contract.rs::debug_prints_location_markers` (asserts Display text `"boom"` appears in Debug output). Would also require changing `format_traceback`'s strip logic.

Assume **(A)**.

**Q2 — Where should the shared Debug helper live?**
- **(A)** `pub fn` on `ErrorFields` itself: `impl ErrorFields { pub fn fmt_chain_and_backtrace(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result }`. Zero new API surface at crate root; call sites read `self.fields.fmt_chain_and_backtrace(f)`. **Default.**
- (B) Free function `pub fn write_location_chain_and_backtrace(fields: &ErrorFields, f: &mut fmt::Formatter<'_>) -> fmt::Result`. Slightly more discoverable at crate root but adds an item to the public API.

Assume **(A)**.

**Q3 — Move `bridge_error!` macro from `fields.rs` to `define.rs`?**
- **(A)** Move it. Both `#[macro_export]` macros end up co-located. `fields.rs` becomes pure data-type definitions. **Default.**
- (B) Leave in place. Pure cosmetic.

Assume **(A)**.

---

## File Structure (after execution)

```
crates/strider-error/
├── src/
│   ├── lib.rs
│   ├── format.rs
│   ├── fields.rs     # ErrorFields, LocationChain, fmt_chain_and_backtrace method (Q2)
│   └── define.rs     # define_error! + bridge_error! (Q3)
└── tests/            # unchanged

crates/dot/
├── src/error.rs      # Debug switches to Display-of-kind (Q1); reuses fmt_chain_and_backtrace (Q2)
└── tests/
    └── error.rs      # NEW — contract tests for Error<E>
```

---

## Task 1: Switch `dot::Error<E>::Debug` to use Display-of-kind (C1 / Q1)

**Files:**
- Modify: [crates/dot/src/error.rs](crates/dot/src/error.rs)

- [ ] **Step 1: Tighten the Debug impl bound and switch to `{kind}`**

Replace lines 66-81 in `crates/dot/src/error.rs`:

```rust
impl<E: Debug + std::fmt::Display> std::fmt::Debug for Error<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "{}", self.kind)?;
        for (i, loc) in self.fields.locations.iter().enumerate() {
            writeln!(
                f,
                "  at [{}] {}:{}:{}",
                i,
                loc.file(),
                loc.line(),
                loc.column()
            )?;
        }
        write!(f, "{}", self.fields.backtrace)
    }
}
```

(Only real changes: `impl<E: Debug + std::fmt::Display>` gains the `Display` bound; `writeln!(f, "{:?}", self.kind)` → `writeln!(f, "{}", self.kind)`. The loop and backtrace write are unchanged — Task 3 will collapse them.)

- [ ] **Step 2: Confirm the whole workspace still compiles**

Run: `cargo build --workspace`
Expected: PASS. The Debug impl on `dot::Error<E>` is only used where `E` already satisfies `Display` (the dot pipeline's dumper error types all do — they're `thiserror`-derived wrappers).

- [ ] **Step 3: Commit**

```bash
git add crates/dot/src/error.rs
git commit -m "fix(dot): Error<E>::Debug uses Display of kind, matching strider-error macro"
```

---

## Task 2: Add `crates/dot/tests/error.rs` — pin `dot::Error<E>` contract (T1)

**Files:**
- Create: `crates/dot/tests/error.rs`

- [ ] **Step 1: Check the crate name / import path**

Run: `grep -E '^name' crates/dot/Cargo.toml`
Expected: `name = "dot"` (a simple crate name). If it's different, adjust the `dot::...` references in the test file below.

- [ ] **Step 2: Write the test file**

Create `crates/dot/tests/error.rs`:

```rust
#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

//! Pins the contract of `dot::error::Error<E>`: chain length, Display
//! delegation, Debug contains Display-line + location markers, `From<io::Error>`
//! seeds a length-1 chain, and the whole wrapper re-derives cleanly through
//! `decompose()`. Mirrors `strider-error/tests/macro_contract.rs` for the
//! hand-rolled generic equivalent.

use std::error::Error as _;
use std::fmt;

use dot::error::{Error, ErrorKind};

// A minimal dumper-error stand-in: `dot::Error<E>` requires `E: Debug`,
// plus `E: Display` for the Debug impl and `E: Error + 'static` for the
// `Error::source` impl. Test fixtures satisfy all of those.
#[derive(Debug)]
struct TestDumperErr(&'static str);

impl fmt::Display for TestDumperErr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "dump-err: {}", self.0)
    }
}

impl std::error::Error for TestDumperErr {}

#[test]
fn from_kind_seeds_single_location() {
    let err: Error<TestDumperErr> =
        ErrorKind::<TestDumperErr>::SvgConversionError("nope".into()).into();
    assert_eq!(err.locations().len(), 1);
    assert!(matches!(err.kind(), ErrorKind::SvgConversionError(_)));
}

#[test]
fn from_io_error_seeds_single_location() {
    fn inner() -> Result<(), Error<TestDumperErr>> {
        let f = std::fs::File::open("/definitely/not/a/real/path")?;
        drop(f);
        Ok(())
    }
    let err = inner().unwrap_err();
    assert_eq!(err.locations().len(), 1);
    assert!(matches!(err.kind(), ErrorKind::IoError(_)));
}

#[test]
fn display_delegates_to_inner_kind() {
    let err: Error<TestDumperErr> =
        ErrorKind::<TestDumperErr>::DotDumpError(TestDumperErr("xyz")).into();
    assert_eq!(err.to_string(), "dump-err: xyz");
}

#[test]
fn debug_contains_display_line_and_location_marker() {
    let err: Error<TestDumperErr> =
        ErrorKind::<TestDumperErr>::DotDumpError(TestDumperErr("xyz")).into();
    let dbg = format!("{err:?}");
    assert!(
        dbg.contains("dump-err: xyz"),
        "Debug should start with the Display line; got {dbg:?}",
    );
    assert!(
        dbg.contains("  at [0] "),
        "Debug should include the origin location; got {dbg:?}",
    );
}

#[test]
fn decompose_preserves_chain_length_and_backtrace_status() {
    let err: Error<TestDumperErr> =
        ErrorKind::<TestDumperErr>::SvgConversionError("nope".into()).into();
    let before_len = err.locations().len();
    let before_status = err.backtrace().status();
    let (kind, fields) = err.decompose();
    assert!(matches!(*kind, ErrorKind::SvgConversionError(_)));
    assert_eq!(fields.locations.len(), before_len);
    assert_eq!(fields.backtrace.status(), before_status);
}

#[test]
fn error_source_threads_through_io_variant() {
    let err: Error<TestDumperErr> =
        std::fs::File::open("/definitely/not/a/real/path").unwrap_err().into();
    let src = err.source().expect("Io variant exposes its source");
    assert!(src.is::<std::io::Error>());
}
```

- [ ] **Step 3: Run the tests**

Run: `cargo test -p dot --test error`
Expected: 6 passed.

- [ ] **Step 4: Commit**

```bash
git add crates/dot/tests/error.rs
git commit -m "test(dot): pin Error<E> contract (chain length, Display, Debug, decompose)"
```

---

## Task 3: Extract shared Debug helper `ErrorFields::fmt_chain_and_backtrace` (R1 / Q2)

**Files:**
- Modify: [crates/strider-error/src/fields.rs](crates/strider-error/src/fields.rs)
- Modify: [crates/strider-error/src/define.rs](crates/strider-error/src/define.rs)
- Modify: [crates/dot/src/error.rs](crates/dot/src/error.rs)

- [ ] **Step 1: Add the helper method on `ErrorFields`**

Add to `crates/strider-error/src/fields.rs`, inside `impl ErrorFields { ... }`, just after `push_caller`:

```rust
    /// Writes the location chain + backtrace into `f`, using the same
    /// format that `define_error!`-generated Debug impls (and `dot::Error<E>`)
    /// use. Callers must already have written the kind's own representation
    /// (typically one `writeln!(f, "{}", kind)` line before this call).
    pub fn fmt_chain_and_backtrace(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (i, loc) in self.locations.iter().enumerate() {
            writeln!(
                f,
                "  at [{}] {}:{}:{}",
                i,
                loc.file(),
                loc.line(),
                loc.column(),
            )?;
        }
        write!(f, "{}", self.backtrace)
    }
```

- [ ] **Step 2: Use it from the `define_error!` macro**

Replace the Debug impl body at [crates/strider-error/src/define.rs:104-119](crates/strider-error/src/define.rs#L104-L119):

```rust
        impl ::std::fmt::Debug for $wrapper {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                writeln!(f, "{}", self.kind)?;
                self.fields.fmt_chain_and_backtrace(f)
            }
        }
```

- [ ] **Step 3: Use it from `dot::Error<E>::Debug`**

Replace the Debug impl body at `crates/dot/src/error.rs` (the block Task 1 just modified):

```rust
impl<E: Debug + std::fmt::Display> std::fmt::Debug for Error<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "{}", self.kind)?;
        self.fields.fmt_chain_and_backtrace(f)
    }
}
```

- [ ] **Step 4: Verify builds + tests**

Run: `cargo test -p strider-error && cargo test -p dot`
Expected: strider-error 14 passed + doctests; dot 6 passed (Task 2's tests still pass — Debug output format is identical byte-for-byte).

- [ ] **Step 5: Commit**

```bash
git add crates/strider-error/src/fields.rs crates/strider-error/src/define.rs crates/dot/src/error.rs
git commit -m "refactor(strider-error): share Debug formatting via ErrorFields::fmt_chain_and_backtrace"
```

---

## Task 4: Move `bridge_error!` from `fields.rs` to `define.rs` (S1 / Q3)

**Files:**
- Modify: [crates/strider-error/src/fields.rs](crates/strider-error/src/fields.rs)
- Modify: [crates/strider-error/src/define.rs](crates/strider-error/src/define.rs)

- [ ] **Step 1: Cut the `bridge_error!` block from `fields.rs`**

Delete [fields.rs:53-116](crates/strider-error/src/fields.rs#L53-L116) — the entire doc comment + `#[macro_export] macro_rules! bridge_error { ... }` block.

- [ ] **Step 2: Paste it at the bottom of `define.rs`**

Append to `crates/strider-error/src/define.rs` (after `define_error!`'s closing `};}`):

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
///
/// ```
/// strider_error::define_error! {
///     pub struct InnerError wraps InnerKind;
///     #[derive(Debug, thiserror::Error)]
///     pub enum InnerKind { #[error("boom")] Boom }
/// }
///
/// strider_error::define_error! {
///     pub struct OuterError wraps OuterKind;
///     #[derive(Debug, thiserror::Error)]
///     pub enum OuterKind {
///         #[error(transparent)]
///         Inner(InnerKind),
///     }
/// }
///
/// strider_error::bridge_error!(InnerError => OuterError, OuterKind::Inner);
///
/// fn inner() -> Result<(), InnerError> { Err(InnerKind::Boom.into()) }
/// fn outer() -> Result<(), OuterError> { inner()?; Ok(()) }
///
/// let err = outer().unwrap_err();
/// assert_eq!(err.locations().len(), 2, "origin + bridge push_caller");
/// ```
///
/// Expands to:
///
/// ```text
/// impl ::core::convert::From<InnerError> for OuterError {
///     #[track_caller]
///     fn from(e: InnerError) -> Self {
///         let (kind, fields) = e.decompose();
///         Self {
///             kind: ::std::boxed::Box::new(OuterKind::Inner(*kind)),
///             fields: fields.push_caller(),
///         }
///     }
/// }
/// ```
#[macro_export]
macro_rules! bridge_error {
    ($inner:ty => $outer:ident, $outer_kind:ident :: $variant:ident) => {
        impl ::core::convert::From<$inner> for $outer {
            #[track_caller]
            fn from(e: $inner) -> Self {
                let (kind, fields) = e.decompose();
                Self {
                    kind: ::std::boxed::Box::new($outer_kind::$variant(*kind)),
                    fields: fields.push_caller(),
                }
            }
        }
    };
}
```

- [ ] **Step 3: Verify**

Run: `cargo test -p strider-error && cargo build --workspace`
Expected: PASS. `#[macro_export]` places the macro at the crate root regardless of the module it's defined in, so all `strider_error::bridge_error!(...)` call sites in `analyzer`, `opt`, `pattern` still resolve. The doctest for `bridge_error!` lives inside the moved doc comment and moves with it.

- [ ] **Step 4: Commit**

```bash
git add crates/strider-error/src/fields.rs crates/strider-error/src/define.rs
git commit -m "refactor(strider-error): co-locate bridge_error! with define_error! in define.rs"
```

---

## Task 5: Workspace sanity sweep

**Files:** Run-only, no edits.

- [ ] **Step 1: Full workspace build**

Run: `cargo build --workspace`
Expected: PASS.

- [ ] **Step 2: Full workspace tests**

Run: `cargo test --workspace`
Expected: PASS. In particular:
- `cargo test -p strider-error` — 14 tests + 3 doctests.
- `cargo test -p dot --test error` — 6 tests (new this round).
- `cargo test -p analyzer --test error_chain` — chain lengths still match expectations.
- `cargo test -p reader --test error` — still passes.

- [ ] **Step 3: strider-error strict lint**

Run: `cargo clippy -p strider-error --all-targets --no-deps -- -D warnings`
Expected: PASS.

- [ ] **Step 4: dot strict lint (the crate whose Debug impl we just changed)**

Run: `cargo clippy -p dot --all-targets --no-deps -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Workspace lint**

Run: `cargo clippy --workspace -- -D warnings`
Expected: PASS.

- [ ] **Step 6: Smoke-run the example**

Run: `cargo run --example analyzer`
Expected: `cfg.html`, `graph.html`, `graph-opt.html` produced — confirms no runtime regression.

---

## Out of Scope (considered, deferred)

- **`Option<Box<Backtrace>>` to skip the alloc when `RUST_BACKTRACE` is unset.** Potentially worthwhile but speculative; no measurement and no complaint. Park until someone profiles the error path.
- **`format_traceback` blank-line inconsistency** between the source-chain walk (no blank line) and the Debug tail (blank line before). Cosmetic; the current format doesn't bother anybody.
- **Consolidating `ir::ValidationErrors` bridge in `opt::error`.** Already 5 lines, generalizing would need a second macro form for "capture a fresh backtrace then hop". Not worth the macro complexity for one call site.
