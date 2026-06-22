//! Integration coverage for the typed template side: a `TemplatePat`
//! RHS instantiated as fresh IR against a matched LHS.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

use strider_ir::EditFunction;
use strider_ir::IRBuilderExt;
use strider_ir::IntBinaryOp;
use strider_ir::{ConstId, node::{NodeKind, ValueKind, ValueType as T}};
use strider_ir::{IRViewer, IRWalker};
use strider_ir_test_utils::make_empty_fn;

use strider_ir::Function;
use strider_ir::node::{NodeId, ValueId, ValueType};
use strider_pattern::matcher::{KindSpec, Pattern};
use strider_pattern::template::{self, Template, TemplateBuilder, instantiate};
use strider_pattern::{
    Bindings, Capture, MatchPat, Matcher, TemplatePat, add, int_const, signed_int_const, var,
};

// ── Shared match-then-instantiate scaffold ───────────────────────────────────

/// Matches `lhs` exactly once against `fx` and returns the matched root
/// node, the captured bindings, and the root's single value-output type.
#[track_caller]
fn match_lhs_once(fx: &Function, lhs: &Pattern) -> (NodeId, Bindings, ValueType) {
    let m = Matcher::try_new(fx).unwrap();
    let hits = m.find_all(lhs).unwrap();
    assert_eq!(hits.len(), 1, "LHS must match exactly once");
    let root_node = hits[0].root();
    let bindings = hits[0].bindings_clone();
    let [root_value] = fx.node_outputs_exact::<1>(root_node).unwrap();
    let root_ty = fx.value_kind(root_value).as_value().unwrap();
    (root_node, bindings, root_ty)
}

/// Instantiates `rhs` as fresh IR against the matched `root`, with the
/// proof-node set defaulted to `[root]`, returning the new root value.
#[track_caller]
fn instantiate_at_root(
    fx: &mut Function,
    rhs: &Template,
    bindings: &Bindings,
    root: NodeId,
    root_ty: ValueType,
) -> ValueId {
    let mut ef = EditFunction::new(fx).unwrap();
    instantiate(rhs, &mut ef, bindings, root, &[root], root_ty).unwrap()
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
    let (root_node, bindings, root_ty) = match_lhs_once(&fx, &lhs);

    // Build the RHS as fresh IR.
    let rhs = template::add(var(x), int_const(2u128)).into_template();
    let new_value = instantiate_at_root(&mut fx, &rhs, &bindings, root_node, root_ty);

    // The new output is an Add node.
    let new_node = fx.producer(new_value);
    assert!(matches!(
        fx.node_kind(new_node),
        NodeKind::IntBinaryOp(IntBinaryOp::Add)
    ));

    // Its constant operand is the freshly built `IntConst(2)`.
    let has_two = fx
        .node_inputs(new_node)
        .into_iter()
        .map(|inp| fx.producer(inp))
        .any(|n| {
            matches!(fx.node_kind(n), NodeKind::IntConst(_))
                && fx
                    .node_outputs(n)
                    .iter()
                    .any(|&o| fx.int_const_val(o) == Some(2))
        });
    assert!(has_two, "RHS should materialise IntConst(2)");
}

