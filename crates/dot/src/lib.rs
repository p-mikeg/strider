#![cfg_attr(
    test,
    allow(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::unreachable
    )
)]

//! Graphviz `.dot` and interactive `.html` rendering for Strider graphs.
//!
//! Implement [`GraphDotDumper`] for a graph type to obtain `.dot` and `.html`
//! output via [`GraphDot`].
//!
//! # Rendering pipeline
//!
//! ```text
//! GraphDotDumper::dump_as_dot() ──► DotEmitter ──► .dot string
//!                                                       │
//!                                          ┌────────────┴────────────┐
//!                                          ▼                         ▼
//!                                   dot(1) → SVG              embedded DOT
//!                                       │                           │
//!                                       ▼                           ▼
//!                               as_html_from_svg             as_html_from_dot
//! ```
//!
//! [`GraphDot::as_html_from_dot`] embeds the raw DOT source in an HTML page
//! that renders it client-side via Graphviz WASM ([`@viz-js/viz`]).  No local
//! `dot` install is required.  This is what [`GraphDot::dump_as_html`] uses.
//!
//! [`GraphDot::as_html_from_svg`] calls the system `dot` binary and inlines
//! the resulting SVG.  Useful for offline / headless export.
//!
//! [`DotStyle`] provides pre-built dark and empty visual themes.
//! [`DotEmitter`] is a low-level string builder for Graphviz DOT syntax.
//!
//! [`@viz-js/viz`]: https://github.com/mdaines/viz-js

use std::{fmt::Debug, io::Write, path::Path};

pub mod error;
pub use error::{Error, ErrorKind, Result};

const HTML_SVG_TEMPLATE: &str = include_str!("../assets/graph_template_svg.html");
const HTML_DOT_TEMPLATE: &str = include_str!("../assets/graph_template_dot.html");

/// A graph type that can be serialised to Graphviz DOT format node by node.
pub trait GraphDotDumper {
    type Node;
    type Error: Debug;
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
    /// Returns a dark-background theme suitable for modern editors / terminals.
    #[must_use]
    pub fn dark() -> Self {
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
                ("fontname", "monospace"),
                ("margin", "0.2"),
            ],
            edge: vec![
                ("color", "\"#aaaaaa\""),
                ("fontcolor", "white"),
                ("penwidth", "1.2"),
            ],
        }
    }

    /// Like [`Self::dark`] but with CFG-appropriate node typography: replaces the
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

    /// Returns an empty theme (no default attributes).
    #[must_use]
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
                let _ = std::fmt::write(&mut out, format_args!("\\u{:04x}", c as u32));
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
    #[must_use]
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
            self.out.push_str(k);
            self.out.push('=');
            self.out.push_str(v);
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
                self.out.push_str(k);
                self.out.push('=');
                self.out.push_str(v);
            }
            self.out.push(']');
        }

        self.out.push_str(";\n");
    }

    /// Finalises the digraph and returns the complete DOT string.
    #[must_use]
    pub fn finish(mut self) -> String {
        self.out.push_str("}\n");
        self.out
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
        out.push_str(k);
        out.push('=');
        out.push_str(v);
        out.push_str(",\n");
    }
    out.push_str("  ];\n\n");
}

// ── GraphDot ──────────────────────────────────────────────────────────────────

/// Wraps a [`GraphDotDumper`] and produces DOT / SVG / HTML output.
pub struct GraphDot<G: GraphDotDumper> {
    dumper: G,
    style: DotStyle,
    name: String,
}

impl<G: GraphDotDumper> GraphDot<G> {
    /// Creates a new `GraphDot` with the given dumper and visual style.
    pub fn new(dumper: G, style: DotStyle) -> Self {
        Self {
            dumper,
            style,
            name: "G".to_string(),
        }
    }

    /// Overrides the digraph name (default: `"G"`).
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    fn build_dot(&self) -> Result<String, G::Error> {
        let mut dot = DotEmitter::new(&self.name, &self.style);
        let mut state = self.dumper.create_initial_state();
        for node in self.dumper.iter_nodes() {
            self.dumper
                .dump_as_dot(node, &mut dot, &mut state)
                .map_err(|e| Error::from(ErrorKind::DotDumpError(e)))?;
        }

        Ok(dot.finish())
    }

    /// Returns the raw DOT source string.
    ///
    /// # Errors
    /// Forwards any `Self::Error` returned by the underlying
    /// [`GraphDotDumper::dump_as_dot`] for any node.
    pub fn as_dot(&self) -> Result<String, G::Error> {
        self.build_dot()
    }

    /// Calls the system `dot` binary to render SVG from the DOT source.
    ///
    /// Returns an error if `dot` is not installed or the conversion fails.
    ///
    /// # Errors
    /// - [`ErrorKind::DotDumpError`] propagated from the dumper.
    /// - [`ErrorKind::SvgConversionError`] if the system `dot` binary cannot
    ///   be spawned, returns a non-zero exit status, or its stdin/stdout
    ///   pipes cannot be opened.
    pub fn as_svg(&self) -> Result<String, G::Error> {
        let dot_src = self.as_dot()?;

        let svg_err =
            |msg: String| -> Error<G::Error> { ErrorKind::SvgConversionError(msg).into() };

        let mut child = std::process::Command::new("dot")
            .arg("-Tsvg")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| svg_err(e.to_string()))?;

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

    /// Produces an HTML page that inlines a pre-rendered SVG with pan/zoom.
    ///
    /// Requires the system `dot` binary.  For a browser-rendered interactive
    /// viewer that works without `dot`, use [`Self::as_html_from_dot`] instead.
    ///
    /// # Errors
    /// Same as [`Self::as_svg`].
    pub fn as_html_from_svg(&self) -> Result<String, G::Error> {
        let mut svg = self.as_svg()?;
        // Strip the XML declaration and DOCTYPE that `dot` emits — they can
        // confuse HTML parsers when the SVG is inlined in a <body>.
        if let Some(pos) = svg.find("<svg") {
            svg.drain(..pos);
        }
        Ok(HTML_SVG_TEMPLATE.replace("__SVG__", &svg))
    }

    /// Produces an interactive HTML page that renders the DOT source
    /// client-side via Graphviz WASM.  No local `dot` install required.
    ///
    /// The DOT source is embedded as a JSON string inside a
    /// `<script type="application/json">` element, so it is safe regardless of
    /// what characters appear in node labels.
    ///
    /// # Errors
    /// Same as [`Self::as_dot`].
    pub fn as_html_from_dot(&self) -> Result<String, G::Error> {
        let dot_src = self.as_dot()?;
        Ok(HTML_DOT_TEMPLATE.replace("__DOT_JSON__", &json_quote(&dot_src)))
    }

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
    /// Same as [`Self::dump_as_html`].
    pub fn dump_as_dot(&self, out_path: impl AsRef<Path>) -> Result<(), G::Error> {
        std::fs::write(out_path, self.as_dot()?)?;
        Ok(())
    }
}

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
