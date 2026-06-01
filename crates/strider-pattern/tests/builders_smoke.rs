//! Smoke tests for the leaf chained builders.  Each test constructs a
//! tiny IR function and asserts the builder's `Pat<R>` finds the
//! expected hit(s) via `Matcher::find_all`.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

use strider_ir::FunctionBuilder;
use strider_ir::node::NodeOutputType;
use strider_ir_test_utils::RegisterSet;
use strider_pattern::{
    add, any_int_const, bit_not, bool_and, bool_not, bool_or, bool_xor, call, float_add, float_eq,
    float_le, float_mul, float_ne, float_neg, float_to_int, if_node, initial_var, initial_var_for,
    int_binary_any, int_const, int_const_all_ones, int_eq, int_le, int_lt, int_ne, int_to_float,
    int_unary_any, load, lzcount, mem_phi, mul, phi, popcount, ret, sign_extend, store, truncate,
    value_phi, var, xor, zero_extend, Capture, Matcher, Pat,
};

#[test]
fn int_const_builder_matches_via_find_all() {
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let five = b.build_int_const(5u64, NodeOutputType::I64).unwrap();
    b.build_return(Some(five), &[]).unwrap();
    let function = b.build().unwrap();

    let pat = int_const(5u128);
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat);
    assert_eq!(hits.len(), 1);
}

#[test]
fn var_builder_captures() {
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let v = b.build_int_const(7u64, NodeOutputType::I64).unwrap();
    b.build_return(Some(v), &[]).unwrap();
    let function = b.build().unwrap();

    let c = Capture::default();
    let pat = var(c);
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat);
    assert!(!hits.is_empty());
    // Each hit should bind c to some NodeOutputId.
    assert!(hits[0].output(c).is_some());
}

#[test]
fn any_int_const_matches_multiple() {
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let five = b.build_int_const(5u64, NodeOutputType::I64).unwrap();
    let seven = b.build_int_const(7u64, NodeOutputType::I64).unwrap();
    let sum = b
        .build_int_binary_operation(
            five,
            seven,
            strider_ir::IntBinaryOp::Add,
            NodeOutputType::I64,
        )
        .unwrap();
    b.build_return(Some(sum), &[]).unwrap();
    let function = b.build().unwrap();

    let pat = any_int_const();
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat);
    assert!(
        hits.len() >= 2,
        "expected at least 2 IntConst matches, got {}",
        hits.len()
    );
}

#[test]
fn add_builder_matches_chain() {
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let five = b.build_int_const(5u64, NodeOutputType::I64).unwrap();
    let seven = b.build_int_const(7u64, NodeOutputType::I64).unwrap();
    let sum = b
        .build_int_binary_operation(
            five,
            seven,
            strider_ir::IntBinaryOp::Add,
            NodeOutputType::I64,
        )
        .unwrap();
    b.build_return(Some(sum), &[]).unwrap();
    let function = b.build().unwrap();

    let c = Capture::default();
    let pat = add(int_const(5u128), var(c));
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat);
    assert_eq!(hits.len(), 1);
    assert!(hits[0].output(c).is_some());
}

#[test]
fn mul_builder_matches_chain() {
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let three = b.build_int_const(3u64, NodeOutputType::I64).unwrap();
    let four = b.build_int_const(4u64, NodeOutputType::I64).unwrap();
    let product = b
        .build_int_binary_operation(
            three,
            four,
            strider_ir::IntBinaryOp::Mul,
            NodeOutputType::I64,
        )
        .unwrap();
    b.build_return(Some(product), &[]).unwrap();
    let function = b.build().unwrap();

    let c = Capture::default();
    let pat = mul(int_const(3u128), var(c));
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat);
    assert_eq!(hits.len(), 1);
    assert!(hits[0].output(c).is_some());
}

#[test]
fn int_eq_builder_matches() {
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let lhs = b.build_int_const(2u64, NodeOutputType::I64).unwrap();
    let rhs = b.build_int_const(3u64, NodeOutputType::I64).unwrap();
    let cmp = b
        .build_int_cmp_operation(lhs, rhs, strider_ir::IntCmpOp::Equal, NodeOutputType::I64)
        .unwrap();
    b.build_return(Some(cmp), &[]).unwrap();
    let function = b.build().unwrap();

    let c = Capture::default();
    let pat = int_eq(int_const(2u128), var(c));
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat);
    assert_eq!(hits.len(), 1);
    assert!(hits[0].output(c).is_some());
}

