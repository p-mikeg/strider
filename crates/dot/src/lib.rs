use std::fmt::Write;
use std::path::Path;

const HTML_DOT_TEMPLATE: &str = include_str!("../assets/graph_template_dot.html");

/// `@viz-js/viz` v3.5.0 standalone build: Graphviz 11.x as base64-embedded
/// Wasm, inlined so the page is self-contained.
pub const VIZ_STANDALONE_JS: &str = include_str!("../assets/vendored/viz-standalone.js");

/// `svg-pan-zoom` v3.6.1 minified.
const SVG_PAN_ZOOM_JS: &str = include_str!("../assets/vendored/svg-pan-zoom.min.js");

/// A restricted render: the nodes it draws, ascending, and the one it
/// highlights.
pub struct View<N> {
    nodes: Vec<N>,
    center: N,
}

impl<N: Copy + Ord> View<N> {
    pub fn new(nodes: impl IntoIterator<Item = N>, center: N) -> Self {
        let mut nodes: Vec<N> = nodes.into_iter().collect();
        nodes.sort_unstable();
        Self { nodes, center }
    }
}

/// A graph type that can be serialised to Graphviz DOT format node by node.
pub trait GraphDotDumper {
    type Node: Copy + Ord;
    /// State threaded through the [`Self::dump_as_dot`] calls of one render.
    type State: Default;

    /// Every node of a whole render; a restricted one draws its view's.
    fn iter_nodes(&self) -> impl IntoIterator<Item = Self::Node>;

    /// `None` renders the whole graph.
    fn view(&self) -> Option<&View<Self::Node>> {
        None
    }

    /// Whether `node` is drawn; an edge out of an undrawn node is dropped.
    fn in_view(&self, node: Self::Node) -> bool {
        self.view()
            .is_none_or(|v| v.nodes.binary_search(&node).is_ok())
    }

    fn is_center(&self, node: Self::Node) -> bool {
        self.view().is_some_and(|v| v.center == node)
    }

    /// The highlight attributes of the view's center, else none.
    fn center_attrs(&self, node: Self::Node) -> &'static [(&'static str, &'static str)] {
        if self.is_center(node) {
            &[("color", "#ffcc00"), ("penwidth", "2.5")]
        } else {
            &[]
        }
    }

    /// Emits the DOT statements (node and edges) of one graph node.
    ///
    /// # Errors
    /// Whatever the dumper's data source raises.
    fn dump_as_dot(
        &self,
        node: Self::Node,
        out: &mut DotEmitter,
        state: &mut Self::State,
    ) -> anyhow::Result<()>;
}

/// Graph, node and edge default attributes. `Default` sets none.
#[derive(Clone, Default)]
pub struct DotStyle {
    graph: Vec<(&'static str, &'static str)>,
    node: Vec<(&'static str, &'static str)>,
    edge: Vec<(&'static str, &'static str)>,
}

impl DotStyle {
    fn dark_with_font(fontname: &'static str) -> Self {
        Self {
            graph: vec![
                ("rankdir", "TB"),
                ("bgcolor", "#1e1e1e"),
                ("fontcolor", "white"),
            ],
            node: vec![
                ("shape", "box"),
                ("style", "filled,rounded"),
                ("fillcolor", "#2d2d2d"),
                ("color", "#888888"),
                ("fontcolor", "white"),
                ("fontname", fontname),
                ("margin", "0.2"),
            ],
            edge: vec![
                ("color", "#aaaaaa"),
                ("fontcolor", "white"),
                ("penwidth", "1.2"),
            ],
        }
    }

    pub fn dark() -> Self {
        Self::dark_with_font("monospace")
    }

    /// `Courier`: the only font whose metrics viz.js bundles, so multiline
    /// labels fit their boxes.
    pub fn dark_cfg() -> Self {
        Self::dark_with_font("Courier")
    }
}

