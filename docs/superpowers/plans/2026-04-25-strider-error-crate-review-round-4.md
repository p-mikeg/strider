# strider-error Crate Review — Round 4 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fourth-pass review of `strider-error` (rounds 1–3 are committed as of 2026-04-25). This round closes the last two items I could find: one duplicated-code simplification (`format_traceback` re-implements the chain+backtrace write-loop that already exists on `ErrorFields::fmt_chain_and_backtrace`), and one undocumented pitfall (`ErrorKind::X.into()` loses `#[track_caller]` propagation because std's `Into::into` is not `#[track_caller]`). Scope is deliberately small.

**Architecture:** Four-file crate (`lib.rs`, `fields.rs`, `define.rs`, `format.rs`), ~330 lines. No new abstractions, no new public API items — the helper method on `ErrorFields` gains a generic `W: fmt::Write` bound and gets a new caller, nothing more. The macro doc gets a "Caveats" section.

**Tech Stack:** Rust 2024, `thiserror`, `std::backtrace::Backtrace`, `std::panic::Location`. Relies on `Formatter<'_>: fmt::Write` (stable since 1.28) and trait upcasting (stable since 1.86).

---

## Baseline (verified 2026-04-25)

- `cargo test -p strider-error` → 14 tests pass (3 in `tests/fields.rs` + 9 in `tests/macro_contract.rs` + 2 in `tests/format.rs` + 3 doctests).
- `cargo test -p dot --test error` → 6 tests pass.
- `cargo clippy -p strider-error --all-targets --no-deps -- -D warnings` → clean.
- `grep -rn format_traceback crates/ | grep -v strider-error` → empty (no workspace callers outside the crate).
- `grep -rn 'strider_error::Traceback' crates/` → one hit in [dot/src/error.rs:79](crates/dot/src/error.rs#L79).

---

## Review Findings — Executive Summary

**Zero correctness bugs found this round.** Rounds 1–3 addressed everything of substance. The two items below are small.

### Simplification (S)

- **S1 — `format_traceback` re-implements the chain+backtrace write-loop.** The body at [format.rs:47-58](crates/strider-error/src/format.rs#L47-L58) is byte-for-byte the same loop + final `write!(out, "{}", backtrace)` already present at [fields.rs:69-79](crates/strider-error/src/fields.rs#L69-L79) in `ErrorFields::fmt_chain_and_backtrace`. The only reason they can't share code today is that `fmt_chain_and_backtrace` takes `&mut std::fmt::Formatter<'_>` while `format_traceback` writes into `&mut String`. Both `Formatter<'_>` and `String` implement `std::fmt::Write` (the former since Rust 1.28), so generalizing the helper over `W: fmt::Write` collapses the duplication without an API break.

  Concretely: change the signature to `pub fn fmt_chain_and_backtrace<W: std::fmt::Write>(&self, w: &mut W) -> std::fmt::Result`. The two existing call sites ([define.rs:100](crates/strider-error/src/define.rs#L100) macro-emitted Debug, [dot/src/error.rs:69](crates/dot/src/error.rs#L69)) keep passing `&mut Formatter` and compile unchanged. Then `format_traceback` drops its own loop and calls the method via the `Traceback` trait's accessors.

  **Caveat:** The Traceback trait exposes `location_chain()` and `origin_backtrace()` but not `fmt_chain_and_backtrace` — a trait method with a generic type parameter isn't object-safe, so we can't put one there without giving up the `&dyn Traceback` story. Workable shape: keep the method on `ErrorFields`, and have `format_traceback` build a temporary `write_chain_and_backtrace(chain, backtrace, &mut W)` free function out of the shared body, called by both `ErrorFields::fmt_chain_and_backtrace` and `format_traceback`. See Task 1 for the exact shape.

### Readability / Docs (R)

- **R1 — The `.into()` escape hatch silently breaks location tracking.** Round 1's finding F2 ("`#[track_caller]` on `From<ErrorKind> for $wrapper` is not pinned by a direct test") was partially addressed in round 3 via [macro_contract.rs:60-76](crates/strider-error/tests/macro_contract.rs#L60-L76), which pins `?`-site forwarding. But the `.into()` path was never documented.

  Mechanics: the std blanket `impl<T, U: From<T>> Into<U> for T` has an `Into::into` that is **not** `#[track_caller]`. When a user writes `MyKind::Boom.into()`:

  ```text
  user site → Into::into (NOT tc)   ← track_caller chain ends here
                  ↓
            From<MyKind>::from (tc)  ← sees Into::into's call site, not user's
                  ↓
            ErrorFields::new (tc)    ← Location::caller() = core/src/convert/mod.rs
  ```

  So the location captured is somewhere inside stdlib, not the user's line. The `?` operator and explicit `MyError::from(MyKind::Boom)` both preserve the chain correctly — only `.into()` doesn't. Today every in-tree use of `.into()` is in tests that only assert chain *length*, so nothing is observably broken; a future user writing `ErrorKind::X.into()` and expecting their own site in `err.locations()[0]` will be surprised.

  Fix is **documentation only**. A regression test would have to pin "location is NOT the user site" via file/path matching, which is fragile against future stdlib changes (e.g. if `Into::into` is ever marked `#[track_caller]`, we'd *want* the test to notice, but encoding "not equal to" is the wrong shape). Just a Caveats section in the `define_error!` doc suffices.

### Out of scope for this round

The following were considered and deliberately left alone:

- **Privatize `ErrorFields.{backtrace, locations}` fields.** Round 3 skipped this; still not worth the API churn. `dot::Error<E>` legitimately reads these directly, and the `Traceback` trait + the `{locations, backtrace}()` accessors already provide a private-field equivalent for new consumers.
- **`fmt_chain_and_backtrace`-like method on the `Traceback` trait directly.** Would need to be object-safe, which requires either pushing generic bound to a supertype or dispatching dynamically through a trait object — neither is worth the complexity for two callers.
- **`LocationChain` → `SmallVec`.** Round 3 deferred. No profiling demand.
- **`Option<Box<Backtrace>>` to skip the alloc when `RUST_BACKTRACE` is unset.** Round 2 deferred. Still speculative.
- **`format_traceback` blank-line separator between source walk / locations / backtrace.** Cosmetic; output is currently a single run of lines and has no in-tree consumer complaining.
- **Teaching `bridge_error!` to consume `ir::ValidationErrors` via a second arm.** One call site, not worth a second macro form. Still noted in round 2's out-of-scope list.

---

## Open Questions for the Reviewer

Each of these changes the shape of a task. Pick one per group. Assumed defaults are marked and reflected in the tasks below.

**Q1 — S1 shape: shared code lives as a free function, or as a generic method?**

- **(A) Free function** `pub(crate) fn write_chain_and_backtrace<W: Write>(chain: &LocationChain, bt: &Backtrace, w: &mut W) -> Result`, called by both `ErrorFields::fmt_chain_and_backtrace` (which keeps its `&mut Formatter` signature) and `format_traceback`. Zero public-API change; internal helper is `pub(crate)` to avoid adding items to the crate's surface.
- **(B) Generic method** — change `ErrorFields::fmt_chain_and_backtrace` itself to `<W: Write>(&self, w: &mut W)`. Two existing callers pass `&mut Formatter` and keep compiling. `format_traceback` calls `err.origin_fields()` — wait, the `Traceback` trait doesn't expose `ErrorFields`; it exposes `location_chain` and `origin_backtrace` separately, so we'd need an `&ErrorFields` accessor on the trait, which pulls `ErrorFields` into the trait surface and couples `dot::Error<E>` more tightly to it.

Assume **(A)** — cleanest split, no new public item, no coupling increase. The method keeps its existing signature.

**Q2 — R1 placement: caveat in `define_error!` doc, or also in top-level module doc?**

- **(A)** Only in `define_error!` doc. The macro is where users learn construction patterns; the caveat belongs there. **Default.**
- **(B)** Both places. Duplicates one paragraph; low cost. Not worth it — macro doc is the landing page anyone writing `.into()` will have open.

Assume **(A)**.

---

## File Structure (after execution, assuming defaults)

```
crates/strider-error/
├── src/
│   ├── lib.rs        # unchanged
│   ├── fields.rs     # + pub(crate) free fn write_chain_and_backtrace<W>
│   │                 # ErrorFields::fmt_chain_and_backtrace delegates to it
│   ├── define.rs     # + "Caveats" section in define_error! doc (R1)
│   └── format.rs     # format_traceback replaces its loop with
│                     # write_chain_and_backtrace call
└── tests/            # unchanged (no new tests this round;
                      # R1 is doc-only, S1 is a refactor pinned by existing tests)
```

Downstream: no changes. `dot::error::Error<E>::Debug` keeps calling `self.fields.fmt_chain_and_backtrace(f)`, whose signature is unchanged.

---

## Task 1: Extract shared chain+backtrace formatter (S1 / Q1)

**Files:**
- Modify: [crates/strider-error/src/fields.rs](crates/strider-error/src/fields.rs)
- Modify: [crates/strider-error/src/format.rs](crates/strider-error/src/format.rs)

- [ ] **Step 1: Add the shared helper to `fields.rs`**

Insert a new `pub(crate)` free function above the `impl ErrorFields` block (right after the `impl ErrorFields { ... }` or at end of file), and have the existing method delegate to it.

Replace the existing `impl ErrorFields { ... fmt_chain_and_backtrace ... }` method body at [crates/strider-error/src/fields.rs:60-80](crates/strider-error/src/fields.rs#L60-L80) with:

```rust
    /// Writes the location chain + backtrace into `f`, using the same
    /// format that `define_error!`-generated Debug impls (and `dot::Error<E>`)
    /// use. Callers must already have written the kind's own representation
    /// (typically one `writeln!(f, "{}", kind)` line before this call).
    ///
    /// # Errors
    ///
    /// Propagates any `fmt::Error` raised by the underlying formatter.
    pub fn fmt_chain_and_backtrace(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write_chain_and_backtrace(&self.locations, &self.backtrace, f)
    }
}

/// Shared implementation for writing a location chain followed by a
/// backtrace, used by [`ErrorFields::fmt_chain_and_backtrace`] (which
/// writes into a `std::fmt::Formatter`) and by
/// [`crate::format_traceback`] (which writes into a `String`).
///
/// Generic over `W: std::fmt::Write` so both sinks work with one body;
/// `Formatter<'_>` and `String` both implement the trait.
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

(Note: the closing `}` of `impl ErrorFields { ... }` stays where it is — the new free function comes *after* it. The `pub(crate)` means it's not part of the crate's public API; only `format.rs` needs to see it.)

- [ ] **Step 2: Build and test — nothing observable should change yet**

Run: `cargo test -p strider-error`
Expected: 14 tests pass + 3 doctests pass. The byte-for-byte output of Debug on wrappers is unchanged because the method delegates to the exact same logic.

- [ ] **Step 3: Replace `format_traceback`'s inline loop with a call to the helper**

In [crates/strider-error/src/format.rs](crates/strider-error/src/format.rs), replace the current body from the `for (i, loc) in ...` loop through the final `write!(out, "{}", err.origin_backtrace())`:

Current (lines 47-58):

```rust
    for (i, loc) in err.location_chain().iter().enumerate() {
        let _ = writeln!(
            out,
            "  at [{}] {}:{}:{}",
            i,
            loc.file(),
            loc.line(),
            loc.column(),
        );
    }

    let _ = write!(out, "{}", err.origin_backtrace());
```

Replace with:

```rust
    // Reuse the same chain+backtrace formatting that Debug impls use
    // (ErrorFields::fmt_chain_and_backtrace). Writing into a String is
    // infallible; the `let _` silences the fmt::Result.
    let _ = crate::fields::write_chain_and_backtrace(
        err.location_chain(),
        err.origin_backtrace(),
        &mut out,
    );
```

And update the imports at the top of `format.rs`. The file currently has:

```rust
use std::error::Error;
use std::fmt::Write;

use crate::Traceback;
```

Keep `use std::fmt::Write;` — it's still needed for the `writeln!(out, ...)` calls that survive (the `"error: ..."` line and the `"  caused by: ..."` walk). Add the helper import either by calling it through its module path (`crate::fields::write_chain_and_backtrace(...)` as shown) or by adding `use crate::fields::write_chain_and_backtrace;` at the top. Either works; pick the call-site-qualified form to keep the import block terse.

- [ ] **Step 4: Re-run all tests — output shape must be identical**

Run: `cargo test -p strider-error`
Expected: 14 tests pass + 3 doctests pass. The key regression targets:
- `tests/format.rs::format_traceback_prints_wrapper_display_exactly_once` — still passes because the body the helper writes is exactly the body the inline loop wrote before.
- `tests/format.rs::format_traceback_does_not_duplicate_multiline_display` — same.
- `tests/format.rs::format_traceback_walks_source_chain_top_to_bottom` — unrelated to the changed section, still passes.
- `tests/format.rs::format_traceback_includes_location_marker` — the `"  at [0] "` marker is produced by the helper, present as before.

Run: `cargo test -p dot --test error`
Expected: 6 tests pass. `dot::Error<E>::Debug` is unchanged.

- [ ] **Step 5: Commit**

```bash
git add crates/strider-error/src/fields.rs crates/strider-error/src/format.rs
git commit -m "$(cat <<'EOF'
refactor(strider-error): share chain+backtrace write loop between Debug and format_traceback

Both ErrorFields::fmt_chain_and_backtrace (Formatter sink) and
format_traceback (String sink) wrote identical code:
  - numbered "at [N] file:line:col" lines for each LocationChain entry
  - final "{}" on the origin backtrace

Extracts a pub(crate) free function generic over W: fmt::Write. The
method keeps its existing signature; format_traceback drops ~11 lines.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Document the `.into()` caveat in `define_error!` (R1 / Q2)

**Files:**
- Modify: [crates/strider-error/src/define.rs](crates/strider-error/src/define.rs)

- [ ] **Step 1: Add a "Caveats" section to the `define_error!` doc**

In [crates/strider-error/src/define.rs](crates/strider-error/src/define.rs), locate the doc comment for `define_error!` (currently ends just before `# Cross-crate bridges` around [define.rs:46-52](crates/strider-error/src/define.rs#L46-L52)). Insert a new `# Caveats` section between the `# Example` block and the `# Cross-crate bridges` block.

Insert immediately after the closing \`\`\` of the `# Example` section (the line `/// ```` that ends the example) and before the line `/// # Cross-crate bridges`:

```rust
///
/// # Caveats
///
/// **`.into()` loses the `#[track_caller]` chain.** The wrapper's
/// `From<$kind>` impl is `#[track_caller]`, so `?` and explicit
/// `Wrapper::from(kind)` both place the first entry of `err.locations()`
/// at the user's call site. The std blanket `Into::into` is *not*
/// `#[track_caller]`, though, so `ErrorKind::X.into()` resolves
/// `Location::caller()` to a line inside `core/src/convert/mod.rs`
/// rather than the user's code. The backtrace is unaffected (it's
/// captured unconditionally inside `ErrorFields::new`), but the
/// location chain's origin entry is misleading.
///
/// If you need the chain to point at your own site and don't have a
/// `?` context handy, use `Wrapper::from(kind)` explicitly:
///
/// ```ignore
/// let err = MyError::from(MyKind::Boom);   // ← caller site captured
/// // vs.
/// let err: MyError = MyKind::Boom.into();  // ← core::convert captured
/// ```
```

Do not change anything else in the macro body or in the `# Cross-crate bridges` section.

- [ ] **Step 2: Verify the doc-comment block still parses and the crate builds**

Run: `cargo check -p strider-error`
Expected: PASS. Rust parses the doc block as attributes on the macro; a malformed doc fails at parse time.

- [ ] **Step 3: Verify the existing doctests still pass (the new block is non-executable prose + one `ignore` fence)**

Run: `cargo test -p strider-error --doc`
Expected: 3 doctests pass (the new `ignore` fence is not executed).

- [ ] **Step 4: Commit**

```bash
git add crates/strider-error/src/define.rs
git commit -m "docs(strider-error): document that .into() loses #[track_caller] chain propagation"
```

---

## Task 3: Workspace sanity sweep

**Files:** Run-only, no edits.

- [ ] **Step 1: Full workspace build**

Run: `cargo build --workspace`
Expected: PASS.

- [ ] **Step 2: Full workspace tests**

Run: `cargo test --workspace`
Expected: PASS. Specifically:
- `cargo test -p strider-error` — 14 tests + 3 doctests.
- `cargo test -p dot --test error` — 6 tests.
- `cargo test -p analyzer --test error_chain` — unaffected.
- `cargo test -p reader --test error` — unaffected.

- [ ] **Step 3: Strict lint on the touched crates**

Run: `cargo clippy -p strider-error --all-targets --no-deps -- -D warnings`
Expected: clean.

Run: `cargo clippy -p dot --all-targets --no-deps -- -D warnings`
Expected: clean (no behavioral change in `dot`, but the helper it calls into now routes through a `pub(crate)` free fn — this is compile-only).

- [ ] **Step 4: Workspace lint**

Run: `cargo clippy --workspace -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Smoke-run the example**

Run: `cargo run --example analyzer`
Expected: `cfg.html`, `graph.html`, `graph-opt.html` produced — confirms no runtime regression via the error path.

---

## Out of Scope (considered, rejected or deferred)

Items already listed inline under **Out of scope for this round** above. Restated here for completeness:

- Privatize `ErrorFields.backtrace` / `ErrorFields.locations` fields (round 3 deferred; unchanged).
- Object-safe `fmt_chain_and_backtrace` on `Traceback` (requires either dropping the `&dyn` story or dispatching dynamically; not worth it for two callers).
- `LocationChain` → `SmallVec` micro-optimization.
- `Option<Box<Backtrace>>` to skip the alloc under disabled backtraces.
- Blank-line separators between source walk / locations / backtrace in `format_traceback` output.
- Second-arm `bridge_error!` that consumes `ir::ValidationErrors` directly.
- Adding a regression test for the `.into()` caveat (testing "not the user's file" is fragile and fires a false positive if stdlib ever marks `Into::into` as `#[track_caller]`).
