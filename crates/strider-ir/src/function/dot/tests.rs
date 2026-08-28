use super::*;
use crate::function::Function;
use crate::function::test_function;
use crate::node::{NodeKind, ValueKind, ValueType};
use crate::{IRViewer, IRWalker};
use ::dot::GraphDotDumper as _;

/// Interns `v` (masked to `ty`), then builds the `IntConst` node.
fn int_const_node(f: &mut Function, v: u128, ty: ValueType) -> NodeId {
    let id = f.intern_int_const(v, ty);
    f.graph_mut()
        .create_node(NodeKind::IntConst(id), [], [ValueKind::Typed(ty)])
}

/// Empty-buffer probe: these tests decode no instructions.
fn probe_sleigh() -> rsleigh::Sleigh<rsleigh::mem_readers::BufMemReader<Vec<u8>>> {
    let probe = rsleigh::mem_readers::BufMemReader::new(vec![], 0x0);
    rsleigh::Sleigh::new(
        rsleigh::sla_spec::SLA_SPEC_X86_64,
        rsleigh::pspec::PSPEC_X86_64,
        probe,
    )
    .expect("create probe Sleigh")
}

fn render(function: &Function, entry: NodeId) -> String {
    render_with_state(function, entry).0
}

/// [`render`] plus the dumper state, for the `dot id -> NodeId` mapping.
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

fn count_lines<'a>(s: &'a str, pred: impl Fn(&'a str) -> bool) -> usize {
    s.lines().filter(|l| pred(l)).count()
}

fn node_decls(dot: &str) -> Vec<&str> {
    dot.lines()
        .filter(|l| l.contains("[label=") && !l.contains("->"))
        .collect()
}

fn edge_lines(dot: &str) -> Vec<&str> {
    dot.lines().filter(|l| l.contains("->")).collect()
}

/// `f16` and `f128` have no Rust carrier to print through, so the label shows
/// raw bits, never an `f64::from_bits` reinterpretation.
#[test]
fn render_float_const_beyond_f64_shows_raw_bits() {
    for (ty, suffix) in [(ValueType::F16, ":f16"), (ValueType::F128, ":f128")] {
        let mut f = test_function();
        let entry = f
            .graph_mut()
            .create_node(NodeKind::Entry, [], [ValueKind::Control]);
        let mem = f
            .graph_mut()
            .create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
        let [ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
        let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();
        let c = f
            .graph_mut()
            .create_node(NodeKind::FloatConst(0x3c00), [], [ValueKind::Typed(ty)]);
        let [c_value] = f.node_outputs_exact::<1>(c).unwrap();
        f.graph_mut()
            .create_node(NodeKind::Return, [ctrl, mem_value, c_value], []);

        let dot = render(&f, entry);
        assert!(
            dot.contains(suffix),
            "{ty} const must render as {suffix}: {dot}"
        );
        assert!(
            !dot.contains(":f64"),
            "{ty} const must not render as f64: {dot}"
        );
    }
}

/// A wide `IntConst` must render its value, not the interning id.
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
    // Limbs are little-endian; the high limb keeps this genuinely Wide.
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

/// The raw renderer emits exactly one DOT node per reachable `NodeId` and
/// omits detached ones.
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
    let c = int_const_node(&mut f, 0xABC_u128, ValueType::I64);
    let [c_value] = f.node_outputs_exact::<1>(c).unwrap();
    f.graph_mut()
        .create_node(NodeKind::Return, [ctrl, mem_value, c_value], []);
    // Not reachable from entry, so it must not appear.
    let zombie = int_const_node(&mut f, 0xDEAD_BEEF_u128, ValueType::I64);

    let dot = f.raw_dot().expect("raw_dot must render");

    assert_eq!(
        node_decls(&dot).len(),
        f.walk().count(),
        "raw dot must have one node per reachable NodeId, got dot:\n{dot}"
    );
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
    assert!(
        edge_lines(&dot).len() >= 3,
        "Return's input edges must be present"
    );
}

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

/// A diamond must render deterministically regardless of walk order.
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

