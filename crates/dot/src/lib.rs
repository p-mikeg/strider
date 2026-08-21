use std::fmt::{Debug, Write};
use std::path::Path;

pub type Result<T> = anyhow::Result<T>;

const HTML_DOT_TEMPLATE: &str = include_str!("../assets/graph_template_dot.html");

/// `@viz-js/viz` v3.5.0 standalone build: Graphviz 11.x as Wasm, the Wasm
/// itself base64-embedded in the JS. Inlined into the template so the
/// generated HTML is self-contained: no CDN fetch, no `.wasm` side-load.
const VIZ_STANDALONE_JS: &str = include_str!("../assets/vendored/viz-standalone.js");

pub fn viz_standalone_js() -> &'static str {
    VIZ_STANDALONE_JS
}

/// `svg-pan-zoom` v3.6.1 minified. Inlined for the same reason as
/// [`VIZ_STANDALONE_JS`].
const SVG_PAN_ZOOM_JS: &str = include_str!("../assets/vendored/svg-pan-zoom.min.js");

/// A graph type that can be serialised to Graphviz DOT format node by node.
pub trait GraphDotDumper {
    type Node;
    // Bound is Display, not `std::error::Error`, so impls can pick
    // `type Error = anyhow::Error`, which implements `Display` alone.
    type Error: Debug + std::fmt::Display + Send + Sync + 'static;
    type State;

    /// Creates the mutable state threaded through all [`Self::dump_as_dot`] calls.
    fn create_initial_state(&self) -> Self::State;

    fn iter_nodes(&self) -> impl IntoIterator<Item = Self::Node>;

    /// Emits DOT statements (nodes + edges) for a single graph node.
    ///
    /// # Errors
    /// Whatever the dumper's own data source raises.
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
    /// `fontname` is the only attribute that differs between [`Self::dark`]
    /// and [`Self::dark_cfg`].
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

    pub fn dark() -> Self {
        Self::dark_with_font("monospace")
    }

    /// `Courier` instead of `monospace`: only Courier's character-width
    /// metrics are bundled into the Graphviz/viz.js layout engine, and
    /// without them multiline labels overflow their boxes in the WASM render.
    pub fn dark_cfg() -> Self {
        Self::dark_with_font("Courier")
    }

    pub fn empty() -> Self {
        Self {
            graph: vec![],
            node: vec![],
            edge: vec![],
        }
    }
}

