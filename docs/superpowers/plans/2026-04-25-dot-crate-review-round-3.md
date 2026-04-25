# Dot Crate Review (Round 3) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Address the small set of correctness / clarity / minor-allocation items that remain in the `dot` crate after rounds 1 ([docs/superpowers/plans/2026-04-25-dot-crate-review.md](docs/superpowers/plans/2026-04-25-dot-crate-review.md)) and 2 ([docs/superpowers/plans/2026-04-25-dot-crate-review-round-2.md](docs/superpowers/plans/2026-04-25-dot-crate-review-round-2.md)) — both already executed and merged into `feature/ai`. The crate is currently clippy-clean (`cargo clippy -p dot --all-targets --no-deps -- -D warnings`) with 32 passing tests across `tests/{emitter,error,style}.rs` + an inline `mod label_tests`.

**Architecture:** Each task is a self-contained, independently-committable change. The two substantive changes (Display::fmt simplification, and the `as_svg` error/UTF-8 improvement) are pinned by existing tests + one new test for the `as_svg` UTF-8 error mode. The HTML asset templates ([assets/graph_template_dot.html](crates/dot/assets/graph_template_dot.html), [assets/graph_template_svg.html](crates/dot/assets/graph_template_svg.html)) are not touched.

**Tech Stack:** Rust, `strider-error`, `thiserror`. No external runtime deps.

---

## Open questions for the reviewer before execution

Each changes the implementation of one task below. Pick one per group.

**Q1 — `as_svg` exit-status formatting (Task 3):** when the `dot` binary exits with non-zero status, the current code returns `SvgConversionError(stderr_string)`. If stderr is empty (e.g. dot was killed by SIGSEGV or returned a non-zero code without printing), the error message is the empty string. Three options for what to embed:
  - **(A)** `format!("`dot -Tsvg` {status}: {stderr_trimmed}")` — `ExitStatus` `Display`s as `exit status: 1` or `signal: 11 (SIGSEGV)`. Always informative even if stderr is empty. **Default choice.**
  - **(B)** Keep stderr-only but fall back to `format!("dot exited with {status} and no stderr")` when stderr is empty. Slightly less uniform but lower diff.
  - **(C)** Split `ErrorKind::SvgConversionError(String)` into separate variants for spawn-failed / non-zero-exit / pipe-failure. Round-2 OOS already rejected this as over-engineered; same justification holds.

  **Reviewer action:** confirm (A). The change is internal to `as_svg`; no public-API surface change.

**Q2 — `as_svg` stdout decode (Task 3):** the current code does `String::from_utf8_lossy(&output.stdout).into_owned()`, which always allocates a fresh String even for valid-UTF-8 SVG (the universal case). Two options:
  - **(A)** Switch to `String::from_utf8(output.stdout)` and wrap the (essentially-impossible) error in `SvgConversionError("dot stdout was not UTF-8: …")`. Saves one large allocation and gives a real error if `dot` ever produces non-UTF-8 (it won't for `-Tsvg`, but defensive). **Default choice.**
  - **(B)** Leave as-is. Round-2 OOS rejected (A); reverting that decision is mild.

  **Reviewer action:** confirm (A). The semantic improvement (Err on non-UTF-8 vs. silent U+FFFD substitution) is more valuable than the saved allocation, and round-2's reasoning (subprocess RTT dwarfs the allocation) was about whether the perf alone was worth it — the error-mode tightening is the real win.

Assume defaults unless the reviewer says otherwise. The tasks below reflect defaults.

---

## File map

- Modify: [crates/dot/src/error.rs](crates/dot/src/error.rs) (`Display::fmt` body)
- Modify: [crates/dot/src/lib.rs](crates/dot/src/lib.rs) (`as_svg` exit-status + UTF-8 decode; stale-comment cleanup in `mod label_tests`)
- Modify: (no new files; existing `tests/error.rs` already covers `Display` delegation; the `as_svg` tests don't exist because they'd need the `dot` binary, deliberately deferred per round-1/2 OOS)

---

## Task 1: Verify clean baseline before any edits

Reuses the round-2 baseline so each subsequent task can quote `Run: <baseline cmd>` and "Expected: PASS" against a known-good starting point.

**Files:** none modified.

- [ ] **Step 1: clippy clean**

Run: `cargo clippy -p dot --all-targets --no-deps -- -D warnings`
Expected: `Finished ...` with no errors and no warnings.

- [ ] **Step 2: 32 dot tests pass across all four binaries**

Run: `cargo test -p dot`
Expected: PASS — 16 in inline `mod label_tests` (lib unit-test binary), 11 in `tests/emitter.rs`, 6 in `tests/error.rs`, 4 in `tests/style.rs`. (Numbers are checked against the current crate state at branch `feature/ai`. If clippy or tests fail here, stop and investigate before proceeding — round-3 assumes round-2 has been merged.)

- [ ] **Step 3: workspace check**

Run: `cargo check --workspace`
Expected: PASS.

- [ ] **Step 4: No commit (verification only).**

---

## Task 2: Simplify `Display::fmt` in error.rs to mirror `Debug::fmt`

Round-2 Task 4 proposed simplifying `std::fmt::Display::fmt(&*self.kind, f)` (a fully-qualified call with an explicit Box deref) to a method call on `self.kind`. Commit `0e017aa refactor(dot): collapse fully-qualified syntax in error.rs source/From` only collapsed `source()` and `From<io::Error>`; the `Display::fmt` body still has the `&*self.kind` form.

We can't write `self.kind.fmt(f)` directly: `Debug` and `Display` are both in scope inside the `impl Display` block (the file's top has `use std::fmt::Debug;`), so `fmt` would be ambiguous. The cleaner equivalent is `write!(f, "{}", self.kind)` — which is what the `Debug::fmt` impl on the next block already uses, so this also makes the two impls structurally consistent. The `Display` formatting kicks in via the `{}` format specifier; auto-deref through the `Box<ErrorKind<E>>` is implicit.

