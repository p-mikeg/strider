#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

use dot::DotStyle;

#[test]
fn empty_has_no_default_attrs() {
    let s = DotStyle::empty();
    assert!(s.graph.is_empty());
    assert!(s.node.is_empty());
    assert!(s.edge.is_empty());
}

#[test]
fn dark_has_known_graph_node_and_edge_attrs() {
    let s = DotStyle::dark();
    assert!(s.graph.iter().any(|(k, v)| *k == "rankdir" && *v == "TB"));
    assert!(s.graph.iter().any(|(k, _v)| *k == "bgcolor"));
    assert!(s.node.iter().any(|(k, v)| *k == "shape" && *v == "box"));
    assert!(
        s.node
            .iter()
            .any(|(k, v)| *k == "fontname" && *v == "monospace")
    );
    assert!(s.node.iter().any(|(k, v)| *k == "margin" && *v == "0.2"));
    assert!(s.edge.iter().any(|(k, _v)| *k == "fontcolor"));
}

#[test]
fn dark_cfg_replaces_fontname_with_courier() {
    let s = DotStyle::dark_cfg();
    assert!(
        s.node
            .iter()
            .any(|(k, v)| *k == "fontname" && *v == "Courier"),
        "expected fontname=Courier in dark_cfg().node",
    );
    assert!(
        s.node.iter().any(|(k, v)| *k == "margin" && *v == "0.2"),
        "expected margin=0.2 in dark_cfg().node",
    );
}

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
