// ── tests ─────────────────────────────────────────────────────────────────────

use super::*;
use crate::{
    function::Function,
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
fn render(function: &Function, entry: NodeId) -> String {
    let sleigh = probe_sleigh();
    let dumper = GraphDotDumper {
        entry,
        function,
        sleigh: &sleigh,
        call_clobbered: &[],
        ret_val_regs: &[],
        node_filter: None,
        node_to_arg_indices: build_arg_reverse_map(function),
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
    let mut f = Function::new();
    let entry = f.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    f.set_entry(entry);
    let [ctrl] = f.node_outputs_exact::<1>(entry).unwrap();

    let cs = f.create_node(
        NodeKind::Region,
        [ctrl],
        [NodeOutputKind::Control, NodeOutputKind::PhiToken],
    );
    let [cs_ctrl, _] = f.node_outputs_exact::<2>(cs).unwrap();
    f.create_node(NodeKind::Return, [cs_ctrl], []);

    let first = render(&f, entry);
    let second = render(&f, entry);
    assert_eq!(
        first, second,
        "same graph must render identically on two calls"
    );
}

/// A graph with a diamond (If → two branches → merge) must render
/// deterministically regardless of walk order.
#[test]
fn dot_output_diamond_is_deterministic() {
    let mut f = Function::new();

    let entry = f.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    f.set_entry(entry);
    let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
    let cond = f.create_node(
        NodeKind::BoolConst(false),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::Bool)],
    );
    let [cond_out] = f.node_outputs_exact::<1>(cond).unwrap();
    let if_node = f.create_node(
        NodeKind::If,
        [entry_ctrl, cond_out],
        [NodeOutputKind::Control, NodeOutputKind::Control],
    );
    let [true_ctrl, false_ctrl] = f.node_outputs_exact::<2>(if_node).unwrap();

    f.create_node(NodeKind::Return, [true_ctrl], []);
    f.create_node(NodeKind::Return, [false_ctrl], []);

    let first = render(&f, entry);
    let second = render(&f, entry);
    assert_eq!(first, second);
}

// ── structural correctness ────────────────────────────────────────────────

/// The DOT output must begin with `digraph` and end with `}`.
#[test]
fn dot_output_has_digraph_wrapper() {
    let mut f = Function::new();
    let entry = f.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    f.set_entry(entry);
    let dot = render(&f, entry);
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
    let mut f = Function::new();
    let entry = f.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    f.set_entry(entry);
    let [ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
    let cond = f.create_node(
        NodeKind::BoolConst(true),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::Bool)],
    );
    let [cond_out] = f.node_outputs_exact::<1>(cond).unwrap();
    let if_node = f.create_node(
        NodeKind::If,
        [ctrl, cond_out],
        [NodeOutputKind::Control, NodeOutputKind::Control],
    );
    let [tc, fc] = f.node_outputs_exact::<2>(if_node).unwrap();
    f.create_node(NodeKind::Return, [tc], []);
    f.create_node(NodeKind::Return, [fc], []);

    let dot = render(&f, entry);

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