/// `instantiate` must attribute the FULL proof-node set to EVERY node it
/// creates — not just the root output. A multi-node RHS (`add(var(x),
/// int_const(2))` → Add root + intermediate IntConst) is built with a
/// two-node proof set carrying distinct addrs; the intermediate IntConst
/// must carry BOTH proof addrs, proving the proof lands on non-root nodes.
#[test]
fn instantiate_attributes_full_proof_set_to_every_new_node() {
    use strider_ir_test_utils::SENTINEL_LIFT_ADDR;
    const PROOF_A: u64 = 0xA1;
    const PROOF_B: u64 = 0xA2;

    let x = Capture::new();
    let mut fx = make_empty_fn(|b| {
        b.set_lift_addr(Some(PROOF_A));
        let a = b.build_int_const(5u64, T::I64)?; // proof node A
        b.set_lift_addr(Some(PROOF_B));
        let k = b.build_int_const(1u64, T::I64)?; // proof node B
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        b.build_int_binary_operation(a, k, IntBinaryOp::Add, T::I64)
    })
    .unwrap();

    // Match `add(var(x), int_const(1))`; collect the two proof nodes.
    let lhs = add(var(x), int_const(1u128)).into_pattern();
    let (root_node, bindings, root_ty) = match_lhs_once(&fx, &lhs);
    let proof_a = fx
        .walk()
        .find(|&n| {
            matches!(fx.node_kind(n), NodeKind::IntConst(_))
                && fx
                    .node_outputs(n)
                    .iter()
                    .any(|&o| fx.int_const_val(o) == Some(5))
        })
        .unwrap();
    let proof_b = fx
        .walk()
        .find(|&n| {
            matches!(fx.node_kind(n), NodeKind::IntConst(_))
                && fx
                    .node_outputs(n)
                    .iter()
                    .any(|&o| fx.int_const_val(o) == Some(1))
        })
        .unwrap();
    assert!(fx.asm_fingerprint(proof_a).contains(&PROOF_A));
    assert!(fx.asm_fingerprint(proof_b).contains(&PROOF_B));

    // Build the RHS with BOTH proof nodes as the attribution set.
    let rhs = template::add(var(x), int_const(2u128)).into_template();
    let proof_nodes = [proof_a, proof_b];
    let new_value = {
        let mut ef = EditFunction::new(&mut fx).unwrap();
        instantiate(&rhs, &mut ef, &bindings, root_node, &proof_nodes, root_ty).unwrap()
    };

    // The INTERMEDIATE freshly-built IntConst(2) — not the root Add — must
    // carry BOTH proof fingerprints. The new nodes aren't wired into the
    // reachable graph yet (that's `replace_value`'s job), so reach the
    // intermediate via the new root Add's inputs rather than a graph walk.
    let new_root = fx.producer(new_value);
    let new_const2 = fx
        .node_inputs(new_root)
        .into_iter()
        .map(|inp| fx.producer(inp))
        .find(|&n| {
            matches!(fx.node_kind(n), NodeKind::IntConst(_))
                && fx
                    .node_outputs(n)
                    .iter()
                    .any(|&o| fx.int_const_val(o) == Some(2))
        })
        .expect("RHS materialised IntConst(2)");
    let fp = fx.asm_fingerprint(new_const2);
    assert!(
        fp.contains(&PROOF_A) && fp.contains(&PROOF_B),
        "intermediate new node must carry the full proof set; got {fp:?}"
    );
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
    let (root_node, bindings, root_ty) = match_lhs_once(&fx, &lhs);
    let bound = bindings.get(c).unwrap();

    // Instantiating a bare `var(c)` returns the bound output unchanged.
    let pre_count = fx.walk().count();
    let rhs = var(c).into_template();
    let resolved = instantiate_at_root(&mut fx, &rhs, &bindings, root_node, root_ty);
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
    // ConstId::from_u32 is used as a struct-literal placeholder here;
    // these raw-builder tests only check structural wiring, not constant values.
    let addr = b.leaf(KindSpec::Exact(NodeKind::IntConst(ConstId::from_u32(0x100))));
    let data = b.leaf(KindSpec::Exact(NodeKind::IntConst(ConstId::from_u32(42))));

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
    let _load_out = b.value_output(load, 0);

    let tpl = b.finish();

    // Instantiate against a throwaway fixture; the template is
    // pure-`Exact`, so bindings / lhs_root are unused.
    let mut fx = make_empty_fn(|bld| bld.build_int_const(0u64, T::I64)).unwrap();
    let lhs_root = fx.walk().next().unwrap();
    let bindings = Bindings::default();

    let root_value = {
        let mut ef = EditFunction::new(&mut fx).unwrap();
        instantiate(&tpl, &mut ef, &bindings, lhs_root, &[lhs_root], T::I64).unwrap()
    };

    // The root materialised as a Load yielding a value output.
    let load_node = fx.producer(root_value);
    assert!(
        matches!(fx.node_kind(load_node), NodeKind::Load(_)),
        "root must be a Load"
    );
    assert!(
        matches!(fx.value_kind(root_value), ValueKind::Typed(_)),
        "root output must be a value"
    );

    // The Load's memory input (slot 0) is the Store's memory output.
    let load_inputs = fx.node_inputs(load_node);
    let mem_value = load_inputs[0];
    let store_node = fx.producer(mem_value);
    assert!(
        matches!(fx.node_kind(store_node), NodeKind::Store(_)),
        "Load's memory input must come from the Store"
    );
    assert_eq!(
        fx.value_kind(mem_value),
        ValueKind::Memory,
        "the wired Store output must be the memory token"
    );

    // The Store's own memory input traces back to the InitialMemory node.
    let store_inputs = fx.node_inputs(store_node);
    let store_mem_value = store_inputs[0];
    assert!(
        matches!(
            fx.node_kind(fx.producer(store_mem_value)),
            NodeKind::InitialMemory
        ),
        "Store's memory input must be the InitialMemory token"
    );
}