#[test]
fn int_lt_builder_directional() {
    // `Less` is directional — commutative-retry does NOT swap operands.
    // Pattern `int_lt(int_const(2), var(c))` against IR `int_lt(5, 2)`
    // must miss; pattern `int_lt(int_const(5), var(c))` against the same
    // IR hits.
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let five = b.build_int_const(5u64, NodeOutputType::I64).unwrap();
    let two = b.build_int_const(2u64, NodeOutputType::I64).unwrap();
    let cmp = b
        .build_int_cmp_operation(five, two, strider_ir::IntCmpOp::Less, NodeOutputType::I64)
        .unwrap();
    b.build_return(Some(cmp), &[]).unwrap();
    let function = b.build().unwrap();
    let matcher = Matcher::try_new(&function).unwrap();

    let miss = int_lt(int_const(2u128), var(Capture::default()));
    assert_eq!(matcher.find_all(&miss).len(), 0);

    let hit = int_lt(int_const(5u128), var(Capture::default()));
    assert_eq!(matcher.find_all(&hit).len(), 1);
}

#[test]
fn int_ne_matches_lifted_xor_eq() {
    // `int_ne(a, b)` expands to `xor(int_eq(a, b), int_const(1)):I1`.
    // Build that IR shape directly: cmp_eq(2, 3) (which yields I1) then
    // xor with IntConst(1):I1.
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let lhs = b.build_int_const(2u64, NodeOutputType::I64).unwrap();
    let rhs = b.build_int_const(3u64, NodeOutputType::I64).unwrap();
    let eq = b
        .build_int_cmp_operation(lhs, rhs, strider_ir::IntCmpOp::Equal, NodeOutputType::I64)
        .unwrap();
    let one_i1 = b.build_int_const(1u64, NodeOutputType::I1).unwrap();
    let not_eq = b
        .build_int_binary_operation(
            eq,
            one_i1,
            strider_ir::IntBinaryOp::Xor,
            NodeOutputType::I1,
        )
        .unwrap();
    b.build_return(Some(not_eq), &[]).unwrap();
    let function = b.build().unwrap();

    let pat = int_ne(int_const(2u128), int_const(3u128));
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat);
    assert_eq!(hits.len(), 1, "int_ne should match xor(eq, 1):I1 shape");
}

#[test]
fn int_le_matches_lifted_swap_xor() {
    // `int_le(a, b)` expands to `xor(int_lt(b, a), int_const(1)):I1`.
    // Build the IR: cmp_lt(rhs=3, lhs=2) then xor with IntConst(1):I1.
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let a = b.build_int_const(2u64, NodeOutputType::I64).unwrap();
    let bv = b.build_int_const(3u64, NodeOutputType::I64).unwrap();
    let lt = b
        .build_int_cmp_operation(bv, a, strider_ir::IntCmpOp::Less, NodeOutputType::I64)
        .unwrap();
    let one_i1 = b.build_int_const(1u64, NodeOutputType::I1).unwrap();
    let le = b
        .build_int_binary_operation(
            lt,
            one_i1,
            strider_ir::IntBinaryOp::Xor,
            NodeOutputType::I1,
        )
        .unwrap();
    b.build_return(Some(le), &[]).unwrap();
    let function = b.build().unwrap();

    let pat = int_le(int_const(2u128), int_const(3u128));
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat);
    assert_eq!(hits.len(), 1, "int_le should match the swap+xor shape");
}

#[test]
fn popcount_and_lzcount_match_unit_kinds() {
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let x = b.build_int_const(0x1234u64, NodeOutputType::I64).unwrap();
    let pc = b.build_popcount(x, NodeOutputType::I64).unwrap();
    let lz = b.build_lzcount(x, NodeOutputType::I64).unwrap();
    let sum = b
        .build_int_binary_operation(pc, lz, strider_ir::IntBinaryOp::Add, NodeOutputType::I64)
        .unwrap();
    b.build_return(Some(sum), &[]).unwrap();
    let function = b.build().unwrap();
    let matcher = Matcher::try_new(&function).unwrap();

    let pc_pat = popcount(var(Capture::default()));
    let lz_pat = lzcount(var(Capture::default()));
    assert_eq!(matcher.find_all(&pc_pat).len(), 1);
    assert_eq!(matcher.find_all(&lz_pat).len(), 1);
}

