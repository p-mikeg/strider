# strider-error Crate Review — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Review the `strider-error` crate — the shared error-wrapper machinery used by every other crate in the workspace — for correctness, simplification, and readability. Fix one latent bug, drop one piece of premature abstraction, collapse the duplicated cross-crate bridge boilerplate, and pin the crate's invariants with unit tests it currently lacks.

**Architecture:** The crate is tiny (three files, ~250 lines): [lib.rs](crates/strider-error/src/lib.rs), [format.rs](crates/strider-error/src/format.rs) (`format_traceback`), and [wrapper.rs](crates/strider-error/src/wrapper.rs) (`ErrorFields`, `LocationChain`, `define_error!` macro). It is consumed by **eight** downstream crates — [cfg](crates/cfg/src/error.rs), [analyzer](crates/analyzer/src/error.rs), [target](crates/target/src/error.rs), [pattern](crates/pattern/src/error.rs), [reader](crates/reader/src/error.rs), [ir](crates/ir/src/error.rs), [dot](crates/dot/src/error.rs), [opt](crates/opt/src/error.rs) — plus the `analyzer::error_chain` / `reader::error` integration tests that pin the traceback invariants indirectly. Any change here has a blast radius of "the whole workspace," so every task below includes a `cargo check --workspace` / `cargo test --workspace` step. No public API removal; the only signature change (`Arc<Backtrace>` → `Box<Backtrace>` on the `pub` `ErrorFields.backtrace` field) is gated on Q2 below.

**Tech Stack:** Rust 2024, `thiserror`, `std::backtrace::Backtrace`, `std::panic::Location`.

---

## Baseline (verified 2026-04-24)

- `cargo test -p strider-error` → 0 unit tests, 1 doctest passing (`format_traceback`), 1 ignored (`define_error` — `ignore` intentional: the doctest needs a dependency crate to show realistic usage).
- `cargo clippy -p strider-error --all-targets --no-deps -- -D warnings` → clean.
- `crates/strider-error/tests/` → does not exist. All traceback-invariant testing lives in consumer crates.

---

## Review Findings — Executive Summary

**No critical bugs.** The wrapper machinery works: `ErrorFields::new` captures a backtrace + seeds a 1-entry location chain, `push_caller` appends, `decompose` splits, the macro generates `Display` / `Debug` / `Error::source` / `From<$kind>` / `From<$src>` impls exactly as documented. The items below are real but small.

**Correctness (F):**