/// `int_const(V)` as a template RHS for V > u64::MAX must produce the FULL
/// value when the root is I128 — not a u64-truncated one.
///
/// Before the `ConstId` unification, the old narrow-cast path truncated the high
/// bits before the interner could preserve them.
#[test]
fn int_const_wide_template_rhs_preserves_full_value() {
    // A value whose high 64 bits are non-zero — the truncation bug drops them.
    let wide_val: u128 = 1u128 << 100;

    // Build a fixture with an I128 constant so we have something to match on.
    let mut fx = make_empty_fn(|b| b.build_int_const(wide_val, T::I128)).unwrap();

    // Find the I128 constant node.
    let lhs = int_const(wide_val).into_pattern();
    let (root_node, bindings, root_ty) = match_lhs_once(&fx, &lhs);
    assert_eq!(root_ty, T::I128);

    // Instantiate `int_const(wide_val)` as an I128 RHS.
    let rhs = int_const(wide_val).into_template();
    let new_value = instantiate_at_root(&mut fx, &rhs, &bindings, root_node, root_ty);

    // The produced constant must read back as the full wide_val, not truncated.
    let stored = fx
        .int_const_u128(new_value)
        .expect("new value must be an integer constant readable via int_const_u128");
    assert_eq!(
        stored, wide_val,
        "int_const({wide_val}) RHS on I128 root must produce the full value; \
         got {stored:#x} — likely truncated to low 64 bits"
    );
}

/// `int_const(u128::MAX)` as a template RHS on an I128 root must produce
/// the full 128-bit all-ones pattern (I128 all-ones = u128::MAX), not a
/// u64-truncated value.
#[test]
fn int_const_all_ones_i128_template_rhs() {
    let all_ones = u128::MAX;

    let mut fx = make_empty_fn(|b| b.build_int_const(all_ones, T::I128)).unwrap();

    let lhs = int_const(all_ones).into_pattern();
    let (root_node, bindings, root_ty) = match_lhs_once(&fx, &lhs);

    let rhs = int_const(all_ones).into_template();
    let new_value = instantiate_at_root(&mut fx, &rhs, &bindings, root_node, root_ty);

    let stored = fx
        .int_const_u128(new_value)
        .expect("must be a readable integer constant");
    assert_eq!(
        stored, all_ones,
        "int_const(u128::MAX) on I128 root must give all-ones; got {stored:#x}"
    );
}

/// `signed_int_const(-50)` as a template RHS on an I128 root must produce
/// the full 128-bit two's-complement pattern for -50, not a zero-extended
/// low-64 value.
#[test]
fn signed_int_const_negative_i128_template_rhs() {
    let v: i64 = -50;
    // Expected: full I128 two's-complement representation of -50.
    let expected: u128 = i128::from(v) as u128;

    // Build a fixture with the I128 two's-complement constant so we can match it.
    let mut fx = make_empty_fn(|b| b.build_int_const(expected, T::I128)).unwrap();

    let lhs = int_const(expected).into_pattern();
    let (root_node, bindings, root_ty) = match_lhs_once(&fx, &lhs);
    assert_eq!(root_ty, T::I128);

    // Instantiate `signed_int_const(-50)` as an I128 RHS.
    let rhs = signed_int_const(v).into_template();
    let new_value = instantiate_at_root(&mut fx, &rhs, &bindings, root_node, root_ty);

    let stored = fx
        .int_const_u128(new_value)
        .expect("must be a readable integer constant");
    assert_eq!(
        stored, expected,
        "signed_int_const({v}) on I128 root must give full two's-complement {expected:#x}; \
         got {stored:#x} — likely zero-extended low-64 bits only"
    );
}