#[test]
fn truncate_zero_extend_sign_extend_int_to_float_float_to_int_match() {
    // Construct an IR scaffolded with: I64 const → trunc to I32 → zero_extend
    // back to I64 → sign_extend (no-op same width? we need a width change so
    // pick I8 → I32) → int_to_float → float_to_int.  Build each cast with the
    // strict, non-coercing FunctionBuilder methods.

    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    // Build a non-const I64 value so `truncate_if_needed` doesn't fold it.
    // We can use Add(0, IntConst) to get a non-const I64 — but that itself
    // gets constant-folded.  Easier: take the result of build_popcount on
    // an int const; popcount isn't constant-folded by the builder.
    let i64v = b.build_int_const(0x1234_5678u64, NodeOutputType::I64).unwrap();
    let pc = b.build_popcount(i64v, NodeOutputType::I64).unwrap();
    let trunc = b.truncate_if_needed(pc, NodeOutputType::I32).unwrap();
    let zext = b
        .extend_if_needed(trunc, NodeOutputType::I64, strider_ir::ExtendOp::ZeroExtend)
        .unwrap();
    let sext_input = b.truncate_if_needed(zext, NodeOutputType::I8).unwrap();
    let sext = b
        .extend_if_needed(sext_input, NodeOutputType::I32, strider_ir::ExtendOp::SignExtend)
        .unwrap();
    let f = b.build_int_to_float(sext, NodeOutputType::F32).unwrap();
    let back = b.build_float_to_int(f, NodeOutputType::I32).unwrap();
    b.build_return(Some(back), &[]).unwrap();
    let function = b.build().unwrap();
    let matcher = Matcher::try_new(&function).unwrap();

    let trunc_pat = truncate(var(Capture::default()));
    let zext_pat = zero_extend(var(Capture::default()));
    let sext_pat = sign_extend(var(Capture::default()));
    let i2f_pat = int_to_float(var(Capture::default()));
    let f2i_pat = float_to_int(var(Capture::default()));

    // `truncate_if_needed` was called twice (pc → I32 and zext → I8), so the
    // graph contains two `Truncate` nodes.
    assert_eq!(matcher.find_all(&trunc_pat).len(), 2);
    assert_eq!(matcher.find_all(&zext_pat).len(), 1);
    assert_eq!(matcher.find_all(&sext_pat).len(), 1);
    assert_eq!(matcher.find_all(&i2f_pat).len(), 1);
    assert_eq!(matcher.find_all(&f2i_pat).len(), 1);
}

#[test]
fn float_add_mul_neg_eq_match() {
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let one = b.build_float_const(1.0f32.to_bits().into(), NodeOutputType::F32);
    let two = b.build_float_const(2.0f32.to_bits().into(), NodeOutputType::F32);
    let neg = b
        .build_float_unary_op(two, strider_ir::FloatUnaryOp::Neg, NodeOutputType::F32)
        .unwrap();
    let sum = b
        .build_float_binary_op(one, neg, strider_ir::FloatBinaryOp::Add, NodeOutputType::F32)
        .unwrap();
    let prod = b
        .build_float_binary_op(sum, two, strider_ir::FloatBinaryOp::Mul, NodeOutputType::F32)
        .unwrap();
    let eq = b
        .build_float_cmp_op(prod, one, strider_ir::FloatCmpOp::Equal)
        .unwrap();
    b.build_return(Some(eq), &[]).unwrap();
    let function = b.build().unwrap();
    let matcher = Matcher::try_new(&function).unwrap();

    let add_pat = float_add(var(Capture::default()), var(Capture::default()));
    let mul_pat = float_mul(var(Capture::default()), var(Capture::default()));
    let neg_pat = float_neg(var(Capture::default()));
    let eq_pat = float_eq(var(Capture::default()), var(Capture::default()));

    assert_eq!(matcher.find_all(&add_pat).len(), 1);
    assert_eq!(matcher.find_all(&mul_pat).len(), 1);
    assert_eq!(matcher.find_all(&neg_pat).len(), 1);
    assert_eq!(matcher.find_all(&eq_pat).len(), 1);
}

