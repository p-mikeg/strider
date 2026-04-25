# strider-error Crate Review — Round 9 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ninth-pass review of `strider-error` after rounds 1–8. Rounds 7–8 shipped (`a4fb6e8` "tighten format helpers" + `4ad5a65` "drop redundant #[track_caller]"). Round 9 finds **zero correctness bugs** and one small consistency edit in the `define_error!` macro body. No public-API changes, no behavior changes.

**Architecture:** Same four-file layout (`lib.rs`, `fields.rs`, `define.rs`, `format.rs`), ~330 LoC across `src/`. S1 collapses the macro-generated `Display` impl to mirror the `Debug` impl one line down (both go through `Box<$kind>`'s `Deref`). Optional Q1 simplifies the `From<$src>` body to use `Self::from(_)` for the outer dispatch.

**Tech Stack:** Rust 2024, `thiserror`, `std::backtrace::Backtrace`, `std::panic::Location`. No new dependencies; no MSRV change.

---

## Baseline (verified 2026-04-25 against HEAD `0b91d10`)

- `cargo test -p strider-error` → 16 tests pass (3 in `tests/fields.rs` + 4 in `tests/format.rs` + 9 in `tests/macro_contract.rs`) + 3 doctests pass (1 ignored).
- `cargo clippy -p strider-error --all-targets --no-deps -- -D warnings` → clean.
- Round 7–8 commits present: `931080e`, `427b2c8`, `a4fb6e8`, `4ad5a65`.
- 9 in-tree consumers: `cfg`, `ir`, `opt`, `pattern`, `analyzer`, `target`, `reader`, `dot` (hand-rolled generic), plus test wrappers.
- 5 cross-crate `bridge_error!` invocations: `analyzer × {cfg, ir, opt, target}`, `pattern × ir`, `opt × {ir, pattern}`.
- Verified `Box<$kind>` derefs to `$kind`, so `write!(f, "{}", self.kind)` invokes `Display::fmt(&$kind, f)` — same dispatch as the current `Display::fmt(&*self.kind, f)`. Smoke check via the Debug impl one line below at [crates/strider-error/src/define.rs:120](crates/strider-error/src/define.rs#L120) which already uses `writeln!(f, "{}", self.kind)?;` and is exercised by every `format!("{:?}", err)` in tests.

---

## Review Findings — Executive Summary

**Zero correctness bugs.** One small consistency edit in source (S1, default). One borderline simplification offered as Q1 (default skip — would undo the deliberate round-4 FQ harmonization).

### Simplification (S)

- **S1 — `define_error!`'s `Display` impl uses long-form trait dispatch where the macro form is shorter and matches the `Debug` impl directly below.** [crates/strider-error/src/define.rs:112-116](crates/strider-error/src/define.rs#L112-L116):

  ```rust
  impl ::std::fmt::Display for $wrapper {
      fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
          ::std::fmt::Display::fmt(&*self.kind, f)
      }
  }
  ```

  `self.kind` is `Box<$kind>`. `&*self.kind` dereferences the Box to `$kind` then takes `&$kind` (3 chars of nav for one level of Deref). Replace with the macro form, which mirrors the `Debug` impl on lines 118–123:

  ```rust
  impl ::std::fmt::Display for $wrapper {
      fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
          write!(f, "{}", self.kind)
      }
  }
  ```

  Why it's identical: `write!(f, "{}", self.kind)` expands to `f.write_fmt(format_args!("{}", self.kind))`, and `format_args!("{}", x)` invokes `Display::fmt(&x, _)`. Since `x = self.kind: Box<$kind>` and `Box<T>` implements `Display` *via* `T: Display` (delegating through `Deref`), the resulting dispatch is `Display::fmt(&*self.kind, _)` — exactly what the verbose form spells out. Pinned by `tests/macro_contract.rs::display_delegates_to_inner_kind` (`err.to_string()` invokes `Display::fmt`) and `tests/format.rs::format_traceback_prints_wrapper_display_exactly_once` (asserts the unique Display marker appears exactly once in the rendered traceback).

  Net effect: -1 use of `&*` deref, internal consistency with the `Debug` impl, identical generated code.

### Out of scope for this round

The following were considered and deliberately left alone — same rationale as in rounds 3–8 unless noted:

- **Privatize `ErrorFields.{backtrace, locations}` fields.** Rounds 3–8 deferred. Both the macro-generated `locations()` / `backtrace()` accessors *and* `dot::Error<E>` (at [crates/dot/src/error.rs:50-57](crates/dot/src/error.rs#L50-L57) and `:80-86`) read these via field access from outside the strider-error crate. Privatizing requires accessor methods on `ErrorFields` plus updates to one macro arm and the hand-rolled generic. Net benefit is encapsulation hygiene only.
- **Consolidate `fmt_chain_and_backtrace` (`&mut Formatter`) + `write_chain_and_backtrace<W: Write>` (free `pub(crate)`)** into a single `&mut dyn fmt::Write` method or a trait default method. Either form changes the public API touch surface (both the macro and `dot::Error<E>`) for negligible duplication (the wrapper is 3 lines). See Q1 in Open Questions.
- **`Option<Box<Backtrace>>` to skip the alloc when backtraces are disabled.** Round 2 deferred; no profiling demand.
- **`LocationChain` → `SmallVec<[&'static Location<'static>; 4]>`.** No profiling demand.
- **`Traceback::location_chain` returning `&[&'static Location<'static>]` instead of `&LocationChain`.** Public-API change.
- **Generic-aware `define_error!` to subsume `dot::error::Error<E>`.** Round 4 noted; threading a generic doubles macro size for one consumer.
- **Boxed-inner footprint refactor (anyhow-style: combine `kind: Box<Kind>` + `fields: ErrorFields` into a single boxed inner).** No correctness motivation.
- **Drop `+ 'static` from `format_traceback`'s parameter.** Public-API loosening; no demand.
- **Pedantic / nursery clippy lints** (currently off in workspace config): `clippy::doc_markdown` wants "PyO3" backticked (3 sites: `lib.rs:16`, `lib.rs:25`, `format.rs:12`); `clippy::borrow_as_ptr` flags `&*f.backtrace` in `tests/fields.rs:25,28` (use `&raw const`); `clippy::too_long_first_doc_paragraph` on `define.rs:165` and `format.rs:7`; `clippy::redundant_pub_crate` flags `fields.rs:80` (since the module is private, `pub(crate)` could be `pub`). Each is one-character cosmetic. Defer until a workspace-wide lint promotion lands.
- **Pin `loc.line()` in track-caller tests.** Rounds 5/6 deferred — brittle to test-file edits.

---

## Open Questions for the Reviewer

Each of these changes the shape of a task. Pick one per group. Assumed defaults are marked.

**Q1 — Land S2 (`From<$src>` body uses `Self::from(_)`) or skip?**

The macro at [crates/strider-error/src/define.rs:152-159](crates/strider-error/src/define.rs#L152-L159) currently spells out the outer dispatch fully:

```rust
impl ::std::convert::From<$src> for $wrapper {
    #[track_caller]
    fn from(e: $src) -> Self {
        <$wrapper as ::std::convert::From<$kind>>::from(
            <$kind as ::std::convert::From<$src>>::from(e),
        )
    }
}
```

The outer `<$wrapper as ::std::convert::From<$kind>>::from(_)` is unambiguous: `$wrapper` only has a single `From<$kind>` impl (via this same macro), so method resolution on `Self::from(arg)` where `arg: $kind` picks it without help. The inner `<$kind as ::std::convert::From<$src>>::from(e)` *should* stay FQ because `$kind` typically has multiple `#[from]`-generated `From<X>` impls and we want defensive disambiguation in macro-emitted code.

Simpler form:

```rust
impl ::std::convert::From<$src> for $wrapper {
    #[track_caller]
    fn from(e: $src) -> Self {
        Self::from(<$kind as ::std::convert::From<$src>>::from(e))
    }
}
```

- (A) **Default.** Skip. The fully-qualified outer form was the explicit result of round-4 commit `4e920bd` ("harmonize bridge_error! From path on `::std::convert`"). Reverting half of that harmonization is churn.
- (B) Land S2. The outer FQ is unambiguous and adds 32 chars per macro arm; the simpler form makes the `track_caller` chain easier to read.

**Q2 — Land S1 alone or batch?**

- (A) **Default.** Land S1 alone (one commit). It's a small consistency tightening of the `Display` impl.
- (B) If Q1=B, combine S1+S2 into a single "tighten define_error! macro" commit.

---

## File Structure (after execution, assuming all defaults: Q1=A, Q2=A)

```
crates/strider-error/
├── src/
│   ├── lib.rs        # unchanged
│   ├── fields.rs     # unchanged
│   ├── define.rs     # S1: Display impl uses write!(f, "{}", self.kind)
│   └── format.rs     # unchanged
└── tests/
    ├── fields.rs        # unchanged
    ├── format.rs        # unchanged
    └── macro_contract.rs  # unchanged
```

Downstream crates: zero changes. The macro arm change rebuilds 9 wrappers but the emitted code is functionally identical.

If the reviewer flips Q1, Task 2 below also applies.

---

## Task 1: Tighten `define_error!`'s Display impl to mirror Debug (S1)

**Files:**
- Modify: [crates/strider-error/src/define.rs](crates/strider-error/src/define.rs)

- [ ] **Step 1: Replace the `Display::fmt(&*self.kind, f)` form with `write!(f, "{}", self.kind)`**

In [crates/strider-error/src/define.rs](crates/strider-error/src/define.rs), locate the macro arm at lines 112–116:

```rust
        impl ::std::fmt::Display for $wrapper {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                ::std::fmt::Display::fmt(&*self.kind, f)
            }
        }
```

Replace the body with:

```rust
        impl ::std::fmt::Display for $wrapper {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                write!(f, "{}", self.kind)
            }
        }
```

Reason it's correct: `self.kind: Box<$kind>`. `Box<T>` implements `Display` via `T: Display` (delegating through `Deref`), so `write!(f, "{}", self.kind)` resolves to `<Box<$kind> as Display>::fmt(&self.kind, f)`, which forwards to `<$kind as Display>::fmt(&**(&self.kind), f)` = `Display::fmt(&*self.kind, f)`. Same dispatch, same generated code.

The change also matches the `Debug` impl directly below at lines 118–123, which already uses `writeln!(f, "{}", self.kind)?;` for the same wrapper-kind delegation. The two impls now use a consistent shape.

Do not change:
- The macro signature, the `$wrapper` / `$kind` metavars, or any other arm.
- The `Debug` impl below — already correct.
- Any docs or examples (the surface API is unchanged).

- [ ] **Step 2: Verify behavior on the touched crate**

Run: `cargo test -p strider-error`
Expected: 16 tests + 3 doctests pass (1 ignored). In particular:
- `tests/macro_contract.rs::display_delegates_to_inner_kind` asserts `err.to_string() == "boom"` — directly exercises the changed impl.
- `tests/format.rs::format_traceback_prints_wrapper_display_exactly_once` asserts the unique Display marker appears exactly once in the formatted traceback (the first line, written by `writeln!(out, "error: {err}")` in `format_traceback`, invokes the changed impl).
- `tests/format.rs::format_traceback_does_not_duplicate_multiline_display` asserts that a multi-line Display (`"line1\nline2-marker-8b4e"`) is rendered without duplication — pins that delegation through Box's Deref preserves multi-line output exactly.

Run: `cargo clippy -p strider-error --all-targets --no-deps -- -D warnings`
Expected: clean.

- [ ] **Step 3: Workspace-wide rebuild + test sweep**

Because `define_error!` is invoked in 9 crates (`cfg`, `ir`, `opt`, `pattern`, `analyzer`, `target`, `reader`, plus the `dot` hand-rolled which doesn't use the macro but mirrors its shape), every wrapper rebuilds with the new Display body.

Run: `cargo build --workspace`
Expected: PASS.

Run: `cargo test --workspace`
Expected: PASS. Catches any indirect Display-format regression in downstream tests (e.g., `dot::error::Error<E>::Display` is hand-rolled and unaffected; the 9 macro consumers all get the new form).

Run: `cargo clippy --workspace -- -D warnings`
Expected: PASS.

- [ ] **Step 4: Smoke-run the example**

Run: `cargo run --example analyzer`
Expected: `cfg.html`, `graph.html`, `graph-opt.html` produced. (The example runs the full pipeline; if any error rendering regressed, it would surface here.)

- [ ] **Step 5: Commit**

```bash
git add crates/strider-error/src/define.rs
git commit -m "$(cat <<'EOF'
refactor(strider-error): tighten define_error! Display impl

Replace the long-form `Display::fmt(&*self.kind, f)` in the macro
arm with `write!(f, "{}", self.kind)`. Box<$kind> implements
Display via $kind: Display through Deref, so the dispatch is
identical -- same generated code, same test coverage. Brings
the Display impl into the same shape as the Debug impl two
lines below, which already uses `writeln!(f, "{}", self.kind)`.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2 (only if Q1=B): Drop outer FQ in `From<$src>` body (S2)

**Files:**
- Modify: [crates/strider-error/src/define.rs](crates/strider-error/src/define.rs)

- [ ] **Step 1: Replace the outer FQ dispatch with `Self::from`**

In [crates/strider-error/src/define.rs](crates/strider-error/src/define.rs), locate the source-bridge arm at lines 152–159:

```rust
                impl ::std::convert::From<$src> for $wrapper {
                    #[track_caller]
                    fn from(e: $src) -> Self {
                        <$wrapper as ::std::convert::From<$kind>>::from(
                            <$kind as ::std::convert::From<$src>>::from(e),
                        )
                    }
                }
```

Replace the body with:

```rust
                impl ::std::convert::From<$src> for $wrapper {
                    #[track_caller]
                    fn from(e: $src) -> Self {
                        Self::from(<$kind as ::std::convert::From<$src>>::from(e))
                    }
                }
```

Why the dispatch is unambiguous:
- `Self = $wrapper`. The macro's other `From<$kind> for $wrapper` impl (lines 140–148) is the only `From<$kind>` impl on `$wrapper`.
- The argument to `Self::from` has type `$kind` (the inner FQ call returns `$kind`).
- Method resolution on `Self::from(arg)` looks at all in-scope `From<X>::from` impls on `$wrapper`. The only one whose argument type matches `$kind` is `<$wrapper as From<$kind>>::from`. Argument-type disambiguation is unambiguous.
- The blanket `impl<T> From<T> for T` provides `From<$wrapper> for $wrapper`, but its argument is `$wrapper`, not `$kind`, so it's not a candidate.

`#[track_caller]` propagation: both the new `Self::from(_)` form and the original FQ form delegate to the same `<$wrapper as From<$kind>>::from`, which is `#[track_caller]`-annotated. The chain (`From<$src> for $wrapper` → `From<$kind> for $wrapper` → `ErrorFields::new`) is unchanged.

The inner `<$kind as ::std::convert::From<$src>>::from(e)` stays fully qualified because `$kind` typically has multiple `#[from]`-generated `From<X>` impls in user code, and `<$kind>::from(e)` *would* resolve correctly by argument type but the FQ form is defensive in macro-emitted code.

- [ ] **Step 2: Verify behavior on the touched crate**

Run: `cargo test -p strider-error`
Expected: 16 tests + 3 doctests pass (1 ignored). In particular:
- `tests/macro_contract.rs::from_source_via_question_mark_produces_length_one_chain` exercises `?` on a `Result<_, std::io::Error> → Result<_, MyError>`, which goes through the macro-generated `From<std::io::Error> for MyError` impl.
- `tests/macro_contract.rs::track_caller_on_question_mark_points_at_question_mark_site` pins that the `?`-site location ends up in the chain — sensitive to any track-caller chain regression.
- `tests/macro_contract.rs::error_source_forwards_to_inner_kind` pins that `err.source()` returns the inner `io::Error` — orthogonal but exercises the same source-bridge code path.

Run: `cargo clippy -p strider-error --all-targets --no-deps -- -D warnings`
Expected: clean.

- [ ] **Step 3: Workspace-wide rebuild + test sweep**

The macro arm change touches every `define_error!` invocation that uses `sources: [...]`. Audit:
- `crates/reader/src/error.rs` — uses `sources` ([crates/reader/src/error.rs:22](crates/reader/src/error.rs#L22)).
- `crates/cfg/src/error.rs` — uses `sources` ([crates/cfg/src/error.rs:52](crates/cfg/src/error.rs#L52)).
- `crates/ir/src/error.rs` — uses `sources` ([crates/ir/src/error.rs:107](crates/ir/src/error.rs#L107)).
- `crates/target/src/error.rs` — uses `sources` ([crates/target/src/error.rs:11](crates/target/src/error.rs#L11)).
- `crates/opt/src/error.rs` — uses `sources` ([crates/opt/src/error.rs:40](crates/opt/src/error.rs#L40)).
- `crates/pattern/src/error.rs` — uses `sources` ([crates/pattern/src/error.rs:49](crates/pattern/src/error.rs#L49)).
- `crates/analyzer/src/error.rs` — uses `sources` ([crates/analyzer/src/error.rs:63](crates/analyzer/src/error.rs#L63)).

Verify by grep before running tests:

```bash
grep -rn "define_error!" /home/mike/Desktop/strider/crates/ --include="*.rs" | grep -v "/strider-error/"
```

Expected: 7 invocations across the crates listed above.

Run: `cargo build --workspace`
Expected: PASS.

Run: `cargo test --workspace`
Expected: PASS.

Run: `cargo clippy --workspace -- -D warnings`
Expected: PASS.

- [ ] **Step 4: Commit**

If Q2=A (separate commits):

```bash
git add crates/strider-error/src/define.rs
git commit -m "$(cat <<'EOF'
refactor(strider-error): drop outer FQ in From<$src> body

The outer `<$wrapper as ::std::convert::From<$kind>>::from(_)`
in the macro-generated source-bridge impl is unambiguous:
$wrapper has exactly one From<$kind> impl (also macro-emitted),
so method resolution on `Self::from(_)` where the argument is
$kind picks it without disambiguation help. Replace with
`Self::from(<$kind as ::std::convert::From<$src>>::from(e))`.

The inner FQ stays defensive: $kind in user code typically has
multiple #[from]-generated From<X> impls, and the macro can't
know which.

Track-caller chain unchanged: both forms delegate to the same
#[track_caller]-annotated From<$kind> impl.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

If Q2=B, combine with Task 1's edit into a single "tighten define_error! macro" commit (rewrite both arms in one `git add` + commit).

---

## Task 3: Workspace sanity sweep

**Files:** Run-only, no edits.

- [ ] **Step 1: Full workspace build**

Run: `cargo build --workspace`
Expected: PASS.

- [ ] **Step 2: Full workspace tests**

Run: `cargo test --workspace`
Expected: PASS. (Already covered in Task 1 Step 3 / Task 2 Step 3, but re-run as a final pass to catch any race or order-dependence.)

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

- Privatize `ErrorFields.{backtrace, locations}` fields (rounds 3–8 deferred).
- Consolidate `fmt_chain_and_backtrace` + `write_chain_and_backtrace` into a single `&mut dyn Write` API (Q1 in earlier rounds).
- `Option<Box<Backtrace>>` to skip alloc when backtraces are disabled.
- `LocationChain` → `SmallVec` micro-optimization.
- `Traceback::location_chain` returning `&[…]` instead of `&LocationChain` (public-API change).
- Generic-aware `define_error!` to subsume `dot::error::Error<E>`.
- Boxed-inner footprint refactor (anyhow-style).
- Loosening `+ 'static` on `format_traceback`'s parameter.
- Pedantic / nursery clippy cleanup (doc_markdown, borrow_as_ptr, too_long_first_doc_paragraph, redundant_pub_crate) — defer until workspace lint promotion lands.
- Pinning `loc.line()` in track-caller tests (rounds 5/6 defer).