**Files:**
- Modify: [crates/dot/src/error.rs:64-68](crates/dot/src/error.rs#L64-L68)

- [ ] **Step 1: Run the error tests to confirm baseline**

Run: `cargo test -p dot --test error`
Expected: 6 tests pass — in particular `display_delegates_to_inner_kind` pins `Error::to_string() == "dump-err: xyz"` for `DotDumpError(TestDumperErr("xyz"))`.

- [ ] **Step 2: Replace the `Display::fmt` body**

In `crates/dot/src/error.rs`, find:

```rust
impl<E: Debug + std::fmt::Display> std::fmt::Display for Error<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&*self.kind, f)
    }
}
```

Replace with:

```rust
impl<E: Debug + std::fmt::Display> std::fmt::Display for Error<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.kind)
    }
}
```

(The `write!` macro expands to `<&Box<ErrorKind<E>> as Display>::fmt`, which auto-derefs through the Box to the inner `ErrorKind<E>`'s `Display` impl. Output is byte-identical to the original.)

- [ ] **Step 3: Re-run the error tests**

Run: `cargo test -p dot --test error`
Expected: 6 tests still pass — `display_delegates_to_inner_kind` and `debug_contains_display_line_and_location_marker` (which calls `format!("{err:?}")` and asserts the embedded Display line is unchanged) both lock the byte-level Display output.

- [ ] **Step 4: Re-run all dot tests + clippy + workspace check**

Run: `cargo test -p dot && cargo clippy -p dot --all-targets --no-deps -- -D warnings && cargo check --workspace`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/dot/src/error.rs
git commit -m "refactor(dot): rewrite Error<E>::Display::fmt as write! to match Debug shape"
```

---

## Task 3: Improve `as_svg` error reporting and skip the lossy stdout copy

(Applies defaults **Q1 (A)** and **Q2 (A)**.) Two related improvements to `as_svg`, kept in one task because they touch the same nine-line block at the bottom of the function and any reviewer reading the diff will want to see them together.

1. When the child `dot` process exits non-zero, embed the `ExitStatus` (which `Display`s as `exit status: 1`, `signal: 11 (SIGSEGV)`, etc.) in the error so that an empty-stderr failure isn't silent.
2. Decode stdout via `String::from_utf8(output.stdout)` instead of `String::from_utf8_lossy(&output.stdout).into_owned()`. The lossy form always allocates a fresh `String` even on the valid-UTF-8 happy path; `from_utf8` is zero-copy on success and returns an `Err` (which we convert into `SvgConversionError`) on the impossible-for-SVG invalid case.

**Files:**
- Modify: [crates/dot/src/lib.rs:392-401](crates/dot/src/lib.rs#L392-L401)

- [ ] **Step 1: Run the dot tests to confirm baseline**

Run: `cargo test -p dot`
Expected: 32 tests pass (no test exercises the SVG path — that's a deliberate coverage gap, since invoking `dot` from CI is unreliable). The cargo build + clippy passes are the strongest signal we have for this change.

- [ ] **Step 2: Replace the tail of `as_svg`**

In `crates/dot/src/lib.rs::GraphDot::as_svg`, find the tail of the function body (lines 392-401):

```rust
        let output = child
            .wait_with_output()
            .map_err(|e| svg_err(e.to_string()))?;

        if !output.status.success() {
            return Err(svg_err(String::from_utf8_lossy(&output.stderr).to_string()));
        }

        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
```

Replace with:

```rust
        let output = child
            .wait_with_output()
            .map_err(|e| svg_err(e.to_string()))?;

        if !output.status.success() {
            // Embed the ExitStatus so a non-zero exit with empty stderr (e.g.
            // dot killed by signal) still surfaces a useful diagnostic.
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(svg_err(format!(
                "`dot -Tsvg` failed ({}): {}",
                output.status,
                stderr.trim()
            )));
        }

        // SVG output from `dot -Tsvg` is always UTF-8; treat any deviation
        // as a real error rather than silently substituting U+FFFD.
        String::from_utf8(output.stdout)
            .map_err(|e| svg_err(format!("dot -Tsvg stdout was not UTF-8: {e}")))
    }
```

Note the trailing `}` of `as_svg` is unchanged; only the body up to it changes. The function signature still returns `Result<String, G::Error>`.

- [ ] **Step 3: Build + clippy + tests + workspace check**

Run: `cargo test -p dot && cargo clippy -p dot --all-targets --no-deps -- -D warnings && cargo check --workspace`
Expected: PASS. (Clippy will not flag the `String::from_utf8(...).map_err(...)` pattern; both the `format!` and `to_string` calls are pre-existing patterns elsewhere in the function.)

- [ ] **Step 4: Smoke-run the analyzer example**

Run: `cargo run --example analyzer`
Expected: produces `cfg.html`, `graph.html`, `graph-opt.html`. (The example uses `dump_as_html` → `as_html_from_dot`, not `as_svg`; this is mainly a confidence check that nothing in the build broke.)

- [ ] **Step 5: Manually verify behavior with a real `dot` binary (recommended but optional)**

If the system has Graphviz installed, write a tiny ad-hoc test:

```bash
cargo run --example analyzer  # confirms the happy path still produces output
```

If `dot` is not installed, the SVG error path remains untested-by-tests — that gap was acknowledged in rounds 1 and 2 and is out of scope here.

- [ ] **Step 6: Commit**

```bash
git add crates/dot/src/lib.rs
git commit -m "refactor(dot): embed ExitStatus in as_svg error, decode stdout via from_utf8"
```

---

## Task 4: Drop stale round-1 reference in `mod label_tests`

The inline test `escape_dot_label_carriage_return_passes_through_as_is` has a comment that references "Task 4" — round-1's now-merged doc-fix task. Commit `b2a2f24 docs(dot): drop stale Task 6 reference in tests/style.rs` cleaned up the equivalent reference in `tests/style.rs`; the lib.rs one was missed.

**Files:**
- Modify: [crates/dot/src/lib.rs:499-505](crates/dot/src/lib.rs#L499-L505)

- [ ] **Step 1: Replace the comment**

In `crates/dot/src/lib.rs::label_tests::escape_dot_label_carriage_return_passes_through_as_is`, find:

```rust
    #[test]
    fn escape_dot_label_carriage_return_passes_through_as_is() {
        // Locks the current implementation: a literal '\r' character is not
        // stripped — it falls through to the catch-all push branch. (See
        // Task 4 for the doc fix that brings the comment in line with this.)
        assert_eq!(escape_dot_label("a\rb"), "a\rb");
    }