/// Escapes a string for a DOT double-quoted label.
///
/// Quotes and backslashes are escaped and a newline becomes `\n`; every other
/// control character but tab, and every bidi formatting character, becomes
/// `\xNN`.
fn escape_dot_label(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push('\t'),
            c if c.is_control() || is_bidi_format(c) => {
                // The backslash doubled, so Graphviz renders `\x` as text.
                let _ = write!(out, "\\\\x{:02x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

/// The Unicode bidi overrides, embeddings, isolates and marks.
fn is_bidi_format(c: char) -> bool {
    matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
                | '\u{200e}' | '\u{200f}' | '\u{061c}')
}

/// Wraps `s` in a JSON string literal, `<` escaped so it cannot close the
/// `<script>` element it is embedded in.
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
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Accumulates a Graphviz `digraph { ... }` string.
pub struct DotEmitter {
    out: String,
    nodes: usize,
}

impl DotEmitter {
    pub fn new(name: &str, style: &DotStyle) -> Self {
        let mut s = String::new();
        // Quoted, so any name is valid DOT.
        s.push_str("digraph \"");
        s.push_str(&escape_dot_label(name));
        s.push_str("\" {\n");

        emit_attr_block(&mut s, "graph", &style.graph);
        emit_attr_block(&mut s, "node", &style.node);
        emit_attr_block(&mut s, "edge", &style.edge);

        Self { out: s, nodes: 0 }
    }

    /// `id`, `label`, `shape` and every `extra` value are escaped and quoted,
    /// so a value is passed bare (`#ffcc00`, not `"#ffcc00"`).
    pub fn node(&mut self, id: &str, label: &str, shape: &str, extra: &[(&str, &str)]) {
        self.node_inner(id, &escape_dot_label(label), shape, extra);
    }

    fn node_inner(&mut self, id: &str, escaped_label: &str, shape: &str, extra: &[(&str, &str)]) {
        self.nodes += 1;
        let id = escape_dot_label(id);
        self.out.push_str("  \"");
        self.out.push_str(&id);
        self.out.push_str("\" [");
        push_quoted_attr(&mut self.out, "label", escaped_label);
        self.out.push_str(", ");
        push_attr(&mut self.out, "shape", shape);

        for (k, v) in extra {
            self.out.push_str(", ");
            push_attr(&mut self.out, k, v);
        }

        self.out.push_str("];\n");
    }

    /// [`Self::node`] labelled `head`, then each of `lines` after a DOT `\l`
    /// (left-justified) break, then a closing `\l`; each piece is escaped.
    pub fn node_left_lines<S: AsRef<str>>(
        &mut self,
        id: &str,
        head: &str,
        lines: impl IntoIterator<Item = S>,
        shape: &str,
        extra: &[(&str, &str)],
    ) {
        let mut label = escape_dot_label(head);
        for line in lines {
            label.push_str("\\l");
            label.push_str(&escape_dot_label(line.as_ref()));
        }
        label.push_str("\\l");
        self.node_inner(id, &label, shape, extra);
    }

    /// Endpoints and `extra` values are escaped and quoted, as in
    /// [`DotEmitter::node`].
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

/// Appends one `key="value"` attribute, the value escaped, without framing.
fn push_attr(out: &mut String, k: &str, v: &str) {
    push_quoted_attr(out, k, &escape_dot_label(v));
}

/// [`push_attr`] for a value the caller already escaped.
fn push_quoted_attr(out: &mut String, k: &str, escaped: &str) {
    out.push_str(k);
    out.push_str("=\"");
    out.push_str(escaped);
    out.push('"');
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
/// layout stalls the browser on large graphs.
const DEFAULT_SFDP_NODE_THRESHOLD: usize = 2000;

/// Wraps a [`GraphDotDumper`] and produces DOT / HTML output.
pub struct GraphDot<G: GraphDotDumper> {
    dumper: G,
    style: DotStyle,
}

impl<G: GraphDotDumper> GraphDot<G> {
    /// The emitted digraph is named `"G"`.
    pub fn new(dumper: G, style: DotStyle) -> Self {
        Self { dumper, style }
    }

    /// # Errors
    /// From [`GraphDotDumper::dump_as_dot`].
    pub fn as_dot(&self) -> anyhow::Result<String> {
        Ok(self.emit()?.finish())
    }

    fn emit(&self) -> anyhow::Result<DotEmitter> {
        let mut dot = DotEmitter::new("G", &self.style);
        let mut state = G::State::default();
        let mut dump = |node| {
            self.dumper
                .dump_as_dot(node, &mut dot, &mut state)
                .map_err(|e| anyhow::anyhow!("dot dump error: {e}"))
        };
        match self.dumper.view() {
            Some(view) => view.nodes.iter().try_for_each(|&node| dump(node))?,
            None => self.dumper.iter_nodes().into_iter().try_for_each(dump)?,
        }
        Ok(dot)
    }

    /// A self-contained interactive HTML page rendering the DOT client-side.
    ///
    /// # Errors
    /// Same as [`Self::as_dot`].
    pub fn as_html_from_dot(&self) -> anyhow::Result<String> {
        let dot = self.emit()?;
        let engine = if dot.nodes > DEFAULT_SFDP_NODE_THRESHOLD {
            "sfdp"
        } else {
            "dot"
        };
        let dot_json = json_quote(&dot.finish());
        // In template order; each occurs once.
        let subs = [
            ("__VIZ_STANDALONE_JS__", VIZ_STANDALONE_JS),
            ("__SVG_PAN_ZOOM_JS__", SVG_PAN_ZOOM_JS),
            ("__DOT_JSON__", &dot_json),
            ("__DEFAULT_ENGINE__", engine),
        ];
        let len = HTML_DOT_TEMPLATE.len() + subs.iter().map(|(_, v)| v.len()).sum::<usize>();
        let mut page = String::with_capacity(len);
        let mut rest = HTML_DOT_TEMPLATE;
        for (placeholder, value) in subs {
            let (head, tail) = rest
                .split_once(placeholder)
                .expect("template holds each placeholder once, in this order");
            page.push_str(head);
            page.push_str(value);
            rest = tail;
        }
        page.push_str(rest);
        Ok(page)
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
mod label_tests {
    use super::{escape_dot_label, json_quote};

    /// Every backslash doubles, a DOT line-break bigram included: in a label
    /// it is content, not markup.
    #[test]
    fn escape_dot_label_escapes_exactly_what_dot_needs() {
        for (input, want) in [
            ("hello world", "hello world"),
            ("", ""),
            ("a\"b", "a\\\"b"),
            ("a\nb", "a\\nb"),
            ("a\\nb", "a\\\\nb"),
            ("a\\lb", "a\\\\lb"),
            ("a\\rb", "a\\\\rb"),
            ("a\\b", "a\\\\b"),
            ("\\", "\\\\"),
            ("a\tb", "a\tb"),
            ("C:\\lib\\name", "C:\\\\lib\\\\name"),
        ] {
            assert_eq!(escape_dot_label(input), want, "input {input:?}");
        }
    }

    #[test]
    fn escape_dot_label_makes_control_characters_printable() {
        for (input, want) in [
            ("a\u{0}b", "a\\\\x00b"),
            ("a\u{1}b", "a\\\\x01b"),
            ("a\rb", "a\\\\x0db"),
            ("a\u{1b}b", "a\\\\x1bb"),
            ("a\u{1f}b", "a\\\\x1fb"),
            ("a\u{7f}b", "a\\\\x7fb"),
            ("a\u{9f}b", "a\\\\x9fb"),
            // The boundary on each side: neither is a control character.
            ("a b", "a b"),
            ("a\u{a0}b", "a\u{a0}b"),
            // Bidi formatting: not control characters, but they reorder the
            // text around them in the rendered page.
            ("a\u{202e}b", "a\\\\x202eb"),
            ("a\u{2066}b", "a\\\\x2066b"),
            ("a\u{200f}b", "a\\\\x200fb"),
            // Ordinary non-ASCII stays verbatim.
            ("a\u{5d0}b", "a\u{5d0}b"),
        ] {
            assert_eq!(escape_dot_label(input), want, "input {input:?}");
        }
    }

    /// `<` is escaped unconditionally, not just `</`.
    #[test]
    fn json_quote_escapes_exactly_what_a_script_block_needs() {
        for (input, want) in [
            ("", "\"\""),
            ("hello", "\"hello\""),
            ("\"", "\"\\\"\""),
            ("\\", "\"\\\\\""),
            ("\n", "\"\\n\""),
            ("\r", "\"\\r\""),
            ("\t", "\"\\t\""),
            ("\u{0001}", "\"\\u0001\""),
            ("\u{001f}", "\"\\u001f\""),
            (" ", "\" \""),
            ("café", "\"café\""),
            ("->", "\"->\""),
            ("</script>", "\"\\u003c/script>\""),
            ("a<b", "\"a\\u003cb\""),
        ] {
            assert_eq!(json_quote(input), want, "input {input:?}");
        }
    }
}

#[cfg(test)]
mod template_tests {
    #[test]
    fn template_contains_viewer_controls() {
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
        assert!(dot.contains("style=\"dashed\""), "style not quoted:\n{dot}");
    }

    /// Every attribute value is free text to the emitter, so a quote or a
    /// newline in one cannot end the value early.
    #[test]
    fn non_label_attr_value_is_escaped() {
        let mut e = DotEmitter::new("G", &DotStyle::default());
        e.node("n0", "lbl", "box", &[("tooltip", "say \"hi\"\nbye")]);
        let dot = e.finish();
        assert!(
            dot.contains("tooltip=\"say \\\"hi\\\"\\nbye\""),
            "tooltip not escaped:\n{dot}"
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

#[cfg(test)]
mod style_tests {
    use super::DotStyle;

    #[test]
    fn dark_cfg_inherits_other_dark_attrs_unchanged() {
        let dark = DotStyle::dark();
        let cfg = DotStyle::dark_cfg();

        assert_eq!(dark.node.len(), cfg.node.len());

        for (k, v) in &dark.node {
            if *k == "fontname" {
                continue;
            }
            assert!(
                cfg.node.iter().any(|(ck, cv)| ck == k && cv == v),
                "dark_cfg.node missing or altered ({k}, {v})",
            );
        }

        assert_eq!(dark.graph, cfg.graph);
        assert_eq!(dark.edge, cfg.edge);
    }
}