#[test]
fn float_ne_matches_xor_eq_one() {
    // float_ne(a, b) → xor(float_eq(a, b), int_const(1)):I1
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let a = b.build_float_const(1.0f32.to_bits().into(), NodeOutputType::F32);
    let bv = b.build_float_const(2.0f32.to_bits().into(), NodeOutputType::F32);
    let eq = b
        .build_float_cmp_op(a, bv, strider_ir::FloatCmpOp::Equal)
        .unwrap();
    let one_i1 = b.build_int_const(1u64, NodeOutputType::I1).unwrap();
    let ne = b
        .build_int_binary_operation(eq, one_i1, strider_ir::IntBinaryOp::Xor, NodeOutputType::I1)
        .unwrap();
    b.build_return(Some(ne), &[]).unwrap();
    let function = b.build().unwrap();

    let pat = float_ne(var(Capture::default()), var(Capture::default()));
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat);
    assert_eq!(hits.len(), 1);
}

#[test]
fn float_le_matches_or_less_eq() {
    // float_le(a, b) → or(float_lt(a, b), float_eq(a, b)) at I1.
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let a = b.build_float_const(1.0f32.to_bits().into(), NodeOutputType::F32);
    let bv = b.build_float_const(2.0f32.to_bits().into(), NodeOutputType::F32);
    let lt = b
        .build_float_cmp_op(a, bv, strider_ir::FloatCmpOp::Less)
        .unwrap();
    let eq = b
        .build_float_cmp_op(a, bv, strider_ir::FloatCmpOp::Equal)
        .unwrap();
    let or = b
        .build_int_binary_operation(lt, eq, strider_ir::IntBinaryOp::Or, NodeOutputType::I1)
        .unwrap();
    b.build_return(Some(or), &[]).unwrap();
    let function = b.build().unwrap();

    let pat = float_le(var(Capture::default()), var(Capture::default()));
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat);
    assert_eq!(hits.len(), 1);
}

#[test]
fn bool_and_or_xor_match_int_binary_at_i1() {
    // Boolean ops are IntBinaryOp::{And,Or,Xor} at I1.  Build two I1
    // values via int cmps, then combine.
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let two = b.build_int_const(2u64, NodeOutputType::I64).unwrap();
    let three = b.build_int_const(3u64, NodeOutputType::I64).unwrap();
    let cmp1 = b
        .build_int_cmp_operation(two, three, strider_ir::IntCmpOp::Equal, NodeOutputType::I64)
        .unwrap();
    let cmp2 = b
        .build_int_cmp_operation(two, three, strider_ir::IntCmpOp::Less, NodeOutputType::I64)
        .unwrap();
    let and_node = b
        .build_int_binary_operation(cmp1, cmp2, strider_ir::IntBinaryOp::And, NodeOutputType::I1)
        .unwrap();
    let or_node = b
        .build_int_binary_operation(cmp1, cmp2, strider_ir::IntBinaryOp::Or, NodeOutputType::I1)
        .unwrap();
    let xor_node = b
        .build_int_binary_operation(cmp1, cmp2, strider_ir::IntBinaryOp::Xor, NodeOutputType::I1)
        .unwrap();
    let combined = b
        .build_int_binary_operation(
            and_node,
            or_node,
            strider_ir::IntBinaryOp::And,
            NodeOutputType::I1,
        )
        .unwrap();
    let final_out = b
        .build_int_binary_operation(
            combined,
            xor_node,
            strider_ir::IntBinaryOp::Xor,
            NodeOutputType::I1,
        )
        .unwrap();
    b.build_return(Some(final_out), &[]).unwrap();
    let function = b.build().unwrap();
    let matcher = Matcher::try_new(&function).unwrap();

    let and_pat = bool_and(var(Capture::default()), var(Capture::default()));
    let or_pat = bool_or(var(Capture::default()), var(Capture::default()));
    let xor_pat = bool_xor(var(Capture::default()), var(Capture::default()));

    // We have two `And` nodes (and_node + combined) and two `Xor` nodes
    // (xor_node + final_out) at I1, and one `Or` node.  bool_* don't
    // currently enforce the I1 guard at match time (the field is set on
    // NodeData but the matcher doesn't read it yet), so they match every
    // IntBinaryOp::{And,Or,Xor} regardless of width — but in this graph
    // every IntBinaryOp is at I1 so the counts coincide.
    assert_eq!(matcher.find_all(&and_pat).len(), 2);
    assert_eq!(matcher.find_all(&or_pat).len(), 1);
    assert_eq!(matcher.find_all(&xor_pat).len(), 2);
}