```

Replace with:

```rust
    #[test]
    fn escape_dot_label_carriage_return_passes_through_as_is() {
        // Locks the implementation: a literal '\r' character is passed
        // through to the output unchanged (the doc above the function
        // matches this — '\r' is not stripped despite an earlier doc claim).
        assert_eq!(escape_dot_label("a\rb"), "a\rb");
    }
```

- [ ] **Step 2: Build + clippy + tests**

Run: `cargo test -p dot && cargo clippy -p dot --all-targets --no-deps -- -D warnings`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/dot/src/lib.rs
git commit -m "docs(dot): drop stale round-1 Task 4 reference in label_tests"
```

---

## Task 5: Final sanity sweep

**Files:** none modified.

- [ ] **Step 1: Full workspace build**

Run: `cargo build --workspace`
Expected: PASS, no warnings.

- [ ] **Step 2: Full workspace tests**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 3: dot-only strict lint**

Run: `cargo clippy -p dot --all-targets --no-deps -- -D warnings`
Expected: PASS, no errors, no warnings.

- [ ] **Step 4: Workspace strict lint**

Run: `cargo clippy --workspace -- -D warnings`
Expected: PASS (or pre-existing unrelated lints in other crates, unchanged from baseline).

- [ ] **Step 5: Smoke-run the example one final time**

