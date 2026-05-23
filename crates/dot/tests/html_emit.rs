#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

//! End-to-end checks for the HTML emit path (`GraphDot::as_html_from_dot`):
//!
//! - All placeholder tokens in `graph_template_dot.html`
//!   (`__DEFAULT_ENGINE__`, `__VIZ_STANDALONE_JS__`, `__SVG_PAN_ZOOM_JS__`,
//!   `__DOT_JSON__`) are replaced — none leak through.
//! - The auto engine choice resolves to one of the two engines the policy
//!   produces (`dot` or `sfdp`) and is visibly present in the output.
//! - The output is recognisably HTML (starts with the `<!doctype html>`
//!   declaration the template embeds).
//!
//! Tightens the contract that `as_html_from_dot` is the single place
//! responsible for substituting every placeholder; if a future template
//! revision adds a placeholder without wiring up its replacement, these
//! tests catch the leak.

use dot::{DotEmitter, DotStyle, GraphDot, GraphDotDumper};

/// Minimal dumper that emits a configurable number of nodes — enough to
/// drive `as_html_from_dot` and to cross the auto-sfdp threshold when
/// the test wants to exercise the large-graph branch.
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

#[test]
fn html_emit_substitutes_every_placeholder() {
    let html = render_html(3);
    // All four template placeholders must be substituted — if one slips
    // through, the resulting page is broken (Viz can't load, no JS, no
    // DOT source).
    assert!(
        !html.contains("__DEFAULT_ENGINE__"),
        "__DEFAULT_ENGINE__ leaked into the emitted HTML"
    );
    assert!(
        !html.contains("__VIZ_STANDALONE_JS__"),
        "__VIZ_STANDALONE_JS__ leaked into the emitted HTML"
    );
    assert!(
        !html.contains("__SVG_PAN_ZOOM_JS__"),
        "__SVG_PAN_ZOOM_JS__ leaked into the emitted HTML"
    );
    assert!(
        !html.contains("__DOT_JSON__"),
        "__DOT_JSON__ leaked into the emitted HTML"
    );
}

#[test]
fn html_emit_resolves_engine_to_dot_or_sfdp() {
    // The default policy (`HtmlEngineChoice::Auto`) only ever picks one of
    // these two literals.  Verify the substitution lands in the
    // `pickDefaultEngine` IIFE — that's where the viewer reads it.
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
    // A 3-node graph is far below `DEFAULT_SFDP_NODE_THRESHOLD` (2000),
    // so the auto policy must pick `dot`.
    let html = render_html(3);
    assert!(
        html.contains("const e=\"dot\""),
        "small graph must default to engine `dot`"
    );
}

#[test]
fn html_emit_starts_with_doctype() {
    // Cheap sanity check that the template wasn't accidentally truncated
    // or mangled — the very first non-replaceable byte sequence is
    // `<!doctype html>` (lowercase, per the template file).
    let html = render_html(3);
    assert!(
        html.starts_with("<!doctype html>"),
        "emitted HTML must start with `<!doctype html>`; got: {}",
        &html[..html.len().min(64)]
    );
}

#[test]
fn html_emit_inlines_vendored_payloads() {
    // The viz-js and svg-pan-zoom payloads must be inlined verbatim —
    // they're megabytes of base64-encoded Wasm/JS, so the emitted HTML
    // is necessarily large.  A trivial 3-node graph still produces a
    // hundreds-of-KB document because of these payloads.
    let html = render_html(3);
    assert!(
        html.len() > 100_000,
        "emitted HTML suspiciously small ({} bytes) — vendored JS payloads \
         may not have been inlined",
        html.len()
    );
}
