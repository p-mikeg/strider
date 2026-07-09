//! End-to-end tests for the native-layout render path: a `GraphDotDumper`
//! run captured into a `LayoutInput`, laid out, and serialised to JSON / HTML.

use dot::layout::LayoutOptions;
use dot::{DotEmitter, DotStyle, GraphDot, GraphDotDumper};

/// 0 → {1,2} → 3 diamond.
struct Diamond;

impl GraphDotDumper for Diamond {
    type Node = usize;
    type Error = anyhow::Error;
    type State = ();
    fn create_initial_state(&self) {}
    fn iter_nodes(&self) -> impl IntoIterator<Item = usize> {
        0..4
    }
    fn dump_as_dot(&self, node: usize, out: &mut DotEmitter, _: &mut ()) -> anyhow::Result<()> {
        out.node(
            &format!("n{node}"),
            &format!("label\\lof node {node}"),
            "box",
            &[],
        );
        for &(a, b) in &[(0usize, 1usize), (0, 2), (1, 3), (2, 3)] {
            if b == node {
                out.edge(&format!("n{a}"), &format!("n{b}"), &[("label", "e")]);
            }
        }
        Ok(())
    }
}

fn gd() -> GraphDot<Diamond> {
    GraphDot::new(Diamond, DotStyle::dark())
}

#[test]
fn as_layout_ranks_the_diamond() {
    let pos = gd().as_layout(&LayoutOptions::default()).unwrap();
    assert_eq!(pos.nodes.len(), 4);
    assert_eq!(pos.nodes[0].rank, 0);
    assert_eq!(pos.nodes[3].rank, 2);
    assert_eq!(pos.edges.len(), 4);
    // node boxes are sized from the (2-line) label, not zero.
    assert!(pos.nodes[0].width > 0.0 && pos.nodes[0].height > 0.0);
}

#[test]
fn as_layout_json_is_well_formed() {
    let json = gd().as_layout_json(&LayoutOptions::default()).unwrap();
    // Parse it with a tolerant check: it must be an object with the keys and
    // contain 4 node ids and coordinate fields.
    assert!(json.starts_with("{\"width\":"));
    assert!(json.contains("\"nodes\":["));
    assert!(json.contains("\"edges\":["));
    for id in ["n0", "n1", "n2", "n3"] {
        assert!(
            json.contains(&format!("\"id\":\"{id}\"")),
            "missing {id}: {json}"
        );
    }
    assert!(json.contains("\"x\":") && json.contains("\"y\":"));
    // The label's `\l` break survives as an escaped backslash in the JSON.
    assert!(
        json.contains("label\\\\lof node"),
        "label escaping wrong: {json}"
    );
}

#[test]
fn as_layout_html_substitutes_and_embeds() {
    let html = gd().as_layout_html(&LayoutOptions::default()).unwrap();
    assert!(html.starts_with("<!doctype html>"));
    assert!(
        !html.contains("__LAYOUT_JSON__"),
        "JSON placeholder not filled"
    );
    assert!(
        !html.contains("__SVG_PAN_ZOOM_JS__"),
        "pan-zoom placeholder not filled"
    );
    assert!(html.contains("svgPanZoom"), "viewer script missing");
    assert!(html.contains("\"nodes\":["), "layout JSON not embedded");
}
