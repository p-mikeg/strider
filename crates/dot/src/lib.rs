use std::{fmt::Debug, io::Write};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error<E> {
    #[error("svg conversion error {0:?}")]
    SvgConversionError(String),

    #[error(transparent)]
    IoError(#[from] std::io::Error),

    #[error(transparent)]
    DotDumpError(E)
}

/// the result type using our error.
pub type Result<T,E> = std::result::Result<T, Error<E>>;


const HTML_SVG_TEMPLATE: &str = include_str!("../assets/graph_template_svg.html");
const HTML_DOT_TEMPLATE: &str = include_str!("../assets/graph_template_dot.html");

pub trait GraphDotDumper {
    type Node;
    type Error: Debug;
    type State;

    fn create_initial_state(&self) -> Self::State;

    /// Iterate all nodes in the graph
    fn iter_nodes(&self) -> impl IntoIterator<Item = Self::Node>;

    /// Dump a single node (including edges) into the emitter
    fn dump_as_dot(&self, node: Self::Node, out: &mut DotEmitter, state: &mut Self::State) -> core::result::Result<(), Self::Error>;
}


#[derive(Clone)]
pub struct DotStyle {
    pub graph: Vec<(&'static str, &'static str)>,
    pub node:  Vec<(&'static str, &'static str)>,
    pub edge:  Vec<(&'static str, &'static str)>,
}

impl DotStyle {
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
            ],
            edge: vec![
                ("color", "\"#aaaaaa\""),
                ("fontcolor", "white"),
                ("penwidth", "1.2"),
            ],
        }
    }

    pub fn empty() -> Self {
        Self {
            graph: vec![],
            node: vec![],
            edge: vec![]
        }
    }
}

pub struct DotEmitter {
    out: String,
}

impl DotEmitter {
    pub fn new(name: &str, style: &DotStyle) -> Self {
        let mut s = String::new();
        s.push_str(&format!("digraph {name} {{\n"));

        emit_attr_block(&mut s, "graph", &style.graph);
        emit_attr_block(&mut s, "node",  &style.node);
        emit_attr_block(&mut s, "edge",  &style.edge);

        Self { out: s }
    }

    pub fn node(
        &mut self,
        id: &str,
        label: &str,
        shape: &str,
        extra: &[(&str, &str)],
    ) {
        self.out.push_str(&format!("  \"{id}\" [label=\"{label}\", shape={shape}"));

        for (k, v) in extra {
            self.out.push_str(&format!(", {k}={v}"));
        }

        self.out.push_str("];\n");
    }

    pub fn edge(
        &mut self,
        from: &str,
        to: &str,
        extra: &[(&str, &str)],
    ) {
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

pub struct GraphDot<G: GraphDotDumper + Sized> {
    dumper: G,
    style: DotStyle,
    name: String,
}

impl<G: GraphDotDumper> GraphDot<G> {
    pub fn new(dumper: G, style: DotStyle) -> Self {
        Self {
            dumper: dumper,
            style,
            name: "G".to_string(),
        }
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    fn build_dot(&self) -> Result<String, G::Error> {
        let mut dot = DotEmitter::new(&self.name, &self.style);
        let mut state = self.dumper.create_initial_state();
        for node in self.dumper.iter_nodes() {
            self.dumper.dump_as_dot(node, &mut dot, &mut state)
                .map_err(|e| Error::DotDumpError(e))?;
        }

        Ok(dot.finish())
    }

    pub fn as_dot(&self) -> Result<String, G::Error> {
        self.build_dot()
    }

    pub fn as_svg(&self) -> Result<String, G::Error> {
        let mut child = std::process::Command::new("dot")
            .arg("-Tsvg")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| Error::SvgConversionError(e.to_string()))?;

        {
            let stdin = child.stdin.as_mut().ok_or_else(|| {
                Error::SvgConversionError("failed to open dot stdin".to_owned())
            })?;

            stdin
                .write_all(self.as_dot()?.as_bytes())
                .map_err(|e| Error::SvgConversionError(e.to_string()))?;
        }

        let output = child
            .wait_with_output()
            .map_err(|e| Error::SvgConversionError(e.to_string()))?;

        if !output.status.success() {
            return Err(Error::SvgConversionError(
                String::from_utf8_lossy(&output.stderr).to_string(),
            ));
        }

        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    pub fn as_html_from_svg(&self) -> Result<String, G::Error> {
        Ok(HTML_SVG_TEMPLATE.replace("__SVG__", &self.as_svg()?))
    }

    pub fn as_html_from_dot(&self) -> Result<String, G::Error> {
        Ok(HTML_DOT_TEMPLATE.replace("__DOT__", &self.as_dot()?))
    }

    pub fn dump_as_html(&self, out_path: &str) -> Result<(), G::Error> {
        std::fs::write(out_path, self.as_html_from_svg()?)
            .map_err(Error::IoError)?;
        Ok(())
    }

    pub fn dump_as_dot(&self, out_path: &str) -> Result<(), G::Error> {
        std::fs::write(out_path, self.as_dot()?)
            .map_err(Error::IoError)?;
        Ok(())
    }
}