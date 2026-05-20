// ── tests ─────────────────────────────────────────────────────────────────────

use super::*;
use crate::{
    graph::Graph,
    node::{NodeKind, NodeOutputKind, NodeOutputType},
};
use ::dot::GraphDotDumper as _;

// ── helpers ───────────────────────────────────────────────────────────────

/// Creates a probe `Sleigh` context backed by an empty buffer.
/// Sufficient for all dot tests (no instructions decoded).
fn probe_sleigh() -> rsleigh::Sleigh<rsleigh::mem_readers::BufMemReader<Vec<u8>>> {
    let probe = rsleigh::mem_readers::BufMemReader::new(vec![], 0x0);
    rsleigh::Sleigh::new(
        rsleigh::sla_spec::SLA_SPEC_X86_64,
        rsleigh::pspec::PSPEC_X86_64,
        probe,
    )
    .expect("create probe Sleigh")
}

/// Renders every node reachable from `entry` and returns the DOT string.
fn render(graph: &Graph, entry: NodeId) -> String {
    let sleigh = probe_sleigh();
    let dumper = GraphDotDumper {
        entry,
        graph,
        sleigh: &sleigh,
        call_clobbered: &[],
        ret_val_regs: &[],
    };
    use ::dot::GraphDot;
    GraphDot::new(dumper, ::dot::DotStyle::empty())
        .as_dot()
        .expect("render must succeed")
}

/// Counts lines matching `pred` in `s`.
fn count_lines<'a>(s: &'a str, pred: impl Fn(&'a str) -> bool) -> usize {
    s.lines().filter(|l| pred(l)).count()
}

/// Returns all DOT node-declaration lines (contain `[label=` but not `->`)
fn node_decls(dot: &str) -> Vec<&str> {
    dot.lines()
        .filter(|l| l.contains("[label=") && !l.contains("->"))
        .collect()
}

/// Returns all DOT edge lines (contain `->`)
fn edge_lines(dot: &str) -> Vec<&str> {
    dot.lines().filter(|l| l.contains("->")).collect()
}

// ── determinism ───────────────────────────────────────────────────────────

/// Rendering the same graph twice must produce identical DOT output.
#[test]
fn dot_output_is_deterministic() {
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let [ctrl] = graph.node_outputs_exact::<1>(entry).unwrap();

    let cs = graph.create_node(
        NodeKind::ControlState,
        [ctrl],
        [NodeOutputKind::Control, NodeOutputKind::PhiToken],
    );
    let [cs_ctrl, _] = graph.node_outputs_exact::<2>(cs).unwrap();
    graph.create_node(NodeKind::Return, [cs_ctrl], []);

    let first = render(&graph, entry);
    let second = render(&graph, entry);
    assert_eq!(
        first, second,
        "same graph must render identically on two calls"
    );
}

/// A graph with a diamond (If → two branches → merge) must render
/// deterministically regardless of walk order.
#[test]
fn dot_output_diamond_is_deterministic() {
    let mut graph = Graph::new();

    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let [entry_ctrl] = graph.node_outputs_exact::<1>(entry).unwrap();
    let cond = graph.create_node(
        NodeKind::BoolConst(false),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::Bool)],
    );
    let [cond_out] = graph.node_outputs_exact::<1>(cond).unwrap();
    let if_node = graph.create_node(
        NodeKind::If,
        [entry_ctrl, cond_out],
        [NodeOutputKind::Control, NodeOutputKind::Control],
    );
    let [true_ctrl, false_ctrl] = graph.node_outputs_exact::<2>(if_node).unwrap();

    graph.create_node(NodeKind::Return, [true_ctrl], []);
    graph.create_node(NodeKind::Return, [false_ctrl], []);

    let first = render(&graph, entry);
    let second = render(&graph, entry);
    assert_eq!(first, second);
}

// ── structural correctness ────────────────────────────────────────────────

/// The DOT output must begin with `digraph` and end with `}`.
#[test]
fn dot_output_has_digraph_wrapper() {
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let dot = render(&graph, entry);
    assert!(
        dot.trim_start().starts_with("digraph"),
        "must start with 'digraph':\n{dot}"
    );
    assert!(dot.trim_end().ends_with('}'), "must end with '}}':\n{dot}");
}