/// A linear chain (Entry → Region → Return) must produce exactly
/// those three node declarations and two edges.
#[test]
fn linear_chain_node_and_edge_count() {
    let mut f = Function::new();
    let entry = f.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    f.set_entry(entry);
    let [ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
    let cs = f.create_node(
        NodeKind::Region,
        [ctrl],
        [NodeOutputKind::Control, NodeOutputKind::PhiToken],
    );
    let [cs_ctrl, _] = f.node_outputs_exact::<2>(cs).unwrap();
    f.create_node(NodeKind::Return, [cs_ctrl], []);

    let dot = render(&f, entry);
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
    let mut f = Function::new();
    let entry = f.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    f.set_entry(entry);
    let [ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
    let cond = f.create_node(
        NodeKind::BoolConst(true),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::Bool)],
    );
    let [cond_out] = f.node_outputs_exact::<1>(cond).unwrap();
    let if_node = f.create_node(
        NodeKind::If,
        [ctrl, cond_out],
        [NodeOutputKind::Control, NodeOutputKind::Control],
    );
    let [tc, fc] = f.node_outputs_exact::<2>(if_node).unwrap();
    f.create_node(NodeKind::Return, [tc], []);
    f.create_node(NodeKind::Return, [fc], []);

    let dot = render(&f, entry);

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
    let mut f = Function::new();
    let entry = f.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    f.set_entry(entry);
    let mem_phi = f.create_node(NodeKind::MemPhi, [], [NodeOutputKind::Memory(None)]);
    // mem_phi is only reachable as a data input of Return (graph walk follows inputs)
    let [mp_out] = f.node_outputs_exact::<1>(mem_phi).unwrap();
    let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
    f.create_node(NodeKind::Return, [entry_ctrl, mp_out], []);

    let dot = render(&f, entry);
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
    let mut f = Function::new();
    let entry = f.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    f.set_entry(entry);
    let c = f.create_node(
        NodeKind::IntConst(0xdeadbeef),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U32)],
    );
    let [c_out] = f.node_outputs_exact::<1>(c).unwrap();
    let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
    f.create_node(NodeKind::Return, [entry_ctrl, c_out], []);

    let dot = render(&f, entry);
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
/// diamond to the true-branch Region (3 children on the `If` node).
#[test]
fn if_virtual_nodes_connected_when_consumer_rendered_before_if() {
    let mut f = Function::new();
    let entry = f.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    f.set_entry(entry);
    let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
    let cond_node = f.create_node(
        NodeKind::BoolConst(true),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::Bool)],
    );
    let [cond] = f.node_outputs_exact::<1>(cond_node).unwrap();
    let if_node = f.create_node(
        NodeKind::If,
        [entry_ctrl, cond],
        [NodeOutputKind::Control, NodeOutputKind::Control],
    );
    let [true_ctrl, false_ctrl] = f.node_outputs_exact::<2>(if_node).unwrap();

    let cs_true = f.create_node(
        NodeKind::Region,
        [],
        [NodeOutputKind::Control, NodeOutputKind::PhiToken],
    );
    f.add_node_input(cs_true, true_ctrl).unwrap();
    let [cs_true_ctrl, _] = f.node_outputs_exact::<2>(cs_true).unwrap();

    let cs_false = f.create_node(
        NodeKind::Region,
        [],
        [NodeOutputKind::Control, NodeOutputKind::PhiToken],
    );
    f.add_node_input(cs_false, false_ctrl).unwrap();
    let [cs_false_ctrl, _] = f.node_outputs_exact::<2>(cs_false).unwrap();

    f.create_node(NodeKind::Return, [cs_true_ctrl], []);
    f.create_node(NodeKind::Return, [cs_false_ctrl], []);

    let sleigh = probe_sleigh();
    let dumper = GraphDotDumper {
        entry,
        function: &f,
        sleigh: &sleigh,
        call_clobbered: &[],
        ret_val_regs: &[],
        node_filter: None,
        node_to_arg_indices: build_arg_reverse_map(&f),
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
    let mut f = Function::new();
    let entry = f.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    f.set_entry(entry);
    let init_mem = f.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory(None)]);
    let entry_ctrl = f.node_outputs(entry).iter().copied().next().unwrap();
    let mem = f.node_outputs(init_mem).iter().copied().next().unwrap();

    let c = f.create_node(
        NodeKind::IntConst(0),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let c_out = f.node_outputs(c).iter().copied().next().unwrap();
    let cast = f.create_node(
        NodeKind::CastToInt,
        [c_out],
        [NodeOutputKind::OutputType(NodeOutputType::U32)],
    );
    let cast_out = f.node_outputs(cast).iter().copied().next().unwrap();
    f.create_node(NodeKind::Return, [entry_ctrl, mem, cast_out], []);

    let dot = render(&f, entry);
    assert!(dot.contains("from u64"), "expected 'from u64' in CastToInt label, got:\n{dot}");
    assert!(!dot.contains("from bool"), "CastToInt label must not hard-code 'from bool', got:\n{dot}");
}

/// Rendering a Call node with at least one clobbered output must succeed
/// even when the caller passes `call_clobbered: &[]` (which is what every
/// existing test does). Previously this panicked with an OOB slice index.
#[test]
fn render_call_with_clobbered_output_uses_synthetic_label_when_slice_short() {
    let mut f = Function::new();
    let entry = f.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    f.set_entry(entry);
    let init_mem = f.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory(None)]);
    let entry_ctrl = f.node_outputs(entry).iter().copied().next().unwrap();
    let mem = f.node_outputs(init_mem).iter().copied().next().unwrap();
    let target = f.create_node(
        NodeKind::IntConst(0x1000),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let target_out = f.node_outputs(target).iter().copied().next().unwrap();
    // One Bool clobbered output, but `call_clobbered` slice is empty.
    let call = f.create_node(
        NodeKind::Call,
        [entry_ctrl, mem, target_out],
        [
            NodeOutputKind::Control,
            NodeOutputKind::Memory(None),
            NodeOutputKind::OutputType(NodeOutputType::Bool),
        ],
    );
    let call_ctrl = f.node_outputs(call).iter().copied().next().unwrap();
    let call_mem = f.node_outputs(call).iter().copied().nth(1).unwrap();
    let clob_out = f.node_outputs(call).iter().copied().nth(2).unwrap();
    f.create_node(NodeKind::Return, [call_ctrl, call_mem, clob_out], []);

    // Render must not panic.
    let dot = render(&f, entry);
    assert!(dot.contains("clob0"), "expected synthetic clob0 label, got:\n{dot}");
}

