use dashu_int::UBig;

use super::*;
use crate::function::Function;
use crate::function::test_function;
use crate::node::{NodeKind, ValueKind, ValueType};
use crate::{IRViewer, IRWalker};

/// Interns `v` (masked to `ty`), then builds the `IntConst` node.
fn int_const_node(f: &mut Function, v: u128, ty: ValueType) -> NodeId {
    let id = f.intern_int_const(v, ty);
    f.graph_mut()
        .create_node(NodeKind::IntConst(id), [], [ValueKind::Typed(ty)])
}

fn make_dumper(function: &Function) -> FunctionDotDumper<'_> {
    function.dot_dumper(strider_target::SleighArch::x86_64().names())
}

fn render(function: &Function) -> String {
    render_with_state(function).0
}

/// [`render`] plus the dumper state, for the `dot id -> NodeId` mapping, which
/// only this test needs: the emitter loop is `GraphDot::as_dot`'s, which drops
/// the state.
fn render_with_state(function: &Function) -> (String, super::FunctionDotDumperState) {
    let dumper = make_dumper(function);
    let mut out = ::dot::DotEmitter::new("G", &::dot::DotStyle::default());
    let mut state = FunctionDotDumperState::default();
    for node in dumper.iter_nodes() {
        dumper
            .dump_as_dot(node, &mut out, &mut state)
            .expect("render must succeed");
    }
    (out.finish(), state)
}

