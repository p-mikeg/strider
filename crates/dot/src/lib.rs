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

use std::{fmt::Debug, io::Write};

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

    /// Like [`Self::dark`] but with CFG-appropriate node sizing: `Courier` font
    /// (known metrics in viz.js) and extra margin so multiline labels fit.
    #[must_use]
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
/// - `\` → `\\`
/// - newline → `\n` (Graphviz left-justified line break)
/// - carriage-return stripped
fn escape_dot_label(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => out.push_str("\\\""),
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
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out
}

/// Wraps `s` in a JSON string literal with full escaping.
///
/// Used to safely embed the DOT source inside an HTML file without risk of
/// breaking JavaScript template literals or HTML structure.
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
        s.push_str(&format!("digraph {name} {{\n"));

        emit_attr_block(&mut s, "graph", &style.graph);
        emit_attr_block(&mut s, "node", &style.node);
        emit_attr_block(&mut s, "edge", &style.edge);

        Self { out: s }
    }

    /// Emits a node statement.  The `label` is automatically escaped for DOT.
    pub fn node(&mut self, id: &str, label: &str, shape: &str, extra: &[(&str, &str)]) {
        let label = escape_dot_label(label);
        self.out
            .push_str(&format!("  \"{id}\" [label=\"{label}\", shape={shape}"));

        for (k, v) in extra {
            self.out.push_str(&format!(", {k}={v}"));
        }

        self.out.push_str("];\n");
    }

    /// Emits a directed edge statement.
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

    out.push_str(&format!("  {name} [\n"));
    for (k, v) in attrs {
        out.push_str(&format!("    {k}={v},\n"));
    }
    out.push_str("  ];\n\n");
}

// ── GraphDot ──────────────────────────────────────────────────────────────────

/// Wraps a [`GraphDotDumper`] and produces DOT / SVG / HTML output.
pub struct GraphDot<G: GraphDotDumper + Sized> {
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

        if !output.status.success() {
            return Err(svg_err(String::from_utf8_lossy(&output.stderr).to_string()));
        }

        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
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
            svg = svg[pos..].to_owned();
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
    pub fn dump_as_html(&self, out_path: &str) -> Result<(), G::Error> {
        std::fs::write(out_path, self.as_html_from_dot()?)?;
        Ok(())
    }

    /// Writes the raw DOT source to `out_path`.
    ///
    /// # Errors
    /// Same as [`Self::dump_as_html`].
    pub fn dump_as_dot(&self, out_path: &str) -> Result<(), G::Error> {
        std::fs::write(out_path, self.as_dot()?)?;
        Ok(())
    }
}
