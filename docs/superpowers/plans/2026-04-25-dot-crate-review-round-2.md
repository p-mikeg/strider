# Dot Crate Review (Round 2) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Address one real correctness bug, one minor correctness foot-gun, and a handful of readability/simplification wins in the `dot` crate that a fresh read uncovered after the round-1 review (`docs/superpowers/plans/2026-04-25-dot-crate-review.md` — already executed).

**Architecture:** Each task is a small, independently-committable change. Round-1 already pinned the public DOT output via tests in [tests/emitter.rs](crates/dot/tests/emitter.rs) and [tests/style.rs](crates/dot/tests/style.rs); this round adds tests for the new behaviors and reuses the existing baseline tests as regression coverage. The HTML asset templates ([assets/graph_template_dot.html](crates/dot/assets/graph_template_dot.html), [assets/graph_template_svg.html](crates/dot/assets/graph_template_svg.html)) are not touched.

**Tech Stack:** Rust, `strider-error`, `thiserror`. No external runtime deps.

---

## Open questions for the reviewer before execution

Each changes the implementation of one task below. Pick one per group.

**Q1 — `</script>` HTML-injection in `json_quote` (Task 2):** the round-1 plan added [`fn json_quote`](crates/dot/src/lib.rs#L171-L189) to embed the DOT source inside a `<script type="application/json" id="dot-src">__DOT_JSON__</script>` element ([assets/graph_template_dot.html:128](crates/dot/assets/graph_template_dot.html#L128)). The implementation escapes JSON-significant characters (`"`, `\`, `\n`, `\r`, `\t`, low control chars) but passes `<` through verbatim. Any DOT label that contains the literal substring `</script>` therefore breaks out of the script element and corrupts the rendered HTML. Realistic worst case: a binary whose disassembly produces a string literal like `</script>`; less realistic but still possible: an attacker-controlled binary fed to the analyzer. Three options:
  - **(A)** In `json_quote`, also escape `<` to `<` (HTML-safe JSON, the strategy used by every JSON-in-script library: serde-json's `to_string_pretty_html_safe`, V8's `JSON.stringify` HTML-safe mode, etc.). Universal fix; one extra arm in the `match`. **Default choice.**
  - **(B)** Escape only `</` (case-insensitive) so the rest of the JSON stays human-readable. Fragile (whitespace tolerance, end-tag boundary rules), more code.
  - **(C)** Switch the template from `<script type="application/json">` to a base64-encoded `data-dot` attribute on the body. Avoids the parser-mode question but adds a base64 step and a runtime decode in the template. Out of proportion for the gain.

  **Reviewer action:** confirm (A). The `json_quote` function is private and has only one call site (`as_html_from_dot`); the change is internal and pinned by a new round-trip test.

**Q2 — `DotEmitter::node` `id` and `DotEmitter::edge` `from`/`to` are unescaped (Task 3):** [lib.rs:226-243](crates/dot/src/lib.rs#L226-L243) and [lib.rs:249-270](crates/dot/src/lib.rs#L249-L270) push the caller-supplied id directly into `"<id>"`. Round-1 fixed the same hole for the digraph name. Today every internal caller hands in a derived id (`format!("n{}", i)`, sanitized varnode names, etc.), so this is a foot-gun, not an active bug. Three options:
  - **(A)** Escape ids with `escape_dot_label` exactly like the digraph name was in round-1. Symmetric, defensive, zero output change for current callers (their ids contain no `"` or `\`). **Default choice.**
  - **(B)** Document the contract (caller must supply ids that are valid Graphviz quoted-string contents) and leave behavior unchanged. Lighter, but punts the foot-gun to callers.
  - **(C)** Type-wrap ids in a newtype `DotId<'a>` whose constructor validates / escapes. Heaviest. Most callers already pass `&str` derived from `format!`; introducing a newtype churns more callers than the fix is worth.

  **Reviewer action:** confirm (A). Choose (B) if the cosmetic round-trip change to the test fixtures (which today don't include such characters anyway) is unwelcome.

**Q3 — `as_svg` lifecycle (Task 5):** [lib.rs:359-387](crates/dot/src/lib.rs#L359-L387) writes the DOT source to the child's stdin, then calls `wait_with_output()`. Today this works because `Child::wait_with_output` internally does `drop(self.stdin.take())` before reading stdout/stderr — but the call-site doesn't make that obvious, and a future refactor that switches from `wait_with_output` to a manual `wait()` + `read_to_end` would deadlock (child blocks on stdin EOF). Two options:
  - **(A)** Take and drop `child.stdin` explicitly before `wait_with_output()`. Self-documenting; no behavior change. **Default choice.**
  - **(B)** Leave as-is. Smaller diff; the comment cost is worth zero today, and the implicit drop is fine.

  **Reviewer action:** confirm (A). This task is the smallest in the plan; if (B) is preferred, drop the task entirely.

Assume defaults unless the reviewer says otherwise. The tasks below reflect defaults.

---

## File map

- Modify: [crates/dot/src/lib.rs](crates/dot/src/lib.rs) (json_quote, DotEmitter::node, DotEmitter::edge, as_svg)
- Modify: [crates/dot/src/error.rs](crates/dot/src/error.rs) (From<io::Error> simplification, Display/Debug deref simplification)
- Modify: [crates/dot/tests/emitter.rs](crates/dot/tests/emitter.rs) (id-escape regression tests)
- Modify: [crates/dot/src/lib.rs](crates/dot/src/lib.rs) — `mod label_tests` (json_quote `<` escape regression)
- Create (no — no new files): the existing test files cover everything; no new harness needed.

---

## Task 1: Verify clean baseline before any edits

Round-1 already brought clippy to clean and added 18 unit/integration tests. We restate that baseline once, here, so each subsequent task can quote `Run: <baseline cmd>` and "Expected: PASS" against a known-good starting point.

**Files:** none modified.

- [ ] **Step 1: clippy clean**

Run: `cargo clippy -p dot --all-targets --no-deps -- -D warnings`
Expected: `Finished ...` with no errors and no warnings.

- [ ] **Step 2: 18 tests pass**

Run: `cargo test -p dot`
Expected: PASS — `tests/emitter.rs` (8), `tests/error.rs` (6), `tests/style.rs` (4), and the inline `mod label_tests` (14 — these run inside the lib unit-test binary). Total ≥ 32 across all binaries.

- [ ] **Step 3: workspace check**

Run: `cargo check --workspace`
Expected: PASS.

- [ ] **Step 4: No commit (verification only).**

---

## Task 2: Fix the `</script>` HTML-injection in `json_quote`

(Applies default **Q1 (A)**.) Add an arm in `json_quote` that emits `<` as `<`. This makes the JSON payload safe inside `<script type="application/json">` regardless of label content.

**Files:**
- Modify: [crates/dot/src/lib.rs:171-189](crates/dot/src/lib.rs#L171-L189) (function body)
- Modify: [crates/dot/src/lib.rs:443-540](crates/dot/src/lib.rs#L443-L540) (`mod label_tests` — add the regression tests)

- [ ] **Step 1: Add a failing test for the new behavior**

In `crates/dot/src/lib.rs`, find the `mod label_tests` and append two tests at the end of the `// ── json_quote ──` section (just before the closing `}` of the `mod`):

```rust
    #[test]
    fn json_quote_escapes_left_angle_to_avoid_script_break_out() {
        // The JSON payload is embedded inside `<script type="application/json">`
        // in the HTML template. If a DOT label contained `</script>`, the HTML
        // parser would terminate the script tag and the rest of the JSON would
        // leak into the document body. Escape `<` to `<` to forbid that.
        assert_eq!(json_quote("</script>"), "\"\\u003c/script>\"");
    }

    #[test]
    fn json_quote_escapes_bare_left_angle_too() {
        // The escape is unconditional on `<` (not just `</`) — tagging only `</`
        // would force whitespace / case-tolerance reasoning into the encoder.
        assert_eq!(json_quote("a<b"), "\"a\\u003cb\"");
    }
```

- [ ] **Step 2: Run the new tests to verify they fail**

Run: `cargo test -p dot --lib label_tests::json_quote_escapes_left_angle_to_avoid_script_break_out label_tests::json_quote_escapes_bare_left_angle_too`
Expected: both FAIL — current code passes `<` through verbatim.

- [ ] **Step 3: Add the `<` arm to `json_quote`**

Replace the `match` body inside `json_quote` (lines 174-186):

```rust
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = std::fmt::write(&mut out, format_args!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
```

with:

```rust
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // Escape `<` to `<` so a label containing "</script>" can't
            // terminate the surrounding <script type="application/json"> tag
            // in `as_html_from_dot`'s output.
            '<' => out.push_str("\\u003c"),
            c if (c as u32) < 0x20 => {
                let _ = std::fmt::write(&mut out, format_args!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
```

- [ ] **Step 4: Update the doc on `json_quote`**

Replace the doc above `fn json_quote` (lines 167-170):

```rust
/// Wraps `s` in a JSON string literal with full escaping.
///
/// Used to safely embed the DOT source inside an HTML file without risk of
/// breaking JavaScript template literals or HTML structure.
fn json_quote(s: &str) -> String {
```

with:

```rust
/// Wraps `s` in a JSON string literal with full escaping.
///
/// Tailored for embedding the DOT source inside an HTML
/// `<script type="application/json">` element: in addition to the JSON
/// escapes (`"`, `\`, `\n`, `\r`, `\t`, low control chars as `\uXXXX`),
/// `<` is unconditionally emitted as `<` so a label containing
/// `</script>` cannot break out of the surrounding script tag.
fn json_quote(s: &str) -> String {
```

- [ ] **Step 5: Re-run the new tests + every other label test**

Run: `cargo test -p dot --lib label_tests`
Expected: 16 tests pass (14 pre-existing + 2 new).

- [ ] **Step 6: Re-run all dot tests + clippy + workspace check**

Run: `cargo test -p dot && cargo clippy -p dot --all-targets --no-deps -- -D warnings && cargo check --workspace`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/dot/src/lib.rs
git commit -m "fix(dot): escape '<' in json_quote so DOT labels can't break out of <script>"
```

---

## Task 3: Escape `DotEmitter::node` `id` and `DotEmitter::edge` `from`/`to`

(Applies default **Q2 (A)**.) Wrap each id in `escape_dot_label` exactly like the digraph name in round-1 Task 7. Existing callers all hand in ids that contain no `"` / `\` / newline, so the byte-level DOT output for current workspace callers is unchanged. The new tests pin the escape behavior on adversarial inputs.

**Files:**
- Modify: [crates/dot/src/lib.rs:226-243](crates/dot/src/lib.rs#L226-L243) (`DotEmitter::node`)
- Modify: [crates/dot/src/lib.rs:249-270](crates/dot/src/lib.rs#L249-L270) (`DotEmitter::edge`)
- Modify: [crates/dot/tests/emitter.rs](crates/dot/tests/emitter.rs) (add regression tests)

- [ ] **Step 1: Add failing tests for the new behavior**

Append to `crates/dot/tests/emitter.rs`:

```rust
#[test]
fn node_id_with_special_chars_is_escaped() {
    let style = DotStyle::empty();
    let mut e = DotEmitter::new("G", &style);
    // An id containing internal double-quote and backslash must escape both
    // so the surrounding "..." in the DOT output stays well-formed.
    e.node("a\"b\\c", "lbl", "box", &[]);
    let out = e.finish();
    assert!(
        out.contains("\"a\\\"b\\\\c\" [label=\"lbl\", shape=box];\n"),
        "unexpected DOT: {out}"
    );
}

#[test]
fn edge_endpoints_with_special_chars_are_escaped() {
    let style = DotStyle::empty();
    let mut e = DotEmitter::new("G", &style);
    e.edge("a\"x", "b\\y", &[]);
    let out = e.finish();
    assert!(
        out.contains("\"a\\\"x\" -> \"b\\\\y\";\n"),
        "unexpected DOT: {out}"
    );
}
```

- [ ] **Step 2: Run the new tests to verify they fail**

Run: `cargo test -p dot --test emitter node_id_with_special_chars_is_escaped edge_endpoints_with_special_chars_are_escaped`
Expected: both FAIL — current code emits the unescaped quote / backslash.

- [ ] **Step 3: Escape `id` in `DotEmitter::node`**

Replace the body of `DotEmitter::node` (lines 226-243):

```rust
    pub fn node(&mut self, id: &str, label: &str, shape: &str, extra: &[(&str, &str)]) {
        let label = escape_dot_label(label);
        self.out.push_str("  \"");
        self.out.push_str(id);
        self.out.push_str("\" [label=\"");
        self.out.push_str(&label);
        self.out.push_str("\", shape=");
        self.out.push_str(shape);

        for (k, v) in extra {
            self.out.push_str(", ");
            self.out.push_str(k);
            self.out.push('=');
            self.out.push_str(v);
        }

        self.out.push_str("];\n");
    }
```

with:

```rust
    pub fn node(&mut self, id: &str, label: &str, shape: &str, extra: &[(&str, &str)]) {
        let id = escape_dot_label(id);
        let label = escape_dot_label(label);
        self.out.push_str("  \"");
        self.out.push_str(&id);
        self.out.push_str("\" [label=\"");
        self.out.push_str(&label);
        self.out.push_str("\", shape=");
        self.out.push_str(shape);

        for (k, v) in extra {
            self.out.push_str(", ");
            self.out.push_str(k);
            self.out.push('=');
            self.out.push_str(v);
        }

        self.out.push_str("];\n");
    }
```

- [ ] **Step 4: Escape `from` and `to` in `DotEmitter::edge`**

Replace the body of `DotEmitter::edge` (lines 249-270):

```rust
    pub fn edge(&mut self, from: &str, to: &str, extra: &[(&str, &str)]) {
        self.out.push_str("  \"");
        self.out.push_str(from);
        self.out.push_str("\" -> \"");
        self.out.push_str(to);
        self.out.push('"');

        if !extra.is_empty() {
            self.out.push_str(" [");
            for (i, (k, v)) in extra.iter().enumerate() {
                if i != 0 {
                    self.out.push_str(", ");
                }
                self.out.push_str(k);
                self.out.push('=');
                self.out.push_str(v);
            }
            self.out.push(']');
        }

        self.out.push_str(";\n");
    }
```

with:

```rust
    pub fn edge(&mut self, from: &str, to: &str, extra: &[(&str, &str)]) {
        let from = escape_dot_label(from);
        let to = escape_dot_label(to);
        self.out.push_str("  \"");
        self.out.push_str(&from);
        self.out.push_str("\" -> \"");
        self.out.push_str(&to);
        self.out.push('"');

        if !extra.is_empty() {
            self.out.push_str(" [");
            for (i, (k, v)) in extra.iter().enumerate() {
                if i != 0 {
                    self.out.push_str(", ");
                }
                self.out.push_str(k);
                self.out.push('=');
                self.out.push_str(v);
            }
            self.out.push(']');
        }

        self.out.push_str(";\n");
    }
```

- [ ] **Step 5: Update the doc above `DotEmitter::node`**

Replace the doc block above `pub fn node` (lines 219-225):

```rust
    /// Emits a node statement. The `label` is escaped via `escape_dot_label`
    /// before being wrapped in DOT double-quotes.
    ///
    /// `extra` attributes are inserted verbatim as `key=value` pairs — the
    /// caller is responsible for any quoting or escaping of the value
    /// (e.g. `("fillcolor", "\"#3a2a10\"")` for a hex colour, or
    /// `("style", "dashed")` for a bare identifier).
```

with:

```rust
    /// Emits a node statement. Both `id` and `label` are escaped via
    /// `escape_dot_label` before being wrapped in DOT double-quotes, so any
    /// caller-supplied id with `"` / `\` / newline produces valid DOT.
    ///
    /// `extra` attributes are inserted verbatim as `key=value` pairs — the
    /// caller is responsible for any quoting or escaping of the value
    /// (e.g. `("fillcolor", "\"#3a2a10\"")` for a hex colour, or
    /// `("style", "dashed")` for a bare identifier).
```

- [ ] **Step 6: Update the doc above `DotEmitter::edge`**

Replace the doc block above `pub fn edge` (lines 245-248):

```rust
    /// Emits a directed edge statement.
    ///
    /// `extra` attributes follow the same caller-quotes-the-value contract
    /// as [`DotEmitter::node`] — they are inserted verbatim as `key=value`.
```

with:

```rust
    /// Emits a directed edge statement. Both endpoints (`from`, `to`) are
    /// escaped via `escape_dot_label` for the same reason as
    /// [`DotEmitter::node`].
    ///
    /// `extra` attributes follow the same caller-quotes-the-value contract
    /// as [`DotEmitter::node`] — they are inserted verbatim as `key=value`.
```

- [ ] **Step 7: Re-run the emitter tests**

Run: `cargo test -p dot --test emitter`
Expected: 10 tests pass (8 pre-existing + 2 new). The `node_emits_quoted_id_and_escaped_label` test from round-1 keeps passing because `n1` survives `escape_dot_label` unchanged.

- [ ] **Step 8: Re-run all dot tests + clippy + workspace check**

Run: `cargo test -p dot && cargo clippy -p dot --all-targets --no-deps -- -D warnings && cargo check --workspace`
Expected: PASS.

- [ ] **Step 9: Re-run downstream callers' integration tests**

Run: `cargo test -p cfg --test dot_dumper && cargo test -p ir --test dot`
Expected: PASS — none of the cfg/ir dumpers feed special-cased ids today; their fixtures (numeric ids, derived names) survive `escape_dot_label` byte-identically.

If either fails, look at the specific assertion: it should be a literal `"id" [` substring whose `id` value is unchanged by escaping. If the assertion changed because the fixture *did* include a `"` / `\` (extremely unlikely given the fixtures), update the fixture to expect the escaped form rather than reverting the escape.

- [ ] **Step 10: Commit**

```bash
git add crates/dot/src/lib.rs crates/dot/tests/emitter.rs
git commit -m "fix(dot): escape DotEmitter::node id and DotEmitter::edge endpoints"
```

---

## Task 4: Simplify `error.rs` — collapse the `From<io::Error>` qualifier soup and the `&*self.kind` derefs

[error.rs:102-107](crates/dot/src/error.rs#L102-L107) reads:

```rust
impl<E: Debug> From<std::io::Error> for Error<E> {
    #[track_caller]
    fn from(e: std::io::Error) -> Self {
        <Error<E> as From<ErrorKind<E>>>::from(<ErrorKind<E> as From<std::io::Error>>::from(e))
    }
}
```

The fully-qualified syntax is unnecessary: in this `impl` block `Self = Error<E>` and `ErrorKind` resolves to `ErrorKind<E>` directly. Plus the body of `Display::fmt` and `Debug::fmt` use `&*self.kind` to forcibly deref through the `Box`, but `Box<T>` already auto-derefs to `&T` via method resolution.

**Files:**
- Modify: [crates/dot/src/error.rs:64-75](crates/dot/src/error.rs#L64-L75) (`Display::fmt` and `Debug::fmt`)
- Modify: [crates/dot/src/error.rs:102-107](crates/dot/src/error.rs#L102-L107) (`From<io::Error>`)

- [ ] **Step 1: Run the error tests to confirm baseline**

Run: `cargo test -p dot --test error`
Expected: 6 tests pass.

- [ ] **Step 2: Simplify `Display::fmt` and `Debug::fmt`**

Replace lines 64-75:

```rust
impl<E: Debug + std::fmt::Display> std::fmt::Display for Error<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&*self.kind, f)
    }
}

impl<E: Debug + std::fmt::Display> std::fmt::Debug for Error<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "{}", self.kind)?;
        self.fields.fmt_chain_and_backtrace(f)
    }
}
```

with:

```rust
impl<E: Debug + std::fmt::Display> std::fmt::Display for Error<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.kind.fmt(f)
    }
}

impl<E: Debug + std::fmt::Display> std::fmt::Debug for Error<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "{}", self.kind)?;
        self.fields.fmt_chain_and_backtrace(f)
    }
}
```

(Only the `Display::fmt` body changed; `Debug::fmt` already uses the `Display` impl via `writeln!("{}")` so no edit needed there. Restated in full so the diff is self-contained. `self.kind.fmt(f)` resolves to `<ErrorKind<E> as Display>::fmt(&*self.kind, f)` via auto-deref.)

`std::error::Error::source` at [error.rs:78-80](crates/dot/src/error.rs#L78-L80) similarly does `std::error::Error::source(&*self.kind)`. Replace:

```rust
impl<E: Debug + std::error::Error + 'static> std::error::Error for Error<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        std::error::Error::source(&*self.kind)
    }
}
```

with:

```rust
impl<E: Debug + std::error::Error + 'static> std::error::Error for Error<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.kind.source()
    }
}
```

- [ ] **Step 3: Simplify `From<io::Error>`**

Replace lines 102-107:

```rust
impl<E: Debug> From<std::io::Error> for Error<E> {
    #[track_caller]
    fn from(e: std::io::Error) -> Self {
        <Error<E> as From<ErrorKind<E>>>::from(<ErrorKind<E> as From<std::io::Error>>::from(e))
    }
}
```

with:

```rust
impl<E: Debug> From<std::io::Error> for Error<E> {
    #[track_caller]
    fn from(e: std::io::Error) -> Self {
        Self::from(ErrorKind::from(e))
    }
}
```

(`ErrorKind::from` resolves to `<ErrorKind<E> as From<std::io::Error>>::from` because `E` is the only type parameter in scope; `Self::from` resolves to `<Error<E> as From<ErrorKind<E>>>::from` because `From<ErrorKind<E>>` is the only `From` impl on `Error<E>` whose argument type matches `ErrorKind::from(e)`.)

- [ ] **Step 4: Re-run the error tests**

Run: `cargo test -p dot --test error`
Expected: 6 tests pass — `from_io_error_seeds_single_location` and `error_source_delegates_to_inner_kind` in particular pin the `From<io::Error>` chain length and the `source()` delegation.

- [ ] **Step 5: Re-run all dot tests + clippy + workspace check**

Run: `cargo test -p dot && cargo clippy -p dot --all-targets --no-deps -- -D warnings && cargo check --workspace`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/dot/src/error.rs
git commit -m "refactor(dot): collapse fully-qualified syntax and Box derefs in error.rs"
```

---

## Task 5: Self-document `as_svg`'s stdin lifecycle

(Applies default **Q3 (A)**.) Take and drop `child.stdin` explicitly after writing, before `wait_with_output()`. `wait_with_output` does this internally today, but making it explicit means a future refactor that swaps to a manual `wait()` + read pair won't deadlock on stdin EOF.

**Files:**
- Modify: [crates/dot/src/lib.rs:359-387](crates/dot/src/lib.rs#L359-L387) (`as_svg`)

- [ ] **Step 1: Replace the `as_svg` body**

In `crates/dot/src/lib.rs::GraphDot::as_svg`, replace lines 367-380:

```rust
        {
            let stdin = child
                .stdin
                .as_mut()
                .ok_or_else(|| svg_err("failed to open dot stdin".to_owned()))?;

            stdin
                .write_all(dot_src.as_bytes())
                .map_err(|e| svg_err(e.to_string()))?;
        }

        let output = child
            .wait_with_output()
            .map_err(|e| svg_err(e.to_string()))?;
```

with:

```rust
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| svg_err("failed to open dot stdin".to_owned()))?;
        stdin
            .write_all(dot_src.as_bytes())
            .map_err(|e| svg_err(e.to_string()))?;
        // Closing stdin signals EOF to `dot` so it produces SVG and exits.
        // `wait_with_output` would also drop it on our behalf, but doing it
        // here makes the lifecycle obvious at the call site.
        drop(stdin);

        let output = child
            .wait_with_output()
            .map_err(|e| svg_err(e.to_string()))?;
```

- [ ] **Step 2: Build + clippy + tests + workspace check**

Run: `cargo test -p dot && cargo clippy -p dot --all-targets --no-deps -- -D warnings && cargo check --workspace`
Expected: PASS. (No test exercises the SVG path because it requires the system `dot` binary; that's a deliberate coverage gap left from round-1.)

- [ ] **Step 3: Smoke-run the analyzer example**

Run: `cargo run --example analyzer`
Expected: produces `cfg.html`, `graph.html`, `graph-opt.html` exactly as before. (The example uses `dump_as_html` which goes through `as_html_from_dot`, not `as_svg`, so this is mainly a confidence check; the real exercise of `as_svg` would be a manual `cargo run` that invokes a feature using `as_html_from_svg`. None exists in-tree, so the cargo build is the strongest signal we have.)

- [ ] **Step 4: Commit**

```bash
git add crates/dot/src/lib.rs
git commit -m "refactor(dot): take and drop dot's stdin explicitly in as_svg"
```

---

## Task 6: Final sanity sweep

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
Expected: `cfg.html`, `graph.html`, `graph-opt.html` produced.

- [ ] **Step 6: Open `cfg.html` and `graph.html` in a browser**

Confirm the rendered graph is identical to pre-refactor — same nodes, same edges, same layout. The DOT byte-output for the workspace's safe ids is unchanged by Tasks 2/3, so the rendering must match.

---

## Out of scope (considered, rejected, or deferred)

- **Round-1's out-of-scope list (still out of scope):** `as_svg` integration test (needs `dot` binary), `ErrorKind::SvgConversionError` split, `Clone` on `DotStyle`, magic-string `const`s, `GraphDotBuilder`, `iter_nodes -> Iterator`, `\r` strip, key validation in `extra`, `Vec → &[]` in `DotStyle`, `pub` raw `String` access. See [docs/superpowers/plans/2026-04-25-dot-crate-review.md](docs/superpowers/plans/2026-04-25-dot-crate-review.md#out-of-scope) for justifications.
- **Replacing `let _ = std::fmt::write(&mut out, format_args!("\\u{:04x}", c as u32))` with manual hex digit append** in `json_quote`'s low-control-char arm — the `let _` is awkward but the `std::fmt::write` form is idiomatic Rust for "write to a `Write` that can never fail." Manual hex would be three extra lines for a hot-path that runs at most once per JSON character below 0x20 (i.e. essentially never on real DOT input). Net loss.
- **Switching `as_svg` from `String::from_utf8_lossy(&output.stdout).into_owned()` to `String::from_utf8(output.stdout)`** — would save one allocation in the happy path but adds an `.unwrap_or_else(...)` for the (impossible-with-`dot`) non-UTF-8 case. The lossy path is robust against future `dot` versions emitting unexpected output and the allocation cost is dwarfed by the subprocess RTT. Defer.
- **Validating SVG output starts with `<svg` rather than scanning for `<svg`** — the round-1 refactor used `find("<svg")`, which is fine for actual `dot` output. Tightening to a startswith check would break compatibility if `dot` ever leads with whitespace. No-op risk for no win.
- **Tests for `as_html_from_dot`** — would require either capturing the full HTML and asserting on substrings, or testing only that the JSON-encoded DOT round-trips. The Task 2 unit tests on `json_quote` already give the strongest guarantee (per-character escape table is exhaustive); a separate integration test would just re-test `String::replace`.
- **Splitting `escape_dot_label` and `json_quote` into a sub-module** — they're currently file-private in `lib.rs` and tested by `mod label_tests` in the same file. Moving them out adds a `pub(crate)` and an extra file for ~80 lines of code. Not worth it.
- **`DotEmitter` taking ownership of the `String` so callers can pass a pre-allocated buffer** — micro-opt; nobody calls `DotEmitter::new` in a hot loop. The `Self::new` allocation is amortized across thousands of nodes / edges.
