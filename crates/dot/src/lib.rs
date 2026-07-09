//! Graphviz `.dot` and interactive `.html` rendering for any graph type
//! that implements [`GraphDotDumper`] (this crate is domain-agnostic).
//!
//! Implement [`GraphDotDumper`] for a graph type to obtain `.dot` and `.html`
//! output via [`GraphDot`].
//!
//! # Rendering pipeline
//!
//! ```text
//! GraphDotDumper::dump_as_dot() ──► DotEmitter ──► .dot string ──► as_html_from_dot
//! ```
//!
//! [`GraphDot::as_html_from_dot`] embeds the raw DOT source in an HTML page
//! that renders it client-side via Graphviz WASM ([`@viz-js/viz`]).  No local
//! `dot` install is required.  This is what [`GraphDot::dump_as_html`] uses.
//!
//! [`DotStyle`] provides pre-built dark and empty visual themes.
//! [`DotEmitter`] is a low-level string builder for Graphviz DOT syntax.
//!
//! [`@viz-js/viz`]: https://github.com/mdaines/viz-js

use std::fmt::{Debug, Write};
use std::path::Path;

/// Crate-level `Result` alias.  Every fallible function in `dot` returns
/// this type.
pub type Result<T> = anyhow::Result<T>;

const HTML_DOT_TEMPLATE: &str = include_str!("../assets/graph_template_dot.html");

/// Vendored `@viz-js/viz` v3.5.0 standalone build (Graphviz 11.x compiled
/// to Wasm, with the Wasm itself base64-embedded in the JS).  Inlined at
/// build time into [`HTML_DOT_TEMPLATE`] so the generated HTML is fully
/// self-contained — no CDN fetch, no `.wasm` side-load.
const VIZ_STANDALONE_JS: &str = include_str!("../assets/vendored/viz-standalone.js");

/// Vendored `svg-pan-zoom` v3.6.1 minified build.  Same rationale as
/// [`VIZ_STANDALONE_JS`].
const SVG_PAN_ZOOM_JS: &str = include_str!("../assets/vendored/svg-pan-zoom.min.js");

/// A graph type that can be serialised to Graphviz DOT format node by node.
pub trait GraphDotDumper {
    type Node;
    // Bound is `Debug + Display + Send + Sync + 'static`, NOT
    // `std::error::Error + Send + Sync + 'static`, so impls can pick
    // `type Error = anyhow::Error` (anyhow's `Error` does not
    // implement `std::error::Error`, by design).  `render_dot_string` wraps
    // any returned error via `anyhow::anyhow!("dot dump error: {e}")`,
    // which only needs `Display`.
    type Error: Debug + std::fmt::Display + Send + Sync + 'static;
    type State;

    /// Creates the mutable state threaded through all [`Self::dump_as_dot`] calls.
    fn create_initial_state(&self) -> Self::State;

    /// Returns all nodes that should appear in the DOT output.
    fn iter_nodes(&self) -> impl IntoIterator<Item = Self::Node>;

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
}

/// A pre-built Graphviz visual theme (graph/node/edge default attributes).
#[derive(Clone)]
pub struct DotStyle {
    pub graph: Vec<(&'static str, &'static str)>,
    pub node: Vec<(&'static str, &'static str)>,
    pub edge: Vec<(&'static str, &'static str)>,
}

impl DotStyle {
    /// Builds the dark theme with a caller-chosen node `fontname`, the single
    /// attribute that differs between [`Self::dark`] and [`Self::dark_cfg`].
    fn dark_with_font(fontname: &'static str) -> Self {
        Self {
            graph: vec![
                ("rankdir", "TB"),
                ("bgcolor", "\"#1e1e1e\""),
                ("fontcolor", "white"),
            ],
            node: vec![
                ("shape", "box"),
                ("style", "\"filled,rounded\""),
                ("fillcolor", "\"#2d2d2d\""),
                ("color", "\"#888888\""),
                ("fontcolor", "white"),
                ("fontname", fontname),
                ("margin", "0.2"),
            ],
            edge: vec![
                ("color", "\"#aaaaaa\""),
                ("fontcolor", "white"),
                ("penwidth", "1.2"),
            ],
        }
    }