/// No edge may reference an id that was never declared.
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

    // The id is the part before the first space on a `"id" [label=...]` line.
    let declared: std::collections::HashSet<&str> = dot
        .lines()
        .filter(|l| l.contains("[label=") && !l.contains("->"))
        .filter_map(|l| l.trim().split('"').nth(1))
        .collect();

    for line in dot.lines().filter(|l| l.contains("->")) {
        let parts: Vec<&str> = line.trim().split("->").collect();
        if parts.len() < 2 {
            continue;
        }
        let src = parts[0].trim().trim_matches('"');
        // rhs may have attributes after the id like `"b" [color=...]`
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

/// The dot-id map is many-to-one: a real node's id IS its NodeId, while a
/// const gets a fresh `c`-prefixed box per use, each resolving back to it.
#[test]
fn dot_id_maps_back_to_its_ir_node_including_duplicated_consts() {
    let mut f = test_function();
    let entry = f
        .graph_mut()
        .create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let [ctrl] = f.node_outputs_exact::<1>(entry).unwrap();

    // One const feeding three consumers -> three dot boxes, one NodeId.
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
    for id in &const_ids {
        assert_eq!(
            state.node_of_dot_id(id),
            Some(k),
            "dot id {id} -> the const"
        );
    }

    for n in adds {
        assert!(
            state.dot_to_node().any(|(_, m)| m == n),
            "every rendered node is in the map"
        );
    }

    // A virtual / unknown id is absent from the map, not mis-mapped.
    assert_eq!(state.node_of_dot_id("v99"), None, "virtuals are not mapped");
    assert_eq!(state.node_of_dot_id("nope"), None);
}

#[test]
fn mem_phi_label_is_phi_mem() {
    let mut f = test_function();
    let entry = f
        .graph_mut()
        .create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let mem_phi = f
        .graph_mut()
        .create_node(NodeKind::MemPhi, [], [ValueKind::Memory]);
    // Only reachable as a data input of Return; the walk follows inputs.
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

/// The branch virtuals must be wired even when a branch successor renders
/// *before* the `If` itself.
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

    // cs_true *before* if_node is the ordering under test.
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

    let q = format!("\"{if_true_id}\"");
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
        "if.true must have >=1 incoming edge:\n{dot}"
    );
    assert!(
        edges_from >= 1,
        "if.true must have >=1 outgoing edge:\n{dot}"
    );
}

/// A clobbered Call output carrying no `value_vn` tag must still render, under
/// a synthetic `outN` label.
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
    // One clobbered output, left untagged.
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

    // The output carries no `value_vn` tag, so it falls back to `outN`.
    let dot = render(&f, entry);
    assert!(
        dot.contains("out2"),
        "expected synthetic out2 label, got:\n{dot}"
    );
}

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

/// Without a recorded name the label falls back to the bare id, with no stray
/// space where the name would have gone.
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

/// A `Store` with a `memory_offsets` entry keeps its address edge AND gains a
/// `base sp +/- K` line in its label.
#[test]
fn store_keeps_addr_edge_and_labels_base_sp_offset() {
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

    // addr stands in for SP + 0x10.
    let addr = int_const_node(&mut f, 0x10_u128, ValueType::I64);
    let [addr_value] = f.node_outputs_exact::<1>(addr).unwrap();

    let val = int_const_node(&mut f, (0xdeadbeef_u64) as u128, ValueType::I32);
    let [val_value] = f.node_outputs_exact::<1>(val).unwrap();

    let store = f.graph_mut().create_node(
        NodeKind::Store(rsleigh::VnSpace::RAM),
        [mem0, addr_value, val_value],
        [ValueKind::Memory],
    );
    let [store_mem] = f.node_outputs_exact::<1>(store).unwrap();

    f.graph_mut()
        .create_node(NodeKind::Return, [region_ctrl, store_mem], []);

    // No stack_offset entry: the addr edge must be present.
    let dot_no_offset = render(&f, entry);

    assert!(
        dot_no_offset.contains("0x10"),
        "addr const must appear when no stack offset is set:\n{dot_no_offset}",
    );

    let edge_count_no_offset = edge_lines(&dot_no_offset).len();

    f.side_tables_mut()
        .set_stack_slot(addr_value, addr_value, 0x10_i128);

    let dot_with_offset = render(&f, entry);

    assert!(
        dot_with_offset.contains("base sp + 16"),
        "Store label must show `base sp + 16` when stack_offset is set:\n{dot_with_offset}",
    );
    assert!(
        !dot_with_offset.contains("[sp+"),
        "must use `base sp + K`, not the old `[sp+K]` form:\n{dot_with_offset}",
    );

    let edge_count_with_offset = edge_lines(&dot_with_offset).len();
    assert_eq!(
        edge_count_with_offset, edge_count_no_offset,
        "stack_offset must not suppress the addr edge: \
         {edge_count_with_offset} vs {edge_count_no_offset}:\n\
         without offset:\n{dot_no_offset}\n\nwith offset:\n{dot_with_offset}",
    );
}

