#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

use dot::{DotEmitter, DotStyle, GraphDot, GraphDotDumper};

struct TestDumper {
    n: usize,
}

impl GraphDotDumper for TestDumper {
    type Node = usize;
    type Error = anyhow::Error;
    type State = ();

    fn create_initial_state(&self) -> Self::State {}

    fn iter_nodes(&self) -> impl IntoIterator<Item = Self::Node> {
        0..self.n
    }

    fn dump_as_dot(
        &self,
        node: Self::Node,
        out: &mut DotEmitter,
        _state: &mut Self::State,
    ) -> anyhow::Result<()> {
        out.node(&format!("n{node}"), &format!("node {node}"), "box", &[]);
        if node > 0 {
            out.edge(&format!("n{}", node - 1), &format!("n{node}"), &[]);
        }
        Ok(())
    }
}

fn render_html(n: usize) -> String {
    let gd = GraphDot::new(TestDumper { n }, DotStyle::dark());
    gd.as_html_from_dot().expect("html emit succeeded")
}

struct FailingDumper;

impl GraphDotDumper for FailingDumper {
    type Node = usize;
    type Error = anyhow::Error;
    type State = ();

    fn create_initial_state(&self) -> Self::State {}

    fn iter_nodes(&self) -> impl IntoIterator<Item = Self::Node> {
        0..1
    }

    fn dump_as_dot(
        &self,
        _node: Self::Node,
        _out: &mut DotEmitter,
        _state: &mut Self::State,
    ) -> anyhow::Result<()> {
        Err(anyhow::anyhow!("boom"))
    }
}

#[test]
fn dumper_error_propagates_wrapped() {
    // Pin the "dot dump error" prefix as well as the failure itself, so the
    // wrapping stays observable to callers matching on it.
    let gd = GraphDot::new(FailingDumper, DotStyle::dark());

    let dot_err = gd
        .as_dot()
        .expect_err("as_dot must surface the dumper error");
    let msg = format!("{dot_err}");
    assert!(
        msg.contains("dot dump error") && msg.contains("boom"),
        "expected wrapped dumper error, got: {msg}"
    );

    let html_err = gd
        .as_html_from_dot()
        .expect_err("as_html_from_dot must surface the dumper error");
    assert!(
        format!("{html_err}").contains("dot dump error"),
        "html path must propagate the wrapped dumper error"
    );
}

const TEMPLATE: &str = include_str!("../assets/graph_template_dot.html");

/// Every `__NAME__` run the template carries, so a template revision adding a
/// placeholder is covered without editing this file.
fn template_placeholders() -> Vec<String> {
    TEMPLATE
        .split("__")
        .filter(|seg| {
            !seg.is_empty()
                && seg
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        })
        .map(|seg| format!("__{seg}__"))
        .collect()
}

#[test]
fn html_emit_substitutes_every_placeholder() {
    let names = template_placeholders();
    assert!(
        !names.is_empty(),
        "no placeholder found in the template: the scan is broken"
    );
    let html = render_html(3);
    for name in names {
        // One leaked token means a broken page: no Viz, no JS, or no DOT source.
        assert!(!html.contains(&name), "{name} leaked into the emitted HTML");
    }
}

#[test]
fn html_emit_resolves_engine_to_dot_or_sfdp() {
    // The needles sit inside the `pickDefaultEngine` IIFE, which is where the
    // viewer actually reads the engine from.
    let html = render_html(3);
    let needle_dot = "const e=\"dot\"";
    let needle_sfdp = "const e=\"sfdp\"";
    let has_dot = html.contains(needle_dot);
    let has_sfdp = html.contains(needle_sfdp);
    assert!(
        has_dot ^ has_sfdp,
        "expected exactly one of `dot` or `sfdp` engine substitution; \
         dot={has_dot} sfdp={has_sfdp}"
    );
}

#[test]
fn html_emit_picks_dot_for_small_graph() {
    let html = render_html(3);
    assert!(
        html.contains("const e=\"dot\""),
        "small graph must default to engine `dot`"
    );
}

#[test]
fn html_emit_starts_with_doctype() {
    // Cheap check that the template was not truncated or mangled. Lowercase,
    // matching the template file.
    let html = render_html(3);
    assert!(
        html.starts_with("<!doctype html>"),
        "emitted HTML must start with `<!doctype html>`; got: {}",
        &html[..html.len().min(64)]
    );
}

#[test]
fn html_emit_inlines_vendored_payloads() {
    // The vendored payloads are megabytes of base64 Wasm/JS, so even a 3-node
    // graph must emit a hundreds-of-KB document. Size stands in for presence.
    let html = render_html(3);
    assert!(
        html.len() > 100_000,
        "emitted HTML suspiciously small ({} bytes); the vendored JS payloads \
         were probably not inlined",
        html.len()
    );
}