    /// Returns a dark-background theme suitable for modern editors / terminals.
    pub fn dark() -> Self {
        Self::dark_with_font("monospace")
    }

    /// Like [`Self::dark`] but with CFG-appropriate node typography: uses
    /// `Courier`, whose character-width metrics are bundled into the
    /// Graphviz/viz.js layout engine, instead of the generic `monospace`.
    /// Without this swap, multiline labels overflow their node boxes in
    /// WASM-rendered HTML.
    pub fn dark_cfg() -> Self {
        Self::dark_with_font("Courier")
    }

    /// Returns an empty theme (no default attributes).
    pub fn empty() -> Self {
        Self {
            graph: vec![],
            node: vec![],
            edge: vec![],
        }
    }
}

// ── DOT string helpers ────────────────────────────────────────────────────────

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
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => match chars.peek() {
                // Pass through recognised DOT label escapes: \n \l \r.
                Some(&c @ ('n' | 'l' | 'r')) => {
                    chars.next();
                    out.push('\\');
                    out.push(c);
                }
                _ => out.push_str("\\\\"),
            },
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out
}

/// Wraps `s` in a JSON string literal with full escaping.
///
/// Tailored for embedding the DOT source inside an HTML
/// `<script type="application/json">` element: in addition to the JSON
/// escapes (`"`, `\`, `\n`, `\r`, `\t`, low control chars as `\uXXXX`),
/// `<` is unconditionally emitted as `<` so a label containing
/// `</script>` cannot break out of the surrounding script tag.
fn json_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
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
                // writing to a `String` via `Write` is infallible
                // (`String::write_str` returns `Ok(())` unconditionally),
                // but clippy::expect_used flags the literal `.expect`.
                // `let _ =` documents that the `fmt::Result` is
                // intentionally discarded.
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ── DotEmitter ────────────────────────────────────────────────────────────────

/// Low-level builder that accumulates a Graphviz `digraph { … }` string.
pub struct DotEmitter {
    out: String,
}

impl DotEmitter {
    /// Creates a new emitter for a digraph named `name` with the given style.
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

    /// Emits a node statement. Both `id` and `label` are escaped via
    /// `escape_dot_label` before being wrapped in DOT double-quotes, so any
    /// caller-supplied id with `"` / `\` / newline produces valid DOT.
    ///
    /// `extra` attributes are inserted verbatim as `key=value` pairs — the
    /// caller is responsible for any quoting or escaping of the value
    /// (e.g. `("fillcolor", "\"#3a2a10\"")` for a hex colour, or
    /// `("style", "dashed")` for a bare identifier).
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
            push_attr(&mut self.out, k, v);
        }

        self.out.push_str("];\n");
    }

    /// Emits a directed edge statement. Both endpoints (`from`, `to`) are
    /// escaped via `escape_dot_label` for the same reason as
    /// [`DotEmitter::node`].
    ///
    /// `extra` attributes follow the same caller-quotes-the-value contract
    /// as [`DotEmitter::node`] — they are inserted verbatim as `key=value`.
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
                push_attr(&mut self.out, k, v);
            }
            self.out.push(']');
        }

        self.out.push_str(";\n");
    }

    /// Finalises the digraph and returns the complete DOT string.
    pub fn finish(mut self) -> String {
        self.out.push_str("}\n");
        self.out
    }
}