/// LOW-2: a raw-built template whose node wires NON-CONTIGUOUS input
/// slots (here slots 0 and 2, with a gap at 1) must fail `instantiate`
/// with a typed error rather than silently closing the gap (which would
/// land slot 2's producer at IR input index 1 — wrong IR, no diagnostic).
#[test]
fn instantiate_noncontiguous_raw_template_slots_errors() {
    // Build an `Add` node wired at slots 0 and 2 — slot 1 is left empty.
    let mut b = TemplateBuilder::new();
    let l = b.leaf(KindSpec::Exact(NodeKind::IntConst(ConstId::from_u32(5))));
    let r = b.leaf(KindSpec::Exact(NodeKind::IntConst(ConstId::from_u32(7))));
    let add_node = b.node(KindSpec::Exact(NodeKind::IntBinaryOp(IntBinaryOp::Add)));
    b.input(add_node, 0, l);
    b.input(add_node, 2, r); // gap at slot 1
    let _out = b.value_output(add_node, 0);
    let tpl = b.finish();

    let mut fx = make_empty_fn(|bld| bld.build_int_const(0u64, T::I64)).unwrap();
    let lhs_root = fx.walk().next().unwrap();
    let bindings = Bindings::default();

    let mut ef = EditFunction::new(&mut fx).unwrap();
    let err = instantiate(&tpl, &mut ef, &bindings, lhs_root, &[lhs_root], T::I64)
        .expect_err("non-contiguous slots must error");
    let msg = err.to_string();
    assert!(
        msg.contains("slot"),
        "error should name the slot-contiguity violation; got: {msg}"
    );
}

/// LOW-2: a raw-built template wiring TWO producers into the SAME input
/// slot silently dropped the earlier edge; `instantiate` must reject it.
#[test]
fn instantiate_duplicate_raw_template_slot_errors() {
    let mut b = TemplateBuilder::new();
    let l = b.leaf(KindSpec::Exact(NodeKind::IntConst(ConstId::from_u32(5))));
    let r = b.leaf(KindSpec::Exact(NodeKind::IntConst(ConstId::from_u32(7))));
    let add_node = b.node(KindSpec::Exact(NodeKind::IntBinaryOp(IntBinaryOp::Add)));
    b.input(add_node, 0, l);
    b.input(add_node, 0, r); // duplicate slot 0
    let _out = b.value_output(add_node, 0);
    let tpl = b.finish();

    let mut fx = make_empty_fn(|bld| bld.build_int_const(0u64, T::I64)).unwrap();
    let lhs_root = fx.walk().next().unwrap();
    let bindings = Bindings::default();

    let mut ef = EditFunction::new(&mut fx).unwrap();
    let err = instantiate(&tpl, &mut ef, &bindings, lhs_root, &[lhs_root], T::I64)
        .expect_err("duplicate slot must error");
    let msg = err.to_string();
    assert!(
        msg.contains("slot"),
        "error should name the duplicate-slot violation; got: {msg}"
    );
}

/// A template that references a capture the LHS never bound must fail
/// `instantiate` with a typed error (not a panic) naming the unbound
/// capture contract.
#[test]
fn instantiate_with_unbound_template_capture_errors() {
    let mut fx = make_empty_fn(|b| b.build_int_const(5u64, T::I64)).unwrap();

    // LHS binds nothing.
    let lhs = int_const(5u128).into_pattern();
    let (root_node, bindings, root_ty) = match_lhs_once(&fx, &lhs);

    // RHS references a capture no pattern ever bound.
    let unbound = Capture::new();
    let rhs = template::add(var(unbound), int_const(1u128)).into_template();

    let mut ef = EditFunction::new(&mut fx).unwrap();
    let err = instantiate(&rhs, &mut ef, &bindings, root_node, &[root_node], root_ty)
        .expect_err("unbound template capture must error");
    let msg = err.to_string();
    assert!(
        msg.contains("unbound by LHS"),
        "error names the unbound-capture contract; got: {msg}"
    );
}
