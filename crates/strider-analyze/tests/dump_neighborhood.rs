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

/// Pins the `Graph::has_node` precondition added to `dump_neighborhood`.
/// A foreign / stale anchor used to panic inside the renderer's
/// `nodes[anchor]` index op; now it must surface a typed error.
#[test]
fn dump_neighborhood_rejects_foreign_node_id() {
    // Lift two separate graphs.  The second graph's entry node id
    // is foreign relative to the first graph's arena — even when the
    // two arenas happen to assign the same numeric `NodeId`, treating
    // the id as live in the wrong graph is undefined behaviour we
    // want to reject explicitly.
    let (outcome_a, cfg) = lift_ret_snippet_x86_64();
    let (outcome_b, _cfg_b) = lift_ret_snippet_x86_64();

    let foreign_anchor = outcome_b
        .graph
        .all_node_ids()
        .last()
        .expect("graph b has at least one node");

    // Sanity check: if `foreign_anchor` happens to coincide with a
    // live id in graph A, the test won't exercise the rejection path.
    // The `ret` snippet's two graphs have the same shape, so we
    // additionally pick an id past A's arena tail by adding new
    // unreachable nodes — but the simpler path is to ensure the
    // foreign id is past A's max id; rather than gamble, walk all of
    // A's ids and synthesise a fresh one.
    use cranelift_entity::EntityRef;
    let a_max = outcome_a
        .graph
        .all_node_ids()
        .map(|id| id.index())
        .max()
        .unwrap_or(0);
    // Construct a NodeId one past A's max — guaranteed foreign.
    let foreign_id = strider_ir::node::NodeId::new(a_max + 100);

    // Sanity: the foreign id must NOT be a live arena slot in A.
    assert!(
        !outcome_a.graph.has_node(foreign_id),
        "test precondition: foreign_id must not collide with a live A slot",
    );

    let tmp = unique_tmp_dir("dump-neighborhood-foreign");
    let out = tmp.join("focus.html");

    let err = strider_analyze::dump_neighborhood(
        &outcome_a.graph,
        foreign_id,
        /* depth */ 1,
        cfg.sleigh(),
        &out,
    )
    .expect_err("dump_neighborhood must reject foreign anchors");
    let msg = format!("{err}");
    assert!(
        msg.contains("not a live node"),
        "unexpected error message: {msg}",
    );

    // Sanity reference: `foreign_anchor` exists in graph B (covers
    // the warning-only branch and keeps the variable in use).
    assert!(outcome_b.graph.has_node(foreign_anchor));

    let _ = std::fs::remove_dir_all(&tmp);
}