/// Appends a single `key=value` DOT attribute to `out`.  Shared by every
/// attribute emitter so the `key=value` shape lives in one place; callers
/// supply their own framing (leading comma, separator, bracket block).
///
/// A `label` value is free text (edge and node-`extra` labels flow through
/// here), so it is quoted and escaped exactly like [`DotEmitter::node`]'s
/// dedicated label — otherwise a value with a DOT-special character (a hyphen
/// as in `if-true`/`if-false`, a colon, a space, a quote) is invalid unquoted
/// and Graphviz aborts with "syntax error near '-'".  Every other attribute
/// keeps the caller-owns-quoting contract (the style blocks pass pre-quoted
/// colors like `"#1e1e1e"` alongside bare idents like `TB` / `dashed`).
fn push_attr(out: &mut String, k: &str, v: &str) {
    out.push_str(k);
    out.push('=');
    if k == "label" {
        out.push('"');
        out.push_str(&escape_dot_label(v));
        out.push('"');
    } else {
        out.push_str(v);
    }
}

fn emit_attr_block(out: &mut String, name: &str, attrs: &[(&str, &str)]) {
    if attrs.is_empty() {
        return;
    }

    out.push_str("  ");
    out.push_str(name);
    out.push_str(" [\n");
    for (k, v) in attrs {
        out.push_str("    ");
        push_attr(out, k, v);
        out.push_str(",\n");
    }
    out.push_str("  ];\n\n");
}

/// Node-statement count above which the HTML viewer defaults to `sfdp`
/// instead of `dot`: `dot`'s layered layout is superlinear and stalls the
/// browser on large graphs, while `sfdp` (force-directed) handles them.
/// The viewer's engine picker still lets the user switch.
pub const DEFAULT_SFDP_NODE_THRESHOLD: usize = 2000;

/// Approximate node-statement count of a DOT source (counts `[label=`
/// occurrences — an over-count that also catches edge-label attribute
/// blocks, but it only drives the default engine choice, so a small
/// over-count harmlessly biases large graphs toward `sfdp`).
pub(crate) fn dot_node_count(dot: &str) -> usize {
    dot.matches("[label=").count()
}

// ── GraphDot ──────────────────────────────────────────────────────────────────

/// Wraps a [`GraphDotDumper`] and produces DOT / HTML output (the HTML
/// renders to SVG client-side via viz.js).
pub struct GraphDot<G: GraphDotDumper> {
    dumper: G,
    style: DotStyle,
    name: String,
}

impl<G: GraphDotDumper> GraphDot<G> {
    /// Creates a new `GraphDot` with the given dumper and visual style.
    ///
    /// The emitted digraph is named `"G"`.
    pub fn new(dumper: G, style: DotStyle) -> Self {
        Self {
            dumper,
            style,
            name: "G".to_string(),
        }
    }

    /// Returns the raw DOT source string.
    ///
    /// # Errors
    /// Forwards any `Self::Error` returned by the underlying
    /// [`GraphDotDumper::dump_as_dot`] for any node.
    pub fn as_dot(&self) -> anyhow::Result<String> {
        let mut dot = DotEmitter::new(&self.name, &self.style);
        let mut state = self.dumper.create_initial_state();
        for node in self.dumper.iter_nodes() {
            self.dumper
                .dump_as_dot(node, &mut dot, &mut state)
                .map_err(|e| anyhow::anyhow!("dot dump error: {e}"))?;
        }

        Ok(dot.finish())
    }

    /// Produces an interactive HTML page that renders the DOT source
    /// client-side via Graphviz WASM.  No local `dot` install required.
    ///
    /// The DOT source is embedded as a JSON string inside a
    /// `<script type="application/json">` element, so it is safe regardless of
    /// what characters appear in node labels.
    ///
    /// The vendored `@viz-js/viz` and `svg-pan-zoom` JS payloads are
    /// inlined directly into the output, so the resulting HTML works
    /// fully offline — no CDN fetch, no `.wasm` side-load.
    ///
    /// # Errors
    /// Same as [`Self::as_dot`].
    pub fn as_html_from_dot(&self) -> anyhow::Result<String> {
        let dot_src = self.as_dot()?;
        // `dot`'s layered layout does not scale to large sea-of-nodes graphs
        // (a ~1800-node IR is ~1400 ranks deep, and mincross explodes on the
        // resulting virtual nodes — it hangs the browser). Default such graphs
        // to `sfdp`; the viewer's picker still exposes `dot` for small ones.
        let engine = if dot_node_count(&dot_src) > DEFAULT_SFDP_NODE_THRESHOLD {
            "sfdp"
        } else {
            "dot"
        };
        Ok(HTML_DOT_TEMPLATE
            .replace("__VIZ_STANDALONE_JS__", VIZ_STANDALONE_JS)
            .replace("__SVG_PAN_ZOOM_JS__", SVG_PAN_ZOOM_JS)
            .replace("__DEFAULT_ENGINE__", engine)
            .replace("__DOT_JSON__", &json_quote(&dot_src)))
    }