#[test]
fn bool_not_matches_xor_one_i1() {
    // bool_not(x) → xor(x, int_const(1)):I1.
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let two = b.build_int_const(2u64, NodeOutputType::I64).unwrap();
    let three = b.build_int_const(3u64, NodeOutputType::I64).unwrap();
    let cmp = b
        .build_int_cmp_operation(two, three, strider_ir::IntCmpOp::Equal, NodeOutputType::I64)
        .unwrap();
    let one_i1 = b.build_int_const(1u64, NodeOutputType::I1).unwrap();
    let not_node = b
        .build_int_binary_operation(cmp, one_i1, strider_ir::IntBinaryOp::Xor, NodeOutputType::I1)
        .unwrap();
    b.build_return(Some(not_node), &[]).unwrap();
    let function = b.build().unwrap();

    let pat = bool_not(var(Capture::default()));
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat);
    assert_eq!(hits.len(), 1);
}

#[test]
fn load_builder_matches() {
    // Build: addr = IntConst(0x10):I64; load(addr); return loaded.
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let addr = b.build_int_const(0x10u64, NodeOutputType::I64).unwrap();
    let loaded = b
        .build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::I64)
        .unwrap();
    b.build_return(Some(loaded), &[]).unwrap();
    let function = b.build().unwrap();

    // Unconstrained load — should hit exactly one node.
    let pat: Pat<_> = load().into();
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat);
    assert_eq!(hits.len(), 1);

    // With an address-pattern constraint.
    let c = Capture::default();
    let pat: Pat<_> = load().addr(var(c)).into();
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat);
    assert_eq!(hits.len(), 1);
    assert!(hits[0].output(c).is_some());

    // Constrain to RAM — should hit; to a different space — should miss.
    let pat: Pat<_> = load().space(rsleigh::VnSpace::RAM).into();
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat);
    assert_eq!(hits.len(), 1);
}

#[test]
fn store_builder_matches() {
    // Build: addr = IntConst(0x20):I64; data = IntConst(0x42):I32;
    // store(addr, data); return data.
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let addr = b.build_int_const(0x20u64, NodeOutputType::I64).unwrap();
    let data = b.build_int_const(0x42u64, NodeOutputType::I32).unwrap();
    b.build_store(addr, data, rsleigh::VnSpace::RAM).unwrap();
    b.build_return(Some(data), &[]).unwrap();
    let function = b.build().unwrap();

    // Unconstrained store — should hit exactly one node.
    let pat: Pat<_> = store().into();
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat);
    assert_eq!(hits.len(), 1);

    // Capture the data input.
    let c = Capture::default();
    let pat: Pat<_> = store().data(var(c)).into();
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat);
    assert_eq!(hits.len(), 1);
    assert!(hits[0].output(c).is_some());

    // Address + data constraint together.
    let ca = Capture::default();
    let cd = Capture::default();
    let pat: Pat<_> = store().addr(var(ca)).data(var(cd)).into();
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat);
    assert_eq!(hits.len(), 1);
    assert!(hits[0].output(ca).is_some());
    assert!(hits[0].output(cd).is_some());
}

#[test]
fn mem_phi_matches_freshly_created_region() {
    // Every `create_region()` synthesises a MemPhi at the region head.
    // A single-region function therefore has exactly one MemPhi.
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let v = b.build_int_const(0u64, NodeOutputType::I64).unwrap();
    b.build_return(Some(v), &[]).unwrap();
    let function = b.build().unwrap();

    let pat: Pat<_> = mem_phi().into();
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat);
    assert_eq!(hits.len(), 1);
}