/// `CallOther` whose user-op name is recorded in `Function::call_other_names`
/// must render with both the symbolic name and the numeric id.
#[test]
fn call_other_label_includes_resolved_name() {
    let mut f = Function::new();
    let entry = f.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    f.set_entry(entry);
    let init_mem = f.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory(None)]);
    let entry_ctrl = f.node_outputs(entry).iter().copied().next().unwrap();
    let mem = f.node_outputs(init_mem).iter().copied().next().unwrap();
    let co = f.create_node(
        NodeKind::CallOther { user_op_id: 62 },
        [entry_ctrl, mem],
        [NodeOutputKind::Control, NodeOutputKind::Memory(None)],
    );
    f.set_call_other_name(co, "setISAMode".to_string());
    let co_ctrl = f.node_outputs(co).iter().copied().next().unwrap();
    let co_mem = f.node_outputs(co).iter().copied().nth(1).unwrap();
    f.create_node(NodeKind::Return, [co_ctrl, co_mem], []);

    let dot = render(&f, entry);
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
    let mut f = Function::new();
    let entry = f.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    f.set_entry(entry);
    let init_mem = f.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory(None)]);
    let entry_ctrl = f.node_outputs(entry).iter().copied().next().unwrap();
    let mem = f.node_outputs(init_mem).iter().copied().next().unwrap();
    let co = f.create_node(
        NodeKind::CallOther { user_op_id: 7 },
        [entry_ctrl, mem],
        [NodeOutputKind::Control, NodeOutputKind::Memory(None)],
    );
    // Intentionally do NOT call `set_call_other_name`.
    let co_ctrl = f.node_outputs(co).iter().copied().next().unwrap();
    let co_mem = f.node_outputs(co).iter().copied().nth(1).unwrap();
    f.create_node(NodeKind::Return, [co_ctrl, co_mem], []);

    let dot = render(&f, entry);
    assert!(
        dot.contains("CallOther #7"),
        "label must show the bare id when no name is recorded:\n{dot}",
    );
    assert!(
        !dot.contains("CallOther  #7"),
        "no double-space (placeholder for the missing name) should leak:\n{dot}",
    );
}

/// `MemProject` nodes must render as "MemProject" in the label.
#[test]
fn mempartition_label_is_memproject() {
    let mut f = Function::new();
    let entry = f.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    f.set_entry(entry);
    let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();

    // One MemProject node with two output slots (Stack=0, Unknown=1).
    use strider_target::AliasClass;
    let init_mem = f.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory(None)]);
    let [mem_out] = f.node_outputs_exact::<1>(init_mem).unwrap();
    let mp = f.create_node(
        NodeKind::MemProject,
        [mem_out],
        [
            NodeOutputKind::Memory(Some(AliasClass::Stack)),
            NodeOutputKind::Memory(Some(AliasClass::Unknown)),
        ],
    );
    let mp_outs = f.node_outputs(mp).to_vec();
    // Use the Stack output (slot 0) to feed Return.
    let mu = f.create_node(
        NodeKind::MemUnion,
        [mp_outs[0], mp_outs[1]],
        [NodeOutputKind::Memory(None)],
    );
    let [mu_out] = f.node_outputs_exact::<1>(mu).unwrap();
    f.create_node(NodeKind::Return, [entry_ctrl, mu_out], []);

    let dot = render(&f, entry);

    assert!(
        dot.contains("MemProject"),
        "MemProject label must contain 'MemProject':\n{dot}",
    );
    assert!(
        !dot.contains("MemProjectId("),
        "raw MemProjectId debug repr must not appear in label:\n{dot}",
    );
}