/// `to_dot` renders un-validated IR, where a `Phi` can carry no output at all.
#[test]
fn phi_without_an_output_labels_rather_than_panicking() {
    let mut f = test_function();
    let phi = f.graph_mut().create_node(NodeKind::Phi, [], []);
    let dumper = make_dumper(&f);
    assert_eq!(dumper.pretty_label(phi), "φ Val");
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
    for (ty, suffix) in [
        (ValueType::float(16), ":f16"),
        (ValueType::float(128), ":f128"),
    ] {
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

        let dot = render(&f);
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

/// Every NaN prints as `NaN`, so a NaN label spells its bits instead.
#[test]
fn render_float_const_nan_shows_bits() {
    for (ty, quiet, signalling) in [
        (ValueType::float(32), 0x7fc0_0000_u64, 0x7fa0_0000_u64),
        (
            ValueType::float(64),
            0x7ff8_0000_0000_0000,
            0xfff4_0000_0000_0000,
        ),
    ] {
        let mut f = test_function();
        let entry = f
            .graph_mut()
            .create_node(NodeKind::Entry, [], [ValueKind::Control]);
        let mem = f
            .graph_mut()
            .create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
        let [ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
        let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();
        let mut rets = vec![];
        for bits in [quiet, signalling] {
            let c =
                f.graph_mut()
                    .create_node(NodeKind::FloatConst(bits), [], [ValueKind::Typed(ty)]);
            let [v] = f.node_outputs_exact::<1>(c).unwrap();
            rets.push(v);
        }
        f.graph_mut()
            .create_node(NodeKind::Return, [ctrl, mem_value, rets[0], rets[1]], []);

        let dot = render(&f);
        for bits in [quiet, signalling] {
            let label = format!("const {bits:#x}:{ty}");
            assert!(dot.contains(&label), "missing {label}: {dot}");
        }
        assert!(!dot.contains("NaN"), "{dot}");
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
    // Limbs are little-endian; the high limb puts this past `u128`.
    let id = f.intern_int_const(
        UBig::from_words(&[0x1234, 0xabcd, 0, 0x8000_0000_0000_0000]),
        ValueType::int(256),
    );
    let wide = f.graph_mut().create_node(
        NodeKind::IntConst(id),
        [],
        [ValueKind::Typed(ValueType::int(256))],
    );
    let [wide_value] = f.node_outputs_exact::<1>(wide).unwrap();
    f.graph_mut()
        .create_node(NodeKind::Return, [ctrl, mem_value, wide_value], []);

    let dot = render(&f);
    assert!(
        dot.contains(":i256"),
        "wide const must render with a u256 suffix, got: {dot}"
    );
    assert!(
        !dot.contains("IntConstId"),
        "wide const must not render as the Debug fallback (IntConstId(...)), got: {dot}"
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
    let c = int_const_node(&mut f, 0xABC_u128, ValueType::int(64));
    let [c_value] = f.node_outputs_exact::<1>(c).unwrap();
    f.graph_mut()
        .create_node(NodeKind::Return, [ctrl, mem_value, c_value], []);
    // Not reachable from entry, so it must not appear.
    let zombie = int_const_node(&mut f, 0xDEAD_BEEF_u128, ValueType::int(64));

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
        dot.contains("IntConst(#") && dot.contains("= 2748"),
        "the reachable constant is a standalone node, not inlined, got dot:\n{dot}"
    );
    assert!(
        edge_lines(&dot).len() >= 3,
        "Return's input edges must be present"
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

    let first = render(&f);
    let second = render(&f);
    assert_eq!(first, second);
}

#[test]
fn dot_output_has_digraph_wrapper() {
    let mut f = test_function();
    f.graph_mut()
        .create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let dot = render(&f);
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

    let dot = render(&f);

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

    let dot = render(&f);
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

    let dot = render(&f);

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
    let k = int_const_node(&mut f, 7_u128, ValueType::int(64));
    let [kv] = f.node_outputs_exact::<1>(k).unwrap();
    let mut adds = Vec::new();
    let mut add_values = vec![ctrl];
    for _ in 0..3 {
        let a = f.graph_mut().create_node(
            NodeKind::IntBinaryOp(crate::IntBinaryOp::Add),
            [kv, kv],
            [ValueKind::Typed(ValueType::int(64))],
        );
        let [av] = f.node_outputs_exact::<1>(a).unwrap();
        adds.push(a);
        add_values.push(av);
    }
    f.graph_mut().create_node(NodeKind::Return, add_values, []);

    let (dot, state) = render_with_state(&f);

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

    let dot = render(&f);
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
    let c = int_const_node(&mut f, 0xdeadbeef_u128, ValueType::int(32));
    let [c_value] = f.node_outputs_exact::<1>(c).unwrap();
    let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
    f.graph_mut()
        .create_node(NodeKind::Return, [entry_ctrl, c_value], []);

    let dot = render(&f);
    assert!(
        dot.contains("0xdeadbeef"),
        "hex value must be in label:\n{dot}"
    );
    assert!(dot.contains("i32"), "type must be in label:\n{dot}");
}

/// The pretty label is the hex value plus the declared type, at a width the
/// `u128` carrier holds and at one past it alike.
#[test]
fn pretty_const_label_is_the_hex_value_and_the_type_at_every_width() {
    fn render_one(value: impl Into<UBig>, ty: ValueType) -> String {
        let mut f = test_function();
        let entry = f
            .graph_mut()
            .create_node(NodeKind::Entry, [], [ValueKind::Control]);
        let id = f.intern_int_const(value, ty);
        let c = f
            .graph_mut()
            .create_node(NodeKind::IntConst(id), [], [ValueKind::Typed(ty)]);
        let [c_value] = f.node_outputs_exact::<1>(c).unwrap();
        let [entry_ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
        f.graph_mut()
            .create_node(NodeKind::Return, [entry_ctrl, c_value], []);
        render(&f)
    }

    let narrow = render_one(0xdead_beef_u128, ValueType::int(32));
    assert!(
        narrow.contains("const 0xdeadbeef:i32"),
        "narrow label:\n{narrow}"
    );
    // Over 64 bits, still within the `u128` carrier.
    let i72 = render_one(UBig::from_words(&[0xdead_beef, 0xff]), ValueType::int(72));
    assert!(
        i72.contains("const 0xff00000000deadbeef:i72"),
        "i72 label:\n{i72}"
    );
    // Bit 128 set: no `u128` carries it.
    let wide = render_one(
        UBig::from_words(&[0xdead_beef, 0, 1, 0]),
        ValueType::int(256),
    );
    assert!(
        wide.contains("const 0x1000000000000000000000000deadbeef:i256"),
        "wide label:\n{wide}"
    );
    // Zero at a wide width: the hex trim must not render an empty value.
    let zero = render_one(UBig::ZERO, ValueType::int(256));
    assert!(zero.contains("const 0x0:i256"), "zero label:\n{zero}");
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

    let dumper = make_dumper(&f);

    let style = ::dot::DotStyle::default();
    let mut emitter = ::dot::DotEmitter::new("test", &style);
    let mut state = FunctionDotDumperState::default();

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
    let target = int_const_node(&mut f, 0x1000_u128, ValueType::int(64));
    let target_value = f.node_outputs(target).iter().copied().next().unwrap();
    // One clobbered output, left untagged.
    let call = f.graph_mut().create_node(
        NodeKind::Call { cc: None },
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
    let dot = render(&f);
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
    f.set_call_other_name(62, "setISAMode");
    let co_ctrl = f.node_outputs(co).iter().copied().next().unwrap();
    let co_mem = f.node_outputs(co).iter().copied().nth(1).unwrap();
    f.graph_mut()
        .create_node(NodeKind::Return, [co_ctrl, co_mem], []);

    let dot = render(&f);
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

    let dot = render(&f);
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
    let addr = int_const_node(&mut f, 0x10_u128, ValueType::int(64));
    let [addr_value] = f.node_outputs_exact::<1>(addr).unwrap();

    let val = int_const_node(&mut f, (0xdeadbeef_u64) as u128, ValueType::int(32));
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
    let dot_no_offset = render(&f);

    assert!(
        dot_no_offset.contains("0x10"),
        "addr const must appear when no stack offset is set:\n{dot_no_offset}",
    );

    let edge_count_no_offset = edge_lines(&dot_no_offset).len();

    f.set_stack_slot(addr_value, addr_value, 0x10_i128);

    let dot_with_offset = render(&f);

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

    let addr = int_const_node(&mut f, 0x8_u128, ValueType::int(64));
    let [addr_value] = f.node_outputs_exact::<1>(addr).unwrap();

    let load = f.graph_mut().create_node(
        NodeKind::Load(rsleigh::VnSpace::RAM),
        [mem0, addr_value],
        [ValueKind::Typed(ValueType::int(64))],
    );
    let [load_value] = f.node_outputs_exact::<1>(load).unwrap();

    f.graph_mut()
        .create_node(NodeKind::Return, [entry_ctrl, load_value], []);

    let dot_no_offset = render(&f);
    let edges_no_offset = edge_lines(&dot_no_offset).len();

    f.set_stack_slot(addr_value, addr_value, -8_i128);
    let dot_with_offset = render(&f);

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
        [ValueKind::Typed(ValueType::int(64))],
    );
    let [iv_value] = f.node_outputs_exact::<1>(init_var).unwrap();
    f.side_tables_mut().register_arg_value(0, iv_value);

    f.graph_mut()
        .create_node(NodeKind::Return, [entry_ctrl, iv_value], []);

    let dot = render(&f);

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

    let v_t = int_const_node(&mut f, 0xAA_u128, ValueType::int(64));
    let v_f = int_const_node(&mut f, 0xBB_u128, ValueType::int(64));
    let [v_t_value] = f.node_outputs_exact::<1>(v_t).unwrap();
    let [v_f_value] = f.node_outputs_exact::<1>(v_f).unwrap();

    let phi = f.graph_mut().create_node(
        NodeKind::Phi,
        [join_phi_token, v_t_value, v_f_value],
        [ValueKind::Typed(ValueType::int(64))],
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

    render(&f)
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

/// Wires `value` into a `Return`, so the walk from `entry` reaches its
/// producer.
fn return_value(f: &mut Function, value: ValueId) -> NodeId {
    let [ctrl] = f.node_outputs_exact::<1>(f.entry()).unwrap();
    let mem = crate::function::test_initial_memory(f);
    let [memv] = f.node_outputs_exact::<1>(mem).unwrap();
    f.graph_mut()
        .create_node(NodeKind::Return, [ctrl, memv, value], [])
}

#[test]
fn neighborhood_bfs_bounds_depth_and_walks_both_directions() {
    use super::neighborhood::neighborhood_nodes;
    use crate::node::IntBinaryOp;

    let mut f = test_function();
    let c1 = int_const_node(&mut f, 5, ValueType::int(32));
    let c2 = int_const_node(&mut f, 8, ValueType::int(32));
    let v1 = f.node_outputs(c1)[0];
    let v2 = f.node_outputs(c2)[0];
    let add = f.graph_mut().create_node(
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        [v1, v2],
        [ValueKind::Typed(ValueType::int(32))],
    );
    let av = f.node_outputs(add)[0];
    let ret = return_value(&mut f, av);

    assert_eq!(
        neighborhood_nodes(&f, add, 0, 12, usize::MAX, false).len(),
        1
    );
    // Depth 1 from Add: its producers and its consumer, both directions.
    let d1 = neighborhood_nodes(&f, add, 1, 12, usize::MAX, false);
    assert!(d1.contains(&c1) && d1.contains(&c2) && d1.contains(&ret));
    assert_eq!(d1.len(), 4);
    // Depth 1 from a const reaches Add via the consumer edge.
    assert!(neighborhood_nodes(&f, c1, 1, 12, usize::MAX, false).contains(&add));
    // Depth 2 from c1: Add, then its other producer and its consumer.
    let d2 = neighborhood_nodes(&f, c1, 2, 12, usize::MAX, false);
    assert!(d2.contains(&add) && d2.contains(&c2) && d2.contains(&ret));
    assert_eq!(d2.len(), 4);
}

#[test]
fn neighborhood_hub_follows_producers_not_consumers() {
    use super::neighborhood::neighborhood_nodes;
    use crate::node::IntBinaryOp;

    // c1, c2 -> add(hub) -> use1 -> Return.
    let mut f = test_function();
    let c1 = int_const_node(&mut f, 5, ValueType::int(32));
    let c2 = int_const_node(&mut f, 8, ValueType::int(32));
    let v1 = f.node_outputs(c1)[0];
    let v2 = f.node_outputs(c2)[0];
    let add = f.graph_mut().create_node(
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        [v1, v2],
        [ValueKind::Typed(ValueType::int(32))],
    );
    let av = f.node_outputs(add)[0];
    let c3 = int_const_node(&mut f, 1, ValueType::int(32));
    let v3 = f.node_outputs(c3)[0];
    let use1 = f.graph_mut().create_node(
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        [av, v3],
        [ValueKind::Typed(ValueType::int(32))],
    );
    let uv = f.node_outputs(use1)[0];
    return_value(&mut f, uv);

    // Add has 2 producers (c1, c2) and 1 consumer (use1). With the default
    // consumer-only hub count at cap 2, Add is NOT a hub (1 <= 2), so its
    // consumer use1 is followed.
    let n = neighborhood_nodes(&f, c1, 4, 2, usize::MAX, false);
    assert!(n.contains(&use1), "consumer-only count keeps Add a non-hub");
    // count_producers folds in the 2 inputs (degree 3 > 2), so Add becomes a
    // hub: its producer c2 is still reached but its consumer use1 is suppressed.
    let n2 = neighborhood_nodes(&f, c1, 4, 2, usize::MAX, true);
    assert!(n2.contains(&c2), "hub producers followed");
    assert!(!n2.contains(&use1), "hub consumer fan-out suppressed");
}

#[test]
fn neighborhood_bfs_bounds_total_node_count() {
    use super::neighborhood::neighborhood_nodes;
    use crate::node::IntBinaryOp;

    // Depth 1 from Add reaches all 4 nodes, so a budget of 2 must clamp it.
    let mut f = test_function();
    let c1 = int_const_node(&mut f, 5, ValueType::int(32));
    let c2 = int_const_node(&mut f, 8, ValueType::int(32));
    let v1 = f.node_outputs(c1)[0];
    let v2 = f.node_outputs(c2)[0];
    let add = f.graph_mut().create_node(
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        [v1, v2],
        [ValueKind::Typed(ValueType::int(32))],
    );
    let av = f.node_outputs(add)[0];
    return_value(&mut f, av);

    let budgeted = neighborhood_nodes(&f, add, 1, 12, 2, false);
    assert_eq!(
        budgeted.len(),
        2,
        "budget of 2 must cap the neighborhood at 2 nodes"
    );
    assert!(budgeted.contains(&add), "center is always kept");
}

/// An `InitialVar` nothing reads still draws, highlighted, when it is the
/// centre.
#[test]
fn neighborhood_centred_on_an_unused_initial_var_draws_it() {
    let mut f = test_function();
    let init_var = f.graph_mut().create_node(
        NodeKind::InitialVar(crate::node::InitialVnId::from_index(0)),
        [],
        [ValueKind::Typed(ValueType::int(64))],
    );
    let dot = make_dumper(&f)
        .neighborhood_dot(init_var, 2, 12, 100, false)
        .unwrap();
    let decls = node_decls(&dot);
    assert_eq!(decls.len(), 1, "the centre alone:\n{dot}");
    assert!(
        decls[0]
            .trim_start()
            .starts_with(&format!("\"{}\" ", init_var.as_u32())),
        "{dot}"
    );
    assert!(decls[0].contains("init"), "{dot}");
    assert!(
        decls[0].contains("#ffcc00"),
        "centre must be highlighted:\n{dot}"
    );
}

/// The centred node keeps its navigable `NodeId` even when const, where a
/// non-centre const would render as a fresh `c*` box per use.
#[test]
fn neighborhood_center_is_highlighted_and_navigable_even_when_const() {
    use crate::node::IntBinaryOp;

    let mut f = test_function();
    let k = int_const_node(&mut f, 7, ValueType::int(32));
    let a = int_const_node(&mut f, 1, ValueType::int(32));
    let kv = f.node_outputs(k)[0];
    let av = f.node_outputs(a)[0];
    let mk_add = |f: &mut Function, l, r| {
        f.graph_mut().create_node(
            NodeKind::IntBinaryOp(IntBinaryOp::Add),
            [l, r],
            [ValueKind::Typed(ValueType::int(32))],
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

    let dot = make_dumper(&f)
        .neighborhood_dot(add1, 3, 12, 100, false)
        .unwrap();
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
    let dot = make_dumper(&f)
        .neighborhood_dot(k, 3, 12, 100, false)
        .unwrap();
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
    let k = int_const_node(&mut f, 7, ValueType::int(32));
    let a = int_const_node(&mut f, 1, ValueType::int(32));
    let b = int_const_node(&mut f, 2, ValueType::int(32));
    let kv = f.node_outputs(k)[0];
    let av = f.node_outputs(a)[0];
    let bv = f.node_outputs(b)[0];
    let mk_add = |f: &mut Function, l, r| {
        f.graph_mut().create_node(
            NodeKind::IntBinaryOp(IntBinaryOp::Add),
            [l, r],
            [ValueKind::Typed(ValueType::int(32))],
        )
    };
    let add1 = mk_add(&mut f, av, kv);
    let add2 = mk_add(&mut f, bv, kv);
    let a1v = f.node_outputs(add1)[0];
    let a2v = f.node_outputs(add2)[0];
    let center = mk_add(&mut f, a1v, a2v);

    let dumper = make_dumper(&f);
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

/// A carrier's tags print sorted and `", "`-joined, raw and pretty alike.
#[test]
fn arg_tags_join_sorted_in_both_renders() {
    let mut f = test_function();
    let [ctrl] = f.node_outputs_exact::<1>(f.entry()).unwrap();
    let node = f.graph_mut().create_node(
        NodeKind::InitialVar(crate::node::InitialVnId::from_index(0)),
        [],
        [ValueKind::Typed(ValueType::int(64))],
    );
    let [value] = f.node_outputs_exact::<1>(node).unwrap();
    f.side_tables_mut().register_float_arg_value(0, value);
    f.side_tables_mut().register_arg_value(1, value);
    f.side_tables_mut().register_arg_value(0, value);
    f.graph_mut()
        .create_node(NodeKind::Return, [ctrl, value], []);

    let tags = "[arg 0], [arg 1], [float arg 0]";
    let raw = f.raw_dot().unwrap();
    assert!(raw.contains(&format!("\\n{tags}\"")), "{raw}");
    let pretty = render(&f);
    assert!(pretty.contains(&format!("\"{tags}\\ninit")), "{pretty}");
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
            [ValueKind::Typed(ValueType::int(64))],
        );
        f.node_outputs_exact::<1>(node).unwrap()[0]
    };
    let int_value = carrier(&mut f, 0);
    let float_value = carrier(&mut f, 1);
    f.side_tables_mut().register_arg_value(0, int_value);
    f.side_tables_mut().register_float_arg_value(0, float_value);

    f.graph_mut()
        .create_node(NodeKind::Return, [entry_ctrl, int_value, float_value], []);

    let dot = render(&f);

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

/// The raw neighbourhood is the raw dumper restricted, not a second emitter,
/// so it declares its nodes in the same ascending-id order the pretty
/// restricted render uses. An `FxHashSet` iteration order does not.
#[test]
fn raw_neighborhood_declares_its_nodes_in_id_order() {
    use crate::node::IntBinaryOp;

    let mut f = test_function();
    let ty = ValueType::int(32);
    let c0 = int_const_node(&mut f, 1, ty);
    let mut prev = f.node_outputs(c0)[0];
    // A chain long enough that a hash set's order is unlikely to be ascending.
    for k in 2u128..24 {
        let cn = int_const_node(&mut f, k, ty);
        let c = f.node_outputs(cn)[0];
        let add = f.graph_mut().create_node(
            NodeKind::IntBinaryOp(IntBinaryOp::Add),
            [prev, c],
            [ValueKind::Typed(ty)],
        );
        prev = f.node_outputs(add)[0];
    }
    let center = f.producer(prev);
    let entry = f.entry();
    let [ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
    let mem = crate::function::test_initial_memory(&f);
    let [memv] = f.node_outputs_exact::<1>(mem).unwrap();
    f.graph_mut()
        .create_node(NodeKind::Return, [ctrl, memv, prev], []);

    let dot = f
        .raw_neighborhood_dot(center, 40, 1000, 1000, true)
        .expect("raw neighborhood");
    let ids: Vec<u32> = node_decls(&dot)
        .iter()
        .filter_map(|l| l.trim().strip_prefix("\"n"))
        .filter_map(|l| l.split('"').next())
        .filter_map(|n| n.parse().ok())
        .collect();
    assert!(ids.len() > 5, "too few node declarations: {ids:?}\n{dot}");
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    assert_eq!(ids, sorted, "raw neighbourhood node order:\n{dot}");

    // The centre, and only the centre, carries the gold border.
    let gold: Vec<&str> = node_decls(&dot)
        .into_iter()
        .filter(|l| l.contains("#ffcc00"))
        .collect();
    assert_eq!(gold.len(), 1, "exactly one highlighted box:\n{dot}");
    assert!(
        gold[0].contains(&format!("\"n{}\"", center.as_u32())),
        "the centre is the highlighted box: {}",
        gold[0]
    );
}

/// A budgeted neighbourhood, raw or pretty, draws no edge to a node it did not
/// declare: Graphviz would otherwise mint an unlabelled box for the missing end.
#[test]
fn neighborhood_draws_no_edge_out_of_view() {
    use crate::node::IntBinaryOp;

    let mut f = test_function();
    let ty = ValueType::int(32);
    let c0 = int_const_node(&mut f, 1, ty);
    let mut prev = f.node_outputs(c0)[0];
    for k in 2u128..24 {
        let cn = int_const_node(&mut f, k, ty);
        let c = f.node_outputs(cn)[0];
        let add = f.graph_mut().create_node(
            NodeKind::IntBinaryOp(IntBinaryOp::Add),
            [prev, c],
            [ValueKind::Typed(ty)],
        );
        prev = f.node_outputs(add)[0];
    }
    let center = f.producer(prev);
    let entry = f.entry();
    let [ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
    let mem = crate::function::test_initial_memory(&f);
    let [memv] = f.node_outputs_exact::<1>(mem).unwrap();
    f.graph_mut()
        .create_node(NodeKind::Return, [ctrl, memv, prev], []);

    // A budget well under the function's node count, so the walk is cut.
    let raw = f
        .raw_neighborhood_dot(center, 40, 1000, 6, true)
        .expect("raw neighborhood");
    let pretty = make_dumper(&f)
        .neighborhood_dot(center, 40, 1000, 6, true)
        .expect("pretty neighborhood");
    for dot in [raw, pretty] {
        let declared: Vec<&str> = node_decls(&dot)
            .iter()
            .filter_map(|l| l.trim().split('"').nth(1))
            .collect();
        assert!(declared.len() <= 6, "budget not honoured: {declared:?}");
        let edges = edge_lines(&dot);
        assert!(
            !edges.is_empty(),
            "the cut neighbourhood kept no edge:\n{dot}"
        );
        for edge in edges {
            let mut ends = edge.split('"').skip(1).step_by(2);
            let (from, to) = (ends.next().expect("from"), ends.next().expect("to"));
            assert!(
                declared.contains(&from),
                "edge from undeclared {from}: {edge}"
            );
            assert!(declared.contains(&to), "edge to undeclared {to}: {edge}");
        }
    }
}

/// A variadic input's edge label is the signature's own tail name plus the
/// ordinal within the tail, so neither the name nor the head length is
/// re-derived in the renderer.
#[test]
fn variadic_edge_labels_come_from_the_signature() {
    let sig = crate::node::signature::expected_signature(&NodeKind::Call { cc: None });
    let tail = sig.inputs.at(sig.inputs.head_len()).expect("a tail slot");

    let mut f = test_function();
    let entry = f
        .graph_mut()
        .create_node(NodeKind::Entry, [], [ValueKind::Control]);
    let init_mem = f
        .graph_mut()
        .create_node(NodeKind::InitialMemory, [], [ValueKind::Memory]);
    let [ctrl] = f.node_outputs_exact::<1>(entry).unwrap();
    let [mem] = f.node_outputs_exact::<1>(init_mem).unwrap();
    let ty = ValueType::int(64);
    let target_n = int_const_node(&mut f, 0x1000, ty);
    let target = f.node_outputs(target_n)[0];
    let sp_n = int_const_node(&mut f, 0x20, ty);
    let sp = f.node_outputs(sp_n)[0];
    let a0_n = int_const_node(&mut f, 0x11, ty);
    let a0 = f.node_outputs(a0_n)[0];
    let a1_n = int_const_node(&mut f, 0x22, ty);
    let a1 = f.node_outputs(a1_n)[0];
    let call = f.graph_mut().create_node(
        NodeKind::Call { cc: None },
        [ctrl, mem, target, sp, a0, a1],
        [ValueKind::Control, ValueKind::Memory],
    );
    let [call_ctrl, call_mem] = f.node_outputs_exact::<2>(call).unwrap();
    f.graph_mut()
        .create_node(NodeKind::Return, [call_ctrl, call_mem], []);

    let dot = render(&f);
    for n in 0..2 {
        let want = format!("label=\"{}{n}\"", tail.name);
        assert!(
            dot.contains(&want),
            "a Call's argument {n} must be labelled {want}:\n{dot}"
        );
    }
    // The fixed head slot before the tail is named by the signature too.
    let head_last = sig.inputs.at(sig.inputs.head_len() - 1).expect("head slot");
    assert!(
        dot.contains(&format!("label=\"{}\"", head_last.name)),
        "the last head slot must be labelled {}:\n{dot}",
        head_last.name
    );
}

/// The raw label spells a value, not a storage form: limb padding and which
/// construction path reached it must not show. Rendered graphs are compared
/// across builds, so this text is an artifact of the renderer, not of the
/// stored `UBig`.
#[test]
fn const_label_spells_the_value_not_the_storage() {
    use dashu_int::UBig;

    use super::raw::const_label;
    let cases: [(&str, UBig, &str); 8] = [
        ("zero", UBig::ZERO, "0"),
        ("zero_no_limbs", UBig::from_words(&[]), "0"),
        ("zero_padded", UBig::from_words(&[0, 0, 0, 0]), "0"),
        ("five_padded", UBig::from_words(&[5, 0, 0, 0]), "5"),
        ("bit_64", UBig::from_words(&[0, 1]), "18446744073709551616"),
        // The exact text the raw DOT label carries for a 96-bit mask.
        (
            "i96_all_ones",
            UBig::from_words(&[u64::MAX, u64::from(u32::MAX)]),
            "79228162514264337593543950335",
        ),
        (
            "past_the_carrier",
            UBig::from_words(&[0, 0, 1, 0]),
            "340282366920938463463374607431768211456",
        ),
        // `as_words` trims the two dead top limbs: a value, not a limb count.
        (
            "zero_top_limbs_are_padded_back",
            UBig::from_words(&[u64::MAX, u64::MAX, u64::MAX, 0, 0, 0]),
            "6277101735386680763835789423207666416102355444464034512895",
        ),
    ];
    for (label, value, want) in cases {
        assert_eq!(const_label(&value), want, "{label}");
    }
}

/// Every route to one value renders alike, declared width included.
#[test]
fn interned_values_render_alike_whatever_route_reached_them() {
    use super::raw::const_label;
    let mut f = test_function();
    let value: u128 = (1u128 << 69) | 0xDEAD_BEEF;
    let cases = [
        (f.intern_int_const(value, ValueType::int(128)), 128),
        (
            f.intern_int_const(
                UBig::from_words(&[0xDEAD_BEEF, 1 << 5]),
                ValueType::int(128),
            ),
            128,
        ),
        (
            f.intern_int_const(
                UBig::from_words(&[0xDEAD_BEEF, 1 << 5, 0, 0]),
                ValueType::int(256),
            ),
            256,
        ),
        (
            f.intern_int_const(UBig::from_words(&[0xDEAD_BEEF, 1 << 5]), ValueType::int(80)),
            80,
        ),
    ];
    let labels: Vec<String> = cases
        .iter()
        .map(|&(id, _bits)| const_label(f.const_value(id)))
        .collect();
    assert_eq!(labels[0], "590295810362441580271");
    assert!(
        labels.iter().all(|d| *d == labels[0]),
        "one value, one label: {labels:?}"
    );
}

/// Every kind's (shape, fill) pair, one representative per variant.
#[test]
fn node_style_per_kind() {
    let expected = [
        ("Mdiamond", "#1a3a5c"),      // Entry
        ("Mdiamond", "#1a3a5c"),      // InitialMemory
        ("Mdiamond", "#1a3a5c"),      // InitialVar
        ("invhouse", "#2a1a4a"),      // Region
        ("house", "#163030"),         // MemPhi
        ("house", "#163030"),         // Phi
        ("diamond", "#3a2a10"),       // If
        ("trapezium", "#103a10"),     // Switch
        ("rarrow", "#3a1010"),        // Call
        ("doublecircle", "#103a10"),  // Return
        ("doublecircle", "#103a10"),  // IndirectBranch
        ("Msquare", "#3a1030"),       // Unreachable
        ("box3d", "#102030"),         // Load
        ("box3d", "#102030"),         // Store
        ("ellipse", "#2d2d2d"),       // IntConst
        ("box", "#2d2d2d"),           // IntUnaryOp
        ("box", "#2d2d2d"),           // IntBinaryOp
        ("box", "#2d2d2d"),           // IntCmpOp
        ("box", "#2d2d2d"),           // Truncate
        ("box", "#2d2d2d"),           // Popcount
        ("box", "#2d2d2d"),           // Lzcount
        ("box", "#2d2d2d"),           // Extend
        ("ellipse", "#1a3020"),       // FloatConst
        ("box", "#1a3020"),           // FloatBinaryOp
        ("box", "#1a3020"),           // FloatUnaryOp
        ("box", "#1a3020"),           // FloatCmpOp
        ("box", "#302018"),           // IntToFloat
        ("box", "#302018"),           // FloatToInt
        ("box", "#302018"),           // FloatToFloat
        ("box", "#302018"),           // IntBitsToFloat
        ("box", "#302018"),           // FloatBitsToInt
        ("doubleoctagon", "#3a2810"), // CallOther
        ("folder", "#2a1a3a"),        // CPoolRef
        ("component", "#103a2a"),     // New
    ];
    let kinds = crate::node::signature::every_node_kind();
    assert_eq!(kinds.len(), expected.len());
    for (k, want) in kinds.iter().zip(expected) {
        assert_eq!(node_style(k), want, "{k:?}");
    }
}