#[test]
fn phi_matches_tagged_phi_for_tracked_var() {
    // A tracked variable means `create_region` emits a Vn-tagged Phi
    // alongside the MemPhi.  Read the variable so the Phi is reachable
    // via the Return's input chain.
    // Fabricate an 8-byte register varnode at offset 0 (RAX-shaped).
    let rax = strider_ir_test_utils::reg_vn(0, 8);
    let mut b: FunctionBuilder = RegisterSet::new()
        .tracked(rax)
        .build_fn_single_region()
        .unwrap();
    let rax_val = b.read_variable(&rax).unwrap();
    b.build_return(Some(rax_val), &[]).unwrap();
    let function = b.build().unwrap();

    // `phi()` matches any `Phi` discriminant today (tagged/anonymous
    // distinction is deferred).  The single tracked-var Phi should hit.
    let pat: Pat<_> = phi().into();
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat);
    assert_eq!(hits.len(), 1);
}

#[test]
fn value_phi_filters_to_anonymous_phis_only() {
    // `value_phi()` matches *anonymous* phis (phi_var_tag == None) only.
    // A tracked-var read produces a tagged Phi (phi_var_tag = Some(rax)),
    // so `value_phi()` should NOT match it — confirming the
    // anonymous-only filter wired through `Function::phi_var_tag`.
    let rax = strider_ir_test_utils::reg_vn(0, 8);
    let mut b: FunctionBuilder = RegisterSet::new()
        .tracked(rax)
        .build_fn_single_region()
        .unwrap();
    let rax_val = b.read_variable(&rax).unwrap();
    b.build_return(Some(rax_val), &[]).unwrap();
    let function = b.build().unwrap();

    // Sanity: phi() (no tag filter) still matches the tagged Phi.
    let any_phi: Pat<_> = phi().into();
    assert_eq!(
        Matcher::try_new(&function).unwrap().find_all(&any_phi).len(),
        1,
        "phi() (no filter) must still match the tagged Phi",
    );

    // value_phi() filters out tagged phis.
    let pat: Pat<_> = value_phi().into();
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat);
    assert!(
        hits.is_empty(),
        "value_phi() must NOT match a lifter-emitted tagged Phi",
    );
}

#[test]
fn call_builder_matches_via_target_const() {
    // Build: addr = IntConst(0x1234):I64; call addr; return.
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let addr = b.build_int_const(0x1234u64, NodeOutputType::I64).unwrap();
    b.build_call(addr).unwrap();
    // build_call terminates the current region — open a new region for
    // the return.
    let post = b.create_region().unwrap();
    b.set_region(post);
    b.build_return(None, &[]).unwrap();
    let function = b.build().unwrap();
    let matcher = Matcher::try_new(&function).unwrap();

    // Unconstrained call.
    let pat: Pat<_> = call().into();
    assert_eq!(matcher.find_all(&pat).len(), 1);

    // .at(addr) — should hit.
    let pat: Pat<_> = call().at(0x1234).into();
    assert_eq!(matcher.find_all(&pat).len(), 1);

    // .at_any covering the addr — should hit.
    let pat: Pat<_> = call().at_any([0x1234, 0xABCD]).into();
    assert_eq!(matcher.find_all(&pat).len(), 1);

    // .at(other) — should miss.
    let pat: Pat<_> = call().at(0xDEAD).into();
    assert_eq!(matcher.find_all(&pat).len(), 0);

    // .target(var(c)) captures the target output.
    let c = Capture::default();
    let pat: Pat<_> = call().target(var(c)).into();
    let hits = matcher.find_all(&pat);
    assert_eq!(hits.len(), 1);
    assert!(hits[0].output(c).is_some());
}

