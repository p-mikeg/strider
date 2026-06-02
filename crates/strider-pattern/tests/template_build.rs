//! Integration coverage for the typed template side: a `TemplatePat`
//! RHS instantiated as fresh IR against a matched LHS.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

use strider_ir::node::{NodeKind, NodeOutputKind, NodeOutputType as T};
use strider_ir::IntBinaryOp;
use strider_ir_test_utils::make_empty_fn;

use strider_pattern::pattern::KindSpec;
use strider_pattern::template::{self, instantiate, TemplateBuilder};
use strider_pattern::{
    add, int_const, var, Bindings, Capture, MatchPat, Matcher, TemplatePat,
};

/// `bool_not(var(c))` is a `TemplatePat`, so it is usable as a buildable
/// rewrite RHS (the type-checker rejects a non-buildable/wildcard RHS).
/// Sealing it into a `Template` must succeed and produce a rooted,
/// buildable-by-construction graph (`xor(var(c), IntConst(1)):I1`).
#[test]
fn bool_not_is_a_buildable_template_rhs() {
    let c = Capture::new();
    let tpl = template::bool_not(var(c)).into_template();
    assert!(tpl.root().is_some(), "sealed template must have a root");
    // The xor + its const operand + the captured var node.
    assert_eq!(tpl.node_count(), 3);
}

/// Match `add(var(x), int_const(1))`, then instantiate
/// `add(var(x), int_const(2))` as fresh IR re-using the captured `x`.
#[test]
fn instantiate_add_const_builds_fresh_node() {
    let x = Capture::new();

    let mut fx = make_empty_fn(|b| {
        let a = b.build_int_const(5u64, T::I64)?;
        let k = b.build_int_const(1u64, T::I64)?;
        b.build_int_binary_operation(a, k, IntBinaryOp::Add, T::I64)
    })
    .unwrap();

    // Match the LHS.
    let lhs = add(var(x), int_const(1u128)).into_pattern();
    let (root_node, bindings) = {
        let m = Matcher::try_new(&fx).unwrap();
        let hits = m.find_all(&lhs);
        assert_eq!(hits.len(), 1);
        (hits[0].root(), hits[0].bindings_clone())
    };

    // Root single value output + type.
    let [root_out] = fx.node_outputs_exact::<1>(root_node).unwrap();
    let root_ty = fx.output_kind(root_out).as_value().unwrap();

    // Build the RHS as fresh IR.
    let rhs = template::add(var(x), int_const(2u128)).into_template();
    let new_out = instantiate(&rhs, &mut fx, &bindings, root_node, root_ty).unwrap();

    // The new output is an Add node.
    let new_node = fx.node_for_output(new_out);
    assert!(matches!(
        fx.node_kind(new_node),
        NodeKind::IntBinaryOp(IntBinaryOp::Add)
    ));

    // Its constant operand is the freshly built `IntConst(2)`.
    let has_two = fx
        .node_inputs(new_node)
        .into_iter()
        .map(|inp| fx.node_for_output(inp))
        .any(|n| matches!(fx.node_kind(n), NodeKind::IntConst(2)));
    assert!(has_two, "RHS should materialise IntConst(2)");
}

/// A bare `var(c)` template resolves to its bound output through the
/// `Bindings` — no fresh node is created.
#[test]
fn instantiate_bare_var_resolves_to_bound_output() {
    let c = Capture::new();

    let mut fx = make_empty_fn(|b| {
        let five = b.build_int_const(5u64, T::I64)?;
        let seven = b.build_int_const(7u64, T::I64)?;
        b.build_int_binary_operation(five, seven, IntBinaryOp::Add, T::I64)
    })
    .unwrap();

    // Match `add(int_const(5), var(c))` — `c` binds to the 7-operand.
    let lhs = add(int_const(5u128), var(c)).into_pattern();
    let (root_node, bindings) = {
        let m = Matcher::try_new(&fx).unwrap();
        let hits = m.find_all(&lhs);
        assert_eq!(hits.len(), 1);
        (hits[0].root(), hits[0].bindings_clone())
    };
    let bound = bindings.get(c).unwrap();

    // Instantiating a bare `var(c)` returns the bound output unchanged.
    let pre_count = fx.walk().count();
    let rhs = var(c).into_template();
    let resolved = instantiate(&rhs, &mut fx, &bindings, root_node, T::I64).unwrap();
    assert_eq!(resolved, bound, "var(c) must resolve to its bound output");
    assert_eq!(fx.walk().count(), pre_count, "no fresh node created");
}