/// Every declared node id referenced on an edge must also appear as a node
/// declaration (no edge references an id that was never declared).
#[test]
fn all_edge_endpoints_are_declared() {
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let [ctrl] = graph.node_outputs_exact::<1>(entry).unwrap();
    let cond = graph.create_node(
        NodeKind::BoolConst(true),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::Bool)],
    );
    let [cond_out] = graph.node_outputs_exact::<1>(cond).unwrap();
    let if_node = graph.create_node(
        NodeKind::If,
        [ctrl, cond_out],
        [NodeOutputKind::Control, NodeOutputKind::Control],
    );
    let [tc, fc] = graph.node_outputs_exact::<2>(if_node).unwrap();
    graph.create_node(NodeKind::Return, [tc], []);
    graph.create_node(NodeKind::Return, [fc], []);

    let dot = render(&graph, entry);

    // Collect every declared dot node id (the part before the first space on
    // a `"id" [label=…]` line).
    let declared: std::collections::HashSet<&str> = dot
        .lines()
        .filter(|l| l.contains("[label=") && !l.contains("->"))
        .filter_map(|l| l.trim().split('"').nth(1))
        .collect();

    // For every edge `"a" -> "b"` check both endpoints are declared.
    for line in dot.lines().filter(|l| l.contains("->")) {
        let parts: Vec<&str> = line.trim().split("->").collect();
        if parts.len() < 2 {
            continue;
        }
        let src = parts[0].trim().trim_matches('"');
        // rhs may have attributes after the id like `"b" [color=…]`
        let dst = parts[1].trim().split('"').nth(1).unwrap_or("").trim();
        assert!(
            declared.contains(src),
            "edge source '{src}' has no node declaration:\n{dot}"
        );
        assert!(
            declared.contains(dst),
            "edge destination '{dst}' has no node declaration:\n{dot}"
        );
    }
}

/// A linear chain (Entry → ControlState → Return) must produce exactly
/// those three node declarations and two edges.
#[test]
fn linear_chain_node_and_edge_count() {
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let [ctrl] = graph.node_outputs_exact::<1>(entry).unwrap();
    let cs = graph.create_node(
        NodeKind::ControlState,
        [ctrl],
        [NodeOutputKind::Control, NodeOutputKind::PhiToken],
    );
    let [cs_ctrl, _] = graph.node_outputs_exact::<2>(cs).unwrap();
    graph.create_node(NodeKind::Return, [cs_ctrl], []);

    let dot = render(&graph, entry);
    assert_eq!(
        node_decls(&dot).len(),
        3,
        "exactly 3 node declarations:\n{dot}"
    );
    assert_eq!(edge_lines(&dot).len(), 2, "exactly 2 edges:\n{dot}");
}

/// An `If` node must produce exactly two virtual-node declarations
/// ("if.true" and "if.false") and exactly two edges from the `If` diamond.
#[test]
fn if_node_produces_exactly_two_branch_virtual_nodes() {
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let [ctrl] = graph.node_outputs_exact::<1>(entry).unwrap();
    let cond = graph.create_node(
        NodeKind::BoolConst(true),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::Bool)],
    );
    let [cond_out] = graph.node_outputs_exact::<1>(cond).unwrap();
    let if_node = graph.create_node(
        NodeKind::If,
        [ctrl, cond_out],
        [NodeOutputKind::Control, NodeOutputKind::Control],
    );
    let [tc, fc] = graph.node_outputs_exact::<2>(if_node).unwrap();
    graph.create_node(NodeKind::Return, [tc], []);
    graph.create_node(NodeKind::Return, [fc], []);

    let dot = render(&graph, entry);

    let if_true_count = count_lines(&dot, |l| l.contains("if.true") && l.contains("[label="));
    let if_false_count = count_lines(&dot, |l| l.contains("if.false") && l.contains("[label="));
    assert_eq!(if_true_count, 1, "exactly one if.true declaration:\n{dot}");
    assert_eq!(
        if_false_count, 1,
        "exactly one if.false declaration:\n{dot}"
    );
}

// ── label content ─────────────────────────────────────────────────────────

/// `MemPhi` nodes must render with the label "φ Mem".
#[test]
fn mem_phi_label_is_phi_mem() {
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let mem_phi = graph.create_node(NodeKind::MemPhi, [], [NodeOutputKind::Memory]);
    // mem_phi is only reachable as a data input of Return (graph walk follows inputs)
    let [mp_out] = graph.node_outputs_exact::<1>(mem_phi).unwrap();
    let [entry_ctrl] = graph.node_outputs_exact::<1>(entry).unwrap();
    graph.create_node(NodeKind::Return, [entry_ctrl, mp_out], []);

    let dot = render(&graph, entry);
    assert!(
        dot.contains("φ Mem"),
        "MemPhi label must be 'φ Mem':\n{dot}"
    );
    assert!(
        !dot.contains("MemPhi"),
        "old 'MemPhi' label must not appear:\n{dot}"
    );
}

