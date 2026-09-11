//! Both vendored JS bundles are inlined verbatim into every emitted `.html`,
//! so the licence notice their terms require must travel with them.

use dot::{DotEmitter, DotStyle, GraphDot, GraphDotDumper};

const SVG_PAN_ZOOM: &str = include_str!("../assets/vendored/svg-pan-zoom.min.js");
const VIZ: &str = include_str!("../assets/vendored/viz-standalone.js");

/// BSD-2-Clause clause 1 / the MIT notice clause: the head of each bundle
/// carries the upstream copyright line and the licence body.
#[test]
fn vendored_bundles_carry_their_copyright_notice() {
    for (name, src, holder) in [
        ("svg-pan-zoom.min.js", SVG_PAN_ZOOM, "Andrea Leofreddi"),
        ("viz-standalone.js", VIZ, "Michael Daines"),
    ] {
        let head = &src[..src.len().min(4096)];
        assert!(
            head.contains("Copyright"),
            "{name}: no copyright notice in the leading comment"
        );
        assert!(
            head.contains(holder),
            "{name}: copyright holder {holder} missing"
        );
    }
    let head = &SVG_PAN_ZOOM[..SVG_PAN_ZOOM.len().min(4096)];
    assert!(
        head.contains("Redistributions of source code must retain the above copyright notice"),
        "svg-pan-zoom.min.js: BSD-2-Clause body missing"
    );
}

struct OneNode;

impl GraphDotDumper for OneNode {
    type Node = usize;
    type Error = anyhow::Error;
    type State = ();

    fn create_initial_state(&self) -> Self::State {}

    fn iter_nodes(&self) -> impl IntoIterator<Item = Self::Node> {
        0..1
    }

    fn dump_as_dot(
        &self,
        node: Self::Node,
        out: &mut DotEmitter,
        _state: &mut Self::State,
    ) -> anyhow::Result<()> {
        out.node(&format!("n{node}"), "n", "box", &[]);
        Ok(())
    }
}

#[test]
fn emitted_html_redistributes_both_notices() {
    let html = GraphDot::new(OneNode, DotStyle::dark())
        .as_html_from_dot()
        .expect("html emit succeeded");
    for holder in ["Andrea Leofreddi", "Michael Daines"] {
        assert!(
            html.contains(holder),
            "emitted HTML drops the {holder} copyright notice"
        );
    }
}
