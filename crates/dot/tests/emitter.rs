#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

//! Pins the exact DOT string `DotEmitter` produces: digraph-name quoting,
//! id/label escaping, and the `extra` attribute contract.

use dot::{DotEmitter, DotStyle};

#[test]
fn empty_emitter_produces_minimal_digraph() {
    let style = DotStyle::empty();
    let out = DotEmitter::new("G", &style).finish();
    assert_eq!(out, "digraph \"G\" {\n}\n");
}

#[test]
fn empty_emitter_with_dark_style_emits_attr_blocks_in_order() {
    let style = DotStyle::dark();
    let out = DotEmitter::new("G", &style).finish();

    let g_pos = out.find("graph [").expect("expected graph block");
    let n_pos = out.find("node [").expect("expected node block");
    let e_pos = out.find("edge [").expect("expected edge block");
    assert!(
        g_pos < n_pos && n_pos < e_pos,
        "block ordering broke: {out}"
    );

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
    assert!(
        !out.contains("[]"),
        "extra=[] must not produce empty brackets"
    );
}

#[test]
fn edge_with_extra_emits_bracketed_attrs() {
    let style = DotStyle::empty();
    let mut e = DotEmitter::new("G", &style);
    e.edge("a", "b", &[("label", "Branch"), ("style", "dashed")]);
    let out = e.finish();
    // `label` is free text so it gets quoted; other attrs stay bare.
    assert!(
        out.contains("\"a\" -> \"b\" [label=\"Branch\", style=dashed];\n"),
        "unexpected DOT: {out}"
    );
}

#[test]
fn finish_appends_closing_brace_and_newline_exactly_once() {
    let style = DotStyle::empty();
    let out = DotEmitter::new("G", &style).finish();
    assert_eq!(out.matches('}').count(), 1);
    assert!(out.ends_with("}\n"));
}

#[test]
fn digraph_name_with_special_chars_is_quoted_and_escaped() {
    let style = DotStyle::empty();
    let out = DotEmitter::new("my graph \"X\"", &style).finish();
    assert!(
        out.starts_with("digraph \"my graph \\\"X\\\"\" {\n"),
        "expected quoted+escaped header, got: {out}",
    );
}

#[test]
fn digraph_name_with_backslash_is_doubled() {
    let style = DotStyle::empty();
    let out = DotEmitter::new("path\\sub", &style).finish();
    // `\s` is not one of the passed-through DOT escapes, so it doubles.
    assert!(
        out.starts_with("digraph \"path\\\\sub\" {\n"),
        "expected doubled backslash, got: {out}",
    );
}

#[test]
fn node_id_with_special_chars_is_escaped() {
    let style = DotStyle::empty();
    let mut e = DotEmitter::new("G", &style);
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