/// Negative offsets render with a minus sign, and the addr edge survives.
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

    let addr = int_const_node(&mut f, 0x8_u128, ValueType::I64);
    let [addr_value] = f.node_outputs_exact::<1>(addr).unwrap();

    let load = f.graph_mut().create_node(
        NodeKind::Load(rsleigh::VnSpace::RAM),
        [mem0, addr_value],
        [ValueKind::Typed(ValueType::I64)],
    );
    let [load_value] = f.node_outputs_exact::<1>(load).unwrap();

    f.graph_mut()
        .create_node(NodeKind::Return, [entry_ctrl, load_value], []);

    let dot_no_offset = render(&f, entry);
    let edges_no_offset = edge_lines(&dot_no_offset).len();

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

/// Arg carrier nodes show `[arg N]` and get `peripheries=2` (double border).
#[test]
fn function_arg_node_label_includes_arg_index() {
    let mut f = test_function();
    let entry = f
        .graph_mut()
        .create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();

    let init_var = f.graph_mut().create_node(
        NodeKind::InitialVar(crate::node::InitialVnId::from_index(0)),
        [],
        [ValueKind::Typed(ValueType::I64)],
    );
    let [iv_value] = f.node_outputs_exact::<1>(init_var).unwrap();
    f.side_tables_mut().register_arg_value(0, iv_value);

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

/// Entry -> If(true){RegionT}{RegionF}, both arms into a 2-predecessor Join with
/// a tagged Phi and a MemPhi.
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

    // A true condition, so DBE/DCE leave the If alone.
    let cond = int_const_node(&mut f, 1_u128, ValueType::I1);
    let [cond_value] = f.node_outputs_exact::<1>(cond).unwrap();
    let if_node = f.graph_mut().create_node(
        NodeKind::If,
        [entry_ctrl, cond_value],
        [ValueKind::Control, ValueKind::Control],
    );
    let if_outs = f.node_outputs(if_node).to_vec();

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

    let join = f.graph_mut().create_node(
        NodeKind::Region,
        [cs_t_ctrl, cs_f_ctrl],
        [ValueKind::Control, ValueKind::PhiToken],
    );
    let [_join_ctrl, join_phi_token] = f.node_outputs_exact::<2>(join).unwrap();

    let v_t = int_const_node(&mut f, 0xAA_u128, ValueType::I64);
    let v_f = int_const_node(&mut f, 0xBB_u128, ValueType::I64);
    let [v_t_value] = f.node_outputs_exact::<1>(v_t).unwrap();
    let [v_f_value] = f.node_outputs_exact::<1>(v_f).unwrap();

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

    let mem_phi = f.graph_mut().create_node(
        NodeKind::MemPhi,
        [join_phi_token, im_value, im_value],
        [ValueKind::Memory],
    );
    let [mp_value] = f.node_outputs_exact::<1>(mem_phi).unwrap();

    let [join_ctrl, _] = f.node_outputs_exact::<2>(join).unwrap();
    f.graph_mut()
        .create_node(NodeKind::Return, [join_ctrl, mp_value, phi_value], []);

    render(&f, entry)
}

