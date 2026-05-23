//! Integration test: `strider_analyze::dump_neighborhood` emits an HTML
//! viewer for the subgraph within N hops of an anchor node.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::dump_helpers::{lift_ret_snippet_x86_64, unique_tmp_dir, VIEWER_JSON_ANCHOR};

#[test]
fn dump_neighborhood_writes_one_html_for_the_anchor() {
    let (outcome, cfg) = lift_ret_snippet_x86_64();
    let entry_node = outcome.graph.entry().expect("entry should be set after analyze_cfg");

    let tmp = unique_tmp_dir("dump-neighborhood");
    let out = tmp.join("focus.html");

    strider_analyze::dump_neighborhood(
        &outcome.graph,
        entry_node,
        /* depth */ 1,
        cfg.sleigh(),
        &out,
    )
    .expect("dump_neighborhood");

    let html = std::fs::read_to_string(&out).expect("read html");
    assert!(
        html.contains(VIEWER_JSON_ANCHOR),
        "viewer JSON script missing from {}",
        out.display()
    );

    let _ = std::fs::remove_dir_all(&tmp);
}
