# Dot Crate Review — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Get the `dot` crate to a clean baseline — make `cargo clippy -p dot --all-targets --no-deps -- -D warnings` pass, fix three small correctness/doc bugs (`escape_dot_label` `\r` doc-vs-code disagreement, `dark_cfg` dead margin block, unquoted `digraph {name}` interpolation), simplify a handful of awkward patterns (peeked-then-`if let` arm in `escape_dot_label`, `push_str(&format!(...))` chains, `to_owned()` of an SVG slice, redundant `+ Sized` bound, `&str` paths), and add unit tests for the three private helpers and the public `DotEmitter` round-trip — all without changing observable DOT output for the four existing callers (`cfg::cfg::dot::CfgDotDumper`, `ir::dot::render::GraphDotDumper`, `analyzer::examples::analyzer`, `cfg::examples::cfg_creator`).

**Architecture:** Each task is a self-contained, independently-committable change. The `dot` crate's public surface (`GraphDotDumper`, `DotStyle`, `DotEmitter`, `GraphDot`, `Error`/`ErrorKind`/`Result`) is consumed only via field-style and method-style calls listed in [crates/cfg/src/cfg/dot.rs](crates/cfg/src/cfg/dot.rs), [crates/ir/src/dot/render.rs](crates/ir/src/dot/render.rs), [crates/cfg/tests/dot_dumper.rs](crates/cfg/tests/dot_dumper.rs), [crates/ir/src/dot/tests.rs](crates/ir/src/dot/tests.rs), and the two example binaries — none rely on `out_path`'s `&str`-ness, on `DotStyle::dark_cfg`'s margin entry order, on the unquoted digraph name, or on the `push_str(&format!(...))` shape internally. The HTML asset templates ([assets/graph_template_dot.html](crates/dot/assets/graph_template_dot.html), [assets/graph_template_svg.html](crates/dot/assets/graph_template_svg.html)) are not touched.

**Tech Stack:** Rust, `strider-error`, `thiserror`. No external runtime deps.

---

## Open questions for the reviewer before execution

Each changes the implementation of one task below. Pick one per group.

**Q1 — `escape_dot_label` `\r` doc-vs-code disagreement (Task 4):** the function's doc comment claims "carriage-return stripped", but the code falls through to the catch-all `c => out.push(c)` arm, so `\r` is passed through verbatim into the DOT label. Three options:
  - **(A)** Fix the doc to match the code (CR is preserved). Lock current behavior with tests. **Default choice.** No DOT output changes for any caller.
  - **(B)** Fix the code to match the doc (strip CR). Stronger sanitisation but visibly changes any label containing `\r` — none of the existing dumpers emit one, so the practical effect is zero, but technically it's a behavior change.
  - **(C)** Document both behaviors and add a `strip_cr: bool` parameter / variant. Over-engineered for the current scope.

  **Reviewer action:** confirm (A). If the project has a stronger preference for matching the documented intent over preserving silent fall-through, pick (B).

