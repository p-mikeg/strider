// ── tests ─────────────────────────────────────────────────────────────────────

use super::*;
use crate::function::Function;
use crate::function::test_function;
use crate::node::{NodeKind, ValueKind, ValueType};
use crate::{IRViewer, IRWalker};
use ::dot::GraphDotDumper as _;

// ── helpers ───────────────────────────────────────────────────────────────

/// Interns `v` (masked to `ty`) and creates a single-output `IntConst` node,
/// returning its `NodeId`.  The const value is interned so the renderers'
/// `int_const_u128` / `const_value` reads resolve.
fn int_const_node(f: &mut Function, v: u128, ty: ValueType) -> NodeId {
    let id = f.intern_int_const(v, ty);
    f.graph_mut()
        .create_node(NodeKind::IntConst(id), [], [ValueKind::Typed(ty)])
}

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
    render_with_state(function, entry).0
}

/// Renders every node reachable from `entry`, returning the DOT string and the
/// dumper state that produced it (for the `dot id -> NodeId` mapping).
fn render_with_state(
    function: &Function,
    entry: NodeId,
) -> (String, super::FunctionDotDumperState) {
    let sleigh = probe_sleigh();
    let dumper = FunctionDotDumper {
        entry,
        function,
        sleigh: &sleigh,
        node_to_arg_indices: build_arg_reverse_map(function),
        nodes: None,
        center: None,
    };
    use ::dot::GraphDot;
    GraphDot::new(dumper, ::dot::DotStyle::empty())
        .as_dot_with_state()
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

/// A wide `IntConst` (I256/I512) node must render its actual value, not the
/// Debug form of the interning id (e.g. `IntConst(ConstId(0))`).
#[test]
fn render_int_const_wide_shows_value_not_debug() {
    let mut f = test_function();
    let entry = f
        .graph_mut()
        .create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let mem = f
        .graph_mut()
        .create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    let [ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
    let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();
    // limbs are little-endian; high limb set keeps it genuinely Wide.
    let id = f.intern_int_const_limbs(&[0x1234, 0xabcd, 0, 0x8000_0000_0000_0000], ValueType::I256);
    let wide = f.graph_mut().create_node(
        NodeKind::IntConst(id),
        [],
        [ValueKind::Typed(ValueType::I256)],
    );
    let [wide_value] = f.node_outputs_exact::<1>(wide).unwrap();
    f.graph_mut()
        .create_node(NodeKind::Return, [ctrl, mem_value, wide_value], []);

    let dot = render(&f, entry);
    assert!(
        dot.contains(":i256"),
        "wide const must render with a u256 suffix, got: {dot}"
    );
    assert!(
        !dot.contains("ConstId"),
        "wide const must not render as the Debug fallback (ConstId(...)), got: {dot}"
    );
    assert!(
        dot.contains("abcd") && dot.contains("1234"),
        "wide const must show its limb hex digits, got: {dot}"
    );
}

/// The raw renderer emits exactly one DOT node per reachable `NodeId` — no
/// constant inlining, no synthetic virtual nodes (unlike the pretty
/// renderer) — and omits detached / unreachable nodes.
#[test]
fn raw_dot_is_one_node_per_reachable_node_no_inlining() {
    let mut f = test_function();
    let entry = f
        .graph_mut()
        .create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let mem = f
        .graph_mut()
        .create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    let [ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
    let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();
    // A constant the pretty renderer would inline into its consumer.
    let c = int_const_node(&mut f, 0xABC_u128, ValueType::I64);
    let [c_value] = f.node_outputs_exact::<1>(c).unwrap();
    f.graph_mut()
        .create_node(NodeKind::Return, [ctrl, mem_value, c_value], []);
    // A detached node not reachable from entry — must NOT appear in the raw view.
    let zombie = int_const_node(&mut f, 0xDEAD_BEEF_u128, ValueType::I64);

    let dot = f.raw_dot().expect("raw_dot must render");

    // Exactly one DOT node declaration per reachable node — no inlining/virtuals.
    assert_eq!(
        node_decls(&dot).len(),
        f.walk().count(),
        "raw dot must have one node per reachable NodeId, got dot:\n{dot}"
    );
    // The detached node is excluded; reachable < arena here.
    assert!(f.walk().count() < f.graph().all_node_ids().count());
    assert!(
        !dot.contains(&format!("\"n{}\"", zombie.as_u32())),
        "detached node n{} must not be rendered",
        zombie.as_u32()
    );
    assert!(
        dot.contains("IntConst(#") && dot.contains("Bits(2748)"),
        "the reachable constant is a standalone node, not inlined, got dot:\n{dot}"
    );
    // The Return's three inputs are real edges.
    assert!(
        edge_lines(&dot).len() >= 3,
        "Return's input edges must be present"
    );
}

// ── determinism ───────────────────────────────────────────────────────────

/// Rendering the same graph twice must produce identical DOT output.
#[test]
fn dot_output_is_deterministic() {
    let mut f = test_function();
    let entry = f
        .graph_mut()
        .create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let [ctrl] = f.node_outputs_exact::<1>(entry).unwrap();

    let cs = f.graph_mut().create_node(
        NodeKind::Region,
        [ctrl],
        [ValueKind::Control, ValueKind::PhiToken],
    );
    let [cs_ctrl, _] = f.node_outputs_exact::<2>(cs).unwrap();
    f.graph_mut().create_node(NodeKind::Return, [cs_ctrl], []);

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
    let mut f = test_function();

    let entry = f
        .graph_mut()
        .create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
    let cond = int_const_node(&mut f, 0_u128, ValueType::I1);
    let [cond_value] = f.node_outputs_exact::<1>(cond).unwrap();
    let if_node = f.graph_mut().create_node(
        NodeKind::If,
        [entry_ctrl, cond_value],
        [ValueKind::Control, ValueKind::Control],
    );
    let [true_ctrl, false_ctrl] = f.node_outputs_exact::<2>(if_node).unwrap();

    f.graph_mut().create_node(NodeKind::Return, [true_ctrl], []);
    f.graph_mut()
        .create_node(NodeKind::Return, [false_ctrl], []);

    let first = render(&f, entry);
    let second = render(&f, entry);
    assert_eq!(first, second);
}

// ── structural correctness ────────────────────────────────────────────────

/// The DOT output must begin with `digraph` and end with `}`.
#[test]
fn dot_output_has_digraph_wrapper() {
    let mut f = test_function();
    let entry = f
        .graph_mut()
        .create_node(NodeKind::Entry, [], [ValueKind::Control]);
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
    let mut f = test_function();
    let entry = f
        .graph_mut()
        .create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let [ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
    let cond = int_const_node(&mut f, 1_u128, ValueType::I1);
    let [cond_value] = f.node_outputs_exact::<1>(cond).unwrap();
    let if_node = f.graph_mut().create_node(
        NodeKind::If,
        [ctrl, cond_value],
        [ValueKind::Control, ValueKind::Control],
    );
    let [tc, fc] = f.node_outputs_exact::<2>(if_node).unwrap();
    f.graph_mut().create_node(NodeKind::Return, [tc], []);
    f.graph_mut().create_node(NodeKind::Return, [fc], []);

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
    let mut f = test_function();
    let entry = f
        .graph_mut()
        .create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let [ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
    let cs = f.graph_mut().create_node(
        NodeKind::Region,
        [ctrl],
        [ValueKind::Control, ValueKind::PhiToken],
    );
    let [cs_ctrl, _] = f.node_outputs_exact::<2>(cs).unwrap();
    f.graph_mut().create_node(NodeKind::Return, [cs_ctrl], []);

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
    let mut f = test_function();
    let entry = f
        .graph_mut()
        .create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let [ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
    let cond = int_const_node(&mut f, 1_u128, ValueType::I1);
    let [cond_value] = f.node_outputs_exact::<1>(cond).unwrap();
    let if_node = f.graph_mut().create_node(
        NodeKind::If,
        [ctrl, cond_value],
        [ValueKind::Control, ValueKind::Control],
    );
    let [tc, fc] = f.node_outputs_exact::<2>(if_node).unwrap();
    f.graph_mut().create_node(NodeKind::Return, [tc], []);
    f.graph_mut().create_node(NodeKind::Return, [fc], []);

    let dot = render(&f, entry);

    let if_true_count = count_lines(&dot, |l| l.contains("if.true") && l.contains("[label="));
    let if_false_count = count_lines(&dot, |l| l.contains("if.false") && l.contains("[label="));
    assert_eq!(if_true_count, 1, "exactly one if.true declaration:\n{dot}");
    assert_eq!(
        if_false_count, 1,
        "exactly one if.false declaration:\n{dot}"
    );
}

// ── dot id -> NodeId mapping ──────────────────────────────────────────────

/// Every emitted DOT id that stands for an IR node resolves back to exactly
/// one node — including a constant, which is deliberately re-emitted as a
/// fresh box per use so a hot value never becomes an edge hub.  The mapping is
/// many-to-one, and that is the contract: a real node's id IS its NodeId, while
/// each const box gets a fresh `c`-prefixed id that still resolves back to it.
#[test]
fn dot_id_maps_back_to_its_ir_node_including_duplicated_consts() {
    let mut f = test_function();
    let entry = f
        .graph_mut()
        .create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let [ctrl] = f.node_outputs_exact::<1>(entry).unwrap();

    // ONE const feeding THREE consumers -> three dot boxes, one NodeId.
    // The adds must be REACHABLE (the dumper walks from entry), so the Return
    // consumes each one.
    let k = int_const_node(&mut f, 7_u128, ValueType::I64);
    let [kv] = f.node_outputs_exact::<1>(k).unwrap();
    let mut adds = Vec::new();
    let mut add_values = vec![ctrl];
    for _ in 0..3 {
        let a = f.graph_mut().create_node(
            NodeKind::IntBinaryOp(crate::IntBinaryOp::Add),
            [kv, kv],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [av] = f.node_outputs_exact::<1>(a).unwrap();
        adds.push(a);
        add_values.push(av);
    }
    f.graph_mut().create_node(NodeKind::Return, add_values, []);

    let (dot, state) = render_with_state(&f, entry);

    // The const really is duplicated (otherwise this test proves nothing).
    let const_ids: Vec<&str> = state
        .dot_to_node()
        .filter(|&(_, n)| n == k)
        .map(|(id, _)| id)
        .collect();
    assert!(
        const_ids.len() > 1,
        "the const must render as several boxes, got {}:\n{dot}",
        const_ids.len()
    );
    // ...and every one of them resolves back to that same const node.
    for id in &const_ids {
        assert_eq!(
            state.node_of_dot_id(id),
            Some(k),
            "dot id {id} -> the const"
        );
    }

    // Every non-const node resolves too.
    for n in adds {
        assert!(
            state.dot_to_node().any(|(_, m)| m == n),
            "every rendered node is in the map"
        );
    }

    // A virtual / unknown id has no NodeId — absent, not wrong.
    assert_eq!(state.node_of_dot_id("v99"), None, "virtuals are not mapped");
    assert_eq!(state.node_of_dot_id("nope"), None);
}

// ── label content ─────────────────────────────────────────────────────────

/// `MemPhi` nodes must render with the label "φ Mem".
#[test]
fn mem_phi_label_is_phi_mem() {
    let mut f = test_function();
    let entry = f
        .graph_mut()
        .create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let mem_phi = f
        .graph_mut()
        .create_node(NodeKind::MemPhi, [], [ValueKind::Memory]);
    // mem_phi is only reachable as a data input of Return (graph walk follows inputs)
    let [mp_value] = f.node_outputs_exact::<1>(mem_phi).unwrap();
    let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
    f.graph_mut()
        .create_node(NodeKind::Return, [entry_ctrl, mp_value], []);

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
    let mut f = test_function();
    let entry = f
        .graph_mut()
        .create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let c = int_const_node(&mut f, 0xdeadbeef_u128, ValueType::I32);
    let [c_value] = f.node_outputs_exact::<1>(c).unwrap();
    let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
    f.graph_mut()
        .create_node(NodeKind::Return, [entry_ctrl, c_value], []);

    let dot = render(&f, entry);
    assert!(
        dot.contains("0xdeadbeef"),
        "hex value must be in label:\n{dot}"
    );
    assert!(dot.contains("i32"), "type must be in label:\n{dot}");
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
    let mut f = test_function();
    let entry = f
        .graph_mut()
        .create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
    let cond_node = int_const_node(&mut f, 1_u128, ValueType::I1);
    let [cond] = f.node_outputs_exact::<1>(cond_node).unwrap();
    let if_node = f.graph_mut().create_node(
        NodeKind::If,
        [entry_ctrl, cond],
        [ValueKind::Control, ValueKind::Control],
    );
    let [true_ctrl, false_ctrl] = f.node_outputs_exact::<2>(if_node).unwrap();

    let cs_true = f.graph_mut().create_node(
        NodeKind::Region,
        [],
        [ValueKind::Control, ValueKind::PhiToken],
    );
    f.graph_mut().add_node_input(cs_true, true_ctrl);
    let [cs_true_ctrl, _] = f.node_outputs_exact::<2>(cs_true).unwrap();

    let cs_false = f.graph_mut().create_node(
        NodeKind::Region,
        [],
        [ValueKind::Control, ValueKind::PhiToken],
    );
    f.graph_mut().add_node_input(cs_false, false_ctrl);
    let [cs_false_ctrl, _] = f.node_outputs_exact::<2>(cs_false).unwrap();

    f.graph_mut()
        .create_node(NodeKind::Return, [cs_true_ctrl], []);
    f.graph_mut()
        .create_node(NodeKind::Return, [cs_false_ctrl], []);

    let sleigh = probe_sleigh();
    let dumper = FunctionDotDumper {
        entry,
        function: &f,
        sleigh: &sleigh,
        node_to_arg_indices: build_arg_reverse_map(&f),
        nodes: None,
        center: None,
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

/// Rendering a Call node with at least one clobbered output must succeed
/// even when the function's `call_clobbered` list is empty (the default for
/// a function built without a calling convention). Previously this panicked
/// with an OOB slice index.
#[test]
fn render_call_with_clobbered_output_uses_synthetic_label_when_slice_short() {
    let mut f = test_function();
    let entry = f
        .graph_mut()
        .create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let init_mem = f
        .graph_mut()
        .create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    let entry_ctrl = f.node_outputs(entry).iter().copied().next().unwrap();
    let mem = f.node_outputs(init_mem).iter().copied().next().unwrap();
    let target = int_const_node(&mut f, 0x1000_u128, ValueType::I64);
    let target_value = f.node_outputs(target).iter().copied().next().unwrap();
    // One Bool clobbered output, but the function's call_clobbered list is empty.
    let call = f.graph_mut().create_node(
        NodeKind::Call,
        [entry_ctrl, mem, target_value],
        [
            ValueKind::Control,
            ValueKind::Memory,
            ValueKind::Typed(ValueType::I1),
        ],
    );
    let call_ctrl = f.node_outputs(call).iter().copied().next().unwrap();
    let call_mem = f.node_outputs(call).iter().copied().nth(1).unwrap();
    let clob_value = f.node_outputs(call).iter().copied().nth(2).unwrap();
    f.graph_mut()
        .create_node(NodeKind::Return, [call_ctrl, call_mem, clob_value], []);

    // Render must not panic.  The clobbered output carries no `value_vn` tag
    // (it was created directly via `graph_mut` without `build_call_kind`), so
    // the fallback label is `out{output_index}` = `out2`.
    let dot = render(&f, entry);
    assert!(
        dot.contains("out2"),
        "expected synthetic out2 label, got:\n{dot}"
    );
}

/// `CallOther` whose user-op name is recorded in `Function::call_other_names`
/// must render with both the symbolic name and the numeric id.
#[test]
fn call_other_label_includes_resolved_name() {
    let mut f = test_function();
    let entry = f
        .graph_mut()
        .create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let init_mem = f
        .graph_mut()
        .create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    let entry_ctrl = f.node_outputs(entry).iter().copied().next().unwrap();
    let mem = f.node_outputs(init_mem).iter().copied().next().unwrap();
    let co = f.graph_mut().create_node(
        NodeKind::CallOther { user_op_id: 62 },
        [entry_ctrl, mem],
        [ValueKind::Control, ValueKind::Memory],
    );
    f.side_tables_mut().set_call_other_name(co, "setISAMode");
    let co_ctrl = f.node_outputs(co).iter().copied().next().unwrap();
    let co_mem = f.node_outputs(co).iter().copied().nth(1).unwrap();
    f.graph_mut()
        .create_node(NodeKind::Return, [co_ctrl, co_mem], []);

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
    let mut f = test_function();
    let entry = f
        .graph_mut()
        .create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let init_mem = f
        .graph_mut()
        .create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    let entry_ctrl = f.node_outputs(entry).iter().copied().next().unwrap();
    let mem = f.node_outputs(init_mem).iter().copied().next().unwrap();
    let co = f.graph_mut().create_node(
        NodeKind::CallOther { user_op_id: 7 },
        [entry_ctrl, mem],
        [ValueKind::Control, ValueKind::Memory],
    );
    // Intentionally do NOT call `set_call_other_name`.
    let co_ctrl = f.node_outputs(co).iter().copied().next().unwrap();
    let co_mem = f.node_outputs(co).iter().copied().nth(1).unwrap();
    f.graph_mut()
        .create_node(NodeKind::Return, [co_ctrl, co_mem], []);

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

// ── stack_offsets: keep addr edge, add a `base sp ± K` label ──────────────

/// A `Store` with a `stack_offsets` entry keeps its full address subtree (the
/// `base + K` address and its edge are always shown) and *additionally* gets a
/// `base sp ± K` quick-read line in its label.  The base is shown generically
/// as `base sp` (not the old `[sp±K]` form), since the address edge resolves
/// the concrete base.
#[test]
fn store_keeps_addr_edge_and_labels_base_sp_offset() {
    // Build: Entry → Region → Store(ram) → Return, with an addr IntConst.
    let mut f = test_function();

    let entry = f
        .graph_mut()
        .create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();

    let init_mem = f
        .graph_mut()
        .create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    let [mem0] = f.node_outputs_exact::<1>(init_mem).unwrap();

    let region = f.graph_mut().create_node(
        NodeKind::Region,
        [entry_ctrl],
        [ValueKind::Control, ValueKind::PhiToken],
    );
    let [region_ctrl, _phi_tok] = f.node_outputs_exact::<2>(region).unwrap();

    // addr is a constant (SP + 0x10 — represented as a raw IntConst here).
    let addr = int_const_node(&mut f, 0x10_u128, ValueType::I64);
    let [addr_value] = f.node_outputs_exact::<1>(addr).unwrap();

    // data value to store.
    let val = int_const_node(&mut f, (0xdeadbeef_u64) as u128, ValueType::I32);
    let [val_value] = f.node_outputs_exact::<1>(val).unwrap();

    // Store(ram): inputs = [mem, addr, data], output = [mem].
    let store = f.graph_mut().create_node(
        NodeKind::Store(rsleigh::VnSpace::RAM),
        [mem0, addr_value, val_value],
        [ValueKind::Memory],
    );
    let [store_mem] = f.node_outputs_exact::<1>(store).unwrap();

    f.graph_mut()
        .create_node(NodeKind::Return, [region_ctrl, store_mem], []);

    // ── Case 1: no stack_offset entry — addr edge MUST be present ──────

    let dot_no_offset = render(&f, entry);

    // The addr const node ("const 0x10:i64") must appear.
    assert!(
        dot_no_offset.contains("0x10"),
        "addr const must appear when no stack offset is set:\n{dot_no_offset}",
    );

    // Count total edges — baseline for the with-offset comparison.
    let edge_count_no_offset = edge_lines(&dot_no_offset).len();

    // ── Case 2: stack_offset present — addr edge kept, label gains offset ──

    f.side_tables_mut()
        .set_stack_slot(addr_value, addr_value, 0x10_i128);

    let dot_with_offset = render(&f, entry);

    // The label gains a `base sp + 16` quick-read line …
    assert!(
        dot_with_offset.contains("base sp + 16"),
        "Store label must show `base sp + 16` when stack_offset is set:\n{dot_with_offset}",
    );
    // … using the generic base form, NOT the old `[sp+K]` substitution.
    assert!(
        !dot_with_offset.contains("[sp+"),
        "must use `base sp + K`, not the old `[sp+K]` form:\n{dot_with_offset}",
    );

    // The addr edge is still present: the edge count is unchanged.
    let edge_count_with_offset = edge_lines(&dot_with_offset).len();
    assert_eq!(
        edge_count_with_offset, edge_count_no_offset,
        "stack_offset must not suppress the addr edge: \
         {edge_count_with_offset} vs {edge_count_no_offset}:\n\
         without offset:\n{dot_no_offset}\n\nwith offset:\n{dot_with_offset}",
    );
}

/// A `Load` with a `stack_offsets` entry keeps its address edge and gains a
/// `base sp - K` label line (negative offset shown with a minus sign).
#[test]
fn load_keeps_addr_edge_and_labels_base_sp_offset() {
    let mut f = test_function();

    let entry = f
        .graph_mut()
        .create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();

    let init_mem = f
        .graph_mut()
        .create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    let [mem0] = f.node_outputs_exact::<1>(init_mem).unwrap();

    // addr.
    let addr = int_const_node(&mut f, 0x8_u128, ValueType::I64);
    let [addr_value] = f.node_outputs_exact::<1>(addr).unwrap();

    // Load(ram): inputs = [mem, addr], output = [value].
    let load = f.graph_mut().create_node(
        NodeKind::Load(rsleigh::VnSpace::RAM),
        [mem0, addr_value],
        [ValueKind::Typed(ValueType::I64)],
    );
    let [load_value] = f.node_outputs_exact::<1>(load).unwrap();

    f.graph_mut()
        .create_node(NodeKind::Return, [entry_ctrl, load_value], []);

    // Without stack offset: addr const appears.
    let dot_no_offset = render(&f, entry);
    let edges_no_offset = edge_lines(&dot_no_offset).len();

    // With stack offset: addr edge kept, label gains `base sp - 8`.
    f.side_tables_mut()
        .set_stack_slot(addr_value, addr_value, -8_i128);
    let dot_with_offset = render(&f, entry);

    assert!(
        dot_with_offset.contains("base sp - 8"),
        "Load label must show `base sp - 8` for a negative offset:\n{dot_with_offset}",
    );
    assert!(
        !dot_with_offset.contains("[sp-"),
        "must use `base sp - K`, not the old `[sp-K]` form:\n{dot_with_offset}",
    );

    let edges_with_offset = edge_lines(&dot_with_offset).len();
    assert_eq!(
        edges_with_offset, edges_no_offset,
        "stack_offset must not suppress the addr edge: \
         {edges_with_offset} vs {edges_no_offset}:\n\
         without offset:\n{dot_no_offset}\n\nwith offset:\n{dot_with_offset}",
    );
}

/// Nodes registered as `FunctionArg` carriers must show `[arg N]` in their
/// rendered label and a `peripheries=2` attribute for the double border.
#[test]
fn function_arg_node_label_includes_arg_index() {
    let mut f = test_function();
    let entry = f
        .graph_mut()
        .create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();

    // Create an InitialVar (stand-in for a register arg carrier) and register
    // it as argument index 0.
    let init_var = f.graph_mut().create_node(
        NodeKind::InitialVar(crate::node::InitialVnId::from_index(0)),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    let [iv_value] = f.node_outputs_exact::<1>(init_var).unwrap();
    f.side_tables_mut().register_arg_value(0, iv_value);

    // Wire it into a Return so it's reachable.
    f.graph_mut()
        .create_node(NodeKind::Return, [entry_ctrl, iv_value], []);

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

// ── pred-numbered labels on Region / Phi / MemPhi inputs ────────────────
//
// A Region's k-th control input pairs 1:1 with the (k+1)-th value input
// (i.e. value-slot k after the leading phi-token at slot 0) of every Phi
// and MemPhi that joins at that Region.  Without per-edge index labels
// the visual graph reader can't tell which value flows from which
// predecessor — they have to count edge endpoints by hand.  Numbering
// edges as `pred0` / `pred1` / … on BOTH ends turns the matching into a
// single-glance scan.

/// Build a function with shape
///   Entry → If(true){RegionT}{RegionF};  RegionT → Join, RegionF → Join.
/// Join is a 2-predecessor Region with a tagged Phi (value-typed) AND
/// a MemPhi joining the per-arm memory chains.  Returns the rendered
/// DOT string + the Join region's `NodeId` for assertion convenience.
fn render_two_pred_join_with_phi_memphi() -> String {
    use rsleigh::Vn;
    let mut f = test_function();
    let entry = f
        .graph_mut()
        .create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
    let init_mem = f
        .graph_mut()
        .create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    let [im_value] = f.node_outputs_exact::<1>(init_mem).unwrap();

    // IntConst(1) (I1-typed true) so DBE/DCE leave the If alone for the test.
    let cond = int_const_node(&mut f, 1_u128, ValueType::I1);
    let [cond_value] = f.node_outputs_exact::<1>(cond).unwrap();
    let if_node = f.graph_mut().create_node(
        NodeKind::If,
        [entry_ctrl, cond_value],
        [ValueKind::Control, ValueKind::Control],
    );
    let if_outs = f.node_outputs(if_node).to_vec();

    // Two 1-pred control-state regions, one per If arm.
    let cs_t = f.graph_mut().create_node(
        NodeKind::Region,
        [if_outs[0]],
        [ValueKind::Control, ValueKind::PhiToken],
    );
    let cs_f = f.graph_mut().create_node(
        NodeKind::Region,
        [if_outs[1]],
        [ValueKind::Control, ValueKind::PhiToken],
    );
    let [cs_t_ctrl, _] = f.node_outputs_exact::<2>(cs_t).unwrap();
    let [cs_f_ctrl, _] = f.node_outputs_exact::<2>(cs_f).unwrap();

    // Join region with 2 control predecessors (the two If arms).
    let join = f.graph_mut().create_node(
        NodeKind::Region,
        [cs_t_ctrl, cs_f_ctrl],
        [ValueKind::Control, ValueKind::PhiToken],
    );
    let [_join_ctrl, join_phi_token] = f.node_outputs_exact::<2>(join).unwrap();

    // Two distinct values to phi over (so the Phi has two value inputs).
    let v_t = int_const_node(&mut f, 0xAA_u128, ValueType::I64);
    let v_f = int_const_node(&mut f, 0xBB_u128, ValueType::I64);
    let [v_t_value] = f.node_outputs_exact::<1>(v_t).unwrap();
    let [v_f_value] = f.node_outputs_exact::<1>(v_f).unwrap();

    // Tagged Phi at the join: [phi_token, v_t, v_f].
    let phi = f.graph_mut().create_node(
        NodeKind::Phi,
        [join_phi_token, v_t_value, v_f_value],
        [ValueKind::Typed(ValueType::I64)],
    );
    let [phi_value] = f.node_outputs_exact::<1>(phi).unwrap();
    f.set_vn_for_value(
        phi_value,
        Vn {
            addr_off: 0x10,
            addr_space: rsleigh::VnSpace::REGISTER,
            size: 8,
        },
    );

    // MemPhi at the join: [phi_token, mem_t, mem_f].  Reuse im_value
    // for both arms (the test doesn't need distinct mem producers; the
    // edge-label structure is what we're pinning).
    let mem_phi = f.graph_mut().create_node(
        NodeKind::MemPhi,
        [join_phi_token, im_value, im_value],
        [ValueKind::Memory],
    );
    let [mp_value] = f.node_outputs_exact::<1>(mem_phi).unwrap();

    // Wire the Phi's value + MemPhi's memory into Return so they're reachable.
    let [join_ctrl, _] = f.node_outputs_exact::<2>(join).unwrap();
    f.graph_mut()
        .create_node(NodeKind::Return, [join_ctrl, mp_value, phi_value], []);

    render(&f, entry)
}

#[test]
fn region_control_inputs_are_labelled_with_pred_index() {
    // The 2-predecessor Region MUST carry edges labelled "pred0" and
    // "pred1" on its incoming control edges.  Without numbering the
    // reader can't tell which arm corresponds to which Phi value slot
    // when there are >1 predecessors.
    let dot = render_two_pred_join_with_phi_memphi();
    assert!(
        dot.contains("label=\"pred0\""),
        "Region control input 0 must be labelled 'pred0':\n{dot}",
    );
    assert!(
        dot.contains("label=\"pred1\""),
        "Region control input 1 must be labelled 'pred1':\n{dot}",
    );
}

#[test]
fn phi_value_inputs_are_labelled_with_matching_pred_index() {
    // Phi inputs at slots 1, 2 (after the phi-token at slot 0) carry the
    // per-predecessor values.  Number them with the SAME `predN`
    // suffix the Region edge uses so the reader can match value to
    // predecessor at a glance.
    let dot = render_two_pred_join_with_phi_memphi();
    // The Region's predN labels are pinned by the test above; here we
    // pin that the Phi shares the same numbering scheme.  We count >=
    // 2 `pred0` occurrences (one on Region, one on Phi) and likewise
    // for pred1, plus the MemPhi pair (the next test pins MemPhi
    // specifically; total here is >= 4 each).
    assert!(
        count_lines(&dot, |l| l.contains("label=pred0")
            || l.contains("label=\"pred0"))
            >= 2,
        "expected pred0 on >= 2 edges (Region + Phi):\n{dot}",
    );
    assert!(
        count_lines(&dot, |l| l.contains("label=pred1")
            || l.contains("label=\"pred1"))
            >= 2,
        "expected pred1 on >= 2 edges (Region + Phi):\n{dot}",
    );
}

#[test]
fn mem_phi_value_inputs_are_labelled_with_matching_pred_index() {
    // MemPhi's per-predecessor memory inputs use the same `predN`
    // numbering as Region + Phi.  When the parent output is a
    // partitioned memory edge (Memory(Some(class))), the label
    // additionally carries the alias-class tag so the existing
    // mem:Stack / mem:Unknown signal is preserved.
    //
    // This fixture uses unified Memory (None) on both inputs so the
    // bare `predN` form is what we assert against.
    let dot = render_two_pred_join_with_phi_memphi();
    // Across Region + Phi + MemPhi we should see at least 3 occurrences
    // of pred0 (one per edge) and at least 3 of pred1.
    let pred0_count = count_lines(&dot, |l| {
        l.contains("label=pred0") || l.contains("label=\"pred0")
    });
    let pred1_count = count_lines(&dot, |l| {
        l.contains("label=pred1") || l.contains("label=\"pred1")
    });
    assert!(
        pred0_count >= 3,
        "expected pred0 on >= 3 edges (Region + Phi + MemPhi), got {pred0_count}:\n{dot}",
    );
    assert!(
        pred1_count >= 3,
        "expected pred1 on >= 3 edges (Region + Phi + MemPhi), got {pred1_count}:\n{dot}",
    );
}

// ── neighborhood BFS ────────────────────────────────────────────────────────

#[test]
fn neighborhood_bfs_bounds_depth_and_walks_both_directions() {
    use super::neighborhood::neighborhood_nodes;
    use crate::node::IntBinaryOp;
    use rustc_hash::FxHashMap;

    // const 5, const 8  →  Add.
    let mut f = test_function();
    let c1 = int_const_node(&mut f, 5, ValueType::I32);
    let c2 = int_const_node(&mut f, 8, ValueType::I32);
    let v1 = f.node_outputs(c1)[0];
    let v2 = f.node_outputs(c2)[0];
    let add = f.graph_mut().create_node(
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        [v1, v2],
        [ValueKind::Typed(ValueType::I32)],
    );
    // Forward (consumer) edges the IR doesn't index.
    let mut consumers: FxHashMap<NodeId, Vec<NodeId>> = FxHashMap::default();
    consumers.entry(c1).or_default().push(add);
    consumers.entry(c2).or_default().push(add);

    // Depth 0 = just the center.
    assert_eq!(
        neighborhood_nodes(&f, add, 0, 12, usize::MAX, &consumers).len(),
        1
    );
    // Depth 1 from Add reaches both operand producers (input edges).
    let d1 = neighborhood_nodes(&f, add, 1, 12, usize::MAX, &consumers);
    assert!(d1.contains(&c1) && d1.contains(&c2) && d1.contains(&add));
    assert_eq!(d1.len(), 3);
    // Depth 1 from a const reaches Add (output/consumer edge) — both directions.
    assert!(neighborhood_nodes(&f, c1, 1, 12, usize::MAX, &consumers).contains(&add));
    // A non-center hub is included but not expanded through: from c1 with cap 1,
    // Add (degree 2 > 1) is reached but not walked past, so c2 is never added.
    let capped = neighborhood_nodes(&f, c1, 3, 1, usize::MAX, &consumers);
    assert!(capped.contains(&c1) && capped.contains(&add) && !capped.contains(&c2));
    assert_eq!(capped.len(), 2);
}

#[test]
fn neighborhood_bfs_bounds_total_node_count() {
    use super::neighborhood::neighborhood_nodes;
    use crate::node::IntBinaryOp;
    use rustc_hash::FxHashMap;

    // const 5, const 8  →  Add. Depth 1 from Add reaches all 3 nodes (proven
    // by the sibling test), so a max_nodes budget of 2 must clamp it to 2.
    let mut f = test_function();
    let c1 = int_const_node(&mut f, 5, ValueType::I32);
    let c2 = int_const_node(&mut f, 8, ValueType::I32);
    let v1 = f.node_outputs(c1)[0];
    let v2 = f.node_outputs(c2)[0];
    let add = f.graph_mut().create_node(
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        [v1, v2],
        [ValueKind::Typed(ValueType::I32)],
    );
    let consumers: FxHashMap<NodeId, Vec<NodeId>> = FxHashMap::default();

    let budgeted = neighborhood_nodes(&f, add, 1, 12, 2, &consumers);
    assert_eq!(
        budgeted.len(),
        2,
        "budget of 2 must cap the neighborhood at 2 nodes"
    );
    assert!(budgeted.contains(&add), "center is always kept");
}

/// The centred node is highlighted and keeps its navigable `NodeId` id — even
/// when it is a constant.  A const normally renders as a fresh `c*` box per use
/// so a hot value never becomes an edge hub, but the centre is what the explorer
/// re-centres and searches on, so it must stay addressable: one shared box that
/// every in-view consumer points at.
#[test]
fn neighborhood_center_is_highlighted_and_navigable_even_when_const() {
    use crate::node::IntBinaryOp;

    let mut f = test_function();
    let k = int_const_node(&mut f, 7, ValueType::I32);
    let a = int_const_node(&mut f, 1, ValueType::I32);
    let kv = f.node_outputs(k)[0];
    let av = f.node_outputs(a)[0];
    let mk_add = |f: &mut Function, l, r| {
        f.graph_mut().create_node(
            NodeKind::IntBinaryOp(IntBinaryOp::Add),
            [l, r],
            [ValueKind::Typed(ValueType::I32)],
        )
    };
    let add1 = mk_add(&mut f, av, kv);
    let a1v = f.node_outputs(add1)[0];
    let add2 = mk_add(&mut f, a1v, kv);
    let a2v = f.node_outputs(add2)[0];

    // The consumer index is built by walking from `entry`, so the adds have to
    // be reachable for the const to have any recorded consumer at all.
    let entry = f.entry();
    let [ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
    let mem = crate::function::test_initial_memory(&f);
    let [memv] = f.node_outputs_exact::<1>(mem).unwrap();
    f.graph_mut()
        .create_node(NodeKind::Return, [ctrl, memv, a2v], []);

    let sleigh = probe_sleigh();
    let dumper = FunctionDotDumper {
        entry,
        function: &f,
        sleigh: &sleigh,
        node_to_arg_indices: build_arg_reverse_map(&f),
        nodes: None,
        center: None,
    };

    // A plain (non-const) centre: highlighted, id is its NodeId.
    let dot = dumper.neighborhood_dot(add1, 3, 12, 100).unwrap();
    let decl = node_decls(&dot)
        .into_iter()
        .find(|l| {
            l.trim_start()
                .starts_with(&format!("\"{}\" ", add1.as_u32()))
        })
        .unwrap_or_else(|| panic!("centre must render under its NodeId:\n{dot}"));
    assert!(
        decl.contains("#ffcc00"),
        "centre must be highlighted: {decl}"
    );

    // A const centre: the SAME box for both consumers (add1, add2), under its
    // NodeId — not one private `c*` box per use.
    let dot = dumper.neighborhood_dot(k, 3, 12, 100).unwrap();
    let kid = k.as_u32().to_string();
    let decls: Vec<_> = node_decls(&dot)
        .into_iter()
        .filter(|l| l.contains("const 0x7"))
        .collect();
    assert_eq!(decls.len(), 1, "centred const stays one shared box:\n{dot}");
    assert!(
        decls[0].trim_start().starts_with(&format!("\"{kid}\" ")),
        "centred const keeps its navigable NodeId id:\n{dot}"
    );
    assert!(
        decls[0].contains("#ffcc00"),
        "centre must be highlighted:\n{dot}"
    );
    for consumer in [add1, add2] {
        assert!(
            dot.contains(&format!("\"{kid}\" -> \"{}\"", consumer.as_u32())),
            "consumer {} must point at the centred const box:\n{dot}",
            consumer.as_u32()
        );
    }
}

#[test]
fn neighborhood_duplicates_shared_const_per_use() {
    use crate::node::IntBinaryOp;

    // const 7 feeds two Adds, both feeding the centered Add. A hot constant
    // like this should render one private box per use (avoiding a hub), not a
    // single shared box.
    let mut f = test_function();
    let k = int_const_node(&mut f, 7, ValueType::I32);
    let a = int_const_node(&mut f, 1, ValueType::I32);
    let b = int_const_node(&mut f, 2, ValueType::I32);
    let kv = f.node_outputs(k)[0];
    let av = f.node_outputs(a)[0];
    let bv = f.node_outputs(b)[0];
    let mk_add = |f: &mut Function, l, r| {
        f.graph_mut().create_node(
            NodeKind::IntBinaryOp(IntBinaryOp::Add),
            [l, r],
            [ValueKind::Typed(ValueType::I32)],
        )
    };
    let add1 = mk_add(&mut f, av, kv);
    let add2 = mk_add(&mut f, bv, kv);
    let a1v = f.node_outputs(add1)[0];
    let a2v = f.node_outputs(add2)[0];
    let center = mk_add(&mut f, a1v, a2v);

    let entry = f.entry();
    let sleigh = probe_sleigh();
    let dumper = FunctionDotDumper {
        entry,
        function: &f,
        sleigh: &sleigh,
        node_to_arg_indices: build_arg_reverse_map(&f),
        nodes: None,
        center: None,
    };
    let dot = dumper.neighborhood_dot(center, 3, 12, 100).unwrap();

    // const 7 is used by add1 and add2 → two distinct boxes, not one shared.
    let sevens = node_decls(&dot)
        .iter()
        .filter(|l| l.contains("const 0x7"))
        .count();
    assert_eq!(sevens, 2, "shared const must be duplicated per use:\n{dot}");

    // The RAW neighborhood is structure-faithful: the shared const stays ONE
    // box (n<id>), never duplicated, and the center keeps its 1:1 id.
    let raw = f.raw_neighborhood_dot(center, 3, 12, 100).unwrap();
    let raw_consts = node_decls(&raw)
        .iter()
        .filter(|l| l.contains("IntConst"))
        .count();
    assert_eq!(
        raw_consts, 3,
        "raw keeps one box per IR const (7,1,2):\n{raw}"
    );
    assert!(
        raw.contains(&format!("n{}", center.as_u32())),
        "raw neighborhood ids are IR node ids"
    );
}