    /// Writes an interactive HTML viewer for this graph to `out_path`.
    ///
    /// Uses client-side Graphviz WASM rendering — no local `dot` binary needed.
    ///
    /// # Errors
    /// - Propagated from the dumper.
    /// - Returns an error if writing `out_path` fails.
    pub fn dump_as_html(&self, out_path: impl AsRef<Path>) -> anyhow::Result<()> {
        std::fs::write(out_path, self.as_html_from_dot()?)?;
        Ok(())
    }

    /// Writes the raw DOT source to `out_path`.
    ///
    /// # Errors
    /// Same as [`Self::dump_as_html`].
    pub fn dump_as_dot(&self, out_path: impl AsRef<Path>) -> anyhow::Result<()> {
        std::fs::write(out_path, self.as_dot()?)?;
        Ok(())
    }
}

#[cfg(test)]
#[allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]
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
        // Locks the implementation: a literal '\r' character is passed
        // through to the output unchanged (the doc above the function
        // matches this — '\r' is not stripped despite an earlier doc claim).
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
        // \u{0001} is < 0x20 and not one of the recognised short escapes,
        // so the implementation falls through to \uXXXX form.
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
}

#[cfg(test)]
mod template_tests {
    /// Guards that the viewer template keeps the controls the JS wires up:
    /// the edge-label / node-name fields and the NodeKind picker. A typo in
    /// an element id or a deleted control would silently break the feature in
    /// the browser; this fails the build instead.
    #[test]
    fn template_contains_new_viewer_controls() {
        let t = super::HTML_DOT_TEMPLATE;
        for id in ["sEdgeLabel", "kindSel", "kindPrev", "kindNext", "kindCount"] {
            assert!(
                t.contains(&format!("id=\"{id}\"")),
                "viewer template missing id=\"{id}\""
            );
        }
        assert!(
            t.contains("function buildKindList"),
            "viewer template missing buildKindList()"
        );
    }
}

#[cfg(test)]
mod attr_quoting_tests {
    use super::{DotEmitter, DotStyle};

    /// An edge `label` containing DOT-special characters (a hyphen, as in the
    /// CFG's `if-true` / `if-false` branch labels) must be emitted quoted, or
    /// Graphviz fails with "syntax error near '-'". Regression test for a
    /// corrupted CFG HTML dump.
    #[test]
    fn edge_label_with_hyphen_is_quoted() {
        let mut e = DotEmitter::new("G", &DotStyle::dark_cfg());
        e.edge("0", "1", &[("label", "if-false"), ("style", "dashed")]);
        let dot = e.finish();
        assert!(
            dot.contains("label=\"if-false\""),
            "label not quoted:\n{dot}"
        );
        assert!(
            !dot.contains("label=if-false"),
            "raw unquoted label present:\n{dot}"
        );
        // Non-label attrs keep the caller-owns-quoting contract (bare ident).
        assert!(
            dot.contains("style=dashed"),
            "style should stay bare:\n{dot}"
        );
    }

    /// A node `extra` label is quoted+escaped the same way.
    #[test]
    fn node_extra_label_is_quoted() {
        let mut e = DotEmitter::new("G", &DotStyle::dark_cfg());
        e.node("n0", "lbl", "box", &[("label", "a-b")]);
        let dot = e.finish();
        assert!(
            dot.contains("label=\"a-b\""),
            "extra label not quoted:\n{dot}"
        );
    }
}