/// A `Template` may declare a multi-output interior node and wire its
/// non-value output into a later consumer. Build
/// `Load(Store(InitialMemory, addr, data).mem, addr)` directly on the
/// `TemplateBuilder`: the `Store` is a multi-output interior node whose
/// **memory** output feeds the `Load`'s memory input slot, while the
/// `Load` (the root) yields the single value. This exercises
/// `instantiate`'s per-output-vertex slot wiring.
#[test]
fn template_wires_multi_output_interior_memory_node() {
    let space = rsleigh::VnSpace::RAM;

    // Build the template imperatively (the typed value-op builders only
    // expose value expressions; memory wiring needs the raw builder).
    let mut b = TemplateBuilder::new();

    // mem0 = InitialMemory (one memory output).
    let mem0_node = b.node(KindSpec::Exact(NodeKind::InitialMemory));
    let mem0 = b.memory_output(mem0_node, 0);

    // addr / data leaves (value).
    let addr = b.leaf(KindSpec::Exact(NodeKind::IntConst(0x100)));
    let data = b.leaf(KindSpec::Exact(NodeKind::IntConst(42)));

    // store = Store(mem0, addr, data) — inputs [MEM, ADDR, DATA],
    // output [MEM]. The memory output is the multi-output interior edge.
    let store = b.node(KindSpec::Exact(NodeKind::Store(space)));
    b.input(store, 0, mem0);
    b.input(store, 1, addr);
    b.input(store, 2, data);
    let store_mem = b.memory_output(store, 0);

    // load = Load(store_mem, addr) — inputs [MEM, ADDR], output [INT_VAL].
    // It consumes the *Store's* memory output, proving the slot wiring.
    let load = b.node(KindSpec::Exact(NodeKind::Load(space)));
    b.input(load, 0, store_mem);
    b.input(load, 1, addr);
    let load_out = b.value_output(load, 0);

    let tpl = b.finish(load_out);

    // Instantiate against a throwaway fixture; the template is
    // pure-`Exact`, so bindings / lhs_root are unused.
    let mut fx = make_empty_fn(|bld| bld.build_int_const(0u64, T::I64)).unwrap();
    let lhs_root = fx.walk().next().unwrap();
    let bindings = Bindings::default();

    let root_out = instantiate(&tpl, &mut fx, &bindings, lhs_root, T::I64).unwrap();

    // The root materialised as a Load yielding a value output.
    let load_node = fx.node_for_output(root_out);
    assert!(
        matches!(fx.node_kind(load_node), NodeKind::Load(_)),
        "root must be a Load"
    );
    assert!(
        matches!(fx.output_kind(root_out), NodeOutputKind::OutputType(_)),
        "root output must be a value"
    );

    // The Load's memory input (slot 0) is the Store's memory output.
    let load_inputs = fx.node_inputs(load_node);
    let mem_in = load_inputs[0];
    let store_node = fx.node_for_output(mem_in);
    assert!(
        matches!(fx.node_kind(store_node), NodeKind::Store(_)),
        "Load's memory input must come from the Store"
    );
    assert_eq!(
        fx.output_kind(mem_in),
        NodeOutputKind::Memory,
        "the wired Store output must be the memory token"
    );

    // The Store's own memory input traces back to the InitialMemory node.
    let store_inputs = fx.node_inputs(store_node);
    let store_mem_in = store_inputs[0];
    assert!(
        matches!(
            fx.node_kind(fx.node_for_output(store_mem_in)),
            NodeKind::InitialMemory
        ),
        "Store's memory input must be the InitialMemory token"
    );
}