/// Escapes a string for use as a DOT double-quoted label.
///
/// The two-char sequences `\n` / `\l` / `\r` pass through verbatim: they are
/// DOT's own line-break escapes (centre / left / right justified) and callers
/// hand-emit them. Any other backslash doubles. A literal newline becomes
/// `\n`; a literal carriage return is left alone.
fn escape_dot_label(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => match chars.peek() {
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

/// Wraps `s` in a JSON string literal.
///
/// Beyond the usual JSON escapes, `<` is unconditionally escaped so a label
/// containing `</script>` cannot break out of the
/// `<script type="application/json">` element the DOT source is embedded in.
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
            '<' => out.push_str("\\u003c"),
            c if (c as u32) < 0x20 => {
                // Writing to a `String` is infallible, but clippy::expect_used
                // forbids the literal `.expect`, hence `let _ =`.
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Low-level builder that accumulates a Graphviz `digraph { … }` string.
pub struct DotEmitter {
    out: String,
}

impl DotEmitter {
    pub fn new(name: &str, style: &DotStyle) -> Self {
        let mut s = String::new();
        // Always quote the digraph name so a caller-supplied name with
        // whitespace or punctuation still produces valid DOT. Graphviz parses
        // quoted and bare identifiers identically where both are legal.
        s.push_str("digraph \"");
        s.push_str(&escape_dot_label(name));
        s.push_str("\" {\n");

        emit_attr_block(&mut s, "graph", &style.graph);
        emit_attr_block(&mut s, "node", &style.node);
        emit_attr_block(&mut s, "edge", &style.edge);

        Self { out: s }
    }

    /// `id` and `label` are escaped and quoted. `extra` attributes are
    /// inserted verbatim; the caller owns quoting the value (a hex colour
    /// needs its own `"..."`, a bare ident like `dashed` does not).
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

    /// Endpoints are escaped; `extra` follows the same caller-quotes-the-value
    /// contract as [`DotEmitter::node`].
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

    pub fn finish(mut self) -> String {
        self.out.push_str("}\n");
        self.out
    }
}

/// Appends one `key=value` attribute; callers supply their own framing
/// (leading comma, separator, bracket block).
///
/// `label` is the one exception to the caller-owns-quoting contract: its value
/// is free text, and a DOT-special character in it (a hyphen, colon, space)
/// makes Graphviz abort with "syntax error near '-'". So labels are quoted and
/// escaped here; everything else is passed through as given.
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

/// Node count above which the viewer defaults to `sfdp`: `dot`'s layered
/// layout is superlinear and stalls the browser on large graphs. The viewer's
/// engine picker still lets the user switch back.
pub const DEFAULT_SFDP_NODE_THRESHOLD: usize = 2000;

/// Approximate: counting `[label=` also catches edge-label attribute blocks.
/// It only picks the default engine, so over-counting harmlessly biases large
/// graphs toward `sfdp`.
pub(crate) fn dot_node_count(dot: &str) -> usize {
    dot.matches("[label=").count()
}

/// Wraps a [`GraphDotDumper`] and produces DOT / HTML output.
pub struct GraphDot<G: GraphDotDumper> {
    dumper: G,
    style: DotStyle,
    name: String,
}

impl<G: GraphDotDumper> GraphDot<G> {
    /// The emitted digraph is named `"G"`.
    pub fn new(dumper: G, style: DotStyle) -> Self {
        Self {
            dumper,
            style,
            name: "G".to_string(),
        }
    }

    /// # Errors
    /// Forwards any error from [`GraphDotDumper::dump_as_dot`].
    pub fn as_dot(&self) -> anyhow::Result<String> {
        self.as_dot_with_state().map(|(dot, _)| dot)
    }

    /// The DOT source plus the state the dumper accumulated while rendering.
    /// For dumpers whose DOT ids are not the node ids, that state is the
    /// mapping back from an emitted id to the node it stands for.
    ///
    /// # Errors
    /// Forwards any error from [`GraphDotDumper::dump_as_dot`].
    pub fn as_dot_with_state(&self) -> anyhow::Result<(String, G::State)> {
        let mut dot = DotEmitter::new(&self.name, &self.style);
        let mut state = self.dumper.create_initial_state();
        for node in self.dumper.iter_nodes() {
            self.dumper
                .dump_as_dot(node, &mut dot, &mut state)
                .map_err(|e| anyhow::anyhow!("dot dump error: {e}"))?;
        }

        Ok((dot.finish(), state))
    }

    /// An interactive HTML page rendering the DOT client-side via Graphviz
    /// WASM. The vendored JS payloads are inlined, so the page works offline.
    /// The single place every `graph_template_dot.html` placeholder is
    /// substituted; a template revision adding one wires it up here.
    ///
    /// # Errors
    /// Same as [`Self::as_dot`].
    pub fn as_html_from_dot(&self) -> anyhow::Result<String> {
        let dot_src = self.as_dot()?;
        // A ~1800-node IR is ~1400 ranks deep, and `dot`'s mincross explodes
        // on the resulting virtual nodes, hanging the browser.
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

    /// # Errors
    /// From the dumper, or if writing `out_path` fails.
    pub fn dump_as_html(&self, out_path: impl AsRef<Path>) -> anyhow::Result<()> {
        std::fs::write(out_path, self.as_html_from_dot()?)?;
        Ok(())
    }

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
        assert_eq!(escape_dot_label("a\nb"), "a\\nb");
    }

    #[test]
    fn escape_dot_label_recognised_dot_escapes_pass_through() {
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
        // A *literal* '\r' is not stripped, unlike the two-char `\r` escape.
        assert_eq!(escape_dot_label("a\rb"), "a\rb");
    }

    #[test]
    fn escape_dot_label_combined_inputs_round_trip() {
        // A real IR/CFG label: both a DOT escape (\l) and a literal newline.
        let input = "Instruction(addr=0x401000)\n\\l0x401000: ADD";
        let want = "Instruction(addr=0x401000)\\n\\l0x401000: ADD";
        assert_eq!(escape_dot_label(input), want);
    }

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
        assert_eq!(json_quote("\u{0001}"), "\"\\u0001\"");
        assert_eq!(json_quote("\u{001f}"), "\"\\u001f\"");
        // 0x20 (space) is the boundary: it must NOT be unicode-escaped.
        assert_eq!(json_quote(" "), "\" \"");
    }

    #[test]
    fn json_quote_passes_through_high_unicode_unchanged() {
        // No surrogate expansion; any compliant JSON parser takes UTF-8.
        assert_eq!(json_quote("café"), "\"café\"");
        assert_eq!(json_quote("→"), "\"→\"");
    }

    #[test]
    fn json_quote_escapes_left_angle_to_avoid_script_break_out() {
        // A `</script>` in a label would otherwise terminate the surrounding
        // script tag and spill the rest of the JSON into the document body.
        assert_eq!(json_quote("</script>"), "\"\\u003c/script>\"");
    }

    #[test]
    fn json_quote_escapes_bare_left_angle_too() {
        // Unconditional on `<`, not just `</`. Matching only `</` would drag
        // whitespace and case tolerance into the encoder.
        assert_eq!(json_quote("a<b"), "\"a\\u003cb\"");
    }
}

#[cfg(test)]
mod template_tests {
    /// A typo in an element id, or a deleted control, would silently break the
    /// viewer in the browser. Fail the build instead.
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

    /// A hyphen in an edge label (the CFG's `if-true` / `if-false`) makes
    /// Graphviz fail with "syntax error near '-'" unless the label is quoted.
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
        // Non-label attrs keep the caller-owns-quoting contract.
        assert!(
            dot.contains("style=dashed"),
            "style should stay bare:\n{dot}"
        );
    }

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