**Q2 — `dark_cfg` margin block (Task 6):** [lib.rs:113-117](crates/dot/src/lib.rs#L113-L117) does
```rust
if let Some(e) = s.node.iter_mut().find(|(k, _)| *k == "margin") {
    e.1 = "0.2";
}
```
but `dark()` already sets `margin = "0.2"` ([lib.rs:94](crates/dot/src/lib.rs#L94)), so this branch is a no-op. Two options:
  - **(A)** Delete the dead block; correct the `dark_cfg` doc comment so it only mentions the fontname swap. **Default choice.**
  - **(B)** Change the value (e.g. `"0.3"`) so the comment is no longer a lie. Visibly changes node padding in CFG renders.

  **Reviewer action:** confirm (A). Choose (B) only if the user has already noticed CFG nodes are too cramped.

**Q3 — `digraph {name}` quoting (Task 7):** [lib.rs:198](crates/dot/src/lib.rs#L198) does `format!("digraph {name} {{\n")` with `name` interpolated raw. All workspace callers default to `"G"`, so this is fine in practice; but `with_name` accepts `impl Into<String>` and a name like `"my graph"` produces malformed DOT. Three options:
  - **(A)** Always wrap `name` in double quotes (`digraph "G" {`) and escape internal `"` and `\` via the existing `escape_dot_label` helper. Graphviz parses both quoted and bare identifiers identically, so existing `dot.html` output is semantically unchanged. **Default choice.**
  - **(B)** Document the contract (`name` must be a Graphviz identifier matching `[A-Za-z_][A-Za-z0-9_]*`); leave behavior unchanged. Lighter, but punts the foot-gun to callers.
  - **(C)** Change `with_name` to return `Result` and validate. Heavy and breaks the fluent builder shape.

  **Reviewer action:** confirm (A). Choose (B) if the doc-only fix is preferred and visible DOT-output changes (now-quoted name) are unwelcome.

**Q4 — `dump_as_html` / `dump_as_dot` path type (Task 10):** both currently take `out_path: &str`, but Rust convention is `impl AsRef<Path>` so callers can pass `&Path` / `PathBuf` / `&str`. The two analyzer-example callers pass string literals (`"cfg.html"`, `"graph.html"`, `"graph-opt.html"`), all of which keep working with either signature.
  - **(A)** Switch to `impl AsRef<std::path::Path>`. Idiomatic. **Default choice.**
  - **(B)** Leave as-is. Avoids a cosmetic change to the API.

  **Reviewer action:** confirm (A) if the cosmetic API improvement is welcome; otherwise (B).

Assume defaults unless the reviewer says otherwise. The tasks below reflect defaults.

---

## Task 1: Make `cargo clippy -p dot --all-targets --no-deps -- -D warnings` pass

`cargo clippy -p dot --all-targets --no-deps -- -D warnings` currently fails with 16 errors (9 × `must_use_candidate`, 7 × `missing_errors_doc`). All 16 are in `crates/dot/src/lib.rs` and `crates/dot/src/error.rs`. The library has no behavior to change — these are pure attribute / doc additions. We do this task **first** so every later task can verify itself with the same strict-clippy command.

**Files:**
- Modify: [crates/dot/src/error.rs:33-58](crates/dot/src/error.rs#L33-L58)
- Modify: [crates/dot/src/lib.rs:50-376](crates/dot/src/lib.rs#L50-L376)

- [ ] **Step 1: Add `#[must_use]` to the four `Error<E>` accessor methods**

In `crates/dot/src/error.rs`, prepend `#[must_use]` to each of these method signatures (the four already flagged by clippy):

- `pub fn kind(&self) -> &ErrorKind<E>` ([error.rs:35](crates/dot/src/error.rs#L35))
- `pub fn into_kind(self) -> ErrorKind<E>` ([error.rs:40](crates/dot/src/error.rs#L40))
- `pub fn decompose(self) -> (Box<ErrorKind<E>>, ErrorFields)` ([error.rs:45](crates/dot/src/error.rs#L45))
- `pub fn locations(&self) -> &LocationChain` ([error.rs:50](crates/dot/src/error.rs#L50))

`backtrace` ([error.rs:55](crates/dot/src/error.rs#L55)) is **not** flagged by clippy (a `&Backtrace` accessor doesn't trip the lint), so leave it alone — adding `#[must_use]` would be churn without a corresponding error.

Result for each method (illustrative for `kind`):

```rust
/// Returns a reference to the underlying `ErrorKind`.
#[must_use]
pub fn kind(&self) -> &ErrorKind<E> {
    &self.kind
}
```

- [ ] **Step 2: Add `#[must_use]` to the five `DotStyle` / `DotEmitter` constructors / `finish`**

In `crates/dot/src/lib.rs`, prepend `#[must_use]` to:

- `pub fn dark() -> Self` ([lib.rs:80](crates/dot/src/lib.rs#L80))
- `pub fn dark_cfg() -> Self` ([lib.rs:106](crates/dot/src/lib.rs#L106))
- `pub fn empty() -> Self` ([lib.rs:121](crates/dot/src/lib.rs#L121))
- `pub fn new(name: &str, style: &DotStyle) -> Self` ([lib.rs:196](crates/dot/src/lib.rs#L196))
- `pub fn finish(mut self) -> String` ([lib.rs:239](crates/dot/src/lib.rs#L239))

- [ ] **Step 3: Add `# Errors` doc sections to the trait method and the six `GraphDot` `Result`-returning methods**

These are the seven sites flagged by clippy's `missing_errors_doc`. Add a `# Errors` section to each.

For [`GraphDotDumper::dump_as_dot` (lib.rs:62-67)](crates/dot/src/lib.rs#L62-L67):

```rust
/// Emits DOT statements (nodes + edges) for a single graph node.
///
/// # Errors
/// Returns the dumper's own error type (`Self::Error`) if the dumper
/// cannot produce DOT for `node` — for example, if a referenced subnode
/// is missing or the dumper's data source returns an I/O error.
fn dump_as_dot(
    &self,
    node: Self::Node,
    out: &mut DotEmitter,
    state: &mut Self::State,
) -> core::result::Result<(), Self::Error>;
```

For [`GraphDot::as_dot` (lib.rs:294-297)](crates/dot/src/lib.rs#L294-L297):

```rust
/// Returns the raw DOT source string.
///
/// # Errors
/// Forwards any `Self::Error` returned by the underlying
/// [`GraphDotDumper::dump_as_dot`] for any node.
pub fn as_dot(&self) -> Result<String, G::Error> {
    self.build_dot()
}
```

For [`GraphDot::as_svg` (lib.rs:299-336)](crates/dot/src/lib.rs#L299-L336): keep the existing two-paragraph doc, then append:

```
/// # Errors
/// - [`ErrorKind::DotDumpError`] propagated from the dumper.
/// - [`ErrorKind::SvgConversionError`] if the system `dot` binary cannot
///   be spawned, returns a non-zero exit status, or its stdin/stdout
///   pipes cannot be opened.
```

For [`GraphDot::as_html_from_svg` (lib.rs:338-350)](crates/dot/src/lib.rs#L338-L350): append after the existing doc:

```
/// # Errors
/// Same as [`as_svg`].
```

For [`GraphDot::as_html_from_dot` (lib.rs:352-361)](crates/dot/src/lib.rs#L352-L361):

```
/// # Errors
/// Same as [`as_dot`].
```

For [`GraphDot::dump_as_html` (lib.rs:363-369)](crates/dot/src/lib.rs#L363-L369):

```
/// # Errors
/// - [`ErrorKind::DotDumpError`] propagated from the dumper.
/// - [`ErrorKind::IoError`] if writing `out_path` fails.
```

For [`GraphDot::dump_as_dot` (lib.rs:371-375)](crates/dot/src/lib.rs#L371-L375):

```
/// # Errors
/// Same as [`dump_as_html`].
```

- [ ] **Step 4: Verify clippy is clean on the dot crate**

Run: `cargo clippy -p dot --all-targets --no-deps -- -D warnings`
Expected: `Finished ...` with no errors and no warnings.

- [ ] **Step 5: Verify nothing else in the workspace regressed**

Run: `cargo check --workspace`
Expected: PASS.

- [ ] **Step 6: Verify existing tests still pass**

Run: `cargo test -p dot`
Expected: PASS — 6 tests in `tests/error.rs`.

- [ ] **Step 7: Commit**

```bash
git add crates/dot/src
git commit -m "docs(dot): add #[must_use] and # Errors sections to satisfy -D warnings clippy"
```

---

## Task 2: Add behavior-pinning tests for `escape_dot_label` and `json_quote`

Both functions are private (file-local in `lib.rs`), so they need a `#[cfg(test)] mod tests` inside `lib.rs` itself — `tests/error.rs` is integration-style and only sees the public API. These tests pin the **current** behavior so the doc-vs-code fix in Task 4 and the simplification in Task 5 don't accidentally regress anything.

**Files:**
- Modify: [crates/dot/src/lib.rs](crates/dot/src/lib.rs) (append a `#[cfg(test)] mod label_tests` after `escape_dot_label`/`json_quote`, before `// ── DotEmitter ──`).

- [ ] **Step 1: Write the tests**

Append to `crates/dot/src/lib.rs`, after the `json_quote` function (before the `// ── DotEmitter ──` comment block):

```rust
#[cfg(test)]
mod label_tests {
    use super::{escape_dot_label, json_quote};

    // ── escape_dot_label ────────────────────────────────────────────────────

    #[test]
    fn escape_dot_label_passes_through_plain_ascii() {
        assert_eq!(escape_dot_label("hello world"), "hello world");
    }

    #[test]
    fn escape_dot_label_empty_input_yields_empty_output() {
        assert_eq!(escape_dot_label(""), "");
    }

    #[test]
    fn escape_dot_label_double_quote_becomes_backslash_quote() {
        assert_eq!(escape_dot_label("a\"b"), "a\\\"b");
    }

    #[test]
    fn escape_dot_label_literal_newline_becomes_backslash_n() {
        // A real \n char in the input is rendered as the DOT centre-justify escape.
        assert_eq!(escape_dot_label("a\nb"), "a\\nb");
    }

    #[test]
    fn escape_dot_label_recognised_dot_escapes_pass_through() {
        // The two-char sequences \n, \l, \r in the input are DOT escape codes
        // (centre / left / right justified line break) and must survive
        // unchanged so callers can hand-emit DOT line breaks.
        assert_eq!(escape_dot_label("a\\nb"), "a\\nb");
        assert_eq!(escape_dot_label("a\\lb"), "a\\lb");
        assert_eq!(escape_dot_label("a\\rb"), "a\\rb");
    }

    #[test]
    fn escape_dot_label_other_backslash_doubles() {
        assert_eq!(escape_dot_label("a\\b"), "a\\\\b");
        assert_eq!(escape_dot_label("\\"), "\\\\");
    }

    #[test]
    fn escape_dot_label_carriage_return_passes_through_as_is() {
        // Locks the current implementation: a literal '\r' character is not
        // stripped — it falls through to the catch-all push branch. (See
        // Task 4 for the doc fix that brings the comment in line with this.)
        assert_eq!(escape_dot_label("a\rb"), "a\rb");
    }

    #[test]
    fn escape_dot_label_combined_inputs_round_trip() {
        // A realistic node label from the IR/CFG dumper: contains both a
        // recognised DOT escape (\l) and a literal newline.
        let input = "Instruction(addr=0x401000)\n\\l0x401000: ADD";
        let want = "Instruction(addr=0x401000)\\n\\l0x401000: ADD";
        assert_eq!(escape_dot_label(input), want);
    }

    // ── json_quote ──────────────────────────────────────────────────────────

    #[test]
    fn json_quote_wraps_empty_input_in_double_quotes() {
        assert_eq!(json_quote(""), "\"\"");
    }

    #[test]
    fn json_quote_passes_through_plain_ascii() {
        assert_eq!(json_quote("hello"), "\"hello\"");
    }

    #[test]
    fn json_quote_escapes_double_quote_backslash_and_whitespace() {
        assert_eq!(json_quote("\""), "\"\\\"\"");
        assert_eq!(json_quote("\\"), "\"\\\\\"");
        assert_eq!(json_quote("\n"), "\"\\n\"");
        assert_eq!(json_quote("\r"), "\"\\r\"");
        assert_eq!(json_quote("\t"), "\"\\t\"");
    }

    #[test]
    fn json_quote_escapes_low_control_chars_as_unicode() {
        //  is < 0x20 and not one of the recognised short escapes,
        // so the implementation falls through to  form.
        assert_eq!(json_quote("\u{0001}"), "\"\\u0001\"");
        assert_eq!(json_quote("\u{001f}"), "\"\\u001f\"");
        // 0x20 (space) is the boundary: it must NOT be unicode-escaped.
        assert_eq!(json_quote(" "), "\" \"");
    }

    #[test]
    fn json_quote_passes_through_high_unicode_unchanged() {
        // Non-ASCII chars >= 0x20 are emitted verbatim (no surrogate
        // expansion). Any compliant JSON parser accepts UTF-8 directly.
        assert_eq!(json_quote("café"), "\"café\"");
        assert_eq!(json_quote("→"), "\"→\"");
    }
}
```

- [ ] **Step 2: Run the tests to verify they pass on the current code**

Run: `cargo test -p dot label_tests`
Expected: 14 tests pass. (If `escape_dot_label_carriage_return_passes_through_as_is` fails, the implementation has already been altered and you should investigate before proceeding.)

- [ ] **Step 3: Re-verify clippy is clean (the inline `#[cfg(test)] mod` is a new compilation unit)**

Run: `cargo clippy -p dot --all-targets --no-deps -- -D warnings`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/dot/src/lib.rs
git commit -m "test(dot): pin escape_dot_label and json_quote behavior with unit tests"
```

---

## Task 3: Add behavior-pinning tests for `DotStyle::{dark, dark_cfg, empty}` and `DotEmitter` round-trip

The public API has zero test coverage today (only `tests/error.rs` exists, and it covers only `Error<E>`). Before we touch `DotEmitter::new` (Task 7) or `DotStyle::dark_cfg` (Task 6), pin the current observable output.

**Files:**
- Create: `crates/dot/tests/emitter.rs`
- Create: `crates/dot/tests/style.rs`

- [ ] **Step 1: Write the `DotEmitter` round-trip tests**

Create `crates/dot/tests/emitter.rs`:

```rust
#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

//! Pins the exact DOT string `DotEmitter` produces for representative inputs.
//! These tests double as regression coverage for Tasks 5–7 of the dot-crate
//! review (digraph-name quoting, push_str-vs-format!, and the `extra` attr
//! interface contract).

use dot::{DotEmitter, DotStyle};

#[test]
fn empty_emitter_produces_minimal_digraph() {
    let style = DotStyle::empty();
    let out = DotEmitter::new("G", &style).finish();
    assert_eq!(out, "digraph G {\n}\n");
}

#[test]
fn empty_emitter_with_dark_style_emits_attr_blocks_in_order() {
    let style = DotStyle::dark();
    let out = DotEmitter::new("G", &style).finish();

    // Attribute-block order: graph, node, edge — pinned by `DotEmitter::new`.
    let g_pos = out.find("graph [").expect("expected graph block");
    let n_pos = out.find("node [").expect("expected node block");
    let e_pos = out.find("edge [").expect("expected edge block");
    assert!(g_pos < n_pos && n_pos < e_pos, "block ordering broke: {out}");

    // Spot-check one attribute from each block (full string is brittle
    // since it's many lines; we only need to lock structure here).
    assert!(out.contains("rankdir=TB,"));
    assert!(out.contains("shape=box,"));
    assert!(out.contains("color=\"#aaaaaa\","));
    assert!(out.ends_with("}\n"));
}

#[test]
fn node_emits_quoted_id_and_escaped_label() {
    let style = DotStyle::empty();
    let mut e = DotEmitter::new("G", &style);
    e.node("n1", "hello \"world\"", "box", &[]);
    let out = e.finish();
    assert!(
        out.contains("\"n1\" [label=\"hello \\\"world\\\"\", shape=box];\n"),
        "unexpected DOT: {out}"
    );
}

#[test]
fn node_with_extra_attrs_emits_them_comma_separated() {
    let style = DotStyle::empty();
    let mut e = DotEmitter::new("G", &style);
    e.node("n1", "lbl", "trapezium", &[("fillcolor", "\"#3a2a10\"")]);
    let out = e.finish();
    assert!(
        out.contains("\"n1\" [label=\"lbl\", shape=trapezium, fillcolor=\"#3a2a10\"];\n"),
        "unexpected DOT: {out}"
    );
}

#[test]
fn edge_with_no_extra_omits_bracket_block() {
    let style = DotStyle::empty();
    let mut e = DotEmitter::new("G", &style);
    e.edge("a", "b", &[]);
    let out = e.finish();
    assert!(out.contains("\"a\" -> \"b\";\n"), "unexpected DOT: {out}");
    assert!(!out.contains("[]"), "extra=[] must not produce empty brackets");
}

#[test]
fn edge_with_extra_emits_bracketed_attrs() {
    let style = DotStyle::empty();
    let mut e = DotEmitter::new("G", &style);
    e.edge("a", "b", &[("label", "Branch"), ("style", "dashed")]);
    let out = e.finish();
    assert!(
        out.contains("\"a\" -> \"b\" [label=Branch, style=dashed];\n"),
        "unexpected DOT: {out}"
    );
}

#[test]
fn finish_appends_closing_brace_and_newline_exactly_once() {
    let style = DotStyle::empty();
    let out = DotEmitter::new("G", &style).finish();
    // Exactly one closing brace, exactly one trailing newline.
    assert_eq!(out.matches('}').count(), 1);
    assert!(out.ends_with("}\n"));
}
```

- [ ] **Step 2: Write the `DotStyle` constructor tests**

Create `crates/dot/tests/style.rs`:

```rust
#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

//! Pins the contract of `DotStyle::{dark, dark_cfg, empty}`: empty has no
//! attributes; dark has the documented dark-theme attributes; dark_cfg is
//! identical to dark except for the fontname swap (Task 6 will remove the
//! dead margin block — these tests stay green either way).

use dot::DotStyle;

#[test]
fn empty_has_no_default_attrs() {
    let s = DotStyle::empty();
    assert!(s.graph.is_empty());
    assert!(s.node.is_empty());
    assert!(s.edge.is_empty());
}

#[test]
fn dark_has_known_graph_node_and_edge_attrs() {
    let s = DotStyle::dark();
    assert!(s.graph.iter().any(|(k, v)| *k == "rankdir" && *v == "TB"));
    assert!(s.graph.iter().any(|(k, v)| *k == "bgcolor"));
    assert!(s.node.iter().any(|(k, v)| *k == "shape" && *v == "box"));
    assert!(s.node.iter().any(|(k, v)| *k == "fontname" && *v == "monospace"));
    assert!(s.node.iter().any(|(k, v)| *k == "margin" && *v == "0.2"));
    assert!(s.edge.iter().any(|(k, v)| *k == "fontcolor"));
}

#[test]
fn dark_cfg_replaces_fontname_with_courier() {
    let s = DotStyle::dark_cfg();
    // The fontname must be Courier (changed from "monospace") so viz.js's
    // bundled metrics match the rendered text width and labels don't
    // overflow node boxes.
    assert!(
        s.node.iter().any(|(k, v)| *k == "fontname" && *v == "Courier"),
        "expected fontname=Courier in dark_cfg().node",
    );
    // Margin stays at "0.2" (same as dark()) — the bare `margin` mutation
    // in `dark_cfg` is a no-op today (see Task 6), so this assertion holds
    // before AND after Task 6's cleanup.
    assert!(
        s.node.iter().any(|(k, v)| *k == "margin" && *v == "0.2"),
        "expected margin=0.2 in dark_cfg().node",
    );
}

#[test]
fn dark_cfg_inherits_other_dark_attrs_unchanged() {
    let dark = DotStyle::dark();
    let cfg = DotStyle::dark_cfg();

    // Same number of node attributes.
    assert_eq!(dark.node.len(), cfg.node.len());

    // Every non-fontname dark node attr is preserved verbatim in dark_cfg.
    for (k, v) in &dark.node {
        if *k == "fontname" {
            continue;
        }
        assert!(
            cfg.node.iter().any(|(ck, cv)| ck == k && cv == v),
            "dark_cfg.node missing or altered ({k}, {v})",
        );
    }

    // Graph and edge sections are untouched by dark_cfg.
    assert_eq!(dark.graph, cfg.graph);
    assert_eq!(dark.edge, cfg.edge);
}
```

- [ ] **Step 3: Run the new test files**

Run: `cargo test -p dot --test emitter --test style`
Expected: all 11 tests pass.

- [ ] **Step 4: Re-verify clippy is clean**

Run: `cargo clippy -p dot --all-targets --no-deps -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/dot/tests/emitter.rs crates/dot/tests/style.rs
git commit -m "test(dot): pin DotEmitter round-trip and DotStyle constructor contracts"
```

---

## Task 4: Fix `escape_dot_label`'s `\r` doc-vs-code disagreement

(Applies default **Q1 (A)**.) The doc claims CR is stripped; the code passes it through. Fix the doc to match the code. Behavior unchanged; the test added in Task 2 (`escape_dot_label_carriage_return_passes_through_as_is`) already pins this.

**Files:**
- Modify: [crates/dot/src/lib.rs:132-138](crates/dot/src/lib.rs#L132-L138)

- [ ] **Step 1: Update the doc comment**

Replace the doc block above `fn escape_dot_label` (lines 132-138):

```rust
/// Escapes a string for use as a DOT double-quoted label.
///
/// - `"` → `\"`
/// - `\` → `\\`
/// - newline → `\n` (Graphviz left-justified line break)
/// - carriage-return stripped
fn escape_dot_label(s: &str) -> String {
```

with:

```rust
/// Escapes a string for use as a DOT double-quoted label.
///
/// - `"` → `\"`
/// - `\` (followed by recognised DOT label escape `n`/`l`/`r`) is
///   passed through verbatim so callers can hand-emit DOT line breaks
///   (`\n` centre-justified, `\l` left-justified, `\r` right-justified).
/// - `\` (followed by anything else) → `\\`
/// - literal newline → `\n` (the DOT centre-justify line-break escape).
/// - any other character (including literal `\r`) is passed through unchanged.
fn escape_dot_label(s: &str) -> String {
```

(If the reviewer chose **Q1 (B)** instead, replace the function body's catch-all arm `c => out.push(c)` with `'\r' => {} // strip` followed by `c => out.push(c)`, AND update the test from Task 2 to assert `escape_dot_label("a\rb") == "ab"`.)

- [ ] **Step 2: Verify all tests still pass**

Run: `cargo test -p dot`
Expected: PASS.

- [ ] **Step 3: Verify clippy still clean**

Run: `cargo clippy -p dot --all-targets --no-deps -- -D warnings`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/dot/src/lib.rs
git commit -m "docs(dot): clarify escape_dot_label CR behavior to match the implementation"
```

---

## Task 5: Simplify the backslash arm in `escape_dot_label`

The current code peeks at the next char, matches `Some('n') | Some('l') | Some('r')`, and then re-fetches it via `if let Some(c) = chars.next()` — a defensive `if let` after a peek that already proved the iterator yields `Some`. Cleaner: bind the peeked char in the pattern, then unconditionally consume it.

**Files:**
- Modify: [crates/dot/src/lib.rs:144-155](crates/dot/src/lib.rs#L144-L155)

- [ ] **Step 1: Run the existing tests to confirm baseline**

Run: `cargo test -p dot label_tests`
Expected: 14 tests pass (added in Task 2). These pin the current behavior.

- [ ] **Step 2: Replace the backslash arm**

In `crates/dot/src/lib.rs`, find the `'\\' =>` arm inside `escape_dot_label`:

```rust
'\\' => {
    // Pass through recognised DOT label escapes: \n \l \r
    match chars.peek() {
        Some('n') | Some('l') | Some('r') => {
            out.push('\\');
            if let Some(c) = chars.next() {
                out.push(c);
            }
        }
        _ => out.push_str("\\\\"),
    }
}
```

Replace with:

```rust
'\\' => match chars.peek() {
    // Pass through recognised DOT label escapes: \n \l \r.
    Some(&c @ ('n' | 'l' | 'r')) => {
        chars.next();
        out.push('\\');
        out.push(c);
    }
    _ => out.push_str("\\\\"),
},
```

The `Some(&c @ ('n' | 'l' | 'r'))` binds the peeked char by value so `chars.next()` (which we know returns `Some(c)`) can be discarded without an `if let`/`unwrap`.

- [ ] **Step 3: Re-run the label tests**

Run: `cargo test -p dot label_tests`
Expected: 14 tests still pass.

- [ ] **Step 4: Re-run all dot tests + clippy**

Run: `cargo test -p dot && cargo clippy -p dot --all-targets --no-deps -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/dot/src/lib.rs
git commit -m "refactor(dot): bind peeked char in escape_dot_label backslash arm"
```

---

## Task 6: Remove the dead margin block in `DotStyle::dark_cfg`

(Applies default **Q2 (A)**.) The `if let Some(e) = ... margin ...` block in `dark_cfg` overwrites `"0.2"` with `"0.2"` — a no-op left over from earlier tuning. Delete it; correct the doc.

**Files:**
- Modify: [crates/dot/src/lib.rs:104-118](crates/dot/src/lib.rs#L104-L118)

- [ ] **Step 1: Run the dark_cfg test to confirm baseline**

Run: `cargo test -p dot --test style dark_cfg`
Expected: 2 tests pass (added in Task 3). They lock fontname=Courier and margin=0.2.

- [ ] **Step 2: Replace the `dark_cfg` body**

Replace lines 104-118 in `crates/dot/src/lib.rs`:

```rust
    /// Like [`dark`] but with CFG-appropriate node sizing: `Courier` font
    /// (known metrics in viz.js) and extra margin so multiline labels fit.
    pub fn dark_cfg() -> Self {
        let mut s = Self::dark();
        // Replace the generic "monospace" entry with "Courier", which has
        // well-known character-width metrics in the bundled Graphviz/viz.js
        // layout engine, preventing text from overflowing node boxes.
        if let Some(e) = s.node.iter_mut().find(|(k, _)| *k == "fontname") {
            e.1 = "Courier";
        }
        if let Some(e) = s.node.iter_mut().find(|(k, _)| *k == "margin") {
            e.1 = "0.2";
        }
        s
    }
```

with:

```rust
    /// Like [`dark`] but with CFG-appropriate node typography: replaces the
    /// generic `monospace` font with `Courier`, whose character-width metrics
    /// are bundled into the Graphviz/viz.js layout engine. Without this swap,
    /// multiline labels overflow their node boxes in WASM-rendered HTML.
    #[must_use]
    pub fn dark_cfg() -> Self {
        let mut s = Self::dark();
        if let Some(e) = s.node.iter_mut().find(|(k, _)| *k == "fontname") {
            e.1 = "Courier";
        }
        s
    }
```

(The `#[must_use]` is already on this method from Task 1; restate it here so the diff is self-contained.)

- [ ] **Step 3: Re-run the style tests**

Run: `cargo test -p dot --test style`
Expected: all 4 tests still pass — `dark_cfg_inherits_other_dark_attrs_unchanged` in particular validates that margin is preserved from dark() and only fontname differs.

- [ ] **Step 4: Re-run all dot tests + clippy + workspace check (CFG renderer uses dark_cfg)**

Run: `cargo test -p dot && cargo clippy -p dot --all-targets --no-deps -- -D warnings && cargo check --workspace`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/dot/src/lib.rs
git commit -m "refactor(dot): remove dead margin no-op in DotStyle::dark_cfg"
```

---

## Task 7: Quote the digraph name in `DotEmitter::new`

(Applies default **Q3 (A)**.) Wrap `name` in double quotes (with `"` and `\` escaped) so any caller-supplied name produces valid DOT regardless of whitespace or special chars. All existing workspace callers default to `"G"`, so the only output change is `digraph G {` → `digraph "G" {` — semantically identical to Graphviz.

**Files:**
- Modify: [crates/dot/src/lib.rs:194-205](crates/dot/src/lib.rs#L194-L205)
- Modify: `crates/dot/tests/emitter.rs` (update assertions to expect the quoted form)

- [ ] **Step 1: Update the existing `emitter.rs` tests to expect the new quoted form**

Edit the assertions in `crates/dot/tests/emitter.rs` (added in Task 3) so the test for `empty_emitter_produces_minimal_digraph` becomes:

```rust
#[test]
fn empty_emitter_produces_minimal_digraph() {
    let style = DotStyle::empty();
    let out = DotEmitter::new("G", &style).finish();
    assert_eq!(out, "digraph \"G\" {\n}\n");
}
```

(No other emitter test asserts on the digraph header line; they all use `out.contains(...)` against post-header content.)

- [ ] **Step 2: Add a new test pinning the escape behavior**

Append to `crates/dot/tests/emitter.rs`:

```rust
#[test]
fn digraph_name_with_special_chars_is_quoted_and_escaped() {
    let style = DotStyle::empty();
    let out = DotEmitter::new("my graph \"X\"", &style).finish();
    // The name lives inside double-quotes, internal quotes are backslash-escaped.
    assert!(
        out.starts_with("digraph \"my graph \\\"X\\\"\" {\n"),
        "expected quoted+escaped header, got: {out}",
    );
}

#[test]
fn digraph_name_with_backslash_is_doubled() {
    let style = DotStyle::empty();
    let out = DotEmitter::new("path\\sub", &style).finish();
    // A bare backslash in the name doubles, since `escape_dot_label` only
    // passes through `\n`/`\l`/`\r` and a bare `\s` is not one of those.
    assert!(
        out.starts_with("digraph \"path\\\\sub\" {\n"),
        "expected doubled backslash, got: {out}",
    );
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p dot --test emitter`
Expected: at least three tests fail (`empty_emitter_produces_minimal_digraph`, `digraph_name_with_special_chars_is_quoted_and_escaped`, `digraph_name_with_backslash_is_doubled`) — current code produces `digraph G {` and `digraph my graph "X" {` (malformed).

- [ ] **Step 4: Quote the name in `DotEmitter::new`**

In `crates/dot/src/lib.rs`, replace the body of `DotEmitter::new`:

```rust
    pub fn new(name: &str, style: &DotStyle) -> Self {
        let mut s = String::new();
        s.push_str(&format!("digraph {name} {{\n"));

        emit_attr_block(&mut s, "graph", &style.graph);
        emit_attr_block(&mut s, "node", &style.node);
        emit_attr_block(&mut s, "edge", &style.edge);

        Self { out: s }
    }
```

with:

```rust
    pub fn new(name: &str, style: &DotStyle) -> Self {
        let mut s = String::new();
        // Always wrap the digraph name in double-quotes (with `"` and `\`
        // escaped via the same rules as a label) so any caller-supplied
        // name — including one with whitespace or punctuation — produces
        // valid DOT. Graphviz parses quoted and bare identifiers
        // identically when the bare form is legal.
        s.push_str("digraph \"");
        s.push_str(&escape_dot_label(name));
        s.push_str("\" {\n");

        emit_attr_block(&mut s, "graph", &style.graph);
        emit_attr_block(&mut s, "node", &style.node);
        emit_attr_block(&mut s, "edge", &style.edge);

        Self { out: s }
    }
```

(Restate `#[must_use]` from Task 1 above the signature so the diff is self-contained.)

- [ ] **Step 5: Re-run the emitter tests**

Run: `cargo test -p dot --test emitter`
Expected: all 8 tests pass.

- [ ] **Step 6: Re-run all dot tests + clippy + workspace check**

Run: `cargo test -p dot && cargo clippy -p dot --all-targets --no-deps -- -D warnings && cargo check --workspace`
Expected: PASS.

- [ ] **Step 7: Re-run callers' integration tests (cfg + ir)**

Run: `cargo test -p cfg --test dot_dumper && cargo test -p ir --test dot`
Expected: PASS — neither test asserts on the literal `digraph G {` substring (cfg's `dot_dumper.rs` matches against `digraph` and per-node lines; ir's `dot/tests.rs` builds the emitter manually with `DotEmitter::new("test", ...)` and doesn't pin the header).

If either fails because of the now-quoted name, update that test's expectation in the same commit; do **not** revert this change.

- [ ] **Step 8: Commit**

```bash
git add crates/dot/src/lib.rs crates/dot/tests/emitter.rs
git commit -m "fix(dot): quote+escape digraph name in DotEmitter::new for arbitrary inputs"
```

---

## Task 8: Replace `push_str(&format!(...))` chains with direct `push_str` in `DotEmitter`

`DotEmitter::node`, `DotEmitter::edge`, and `emit_attr_block` each do `self.out.push_str(&format!(...))` in tight loops, allocating an intermediate `String` per call just to immediately concatenate it. Splitting into a sequence of `push_str(literal_or_borrow)` calls removes the intermediate allocation, is no slower, and stays clippy-clean (no `let _ = write!(...)` or `.expect(...)` needed since these signatures are infallible).

**Files:**
- Modify: [crates/dot/src/lib.rs:207-242](crates/dot/src/lib.rs#L207-L242)
- Modify: [crates/dot/src/lib.rs:245-255](crates/dot/src/lib.rs#L245-L255)

- [ ] **Step 1: Run the emitter tests to confirm baseline**

Run: `cargo test -p dot --test emitter`
Expected: 8 tests pass (after Task 7).

- [ ] **Step 2: Rewrite `DotEmitter::node`**

Replace the body of `DotEmitter::node` (lines 207-218):

```rust
    pub fn node(&mut self, id: &str, label: &str, shape: &str, extra: &[(&str, &str)]) {
        let label = escape_dot_label(label);
        self.out
            .push_str(&format!("  \"{id}\" [label=\"{label}\", shape={shape}"));

        for (k, v) in extra {
            self.out.push_str(&format!(", {k}={v}"));
        }

        self.out.push_str("];\n");
    }
```

with:

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

- [ ] **Step 3: Rewrite `DotEmitter::edge`**

Replace the body of `DotEmitter::edge` (lines 221-236):

```rust
    pub fn edge(&mut self, from: &str, to: &str, extra: &[(&str, &str)]) {
        self.out.push_str(&format!("  \"{from}\" -> \"{to}\""));

        if !extra.is_empty() {
            self.out.push_str(" [");
            for (i, (k, v)) in extra.iter().enumerate() {
                if i != 0 {
                    self.out.push_str(", ");
                }
                self.out.push_str(&format!("{k}={v}"));
            }
            self.out.push(']');
        }

        self.out.push_str(";\n");
    }
```

with:

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

- [ ] **Step 4: Rewrite `emit_attr_block`**

Replace `emit_attr_block` (lines 245-255):

```rust
fn emit_attr_block(out: &mut String, name: &str, attrs: &[(&str, &str)]) {
    if attrs.is_empty() {
        return;
    }

    out.push_str(&format!("  {name} [\n"));
    for (k, v) in attrs {
        out.push_str(&format!("    {k}={v},\n"));
    }
    out.push_str("  ];\n\n");
}
```

with:

```rust
fn emit_attr_block(out: &mut String, name: &str, attrs: &[(&str, &str)]) {
    if attrs.is_empty() {
        return;
    }

    out.push_str("  ");
    out.push_str(name);
    out.push_str(" [\n");
    for (k, v) in attrs {
        out.push_str("    ");
        out.push_str(k);
        out.push('=');
        out.push_str(v);
        out.push_str(",\n");
    }
    out.push_str("  ];\n\n");
}
```

- [ ] **Step 5: Re-run the emitter tests**

Run: `cargo test -p dot --test emitter`
Expected: all 8 tests still pass — these tests pin the exact byte-level DOT output, so any deviation in Tasks 2-4 of this task would surface here.

- [ ] **Step 6: Re-run all dot tests + clippy + workspace check**

Run: `cargo test -p dot && cargo clippy -p dot --all-targets --no-deps -- -D warnings && cargo check --workspace`
Expected: PASS.

- [ ] **Step 7: Run downstream caller integration tests once more**

Run: `cargo test -p cfg --test dot_dumper && cargo test -p ir --test dot`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/dot/src/lib.rs
git commit -m "refactor(dot): replace push_str(&format!()) with direct push_str chains in DotEmitter"
```

---

## Task 9: Avoid the `to_owned()` allocation in `as_html_from_svg`

[lib.rs:346-348](crates/dot/src/lib.rs#L346-L348) does `svg = svg[pos..].to_owned()` to drop the XML preamble — that allocates a new `String` and discards the original. `String::drain` does it in place.

**Files:**
- Modify: [crates/dot/src/lib.rs:343-350](crates/dot/src/lib.rs#L343-L350)

- [ ] **Step 1: Replace the slice-and-rebind with `drain`**

In `crates/dot/src/lib.rs::GraphDot::as_html_from_svg`, replace:

```rust
        let mut svg = self.as_svg()?;
        // Strip the XML declaration and DOCTYPE that `dot` emits — they can
        // confuse HTML parsers when the SVG is inlined in a <body>.
        if let Some(pos) = svg.find("<svg") {
            svg = svg[pos..].to_owned();
        }
        Ok(HTML_SVG_TEMPLATE.replace("__SVG__", &svg))
```

with:

```rust
        let mut svg = self.as_svg()?;
        // Strip the XML declaration and DOCTYPE that `dot` emits — they can
        // confuse HTML parsers when the SVG is inlined in a <body>.
        if let Some(pos) = svg.find("<svg") {
            svg.drain(..pos);
        }
        Ok(HTML_SVG_TEMPLATE.replace("__SVG__", &svg))
```

- [ ] **Step 2: Build + clippy + tests**

Run: `cargo test -p dot && cargo clippy -p dot --all-targets --no-deps -- -D warnings && cargo check --workspace`
Expected: PASS. (No test exercises the SVG path because it requires the system `dot` binary; that's a coverage gap deliberately left alone — see Out of scope.)

- [ ] **Step 3: Commit**

```bash
git add crates/dot/src/lib.rs
git commit -m "refactor(dot): drop XML preamble in place via String::drain instead of reallocating"
```

---

## Task 10: Drop `+ Sized` bound + accept `impl AsRef<Path>` in `dump_as_*`

(Applies default **Q4 (A)**.) Two cosmetic API improvements folded together.

1. `pub struct GraphDot<G: GraphDotDumper + Sized>` ([lib.rs:260](crates/dot/src/lib.rs#L260)) — `Sized` is the default bound for any generic type parameter, so `+ Sized` is redundant.
2. `dump_as_html(&self, out_path: &str, ...)` and `dump_as_dot(&self, out_path: &str, ...)` ([lib.rs:366](crates/dot/src/lib.rs#L366), [lib.rs:372](crates/dot/src/lib.rs#L372)) — switch to `impl AsRef<std::path::Path>` so callers can pass `&Path`/`PathBuf`/`&str` interchangeably.

**Files:**
- Modify: [crates/dot/src/lib.rs:260](crates/dot/src/lib.rs#L260)
- Modify: [crates/dot/src/lib.rs:363-375](crates/dot/src/lib.rs#L363-L375)

- [ ] **Step 1: Drop the `+ Sized` bound**

In `crates/dot/src/lib.rs`, change:

```rust
pub struct GraphDot<G: GraphDotDumper + Sized> {
```

to:

```rust
pub struct GraphDot<G: GraphDotDumper> {
```

- [ ] **Step 2: Switch the two `dump_as_*` paths to `impl AsRef<Path>`**

Add `use std::path::Path;` to the existing `use std::{fmt::Debug, io::Write};` line at [lib.rs:41](crates/dot/src/lib.rs#L41) — make it:

```rust
use std::{fmt::Debug, io::Write, path::Path};
```

Replace `dump_as_html` and `dump_as_dot` (lines 363-375):

```rust
    /// Writes an interactive HTML viewer for this graph to `out_path`.
    ///
    /// Uses client-side Graphviz WASM rendering — no local `dot` binary needed.
    pub fn dump_as_html(&self, out_path: &str) -> Result<(), G::Error> {
        std::fs::write(out_path, self.as_html_from_dot()?)?;
        Ok(())
    }

    /// Writes the raw DOT source to `out_path`.
    pub fn dump_as_dot(&self, out_path: &str) -> Result<(), G::Error> {
        std::fs::write(out_path, self.as_dot()?)?;
        Ok(())
    }
```

with:

```rust
    /// Writes an interactive HTML viewer for this graph to `out_path`.
    ///
    /// Uses client-side Graphviz WASM rendering — no local `dot` binary needed.
    ///
    /// # Errors
    /// - [`ErrorKind::DotDumpError`] propagated from the dumper.
    /// - [`ErrorKind::IoError`] if writing `out_path` fails.
    pub fn dump_as_html(&self, out_path: impl AsRef<Path>) -> Result<(), G::Error> {
        std::fs::write(out_path, self.as_html_from_dot()?)?;
        Ok(())
    }

    /// Writes the raw DOT source to `out_path`.
    ///
    /// # Errors
    /// Same as [`dump_as_html`].
    pub fn dump_as_dot(&self, out_path: impl AsRef<Path>) -> Result<(), G::Error> {
        std::fs::write(out_path, self.as_dot()?)?;
        Ok(())
    }
```

(The `# Errors` blocks are restated here from Task 1 since the doc comment is replaced wholesale.)

- [ ] **Step 3: Build + clippy + tests + workspace check**

Run: `cargo test -p dot && cargo clippy -p dot --all-targets --no-deps -- -D warnings && cargo check --workspace`
Expected: PASS — both analyzer-example callers (`dot.dump_as_html("graph.html")?`, etc.) keep compiling because `&str: AsRef<Path>`.

- [ ] **Step 4: Smoke-run the analyzer example to verify no surprise at runtime**

Run: `cargo run --example analyzer`
Expected: produces `cfg.html`, `graph.html`, `graph-opt.html` exactly as before, with the same content (modulo the `digraph "G"` quoting from Task 7 and DOT byte-output unchanged from Task 8).

- [ ] **Step 5: Commit**

```bash
git add crates/dot/src/lib.rs
git commit -m "refactor(dot): drop redundant + Sized bound; accept impl AsRef<Path> in dump_as_*"
```

---

## Task 11: Document the `extra: &[(&str, &str)]` contract on `DotEmitter::node`/`edge`

The `extra` slice's values are inserted into the DOT output verbatim — the caller is responsible for any quoting, escaping, or ensuring the result is a valid DOT attribute value. This contract is implicit today; both internal callers happen to follow it, but the trait isn't documented. A short doc paragraph closes the foot-gun.

**Files:**
- Modify: [crates/dot/src/lib.rs:207](crates/dot/src/lib.rs#L207) (doc above `pub fn node`)
- Modify: [crates/dot/src/lib.rs:220](crates/dot/src/lib.rs#L220) (doc above `pub fn edge`)

- [ ] **Step 1: Update the `node` doc**

Replace the doc above `pub fn node`:

```rust
    /// Emits a node statement.  The `label` is automatically escaped for DOT.
```

with:

```rust
    /// Emits a node statement. The `label` is escaped via [`escape_dot_label`]
    /// before being wrapped in DOT double-quotes.
    ///
    /// `extra` attributes are inserted verbatim as `key=value` pairs — the
    /// caller is responsible for any quoting or escaping of the value
    /// (e.g. `("fillcolor", "\"#3a2a10\"")` for a hex colour, or
    /// `("style", "dashed")` for a bare identifier).
```

- [ ] **Step 2: Update the `edge` doc**

Replace the doc above `pub fn edge`:

```rust
    /// Emits a directed edge statement.
```

with:

```rust
    /// Emits a directed edge statement.
    ///
    /// `extra` attributes follow the same caller-quotes-the-value contract
    /// as [`DotEmitter::node`] — they are inserted verbatim as `key=value`.
```

- [ ] **Step 3: Build + clippy + tests**

Run: `cargo test -p dot && cargo clippy -p dot --all-targets --no-deps -- -D warnings`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/dot/src/lib.rs
git commit -m "docs(dot): clarify the caller-quotes-the-value contract on DotEmitter::{node,edge}"
```

---

## Task 12: Final sanity sweep

**Files:**
- Run-only, no edits.

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
Expected: `cfg.html`, `graph.html`, `graph-opt.html` produced. Spot-check `cfg.html` opens in a browser and renders the CFG identically to pre-refactor (modulo the `digraph "G" {` header line; the rest of the DOT body is byte-identical).

---

## Out of scope (considered, rejected, or deferred)

- **Adding integration test coverage for `as_svg`/`as_html_from_svg`**: requires the system `dot` binary; tests would be skipped in CI environments without it. Mark as a known coverage gap.
- **Splitting `ErrorKind::SvgConversionError(String)` into `DotBinarySpawnFailed` / `DotProcessFailed`**: useful telemetry distinction but adds two enum variants with no current consumer differentiating them. Defer until a caller actually needs the split.
- **Removing `Clone` on `DotStyle`**: derive is dead today (no workspace caller clones), but the cost is one auto-derived impl. Keeping it preserves the natural copy-and-tweak ergonomic that `dark_cfg` uses internally.
- **Constifying the `"__SVG__"`, `"__DOT_JSON__"`, `"dot"`, `"-Tsvg"` magic strings**: stylistic; one-call-site each. Not worth the indirection.
- **Restructuring `GraphDot::new(dumper, style).with_name(...)` into a `GraphDotBuilder`**: current shape is fine for the two fields. Builder noise without clear win.
- **Changing `iter_nodes(&self) -> impl IntoIterator<Item = Self::Node>` to `Iterator`**: stylistic; both shapes work and `IntoIterator` is more flexible at the trait boundary. Leave.
- **Stripping `\r` from `escape_dot_label` per the original doc** (Q1 B): reviewer-overridable; default is doc-fix to match code. Behavior change has zero practical effect (no current dumper emits `\r`), so the safer no-op route wins.
- **Validating `DotEmitter::node` / `edge` extra-attr keys against a known set**: the API is intentionally low-level; over-validation defeats the purpose.
- **Replacing `Vec<(&'static str, &'static str)>` in `DotStyle` with `&'static [(&'static str, &'static str)]`**: would make `dark_cfg` (which mutates the inner attrs) infeasible, or force an ad-hoc clone. Net loss.
- **Switching `pub struct DotEmitter { out: String }` to expose `&mut String` directly**: encapsulation buys consistent label escaping; exposing the raw string reopens that hole.
- **Centralising the `escape_dot_label` doc into a single doc-link target referenced by both `node` and the function itself**: rustdoc handles `[`escape_dot_label`]` cross-links naturally; no extra change needed.