#[test]
fn ret_builder_matches_with_preceded_by_and_ret_val() {
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let v = b.build_int_const(7u64, NodeOutputType::I64).unwrap();
    b.build_return(Some(v), &[]).unwrap();
    let function = b.build().unwrap();
    let matcher = Matcher::try_new(&function).unwrap();

    // Unconstrained return.
    let pat: Pat<_> = ret().into();
    assert_eq!(matcher.find_all(&pat).len(), 1);

    // Capture the return value.
    let c = Capture::default();
    let pat: Pat<_> = ret().ret_val(0, var(c)).into();
    let hits = matcher.find_all(&pat);
    assert_eq!(hits.len(), 1);
    assert!(hits[0].output(c).is_some());

    // Capture the ctrl predecessor.
    let cp = Capture::default();
    let pat: Pat<_> = ret().preceded_by(var(cp)).into();
    let hits = matcher.find_all(&pat);
    assert_eq!(hits.len(), 1);
    assert!(hits[0].output(cp).is_some());
}

#[test]
fn if_builder_matches_with_cond() {
    // Build a simple if(false) { return 1 } else { return 2 } via the
    // test-utils scaffold.
    let (function, _if_node, _) = RegisterSet::new()
        .build_if_then_else_returns(|b| {
            let c = b.build_boolean_const(false);
            Ok((c, ()))
        })
        .unwrap();
    let matcher = Matcher::try_new(&function).unwrap();

    let pat: Pat<_> = if_node().into();
    assert_eq!(matcher.find_all(&pat).len(), 1);

    // Capture the cond input.
    let c = Capture::default();
    let pat: Pat<_> = if_node().cond(var(c)).into();
    let hits = matcher.find_all(&pat);
    assert_eq!(hits.len(), 1);
    assert!(hits[0].output(c).is_some());
}

#[test]
fn initial_var_matches_arg_register() {
    // Tracked + arg-passing rax becomes InitialVar(rax) when read at the
    // entry region.  `initial_var()` matches any InitialVar;
    // `initial_var_for(rax)` matches only that exact varnode.
    let rax = strider_ir_test_utils::reg_vn(0, 8);
    let rbx = strider_ir_test_utils::reg_vn(16, 8);
    let mut b: FunctionBuilder = RegisterSet::new()
        .tracked(rax)
        .arg(rax)
        .build_fn_single_region()
        .unwrap();
    let v = b.read_variable(&rax).unwrap();
    b.build_return(Some(v), &[]).unwrap();
    let function = b.build().unwrap();
    let matcher = Matcher::try_new(&function).unwrap();

    // initial_var() — exactly one InitialVar in this function.
    let pat: Pat<_> = initial_var();
    assert_eq!(matcher.find_all(&pat).len(), 1);

    // initial_var_for(rax) — hits.
    let pat: Pat<_> = initial_var_for(rax);
    assert_eq!(matcher.find_all(&pat).len(), 1);

    // initial_var_for(rbx) — misses (no InitialVar for that varnode).
    let pat: Pat<_> = initial_var_for(rbx);
    assert_eq!(matcher.find_all(&pat).len(), 0);
}

#[test]
fn xor_is_commutative_via_matcher_retry() {
    // Pattern `xor(int_const(0), var(x))` against IR `xor(var, IntConst(0))`.
    // First-order match fails (slot 0 wants IntConst, IR has var); commutative
    // retry swaps and succeeds — so we still get exactly one hit.
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let nine = b.build_int_const(9u64, NodeOutputType::I64).unwrap();
    let zero = b.build_int_const(0u64, NodeOutputType::I64).unwrap();
    // IR: xor(9, 0) — pattern wants xor(0, _), so slot 0 mismatches on
    // the first attempt and commutative retry must swap.
    let xor_out = b
        .build_int_binary_operation(
            nine,
            zero,
            strider_ir::IntBinaryOp::Xor,
            NodeOutputType::I64,
        )
        .unwrap();
    b.build_return(Some(xor_out), &[]).unwrap();
    let function = b.build().unwrap();

    let c = Capture::default();
    let pat = xor(int_const(0u128), var(c));
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat);
    assert_eq!(
        hits.len(),
        1,
        "commutative retry should match xor(9, 0) against xor(0, _)"
    );
    let out = hits[0].output(c).expect("x must be bound");
    let kind = function.kind_of_output(out);
    assert!(
        matches!(kind, strider_ir::node::NodeKind::IntConst(9)),
        "x should bind to the 9-output after commutative retry; got {kind:?}",
    );
}