/// Edges from a `MemProject` node's output slots must carry alias-class labels
/// (e.g. `"mem:Stack"` / `"mem:Unknown"`) derived from the slot's
/// `NodeOutputKind`.  A `MemUnion` consuming both outputs must also show the
/// class labels on each incoming edge.
#[test]
fn mem_project_edges_carry_alias_class_label() {
    use strider_target::AliasClass;

    let mut f = Function::new();
    let entry = f.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    f.set_entry(entry);
    let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();

    // InitialMemory → MemProject[Stack, Unknown] → MemUnion → Return
    let init_mem = f.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory(None)]);
    let [mem_out] = f.node_outputs_exact::<1>(init_mem).unwrap();

    let mp = f.create_node(
        NodeKind::MemProject,
        [mem_out],
        [
            NodeOutputKind::Memory(Some(AliasClass::Stack)),
            NodeOutputKind::Memory(Some(AliasClass::Unknown)),
        ],
    );
    let mp_outs = f.node_outputs(mp).to_vec();

    // MemUnion joins both partitions back to a unified memory.
    let mu = f.create_node(
        NodeKind::MemUnion,
        [mp_outs[0], mp_outs[1]],
        [NodeOutputKind::Memory(None)],
    );
    let [mu_out] = f.node_outputs_exact::<1>(mu).unwrap();

    f.create_node(NodeKind::Return, [entry_ctrl, mu_out], []);

    let dot = render(&f, entry);

    // Edge labels for MemProject outputs.
    assert!(
        dot.contains("mem:Stack"),
        "partitioned memory edges from MemProject slot 0 must be labelled 'mem:Stack':\n{dot}",
    );
    assert!(
        dot.contains("mem:Unknown"),
        "partitioned memory edges from MemProject slot 1 must be labelled 'mem:Unknown':\n{dot}",
    );

    // At least two edge lines must carry an alias-class label.
    let labelled_edges = edge_lines(&dot)
        .into_iter()
        .filter(|l| l.contains("mem:Stack") || l.contains("mem:Unknown"))
        .count();
    assert!(
        labelled_edges >= 2,
        "expected ≥2 alias-class-labelled edges (one per partition), got {labelled_edges}:\n{dot}",
    );

    // The colon in the label is reserved in unquoted dot identifiers
    // (used for `node:port` references), so the partition labels must
    // appear quoted in the emitted dot.  Unquoted `label=mem:Stack`
    // would be a parse error in any strict dot consumer (viz.js,
    // graphviz).
    assert!(
        dot.contains("label=\"mem:Stack\"") && dot.contains("label=\"mem:Unknown\""),
        "partition-class labels must be quoted in the emitted dot (colon is reserved in unquoted IDs):\n{dot}",
    );
    assert!(
        !dot.contains("label=mem:Stack") && !dot.contains("label=mem:Unknown"),
        "partition-class labels must NEVER appear unquoted in the emitted dot:\n{dot}",
    );
}

// ── stack_offsets addr-edge suppression ──────────────────────────────────

/// A `Store` whose `stack_offsets` entry is populated must NOT have an edge
/// from the addr-producer to the Store node in the rendered DOT output.
/// A `Store` without a `stack_offsets` entry MUST have an addr edge.
#[test]
fn store_addr_edge_suppressed_when_stack_offset_present() {
    // Build: Entry → Region → Store(ram) → Return, with an addr IntConst.
    let mut f = Function::new();

    let entry = f.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    f.set_entry(entry);
    let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();

    let init_mem = f.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory(None)]);
    let [mem0] = f.node_outputs_exact::<1>(init_mem).unwrap();

    let region = f.create_node(
        NodeKind::Region,
        [entry_ctrl],
        [NodeOutputKind::Control, NodeOutputKind::PhiToken],
    );
    let [region_ctrl, _phi_tok] = f.node_outputs_exact::<2>(region).unwrap();

    // addr is a constant (SP + 0x10 — represented as a raw IntConst here).
    let addr = f.create_node(
        NodeKind::IntConst(0x10),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let [addr_out] = f.node_outputs_exact::<1>(addr).unwrap();

    // data value to store.
    let val = f.create_node(
        NodeKind::IntConst(0xdeadbeef_u64 as u128),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U32)],
    );
    let [val_out] = f.node_outputs_exact::<1>(val).unwrap();

    // Store(ram): inputs = [mem, addr, data], output = [mem].
    let store = f.create_node(
        NodeKind::Store(rsleigh::VnSpace::RAM),
        [mem0, addr_out, val_out],
        [NodeOutputKind::Memory(None)],
    );
    let [store_mem] = f.node_outputs_exact::<1>(store).unwrap();

    f.create_node(NodeKind::Return, [region_ctrl, store_mem], []);

    // ── Case 1: no stack_offset entry — addr edge MUST be present ──────

    let dot_no_offset = render(&f, entry);

    // The addr const node ("const 0x10:u64") must appear.
    assert!(
        dot_no_offset.contains("0x10"),
        "addr const must appear when no stack offset is set:\n{dot_no_offset}",
    );

    // Count total edges — baseline for the with-offset comparison.
    let edge_count_no_offset = edge_lines(&dot_no_offset).len();

    // ── Case 2: stack_offset present — addr edge must be suppressed ─────

    f.set_stack_offset(store, 0x10_i64);

    let dot_with_offset = render(&f, entry);

    // The node label must contain the offset text.
    assert!(
        dot_with_offset.contains("[sp+16]"),
        "Store label must show stack offset when stack_offset is set:\n{dot_with_offset}",
    );

    // The addr const's value must NOT appear as an edge endpoint
    // referencing the Store — i.e. the dot output has one fewer edge
    // than the no-offset case (the addr edge was dropped).
    let edge_count_with_offset = edge_lines(&dot_with_offset).len();
    assert!(
        edge_count_with_offset < edge_count_no_offset,
        "stack-offset Store must have fewer edges than non-stack Store \
         (addr edge suppressed): {edge_count_with_offset} vs {edge_count_no_offset}:\n\
         without offset:\n{dot_no_offset}\n\nwith offset:\n{dot_with_offset}",
    );
}