/// `IntConst` nodes must include their hex value and type in the label.
#[test]
fn int_const_label_contains_value_and_type() {
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let c = graph.create_node(
        NodeKind::IntConst(0xdeadbeef),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U32)],
    );
    let [c_out] = graph.node_outputs_exact::<1>(c).unwrap();
    let [entry_ctrl] = graph.node_outputs_exact::<1>(entry).unwrap();
    graph.create_node(NodeKind::Return, [entry_ctrl, c_out], []);

    let dot = render(&graph, entry);
    assert!(
        dot.contains("0xdeadbeef"),
        "hex value must be in label:\n{dot}"
    );
    assert!(dot.contains("u32"), "type must be in label:\n{dot}");
}

// ── if virtual-node ordering regression ───────────────────────────────────

/// Verify that "if.true"/"if.false" virtual nodes are correctly wired even
/// when a branch successor (CS_true / CS_false) is rendered *before* the
/// `If` node itself.
///
/// This is the ordering scenario that used to produce a dangling "if.true"
/// trapezium with no outgoing edge and a spurious direct edge from the `If`
/// diamond to the true-branch ControlState (3 children on the `If` node).
#[test]
fn if_virtual_nodes_connected_when_consumer_rendered_before_if() {
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let [entry_ctrl] = graph.node_outputs_exact::<1>(entry).unwrap();
    let cond_node = graph.create_node(
        NodeKind::BoolConst(true),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::Bool)],
    );
    let [cond] = graph.node_outputs_exact::<1>(cond_node).unwrap();
    let if_node = graph.create_node(
        NodeKind::If,
        [entry_ctrl, cond],
        [NodeOutputKind::Control, NodeOutputKind::Control],
    );
    let [true_ctrl, false_ctrl] = graph.node_outputs_exact::<2>(if_node).unwrap();

    let cs_true = graph.create_node(
        NodeKind::ControlState,
        [],
        [NodeOutputKind::Control, NodeOutputKind::PhiToken],
    );
    graph.add_node_input(cs_true, true_ctrl).unwrap();
    let [cs_true_ctrl, _] = graph.node_outputs_exact::<2>(cs_true).unwrap();

    let cs_false = graph.create_node(
        NodeKind::ControlState,
        [],
        [NodeOutputKind::Control, NodeOutputKind::PhiToken],
    );
    graph.add_node_input(cs_false, false_ctrl).unwrap();
    let [cs_false_ctrl, _] = graph.node_outputs_exact::<2>(cs_false).unwrap();

    graph.create_node(NodeKind::Return, [cs_true_ctrl], []);
    graph.create_node(NodeKind::Return, [cs_false_ctrl], []);

    let sleigh = probe_sleigh();
    let dumper = GraphDotDumper {
        entry,
        graph: &graph,
        sleigh: &sleigh,
        call_clobbered: &[],
        ret_val_regs: &[],
    };

    let style = ::dot::DotStyle::empty();
    let mut emitter = ::dot::DotEmitter::new("test", &style);
    let mut state = dumper.create_initial_state();

    // Render cs_true *before* if_node to trigger the historical bug.
    dumper
        .dump_as_dot(cs_true, &mut emitter, &mut state)
        .unwrap();
    dumper
        .dump_as_dot(if_node, &mut emitter, &mut state)
        .unwrap();

    let dot = emitter.finish();

    let if_true_id = dot
        .lines()
        .find_map(|line| {
            if line.contains("if.true") && line.contains("[label=") {
                line.trim().split('"').nth(1).map(str::to_owned)
            } else {
                None
            }
        })
        .expect("if.true node must be declared in the DOT output");

    let q = format!("\"{}\"", if_true_id);
    let edges_into = edge_lines(&dot)
        .into_iter()
        .filter(|l| l.split("->").nth(1).is_some_and(|rhs| rhs.contains(&q)))
        .count();
    let edges_from = edge_lines(&dot)
        .into_iter()
        .filter(|l| l.split("->").next().is_some_and(|lhs| lhs.contains(&q)))
        .count();

    assert!(
        edges_into >= 1,
        "if.true must have ≥1 incoming edge:\n{dot}"
    );
    assert!(
        edges_from >= 1,
        "if.true must have ≥1 outgoing edge:\n{dot}"
    );
}