#[test]
fn int_binary_any_matches_any_int_binary_op() {
    // IR `5 + 7` and `9 ^ 3` — `int_binary_any(_, _)` should match both
    // Add and Xor nodes since the discriminant test ignores the variant.
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let five = b.build_int_const(5u64, NodeOutputType::I64).unwrap();
    let seven = b.build_int_const(7u64, NodeOutputType::I64).unwrap();
    let sum = b
        .build_int_binary_operation(
            five,
            seven,
            strider_ir::IntBinaryOp::Add,
            NodeOutputType::I64,
        )
        .unwrap();
    let nine = b.build_int_const(9u64, NodeOutputType::I64).unwrap();
    let three = b.build_int_const(3u64, NodeOutputType::I64).unwrap();
    let xored = b
        .build_int_binary_operation(
            nine,
            three,
            strider_ir::IntBinaryOp::Xor,
            NodeOutputType::I64,
        )
        .unwrap();
    let combined = b
        .build_int_binary_operation(
            sum,
            xored,
            strider_ir::IntBinaryOp::Or,
            NodeOutputType::I64,
        )
        .unwrap();
    b.build_return(Some(combined), &[]).unwrap();
    let function = b.build().unwrap();

    let (l, r) = (Capture::new(), Capture::new());
    let pat = int_binary_any(var(l), var(r));
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat);
    // Expect 3 matches: Add, Xor, Or.
    assert_eq!(hits.len(), 3, "int_binary_any should match Add+Xor+Or");
}

#[test]
fn int_unary_any_matches_any_int_unary_op() {
    // IR `Neg(5)` then return.  `int_unary_any(_)` should match the Neg.
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let five = b.build_int_const(5u64, NodeOutputType::I64).unwrap();
    let neg = b
        .build_int_unary_operation(five, strider_ir::IntUnaryOp::Neg, NodeOutputType::I64)
        .unwrap();
    b.build_return(Some(neg), &[]).unwrap();
    let function = b.build().unwrap();

    let c = Capture::new();
    let pat = int_unary_any(var(c));
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat);
    assert_eq!(hits.len(), 1);
}

#[test]
fn int_const_all_ones_matches_max_intconst() {
    // IR `Xor(x, IntConst(u64::MAX))` — the all-ones mask at I64 is
    // `u64::MAX`.  `int_const_all_ones()` should match the IntConst.
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let zero = b.build_int_const(0u64, NodeOutputType::I64).unwrap();
    let all_ones = b
        .build_int_const(u64::MAX, NodeOutputType::I64)
        .unwrap();
    let xored = b
        .build_int_binary_operation(
            zero,
            all_ones,
            strider_ir::IntBinaryOp::Xor,
            NodeOutputType::I64,
        )
        .unwrap();
    b.build_return(Some(xored), &[]).unwrap();
    let function = b.build().unwrap();

    let pat = int_const_all_ones();
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat);
    assert_eq!(hits.len(), 1, "must match the u64::MAX constant");
}

#[test]
fn bit_not_matches_xor_with_all_ones() {
    // IR `Xor(x, IntConst(u64::MAX)) → ~x`.  `bit_not(var(c))` should
    // match and bind `c` to `x`'s output.
    let mut b: FunctionBuilder = RegisterSet::new().build_fn_single_region().unwrap();
    let x = b.build_int_const(42u64, NodeOutputType::I64).unwrap();
    let all_ones = b
        .build_int_const(u64::MAX, NodeOutputType::I64)
        .unwrap();
    let not_x = b
        .build_int_binary_operation(
            x,
            all_ones,
            strider_ir::IntBinaryOp::Xor,
            NodeOutputType::I64,
        )
        .unwrap();
    b.build_return(Some(not_x), &[]).unwrap();
    let function = b.build().unwrap();

    let c = Capture::new();
    let pat = bit_not(var(c));
    let hits = Matcher::try_new(&function).unwrap().find_all(&pat);
    assert_eq!(hits.len(), 1, "bit_not should match Xor(x, all_ones)");
    let bound = hits[0].output(c).expect("c must bind to x");
    assert_eq!(bound, x);
}