/// A `Load` whose `stack_offsets` entry is populated must NOT have an edge
/// from the addr-producer to the Load node in the rendered DOT output.
#[test]
fn load_addr_edge_suppressed_when_stack_offset_present() {
    let mut f = Function::new();

    let entry = f.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    f.set_entry(entry);
    let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();

    let init_mem = f.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory(None)]);
    let [mem0] = f.node_outputs_exact::<1>(init_mem).unwrap();

    // addr.
    let addr = f.create_node(
        NodeKind::IntConst(0x8),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let [addr_out] = f.node_outputs_exact::<1>(addr).unwrap();

    // Load(ram): inputs = [mem, addr], output = [value].
    let load = f.create_node(
        NodeKind::Load(rsleigh::VnSpace::RAM),
        [mem0, addr_out],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let [load_out] = f.node_outputs_exact::<1>(load).unwrap();

    f.create_node(NodeKind::Return, [entry_ctrl, load_out], []);

    // Without stack offset: addr const appears.
    let dot_no_offset = render(&f, entry);
    let edges_no_offset = edge_lines(&dot_no_offset).len();

    // With stack offset: addr edge is suppressed.
    f.set_stack_offset(load, -8_i64);
    let dot_with_offset = render(&f, entry);

    assert!(
        dot_with_offset.contains("[sp-8]"),
        "Load label must show negative stack offset:\n{dot_with_offset}",
    );

    let edges_with_offset = edge_lines(&dot_with_offset).len();
    assert!(
        edges_with_offset < edges_no_offset,
        "stack-offset Load must have fewer edges than non-stack Load \
         (addr edge suppressed): {edges_with_offset} vs {edges_no_offset}:\n\
         without offset:\n{dot_no_offset}\n\nwith offset:\n{dot_with_offset}",
    );
}

/// Nodes registered as `FunctionArg` carriers must show `[arg N]` in their
/// rendered label and a `peripheries=2` attribute for the double border.
#[test]
fn function_arg_node_label_includes_arg_index() {
    let mut f = Function::new();
    let entry = f.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    f.set_entry(entry);
    let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();

    // Create an InitialVar (stand-in for a register arg carrier) and register
    // it as argument index 0.
    let init_var = f.create_node(
        NodeKind::InitialVar(rsleigh::Vn {
            addr_off: 0,
            addr_space: rsleigh::VnSpace::REGISTER,
            size: 8,
        }),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let [iv_out] = f.node_outputs_exact::<1>(init_var).unwrap();
    f.register_arg_node(0, init_var);

    // Wire it into a Return so it's reachable.
    f.create_node(NodeKind::Return, [entry_ctrl, iv_out], []);

    let dot = render(&f, entry);

    assert!(
        dot.contains("[arg 0]"),
        "arg carrier node label must contain '[arg 0]':\n{dot}",
    );
    assert!(
        dot.contains("peripheries"),
        "arg carrier node must have peripheries attribute (double border):\n{dot}",
    );
}