#[test]
fn region_control_inputs_are_labelled_with_pred_index() {
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
    let dot = render_two_pred_join_with_phi_memphi();
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
    let dot = render_two_pred_join_with_phi_memphi();
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

#[test]
fn neighborhood_bfs_bounds_depth_and_walks_both_directions() {
    use super::neighborhood::neighborhood_nodes;
    use crate::node::IntBinaryOp;
    use rustc_hash::FxHashMap;

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
    let mut consumers: FxHashMap<NodeId, Vec<NodeId>> = FxHashMap::default();
    consumers.entry(c1).or_default().push(add);
    consumers.entry(c2).or_default().push(add);

    assert_eq!(
        neighborhood_nodes(&f, add, 0, 12, usize::MAX, false, &consumers).len(),
        1
    );
    let d1 = neighborhood_nodes(&f, add, 1, 12, usize::MAX, false, &consumers);
    assert!(d1.contains(&c1) && d1.contains(&c2) && d1.contains(&add));
    assert_eq!(d1.len(), 3);
    // Depth 1 from a const reaches Add via the consumer edge: both directions.
    assert!(neighborhood_nodes(&f, c1, 1, 12, usize::MAX, false, &consumers).contains(&add));
    // Depth 3 from c1 reaches the whole tiny graph via both directions.
    let all = neighborhood_nodes(&f, c1, 3, 12, usize::MAX, false, &consumers);
    assert!(all.contains(&c1) && all.contains(&add) && all.contains(&c2));
    assert_eq!(all.len(), 3);
}

#[test]
fn neighborhood_hub_follows_producers_not_consumers() {
    use super::neighborhood::neighborhood_nodes;
    use crate::node::IntBinaryOp;
    use rustc_hash::FxHashMap;

    // c1, c2 -> add(hub) -> use1.
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
    let av = f.node_outputs(add)[0];
    let c3 = int_const_node(&mut f, 1, ValueType::I32);
    let v3 = f.node_outputs(c3)[0];
    let use1 = f.graph_mut().create_node(
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        [av, v3],
        [ValueKind::Typed(ValueType::I32)],
    );
    let mut consumers: FxHashMap<NodeId, Vec<NodeId>> = FxHashMap::default();
    consumers.entry(c1).or_default().push(add);
    consumers.entry(c2).or_default().push(add);
    consumers.entry(add).or_default().push(use1);
    consumers.entry(c3).or_default().push(use1);

    // Add has 2 producers (c1, c2) and 1 consumer (use1). With the default
    // consumer-only hub count at cap 2, Add is NOT a hub (1 <= 2), so its
    // consumer use1 is followed.
    let n = neighborhood_nodes(&f, c1, 4, 2, usize::MAX, false, &consumers);
    assert!(n.contains(&use1), "consumer-only count keeps Add a non-hub");
    // count_producers folds in the 2 inputs (degree 3 > 2), so Add becomes a
    // hub: its producer c2 is still reached but its consumer use1 is suppressed.
    let n2 = neighborhood_nodes(&f, c1, 4, 2, usize::MAX, true, &consumers);
    assert!(n2.contains(&c2), "hub producers followed");
    assert!(!n2.contains(&use1), "hub consumer fan-out suppressed");
}

#[test]
fn neighborhood_bfs_bounds_total_node_count() {
    use super::neighborhood::neighborhood_nodes;
    use crate::node::IntBinaryOp;
    use rustc_hash::FxHashMap;

    // Depth 1 from Add reaches all 3 nodes, so a budget of 2 must clamp it.
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

    let budgeted = neighborhood_nodes(&f, add, 1, 12, 2, false, &consumers);
    assert_eq!(
        budgeted.len(),
        2,
        "budget of 2 must cap the neighborhood at 2 nodes"
    );
    assert!(budgeted.contains(&add), "center is always kept");
}

/// The centred node keeps its navigable `NodeId` even when const, where a
/// non-centre const would render as a fresh `c*` box per use.
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

    // The consumer index is built by walking from `entry`, so the adds must be
    // reachable.
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

    let dot = dumper.neighborhood_dot(add1, 3, 12, 100, false).unwrap();
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

    // A const centre: the SAME box for both consumers, under its NodeId.
    let dot = dumper.neighborhood_dot(k, 3, 12, 100, false).unwrap();
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

    // const 7 feeds two Adds, both feeding the centered Add.
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
    let dot = dumper.neighborhood_dot(center, 3, 12, 100, false).unwrap();

    let sevens = node_decls(&dot)
        .iter()
        .filter(|l| l.contains("const 0x7"))
        .count();
    assert_eq!(sevens, 2, "shared const must be duplicated per use:\n{dot}");

    // In the raw neighborhood the shared const stays one box.
    let raw = f.raw_neighborhood_dot(center, 3, 12, 100, false).unwrap();
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

/// The integer and float index spaces are separate, so a float carrier must
/// not render as the integer arg of the same index.
#[test]
fn function_float_arg_node_label_is_distinct_from_the_integer_index() {
    let mut f = test_function();
    let entry = f
        .graph_mut()
        .create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();

    let carrier = |f: &mut Function, vn_index: usize| {
        let node = f.graph_mut().create_node(
            NodeKind::InitialVar(crate::node::InitialVnId::from_index(vn_index)),
            [],
            [ValueKind::Typed(ValueType::I64)],
        );
        f.node_outputs_exact::<1>(node).unwrap()[0]
    };
    let int_value = carrier(&mut f, 0);
    let float_value = carrier(&mut f, 1);
    f.side_tables_mut().register_arg_value(0, int_value);
    f.side_tables_mut().register_float_arg_value(0, float_value);

    f.graph_mut()
        .create_node(NodeKind::Return, [entry_ctrl, int_value, float_value], []);

    let dot = render(&f, entry);

    assert!(
        dot.contains("[float arg 0]"),
        "float carrier must be labelled in its own index space:\n{dot}",
    );
    assert_eq!(
        count_lines(&dot, |l| l.contains("[arg 0]")
            && !l.contains("[float arg 0]")),
        1,
        "exactly one node carries the integer arg 0 label:\n{dot}",
    );
}