/// CastToInt accepts `AnyValue` per its signature; the rendered label must
/// reflect the actual input type, not a hard-coded "from bool".
#[test]
fn cast_to_int_label_reflects_actual_input_type() {
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let init_mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let entry_ctrl = graph.node_outputs(entry).into_iter().next().unwrap();
    let mem = graph.node_outputs(init_mem).into_iter().next().unwrap();

    let c = graph.create_node(
        NodeKind::IntConst(0),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let c_out = graph.node_outputs(c).into_iter().next().unwrap();
    let cast = graph.create_node(
        NodeKind::CastToInt,
        [c_out],
        [NodeOutputKind::OutputType(NodeOutputType::U32)],
    );
    let cast_out = graph.node_outputs(cast).into_iter().next().unwrap();
    graph.create_node(NodeKind::Return, [entry_ctrl, mem, cast_out], []);

    let dot = render(&graph, entry);
    assert!(dot.contains("from u64"), "expected 'from u64' in CastToInt label, got:\n{dot}");
    assert!(!dot.contains("from bool"), "CastToInt label must not hard-code 'from bool', got:\n{dot}");
}

/// Rendering a Call node with at least one clobbered output must succeed
/// even when the caller passes `call_clobbered: &[]` (which is what every
/// existing test does). Previously this panicked with an OOB slice index.
#[test]
fn render_call_with_clobbered_output_uses_synthetic_label_when_slice_short() {
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let init_mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let entry_ctrl = graph.node_outputs(entry).into_iter().next().unwrap();
    let mem = graph.node_outputs(init_mem).into_iter().next().unwrap();
    let target = graph.create_node(
        NodeKind::IntConst(0x1000),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let target_out = graph.node_outputs(target).into_iter().next().unwrap();
    // One Bool clobbered output, but `call_clobbered` slice is empty.
    let call = graph.create_node(
        NodeKind::Call,
        [entry_ctrl, mem, target_out],
        [
            NodeOutputKind::Control,
            NodeOutputKind::Memory,
            NodeOutputKind::OutputType(NodeOutputType::Bool),
        ],
    );
    let call_ctrl = graph.node_outputs(call).into_iter().next().unwrap();
    let call_mem = graph.node_outputs(call).into_iter().nth(1).unwrap();
    let clob_out = graph.node_outputs(call).into_iter().nth(2).unwrap();
    graph.create_node(NodeKind::Return, [call_ctrl, call_mem, clob_out], []);

    // Render must not panic.
    let dot = render(&graph, entry);
    assert!(dot.contains("clob0"), "expected synthetic clob0 label, got:\n{dot}");
}

/// `CallOther` whose user-op name is recorded in `Graph::call_other_names`
/// must render with both the symbolic name and the numeric id.
#[test]
fn call_other_label_includes_resolved_name() {
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let init_mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let entry_ctrl = graph.node_outputs(entry).into_iter().next().unwrap();
    let mem = graph.node_outputs(init_mem).into_iter().next().unwrap();
    let co = graph.create_node(
        NodeKind::CallOther { user_op_id: 62 },
        [entry_ctrl, mem],
        [NodeOutputKind::Control, NodeOutputKind::Memory],
    );
    graph.set_call_other_name(co, "setISAMode".to_string());
    let co_ctrl = graph.node_outputs(co).into_iter().next().unwrap();
    let co_mem = graph.node_outputs(co).into_iter().nth(1).unwrap();
    graph.create_node(NodeKind::Return, [co_ctrl, co_mem], []);

    let dot = render(&graph, entry);
    assert!(
        dot.contains("setISAMode #62"),
        "label must show resolved name and id together:\n{dot}",
    );
}

/// `CallOther` without a recorded name (synthetic test graph) falls back
/// to the bare numeric id; the label must NOT contain a stray space where
/// the missing name would have gone.
#[test]
fn call_other_label_falls_back_to_id_when_name_missing() {
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let init_mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let entry_ctrl = graph.node_outputs(entry).into_iter().next().unwrap();
    let mem = graph.node_outputs(init_mem).into_iter().next().unwrap();
    let co = graph.create_node(
        NodeKind::CallOther { user_op_id: 7 },
        [entry_ctrl, mem],
        [NodeOutputKind::Control, NodeOutputKind::Memory],
    );
    // Intentionally do NOT call `set_call_other_name`.
    let co_ctrl = graph.node_outputs(co).into_iter().next().unwrap();
    let co_mem = graph.node_outputs(co).into_iter().nth(1).unwrap();
    graph.create_node(NodeKind::Return, [co_ctrl, co_mem], []);

    let dot = render(&graph, entry);
    assert!(
        dot.contains("CallOther #7"),
        "label must show the bare id when no name is recorded:\n{dot}",
    );
    assert!(
        !dot.contains("CallOther  #7"),
        "no double-space (placeholder for the missing name) should leak:\n{dot}",
    );
}