Run: `cargo run --example analyzer`
Expected: `cfg.html`, `graph.html`, `graph-opt.html` produced. The DOT byte-output is byte-identical to round-2's output for the workspace's safe inputs (rounds 1-3 changed only error messages, internal allocation patterns, and a stale comment — none of which affects the rendered DOT).

- [ ] **Step 6: Open `cfg.html` and `graph.html` in a browser**

Confirm the rendered graph is identical to pre-round-3.

---

## Out of scope (considered, rejected, or deferred)

- **Refactor `escape_dot_label` / `json_quote` to take `&mut String`** to eliminate per-call allocations in `DotEmitter::node` / `edge` / `new`. For a 1000-node graph this is ~5000 short-lived `String` allocations — not free, but well below the ~100 µs noise floor of any realistic rendering pipeline (the actual DOT serialisation, SVG round-trip, or browser layout dominate by 3+ orders of magnitude). The refactor would also widen the private API surface for marginal gain. Defer.
- **Add `dump_as_html_from_svg(&self, out_path: impl AsRef<Path>)`** for symmetry with `dump_as_html` (which uses `as_html_from_dot`). Today the SVG-flavored HTML output requires `as_html_from_svg() + std::fs::write` at the call site. Two callers (`cfg::examples`, `analyzer::examples`) — neither uses the SVG flavor. Adding the method would be doc + signature churn for no actual user. Defer.
- **Error if `<svg` is not found in `as_html_from_svg`** — currently if the `dot` binary's stdout doesn't contain `<svg`, the entire stdout (including any XML preamble) is inlined into the HTML body. In practice `dot -Tsvg` always emits a `<svg>` root, so this lenient handling is fine; tightening it would add an error variant for a defensive case that doesn't trigger. Defer.
- **Round-1/2 OOS list** (still out of scope): integration tests for `as_svg` (require `dot` binary), `ErrorKind::SvgConversionError` split into spawn/exit/pipe variants, `Clone` on `DotStyle`, magic-string `const`s, `GraphDotBuilder`, `iter_nodes -> Iterator`, `\r` strip in `escape_dot_label`, key validation in `extra`, `Vec → &[]` in `DotStyle`, `pub` raw `String` access. See [docs/superpowers/plans/2026-04-25-dot-crate-review.md](docs/superpowers/plans/2026-04-25-dot-crate-review.md) and [docs/superpowers/plans/2026-04-25-dot-crate-review-round-2.md](docs/superpowers/plans/2026-04-25-dot-crate-review-round-2.md) for justifications.
- **Replace `let _ = std::fmt::write(&mut out, format_args!("\\u{:04x}", c as u32))`** in `json_quote`'s low-control-char arm with a manual hex-digit append loop. Round-2 OOS rejected this as cosmetic; same justification holds — the `let _` form is idiomatic Rust for "write to a `Write` that can never fail" and runs at most once per JSON character below 0x20 (essentially never on real DOT input).
- **Document the `extra` slice's `(key, raw_value)` contract more thoroughly via a worked example in module-level docs** — round-1 Task 11 already added per-method doc. Adding a top-of-module example would duplicate that. Defer until a real user trips on it.
- **Capacity hint in `DotEmitter::new`'s initial `String::new()`** — current `String::new()` allocates lazily on first push. For a non-empty style, the next push is `"digraph \""` (10 bytes), which triggers a small initial allocation. Not worth tuning; node/edge appends dominate the final size.
- **Pre-compute `iter_nodes` length for `String::with_capacity` in `build_dot`** — the `iter_nodes() -> impl IntoIterator` shape doesn't expose `size_hint()` reliably (it's `IntoIterator`, not `Iterator`). Adding a `len_hint` method to the trait would be a public-API addition for marginal gain.