- **F1 — `format_traceback` double-prints the wrapper's Display line.** [format.rs:30-44](crates/strider-error/src/format.rs#L30-L44) does `writeln!(out, "error: {err}")` (Display), walks the `source()` chain with `"caused by:"`, then appends `write!(out, "{err:?}")`. For any `define_error!`-generated wrapper, the Debug impl ([wrapper.rs:161-176](crates/strider-error/src/wrapper.rs#L161-L176)) starts with `writeln!(f, "{}", self.kind)` — the same Display text. Result: the Display line appears twice in the final string. The only existing doctest [format.rs:14-29](crates/strider-error/src/format.rs#L14-L29) happens to pass because its `Oops` test type has a plain `#[derive(Debug)]`, not a wrapper Debug, so the duplication is invisible there. The bug is latent — `format_traceback` has zero in-tree callers yet (it's the planned PyO3 entry point per the `strider-py` comment at [lib.rs:17](crates/strider-error/src/lib.rs#L17)), but will surface the first time the Python layer lights up. See **Q1** for fix shape.

- **F2 — `#[track_caller]` on `From<ErrorKind> for $wrapper` is not pinned by a direct test.** The `into()` path vs `?` path diverges: `?` desugars to `From::from(err)` at the `?` site, which correctly forwards the caller (so the location chain points at the `?` line). Explicit `ErrorKind::X.into()` desugars via the std blanket `impl<T, U: From<T>> Into<U>`, whose `Into::into` fn is **not** `#[track_caller]` in std, so `Location::caller()` inside `ErrorFields::new` resolves to `core/src/convert/mod.rs` (inside `Into::into`), not the user's call site. Consumer tests ([reader/tests/error.rs:27](crates/reader/tests/error.rs#L27)) only assert `!err.locations().is_empty()`, not **where** the chain points. This is a real caveat of the API; downstream users writing `ErrorKind::X.into()` expecting to see their own site will be surprised. The fix is documentation — but the documentation can't land until the behavior is **pinned by a test** (T3 below).

- **F3 — Seven hand-rolled cross-crate bridges are byte-for-byte identical.** Grep shows:
  - [analyzer/src/error.rs:70-115](crates/analyzer/src/error.rs#L70-L115) — 4 bridges (cfg, ir, opt, target)
  - [opt/src/error.rs:47-81](crates/opt/src/error.rs#L47-L81) — 2 bridges (ir, pattern)
  - [pattern/src/error.rs:121-130](crates/pattern/src/error.rs#L121-L130) — 1 bridge (ir)

  Each is `impl From<Inner::Error> for Self { #[track_caller] fn from(e) -> Self { let (kind, fields) = e.decompose(); Self { kind: Box::new(ErrorKind::Variant(*kind)), fields: fields.push_caller() } } }`. If the wrapper's internal shape ever changes (e.g. a new field in `ErrorFields`), all seven must be updated synchronously. A small `bridge_error!` macro would collapse them to one line each. See **Q4**.

**Simplification (S):**

- **S1 — `backtrace: Arc<Backtrace>` is never cloned.** [wrapper.rs:23](crates/strider-error/src/wrapper.rs#L23) stores the backtrace in an `Arc`. A `ripgrep` across the workspace for `Arc::clone` / `.clone()` anywhere near a `Backtrace` or `ErrorFields` returns zero hits. `decompose` moves the `Arc` by value. `push_caller` moves `self`. Nothing constructs an additional reference. The doc justification at [wrapper.rs:15-18](crates/strider-error/src/wrapper.rs#L15-L18) claims Arc exists "so a wrapper can be moved across `From` boundaries without re-capturing," but `Box<Backtrace>` (or even naked `Backtrace`) is moved the same way — the claim is incorrect. `Arc` adds 16 bytes of strong/weak refcount for no present benefit. See **Q2**.

- **S2 — `impl Default for ErrorFields` is unused.** [wrapper.rs:52-57](crates/strider-error/src/wrapper.rs#L52-L57). Grep shows zero callers of `ErrorFields::default()` in or outside the crate. Keeping it costs nothing but adds a second way to spell "new" with slightly different `#[track_caller]` ergonomics. See **Q3**.

- **S3 — `format_traceback` source-chain walk style.** [format.rs:35-39](crates/strider-error/src/format.rs#L35-L39) uses a `while let Some(e) = cur { ...; cur = e.source(); }`. Clean, readable, idiomatic. Not worth rewriting to `std::iter::successors`. **No change.**

**Readability (R):**

- **R1 — `wrapper.rs` mixes three unrelated things under one filename:** `ErrorFields` struct, `LocationChain` type alias, and the whole `define_error!` macro. The macro body is 100+ lines of `tt`-munching. Splitting wouldn't be structurally necessary but would make grep output and editor tabs less noisy (users searching for `ErrorFields` find the macro too). This is pure cosmetic; see **Q5**.

- **R2 — The module-level doc block at [lib.rs:1-17](crates/strider-error/src/lib.rs#L1-L17) narrates the wrapper shape in detail but never names `define_error!` as the entry point** — a new user has to discover the macro by reading the code. One line added to the top-level doc ("Crates opt into this shape via `define_error!` — see [`mod@wrapper`] for the macro.") closes the gap.

- **R3 — `format_traceback` is `pub` at both paths.** [lib.rs:23](crates/strider-error/src/lib.rs#L23) exposes `pub use format::format_traceback` and also `pub mod format`. Either drop the `pub` on the module (`mod format; pub use format::format_traceback;`) so there's one canonical path, or drop the re-export and let callers say `strider_error::format::format_traceback`. Default: privatize the module — the re-export is the public contract.

**Test coverage (T):**

The crate has zero unit tests. Every invariant — backtrace capture, chain seeding, `push_caller` append, `decompose` round-trip, `track_caller` forwarding, `format_traceback` output shape — is asserted **indirectly** through consumer crates. That coupling means a subtle break in `strider-error` can masquerade as a failure in `reader` or `analyzer`. Three focused integration tests (the crate has no reason for unit tests that dip into `pub(crate)` internals; everything non-trivial is exposed via the public API) would close the gap.

- **T1 — `ErrorFields` contract.** `new()` seeds a 1-entry location chain and produces a backtrace whose status is one of `Captured` / `Disabled` / `Unsupported`. `push_caller()` on a chain of length N returns a chain of length N+1 without altering the backtrace pointer.
- **T2 — `define_error!`-generated macro contract.** A test-only wrapper (defined inside the test file) round-trips through `decompose` → reconstruct with `ErrorFields` equality. `From<ErrorKind>` via `?` places the caller location at the `?` site. `From<$src>` via `?` (the `sources: [...]` bridge) produces a chain of length 1.
- **T3 — `format_traceback` output shape.** After the F1 fix: output for a `define_error!` wrapper contains the Display line **exactly once** and contains `"  at [0] "` at least once. Output for a chained error (an error whose `source()` is another error) contains `"  caused by: "` once per link.

---

## Open Questions for the Reviewer

Each of these changes the shape of a task. Pick one per group. Assumed defaults are marked and reflected in the tasks below.

**Q1 — F1 fix shape.** How should `format_traceback` avoid duplicating the Display line?

  - **(A)** Strip the leading Display line from `{err:?}` at the composition site inside `format_traceback` — one `str::find('\n').map_or(dbg.as_str(), |i| &dbg[i+1..])` at the point of append. Minimal, no new public API, relies on the macro's Debug always starting with the Display. **Default.**
  - **(B)** Introduce `pub trait Traceback { fn location_chain(&self) -> &LocationChain; fn origin_backtrace(&self) -> &Backtrace; }`, implement it in the `define_error!`-generated code and in `dot::error::Error<E>`. Add `pub fn format_traceback_full<E: Error + Traceback + ?Sized>(e: &E) -> String` that prints Display + source chain + locations + backtrace without going through `{e:?}`. Leave existing `format_traceback` as source-chain-only (drop the `{e:?}` tail). Cleaner API surface but a new trait + new fn + changed behavior of the existing fn.
  - **(C)** Change `format_traceback` to print only Display + `source()` chain; drop the `{e:?}` tail. Callers who want locations + backtrace format with `{:?}` themselves. Smallest change, but loses the current "one-call gives you everything" story the PyO3 comment relies on.

  Assume **(A)** unless the reviewer picks (B) or (C).

**Q2 — S1 `Arc<Backtrace>` → `Box<Backtrace>`?**

  - **(A)** Switch to `Box<Backtrace>`. The `backtrace` field is `pub` so this is a breaking change to the direct-field-access path; only [dot/src/error.rs:56](crates/dot/src/error.rs#L56) and [dot/src/error.rs:79](crates/dot/src/error.rs#L79) and [wrapper.rs:151](crates/strider-error/src/wrapper.rs#L151) / [wrapper.rs:174](crates/strider-error/src/wrapper.rs#L174) touch `self.fields.backtrace`, and they all go through `write!(f, "{}", &self.fields.backtrace)` or `&self.fields.backtrace` — both work unchanged thanks to `Deref` through `Box`. **Default.**
  - **(B)** Keep `Arc` so a future "tee this backtrace to two consumers" feature is free.

  Assume **(A)**.

**Q3 — S2 drop `impl Default for ErrorFields`?**

  - **(A)** Drop it. No caller uses it; `ErrorFields::new()` is always preferred. **Default.**
  - **(B)** Keep it as a courtesy for future consumers.

  Assume **(A)**.

**Q4 — F3 add `bridge_error!` macro and migrate the seven hand-rolled bridges?**

  - **(A)** Add a small `bridge_error!(InnerCrate::Error => $wrapper, ErrorKind::Variant);` macro in `strider-error`, migrate the four bridges in `analyzer`, two in `opt`, one in `pattern`. Each call site shrinks from ~8 lines to 1. **Default.**
  - **(B)** Leave the hand-rolled bridges — they're explicit and the repetition-cost is bounded at seven.

  Assume **(A)**.

**Q5 — R1 split `wrapper.rs`?**

  - **(A)** Split into `crates/strider-error/src/fields.rs` (`ErrorFields`, `LocationChain`, their `impl`s, and the bridge macro from Q4) and `crates/strider-error/src/define.rs` (the `define_error!` macro). Update `lib.rs` module declarations accordingly. Pure cosmetic; improves `grep` / editor tab clarity. **Default.**
  - **(B)** Leave `wrapper.rs` as one file.

  Assume **(A)**.

---

## File Structure (after execution, assuming all defaults)

```
crates/strider-error/
├── Cargo.toml
├── src/
│   ├── lib.rs              # (R2, R3 touched)
│   ├── format.rs           # (F1, T3 touched)
│   ├── fields.rs           # NEW — ErrorFields / LocationChain / bridge_error!   (Q5 split + Q4)
│   └── define.rs           # NEW — define_error! macro only                      (Q5 split)
└── tests/                  # NEW
    ├── fields.rs           # T1
    ├── macro_contract.rs   # T2
    └── format.rs           # T3
```

Downstream crates touched:

- [crates/analyzer/src/error.rs](crates/analyzer/src/error.rs) — 4 bridges migrated to `bridge_error!` (Q4).
- [crates/opt/src/error.rs](crates/opt/src/error.rs) — 2 bridges migrated.
- [crates/pattern/src/error.rs](crates/pattern/src/error.rs) — 1 bridge migrated.
- [crates/dot/src/error.rs](crates/dot/src/error.rs) — no functional change; `Arc<Backtrace>` → `Box<Backtrace>` is transparent through `Deref`, but the `fields: ErrorFields` construction still works because `ErrorFields::new()` is the entry point.

---

## Task 1: Create `tests/fields.rs` — pin `ErrorFields::new` / `push_caller` contract (T1)

**Files:**
- Create: `crates/strider-error/tests/fields.rs`

- [ ] **Step 1: Write the initial test file**

```rust
//! Pins the public contract of `ErrorFields::new` and `push_caller`.
//! These invariants are assumed by every `define_error!` wrapper and by
//! `dot::error::Error<E>`; this file is the one place they're tested directly.

use std::backtrace::BacktraceStatus;
use strider_error::ErrorFields;

#[test]
fn new_seeds_single_location_and_valid_backtrace_status() {
    let f = ErrorFields::new();
    assert_eq!(f.locations.len(), 1, "chain should have exactly one entry at construction");
    // Status is env-dependent — we accept any of the three documented values.
    let s = f.backtrace.status();
    assert!(
        matches!(s, BacktraceStatus::Captured | BacktraceStatus::Disabled | BacktraceStatus::Unsupported),
        "unexpected backtrace status: {s:?}",
    );
}

#[test]
fn push_caller_appends_location_without_touching_backtrace() {
    let f = ErrorFields::new();
    let before_ptr: *const _ = std::sync::Arc::as_ptr(&f.backtrace);
    let f = f.push_caller();
    assert_eq!(f.locations.len(), 2, "chain should grow by one per push_caller");
    let after_ptr: *const _ = std::sync::Arc::as_ptr(&f.backtrace);
    assert_eq!(
        before_ptr, after_ptr,
        "push_caller must not reallocate the backtrace",
    );
}

#[test]
fn repeated_push_caller_grows_chain_linearly() {
    let f = (0..5).fold(ErrorFields::new(), |acc, _| acc.push_caller());
    assert_eq!(f.locations.len(), 6, "1 from new() + 5 from push_caller()");
}
```

> **Note for Task 5 executor:** If Q2 lands (`Arc` → `Box`), replace the two `Arc::as_ptr(&f.backtrace)` lines with `&*f.backtrace as *const _` — same pointer-stability check, just through `Box::deref` instead of `Arc::as_ptr`. This is the only test that peeks at the backtrace pointer directly.

- [ ] **Step 2: Run it**

Run: `cargo test -p strider-error --test fields`
Expected: 3 passed.

- [ ] **Step 3: Commit**

```bash
git add crates/strider-error/tests/fields.rs
git commit -m "test(strider-error): pin ErrorFields::new/push_caller contract"
```

---

## Task 2: Create `tests/macro_contract.rs` — pin `define_error!` macro contract + `#[track_caller]` behavior (T2)

**Files:**
- Create: `crates/strider-error/tests/macro_contract.rs`

- [ ] **Step 1: Write the test file**

```rust
//! Pins the contract of the `define_error!` macro: the generated wrapper
//! provides kind/into_kind/decompose/locations/backtrace accessors, Display
//! delegates to the inner kind, Debug prints kind+locations+backtrace,
//! `Error::source` forwards to the inner kind, and `From<$kind>` / `From<$src>`
//! are both `#[track_caller]` at the `?` site.

use std::error::Error as _;

strider_error::define_error! {
    pub struct MyError wraps MyKind;
    sources: [std::io::Error];

    #[derive(Debug, thiserror::Error)]
    pub enum MyKind {
        #[error("boom")]
        Boom,
        #[error(transparent)]
        Io(#[from] std::io::Error),
    }
}

#[test]
fn from_kind_via_into_produces_length_one_chain() {
    let err: MyError = MyKind::Boom.into();
    assert_eq!(err.locations().len(), 1);
    assert!(matches!(err.kind(), MyKind::Boom));
}

#[test]
fn from_source_via_question_mark_produces_length_one_chain() {
    fn inner() -> Result<(), MyError> {
        let f = std::fs::File::open("/definitely/not/a/real/path")?;
        drop(f);
        Ok(())
    }
    let err = inner().unwrap_err();
    assert_eq!(err.locations().len(), 1, "source bridge seeds a fresh chain");
    assert!(matches!(err.kind(), MyKind::Io(_)));
}

#[test]
fn question_mark_on_same_wrapper_does_not_extend_chain() {
    fn leaf() -> Result<(), MyError> { Err(MyKind::Boom.into()) }
    fn middle() -> Result<(), MyError> { leaf()?; Ok(()) }
    fn outer() -> Result<(), MyError> { middle()?; Ok(()) }
    let err = outer().unwrap_err();
    assert_eq!(
        err.locations().len(),
        1,
        "same-wrapper ? is a move, not a From — chain stays at 1",
    );
}

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

#[test]
fn decompose_and_reconstruct_preserves_chain_length_and_backtrace_status() {
    let err: MyError = MyKind::Boom.into();
    let before_len = err.locations().len();
    let before_status = err.backtrace().status();
    let (kind, fields) = err.decompose();
    assert!(matches!(*kind, MyKind::Boom));
    assert_eq!(fields.locations.len(), before_len);
    assert_eq!(fields.backtrace.status(), before_status);
}

#[test]
fn display_delegates_to_inner_kind() {
    let err: MyError = MyKind::Boom.into();
    assert_eq!(err.to_string(), "boom");
}

#[test]
fn debug_prints_location_markers() {
    let err: MyError = MyKind::Boom.into();
    let dbg = format!("{err:?}");
    assert!(dbg.contains("boom"), "Debug should start with Display line; got {dbg:?}");
    assert!(dbg.contains("  at [0] "), "Debug should include location[0]; got {dbg:?}");
}

#[test]
fn error_source_forwards_to_inner_kind() {
    // MyKind::Io wraps std::io::Error via #[from], so source() should yield it.
    let err: MyError = std::fs::File::open("/definitely/not/a/real/path").unwrap_err().into();
    let src = err.source().expect("Io variant exposes its source");
    assert!(src.is::<std::io::Error>());
}
```

- [ ] **Step 2: Run it**

Run: `cargo test -p strider-error --test macro_contract`
Expected: 8 passed.

- [ ] **Step 3: Commit**

```bash
git add crates/strider-error/tests/macro_contract.rs
git commit -m "test(strider-error): pin define_error! contract + #[track_caller] behavior"
```

---

## Task 3: Fix `format_traceback` double-printed Display line (F1 + T3)

**Q1 default is (A).** This task implements (A). If the reviewer picks (B), Task 3 changes substantially — see alternate spec at the end of this task.

**Files:**
- Modify: [crates/strider-error/src/format.rs](crates/strider-error/src/format.rs)
- Create: `crates/strider-error/tests/format.rs`

- [ ] **Step 1: Write the failing test first**

Create `crates/strider-error/tests/format.rs`:

```rust
//! Verifies `format_traceback` produces the Display line exactly once when
//! the wrapper's Debug impl already starts with the Display line.

strider_error::define_error! {
    pub struct MyError wraps MyKind;

    #[derive(Debug, thiserror::Error)]
    pub enum MyKind {
        #[error("unique-display-marker-7a3f")]
        Boom,
    }
}

#[test]
fn format_traceback_prints_wrapper_display_exactly_once() {
    let err: MyError = MyKind::Boom.into();
    let s = strider_error::format_traceback(&err);
    let count = s.matches("unique-display-marker-7a3f").count();
    assert_eq!(
        count, 1,
        "expected the Display line once; got {count} occurrences in:\n{s}",
    );
    // Location markers must still be present.
    assert!(s.contains("  at [0] "), "locations dropped; got:\n{s}");
}

#[test]
fn format_traceback_walks_source_chain_top_to_bottom() {
    use std::error::Error as _;
    use std::fmt;

    #[derive(Debug)]
    struct Inner;
    impl fmt::Display for Inner {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("inner-marker") }
    }
    impl std::error::Error for Inner {}

    #[derive(Debug)]
    struct Outer { inner: Inner }
    impl fmt::Display for Outer {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("outer-marker") }
    }
    impl std::error::Error for Outer {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> { Some(&self.inner) }
    }

    let s = strider_error::format_traceback(&Outer { inner: Inner });
    let outer_at = s.find("outer-marker").expect("outer printed");
    let caused_at = s.find("caused by:").expect("caused-by line present");
    let inner_at = s.find("inner-marker").expect("inner printed");
    assert!(outer_at < caused_at && caused_at < inner_at, "ordering wrong in:\n{s}");
}
```

- [ ] **Step 2: Run it — it must fail**

Run: `cargo test -p strider-error --test format format_traceback_prints_wrapper_display_exactly_once`
Expected: FAIL — count will be 2 (once from `error: {err}`, once from `{err:?}`'s Debug which starts with the Display).

- [ ] **Step 3: Fix `format_traceback`**

Replace the body at [crates/strider-error/src/format.rs:30-44](crates/strider-error/src/format.rs#L30-L44):

```rust
pub fn format_traceback(err: &(dyn Error + 'static)) -> String {
    let mut out = String::new();
    // Writing into a String is infallible; explicit let _ silences the
    // Result from the Write trait.
    let _ = writeln!(out, "error: {err}");

    let mut cur = err.source();
    while let Some(e) = cur {
        let _ = writeln!(out, "  caused by: {e}");
        cur = e.source();
    }

    // The Debug of a `define_error!`-generated wrapper starts with its
    // Display line, which we already printed above. Strip it so the line
    // doesn't appear twice. For any other Debug impl the first line is
    // whatever Debug produced — preserving it is harmless.
    let dbg = format!("{err:?}");
    let tail = dbg.split_once('\n').map_or(dbg.as_str(), |(_first, rest)| rest);
    if !tail.is_empty() {
        let _ = writeln!(out);
        let _ = write!(out, "{tail}");
    }
    out
}
```

- [ ] **Step 4: Run the new tests — both must pass**

Run: `cargo test -p strider-error --test format`
Expected: 2 passed.

- [ ] **Step 5: Re-run the crate's doctest to confirm the existing example still works**

Run: `cargo test -p strider-error --doc`
Expected: PASS (the `Oops` doctest type's Debug has no leading Display line, so `split_once('\n')` returns `None` and `tail = dbg.as_str()` — no regression).

- [ ] **Step 6: Commit**

```bash
git add crates/strider-error/src/format.rs crates/strider-error/tests/format.rs
git commit -m "fix(strider-error): format_traceback no longer double-prints wrapper Display"
```

### Alternate spec if reviewer picks Q1 (B)

Replace Step 3 with: add `pub trait Traceback { fn location_chain(&self) -> &LocationChain; fn origin_backtrace(&self) -> &Backtrace; }` in `fields.rs`, implement it inside `define_error!` and in `dot/src/error.rs` manually. Add `pub fn format_traceback_full<E: ?Sized + Error + Traceback + 'static>(e: &E) -> String` that assembles the full output deduplicated. Change the existing `format_traceback` to only print Display + source chain (drop the `{e:?}` tail). Update the T3 tests to use `format_traceback_full` where they previously checked for `"  at [0] "`. Document that downstream callers wanting locations+backtrace should switch to `format_traceback_full`.

---

## Task 4: Drop `impl Default for ErrorFields` (S2 / Q3)

**Files:**
- Modify: [crates/strider-error/src/wrapper.rs](crates/strider-error/src/wrapper.rs)

- [ ] **Step 1: Confirm no caller**

Run: `grep -rn 'ErrorFields::default\|ErrorFields {}\|<ErrorFields as Default>' crates`
Expected: empty output.

- [ ] **Step 2: Remove the impl block**

Delete lines [crates/strider-error/src/wrapper.rs:52-57](crates/strider-error/src/wrapper.rs#L52-L57):

```rust
impl Default for ErrorFields {
    #[track_caller]
    fn default() -> Self {
        Self::new()
    }
}
```

- [ ] **Step 3: Run workspace tests**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/strider-error/src/wrapper.rs
git commit -m "refactor(strider-error): drop unused Default impl for ErrorFields"
```

---

## Task 5: Switch `Arc<Backtrace>` → `Box<Backtrace>` (S1 / Q2)

**Files:**
- Modify: [crates/strider-error/src/wrapper.rs](crates/strider-error/src/wrapper.rs)
- Modify: [crates/strider-error/tests/fields.rs](crates/strider-error/tests/fields.rs) (the pointer-stability check from Task 1, as flagged in that task's note)

- [ ] **Step 1: Change the field type + its constructor**

In `crates/strider-error/src/wrapper.rs`:

Update the doc block at lines 13-26 to reflect the new storage:

```rust
/// Shared payload carried by every crate's `Error` wrapper struct.
///
/// The backtrace is heap-allocated in a `Box` for a stable 1-pointer
/// footprint and so the whole struct fits comfortably in a `Box` inside
/// the outer `$wrapper`. Backtraces are never cloned — `decompose`,
/// `push_caller`, and every cross-crate bridge move the `ErrorFields`
/// by value.
pub struct ErrorFields {
    /// Backtrace captured at the point the error was first constructed.
    /// `Backtrace::capture()` respects `RUST_BACKTRACE`; when unset, it
    /// returns a `Disabled` status and carries no frames (cheap).
    pub backtrace: Box<Backtrace>,
    /// Per-`?` propagation chain. See type docs.
    pub locations: LocationChain,
}
```

Update the top-of-file use list: remove `use std::sync::Arc;`.

Update `ErrorFields::new`:

```rust
#[must_use]
#[track_caller]
pub fn new() -> Self {
    Self {
        backtrace: Box::new(Backtrace::capture()),
        locations: vec![Location::caller()],
    }
}
```

- [ ] **Step 2: Patch the pointer-stability test**

In `crates/strider-error/tests/fields.rs`, replace `push_caller_appends_location_without_touching_backtrace` body to use `&*f.backtrace as *const _` instead of `Arc::as_ptr(&f.backtrace)`:

```rust
#[test]
fn push_caller_appends_location_without_touching_backtrace() {
    let f = ErrorFields::new();
    let before_ptr: *const _ = &*f.backtrace;
    let f = f.push_caller();
    assert_eq!(f.locations.len(), 2);
    let after_ptr: *const _ = &*f.backtrace;
    assert_eq!(before_ptr, after_ptr, "push_caller must not reallocate the backtrace");
}
```

Drop the `use std::sync::Arc;` in that test file if it was added.

- [ ] **Step 3: Workspace build + test**

Run: `cargo build --workspace && cargo test --workspace`
Expected: PASS. The `&self.fields.backtrace` uses in `wrapper.rs` Debug, `dot/src/error.rs` Debug, and the `backtrace()` accessor all `Deref` through `Box` the same way they did through `Arc`.

- [ ] **Step 4: Commit**

```bash
git add crates/strider-error crates/strider-error/tests/fields.rs
git commit -m "refactor(strider-error): store Backtrace in Box instead of Arc"
```

---

## Task 6: Add `bridge_error!` macro and migrate the seven hand-rolled bridges (F3 / Q4)

**Files:**
- Modify: [crates/strider-error/src/wrapper.rs](crates/strider-error/src/wrapper.rs) (or `src/fields.rs` if Q5 split landed first)
- Modify: [crates/analyzer/src/error.rs](crates/analyzer/src/error.rs)
- Modify: [crates/opt/src/error.rs](crates/opt/src/error.rs)
- Modify: [crates/pattern/src/error.rs](crates/pattern/src/error.rs)
- Modify: `crates/strider-error/tests/macro_contract.rs` (add a bridge test)

- [ ] **Step 1: Add the macro**

Append to `crates/strider-error/src/wrapper.rs` (below the existing `define_error!` macro):

```rust
/// Generates a `#[track_caller] impl From<$inner> for $outer` that decomposes
/// the inner wrapper, appends the caller's site, and re-assembles as the outer
/// wrapper with kind wrapped in `$outer_kind::$variant`.
///
/// The inner type must expose `.decompose() -> (Box<InnerKind>, ErrorFields)`
/// (any type produced by [`define_error!`] does, as does the hand-rolled
/// `dot::error::Error<E>` via its manual `decompose` method).
///
/// # Example
///
/// ```ignore
/// strider_error::bridge_error!(ir::Error => Error, ErrorKind::IrError);
/// ```
///
/// expands to:
///
/// ```ignore
/// impl ::core::convert::From<ir::Error> for Error {
///     #[track_caller]
///     fn from(e: ir::Error) -> Self {
///         let (kind, fields) = e.decompose();
///         Self {
///             kind: ::std::boxed::Box::new(ErrorKind::IrError(*kind)),
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

- [ ] **Step 2: Add a test pinning the bridge macro's behavior**

Append to `crates/strider-error/tests/macro_contract.rs`:

```rust
// ── bridge_error! macro contract ─────────────────────────────────────────

strider_error::define_error! {
    pub struct OuterError wraps OuterKind;

    #[derive(Debug, thiserror::Error)]
    pub enum OuterKind {
        #[error(transparent)]
        Inner(MyKind),
    }
}

strider_error::bridge_error!(MyError => OuterError, OuterKind::Inner);

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

- [ ] **Step 3: Migrate `pattern::error`**

Replace [crates/pattern/src/error.rs:118-130](crates/pattern/src/error.rs#L118-L130):

```rust
/// Hand-rolled bridge so `?` across the `ir` → `pattern` boundary preserves the
/// origin backtrace + location chain captured by `ir`.  Decomposes the inner
/// wrapper, moves its `ErrorFields`, and appends the outer caller's site.
impl From<ir::Error> for Error {
    #[track_caller]
    fn from(e: ir::Error) -> Self {
        let (kind, fields) = e.decompose();
        Error {
            kind: Box::new(ErrorKind::IrError(*kind)),
            fields: fields.push_caller(),
        }
    }
}
```

with:

```rust
// Bridge so `?` across the `ir` → `pattern` boundary preserves the origin
// backtrace + location chain captured by `ir`, appending this crossing.
strider_error::bridge_error!(ir::Error => Error, ErrorKind::IrError);
```

- [ ] **Step 4: Migrate `opt::error`**

Replace both bridges at [crates/opt/src/error.rs:44-81](crates/opt/src/error.rs#L44-L81) (and the `ValidationErrors` bridge stays — it goes through `ir::Error::from` which now hits the new bridge macro transparently):

```rust
// Preserves origin backtrace + location chain captured by `ir`.
strider_error::bridge_error!(ir::Error => Error, ErrorKind::IrError);

/// `ir::ValidationErrors` is produced fresh at the validator call site, so
/// route it through `ir::Error` (which captures a fresh backtrace) and then
/// through the bridge above.
impl From<ir::ValidationErrors> for Error {
    #[track_caller]
    fn from(errs: ir::ValidationErrors) -> Self {
        Error::from(ir::Error::from(errs))
    }
}

// Preserves origin backtrace + location chain captured by `pattern`.
strider_error::bridge_error!(pattern::Error => Error, ErrorKind::PatternError);
```

- [ ] **Step 5: Migrate `analyzer::error`**

Replace the four bridges at [crates/analyzer/src/error.rs:68-115](crates/analyzer/src/error.rs#L68-L115) with:

```rust
// Preserves origin backtrace + location chain across each crossing.
strider_error::bridge_error!(cfg::Error    => Error, ErrorKind::CfgError);
strider_error::bridge_error!(ir::Error     => Error, ErrorKind::IrError);
strider_error::bridge_error!(opt::Error    => Error, ErrorKind::OptError);
strider_error::bridge_error!(target::Error => Error, ErrorKind::TargetError);
```

- [ ] **Step 6: Verify workspace builds and tests still pass**

Run: `cargo build --workspace && cargo test --workspace`
Expected: PASS. Specifically:
- `cargo test -p analyzer --test error_chain` still shows chain length ≥ 3 (origin + opt bridge + analyzer bridge).
- `cargo test -p strider-error --test macro_contract bridge_error_macro_extends_chain_by_one` PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/strider-error crates/analyzer/src/error.rs crates/opt/src/error.rs crates/pattern/src/error.rs
git commit -m "refactor(strider-error): add bridge_error! macro; migrate 7 hand-rolled cross-crate bridges"
```

---

## Task 7: Split `wrapper.rs` into `fields.rs` + `define.rs` (R1 / Q5)

**Files:**
- Create: `crates/strider-error/src/fields.rs`
- Create: `crates/strider-error/src/define.rs`
- Delete: `crates/strider-error/src/wrapper.rs`
- Modify: [crates/strider-error/src/lib.rs](crates/strider-error/src/lib.rs)

- [ ] **Step 1: Create `fields.rs` with the struct, types, impls, and `bridge_error!`**

Move [wrapper.rs:1-57](crates/strider-error/src/wrapper.rs#L1-L57) (use imports, `LocationChain`, `ErrorFields`, `ErrorFields::new`, `ErrorFields::push_caller`) plus the `bridge_error!` macro added in Task 6 into `crates/strider-error/src/fields.rs`. (If Task 4 landed, the `Default` impl is already gone; if Task 5 landed, the `Arc` → `Box` swap is already in.)

- [ ] **Step 2: Create `define.rs` with only the `define_error!` macro**

Move [wrapper.rs:59-207](crates/strider-error/src/wrapper.rs#L59-L207) (the whole `define_error!` macro with its doc comment) into `crates/strider-error/src/define.rs`.

- [ ] **Step 3: Update `lib.rs`**

Replace [crates/strider-error/src/lib.rs](crates/strider-error/src/lib.rs) with:

```rust
//! Shared error-wrapper machinery for strider crates.
//!
//! Every crate in the workspace that defines an error type wraps its
//! existing `thiserror` enum in a wrapper struct generated by the
//! [`define_error!`] macro. The wrapper carries:
//!
//! * a `Box<ErrorKind>` — the original enum, unchanged.
//! * an [`ErrorFields`] payload holding:
//!   * a [`std::backtrace::Backtrace`] captured at the point the error
//!     was first created (the "origin" of the error).
//!   * a [`LocationChain`] — a `Vec` of `&'static Location` values, one
//!     pushed at every `?` / `From::from` boundary the error crosses.
//!
//! The combination gives callers a Python-style traceback: a sequence of
//! `file:line:column` frames showing propagation, plus a stack backtrace
//! pointing at the origin. This is what the PyO3 layer (planned
//! `strider-py`) converts into a Python exception's `__traceback__`.
//!
//! ## Where to start
//!
//! - [`define_error!`] — define a crate-local wrapper over a `thiserror` enum.
//! - [`bridge_error!`] — connect one crate's wrapper to another crate's wrapper,
//!   extending the location chain across the boundary.
//! - [`format_traceback`] — render an error + its source chain + locations +
//!   backtrace into a single string, for the PyO3 / logging surface.

mod define;
mod fields;
mod format;

pub use fields::{ErrorFields, LocationChain};
pub use format::format_traceback;
// `define_error!` and `bridge_error!` are `#[macro_export]`-ed from their
// modules — they're reachable as `strider_error::define_error!` /
// `strider_error::bridge_error!` regardless of the mod visibility above.
```

(Note: the `#[macro_use]` attribute is no longer needed — `#[macro_export]` macros live at the crate root and are reachable regardless of module privacy. Drop the `#[macro_use]` line entirely if it was there.)

- [ ] **Step 4: Verify the build**

Run: `cargo check --workspace && cargo test --workspace`
Expected: PASS — everything downstream still references `strider_error::{ErrorFields, LocationChain, format_traceback, define_error!, bridge_error!}` at the same paths.

- [ ] **Step 5: Commit**

```bash
git add crates/strider-error
git commit -m "refactor(strider-error): split wrapper.rs into fields.rs + define.rs"
```

---

## Task 8: Clarify lib.rs docs (R2) and privatize the `format` module (R3)

**Files:**
- Modify: [crates/strider-error/src/lib.rs](crates/strider-error/src/lib.rs)

- [ ] **Step 1: Drop `pub` from `format`**

If Task 7 landed, `mod format;` is already private — this step is a no-op.
Otherwise, change `pub mod format;` to `mod format;` at [crates/strider-error/src/lib.rs:19](crates/strider-error/src/lib.rs#L19). The re-export `pub use format::format_traceback;` keeps the function reachable at its canonical path `strider_error::format_traceback`.

- [ ] **Step 2: Confirm no external caller uses `strider_error::format::format_traceback`**

Run: `grep -rn 'strider_error::format::' crates`
Expected: empty output.

- [ ] **Step 3: Verify workspace builds**

Run: `cargo check --workspace && cargo test --workspace`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/strider-error
git commit -m "refactor(strider-error): privatize format module; single canonical path for format_traceback"
```

---

## Task 9: Final workspace sanity sweep

**Files:**
- Run-only, no edits.

- [ ] **Step 1: Full workspace build**

Run: `cargo build --workspace`
Expected: PASS, no warnings.

- [ ] **Step 2: Full workspace tests**

Run: `cargo test --workspace`
Expected: PASS. Specifically:
- `cargo test -p strider-error` shows 2 doctests + 3 from `tests/fields.rs` + 8 from `tests/macro_contract.rs` + 1 new bridge test = 9 integration + 2 doctest. Each test file also runs independently.
- `cargo test -p analyzer --test error_chain` still passes.
- `cargo test -p reader --test error` still passes.

- [ ] **Step 3: Strider-error-only lint (strict)**

Run: `cargo clippy -p strider-error --all-targets --no-deps -- -D warnings`
Expected: PASS.

- [ ] **Step 4: Workspace lint**

Run: `cargo clippy --workspace -- -D warnings`
Expected: PASS (no new lints introduced by this work).

- [ ] **Step 5: Smoke-run the example**

Run: `cargo run --example analyzer`
Expected: `cfg.html`, `graph.html`, `graph-opt.html` produced (confirming no runtime regression via the error path).

---

## Out of Scope (considered, rejected or deferred)

- **`format_traceback` returning `&str` / a streaming writer instead of `String`.** Every caller (planned PyO3 layer, logging) needs the full buffer. Not worth the API churn.
- **Richer `LocationChain` (include the error kind at each frame).** Currently the chain is purely `Location`s; knowing *which* variant was seen at each boundary would help debugging multi-hop errors. But `push_caller` doesn't have access to the kind, and threading it through every bridge would balloon the API. Fold only if PyO3 users ask.
- **Making `define_error!` support `#[derive(Clone)]` wrappers.** `Backtrace` is not `Clone` and `Arc` would be re-required; not worth re-introducing Arc for a theoretical use case.
- **Replacing `thiserror` with a hand-rolled `impl Display + Error` inside the macro.** `thiserror` is doing real work (variant-level `#[error(...)]` attributes, `#[from]`-generated `From<$src> for $kind`). Swapping it out would eliminate the `$enum_attr` / `$body:tt` passthrough discipline — major regression.
- **Moving `dot::error::Error<E>` into this crate as a second macro (`define_generic_error!`).** `dot` is the only generic consumer; a second macro for one caller is not worth the maintenance.
- **Renaming `LocationChain` to `LocationStack`.** Chain is the term already used in docs and in the PyO3 spec. Skip.
