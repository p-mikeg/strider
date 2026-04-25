#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Smoke tests for `CfgDotDumper` — ensure the produced DOT output is
//! non-empty, contains node labels, and renders the edge-style mapping
//! per `RegionEdgeKind` for real functions of each shape.

mod common;
use common::{binary, build_cfg};

use dot::{DotStyle, GraphDot};

fn cfg_for(fn_name: &str) -> cfg::Cfg<reader::ElfFileMemReader> {
    let p = binary("x64", fn_name);
    build_cfg(
        p.to_str().unwrap(),
        fn_name,
        rsleigh::sla_spec::SLA_SPEC_X86_64,
        rsleigh::pspec::PSPEC_X86_64,
    )
}

fn dot_source(cfg: &cfg::Cfg<reader::ElfFileMemReader>) -> String {
    GraphDot::new(cfg.dot_dumper(), DotStyle::dark())
        .as_dot()
        .expect("dot rendering must not fail")
}

#[test]
fn dot_output_non_empty_for_linear_function() {
    let cfg = cfg_for("add");
    let s = dot_source(&cfg);
    assert!(!s.is_empty(), "DOT output must not be empty");
    assert!(
        s.contains("Instruction(addr="),
        "node label must appear in DOT output"
    );
}

#[test]
fn dot_output_for_conditional_function_contains_if_case_edges_and_dashed_style() {
    let cfg = cfg_for("abs_val");
    let s = dot_source(&cfg);
    assert!(
        s.contains("IfCaseTrue") || s.contains("IfCaseFalse"),
        "a conditional function's DOT output must label the if-case edges"
    );
    // IfCase edges are rendered with `dashed` style per src/cfg/dot.rs.
    assert!(
        s.contains("dashed"),
        "IfCase edges must render with dashed style"
    );
}

#[test]
fn dot_output_for_loop_contains_solid_or_bold_edges() {
    let cfg = cfg_for("sum_to_n");
    let s = dot_source(&cfg);
    assert!(
        s.contains("solid") || s.contains("bold"),
        "a looping function's DOT output should contain solid (fallthrough) or bold (branch) edges"
    );
}

#[test]
fn dot_output_mentions_every_region() {
    // For a multi-region function, every region id must appear as a node
    // declaration in the DOT source. We check by counting "Instruction(addr="
    // occurrences against the region count — they should match.
    let cfg = cfg_for("clamp");
    let s = dot_source(&cfg);
    let expected = cfg.graph.node_count();
    let actual = s.matches("Instruction(addr=").count();
    assert_eq!(
        actual, expected,
        "every region must emit exactly one Instruction(addr=...) label"
    );
}
