use ir::{
    FloatBinaryOp, FloatCmpOp, FloatUnaryOp, FunctionBuilder, IntBinaryOp,
    node::NodeOutputType,
};
use pattern::*;

use super::common::*;

// ── Lzcount pattern tests ───────────────────────────────────────────────────

#[test]
fn lzcount_pattern_matches() -> ir::Result<()> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[], None, 0)?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let v = b.build_int_const(1, NodeOutputType::U8);
    let lz = b.build_lzcount(v, NodeOutputType::U8)?;
    b.build_return(Some(lz), &[])?;
    let g = b.build().expect("build failed: validator rejected graph");

    let m = Matcher::new(&g);
    let hits = m.find_all(&lzcount(any()));
    assert_eq!(hits.len(), 1);
    Ok(())
}

// ── Float pattern tests ───────────────────────────────────────────────────────

/// `float_add(1.0f64, 2.0f64)` — basic binary float pattern match.
#[test]
fn float_add_pattern_matches() -> ir::Result<()> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[], None, 0)?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let c1 = b.build_float_const(1.0f64.to_bits(), NodeOutputType::F64);
    let c2 = b.build_float_const(2.0f64.to_bits(), NodeOutputType::F64);
    let sum = b.build_float_binary_op(c1, c2, FloatBinaryOp::Add, NodeOutputType::F64)?;
    b.build_return(Some(sum), &[])?;
    let g = b.build().expect("build failed: validator rejected graph");

    let m = Matcher::new(&g);

    // Match float_add with exact constants.
    let hits =
        m.find_all(&float_add(float_const(1.0f64.to_bits()), float_const(2.0f64.to_bits())).into());
    assert_eq!(hits.len(), 1);

    // Wrong constant → no match.
    let miss =
        m.find_all(&float_add(float_const(3.0f64.to_bits()), float_const(2.0f64.to_bits())).into());
    assert!(miss.is_empty());
    Ok(())
}

/// Float `mul` is commutative: `float_mul(a, b)` should also match with reversed operands.
#[test]
fn float_mul_commutative_pattern() -> ir::Result<()> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[], None, 0)?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let c3 = b.build_float_const(3.0f32.to_bits() as u64, NodeOutputType::F32);
    let c7 = b.build_float_const(7.0f32.to_bits() as u64, NodeOutputType::F32);
    // Build node as 7 * 3.
    let prod = b.build_float_binary_op(c7, c3, FloatBinaryOp::Mul, NodeOutputType::F32)?;
    b.build_return(Some(prod), &[])?;
    let g = b.build().expect("build failed: validator rejected graph");

    let m = Matcher::new(&g);
    let v_a = Var::new();
    let v_b = Var::new();

    // Pattern states 3 * 7; node stores 7 * 3 — commutative match must succeed.
    let hits = m.find_all(
        &float_mul(
            float_const(3.0f32.to_bits() as u64),
            float_const(7.0f32.to_bits() as u64),
        )
        .into(),
    );
    assert_eq!(hits.len(), 1);

    // Any-capture version also works.
    let hits2 = m.find_all(&float_mul(any_float_const(v_a), any_float_const(v_b)).into());
    assert_eq!(hits2.len(), 1);
    Ok(())
}

/// Float `sub` is NOT commutative: wrong order must fail.
#[test]
fn float_sub_not_commutative() -> ir::Result<()> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[], None, 0)?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let c5 = b.build_float_const(5.0f64.to_bits(), NodeOutputType::F64);
    let c2 = b.build_float_const(2.0f64.to_bits(), NodeOutputType::F64);
    // 5.0 - 2.0
    let diff = b.build_float_binary_op(c5, c2, FloatBinaryOp::Sub, NodeOutputType::F64)?;
    b.build_return(Some(diff), &[])?;
    let g = b.build().expect("build failed: validator rejected graph");

    let m = Matcher::new(&g);
    // Correct order matches.
    assert_eq!(
        m.find_all(&float_sub(float_const(5.0f64.to_bits()), float_const(2.0f64.to_bits())).into())
            .len(),
        1
    );
    // Wrong order does NOT match.
    assert!(
        m.find_all(&float_sub(float_const(2.0f64.to_bits()), float_const(5.0f64.to_bits())).into())
            .is_empty()
    );
    Ok(())
}

/// Float comparison (`float_eq`) produces a `Bool` output.
#[test]
fn float_eq_pattern_matches() -> ir::Result<()> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[], None, 0)?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let c3 = b.build_float_const(3.0f64.to_bits(), NodeOutputType::F64);
    let c4 = b.build_float_const(4.0f64.to_bits(), NodeOutputType::F64);
    let cmp = b.build_float_cmp_op(c3, c4, FloatCmpOp::Equal)?;
    b.build_return(Some(cmp), &[])?;
    let g = b.build().expect("build failed: validator rejected graph");

    let m = Matcher::new(&g);
    let hits = m.find_all(&float_eq(
        float_const(3.0f64.to_bits()),
        float_const(4.0f64.to_bits()),
    ));
    assert_eq!(hits.len(), 1);

    // Wrong op kind → no match.
    let miss = m.find_all(&float_lt(
        float_const(3.0f64.to_bits()),
        float_const(4.0f64.to_bits()),
    ));
    assert!(miss.is_empty());
    Ok(())
}

/// `float_neg` unary pattern match.
#[test]
fn float_unary_pattern_matches() -> ir::Result<()> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[], None, 0)?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let cv = b.build_float_const(2.0f64.to_bits(), NodeOutputType::F64);
    let neg_v = b.build_float_unary_op(cv, FloatUnaryOp::Neg, NodeOutputType::F64)?;
    b.build_return(Some(neg_v), &[])?;
    let g = b.build().expect("build failed: validator rejected graph");

    let m = Matcher::new(&g);
    // Correct unary op matches.
    let hits = m.find_all(&float_neg(float_const(2.0f64.to_bits())));
    assert_eq!(hits.len(), 1);
    // Different unary op → no match.
    let miss = m.find_all(&float_abs(float_const(2.0f64.to_bits())));
    assert!(miss.is_empty());
    Ok(())
}

/// `any_float_const` captures the float constant bits.
#[test]
fn any_float_const_captures_bits() -> ir::Result<()> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[], None, 0)?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let bits = 42.5f64.to_bits();
    let cv = b.build_float_const(bits, NodeOutputType::F64);
    b.build_return(Some(cv), &[])?;
    let g = b.build().expect("build failed: validator rejected graph");

    let m = Matcher::new(&g);
    let v = Var::new();
    let hits = m.find_all(&any_float_const(v));
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].get_float_bits(v, &g), Some(bits));
    Ok(())
}

/// `int_bits_to_float` bitcast pattern match.
#[test]
fn int_bits_to_float_pattern_matches() -> ir::Result<()> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[], None, 0)?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    // Non-const int → explicit IntBitsToFloat node must be emitted.
    let int_val = b.build_int_const(0xDEAD, NodeOutputType::U64);
    // Force a non-const path so we actually get an IntBitsToFloat node.
    // Add 0 to make the optimizer think it's not constant (int_const 0).
    let zero = b.build_int_const(0, NodeOutputType::U64);
    let non_const =
        b.build_int_binary_operation(int_val, zero, IntBinaryOp::Add, NodeOutputType::U64)?;
    let float_v = b.build_int_bits_to_float(non_const, NodeOutputType::F64)?;
    b.build_return(Some(float_v), &[])?;
    let g = b.build().expect("build failed: validator rejected graph");

    let m = Matcher::new(&g);
    let hits = m.find_all(&int_bits_to_float(any()));
    assert_eq!(hits.len(), 1);
    // float_bits_to_int should NOT match.
    assert!(m.find_all(&float_bits_to_int(any())).is_empty());
    Ok(())
}

/// `float_bits_to_int` bitcast pattern match.
#[test]
fn float_bits_to_int_pattern_matches() -> ir::Result<()> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[], None, 0)?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let cv = b.build_float_const(1.0f32.to_bits() as u64, NodeOutputType::F32);
    // Force a non-const float so we get a FloatBitsToInt node.
    let neg_v = b.build_float_unary_op(cv, FloatUnaryOp::Neg, NodeOutputType::F32)?;
    let int_v = b.build_float_bits_to_int(neg_v, NodeOutputType::U32)?;
    b.build_return(Some(int_v), &[])?;
    let g = b.build().expect("build failed: validator rejected graph");

    let m = Matcher::new(&g);
    let hits = m.find_all(&float_bits_to_int(any()));
    assert_eq!(hits.len(), 1);
    Ok(())
}

/// `int_to_float`, `float_to_int`, `float_to_float` conversion patterns.
#[test]
fn float_conversion_patterns_match() -> ir::Result<()> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[], None, 0)?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let int_v = b.build_int_const(42, NodeOutputType::U64);
    let f64_v = b.build_int_to_float(int_v, NodeOutputType::F64)?;
    let f32_v = b.build_float_to_float(f64_v, NodeOutputType::F32)?;
    let int_v2 = b.build_float_to_int(f32_v, NodeOutputType::U32)?;
    b.build_return(Some(int_v2), &[])?;
    let g = b.build().expect("build failed: validator rejected graph");

    let m = Matcher::new(&g);
    assert_eq!(m.find_all(&int_to_float(any())).len(), 1);
    assert_eq!(m.find_all(&float_to_float(any())).len(), 1);
    assert_eq!(m.find_all(&float_to_int(any())).len(), 1);
    Ok(())
}

/// `.ordered()` on `float_add` prevents commutative fallback.
#[test]
fn float_add_ordered_no_commutative_fallback() -> ir::Result<()> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[], None, 0)?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let c1 = b.build_float_const(1.0f64.to_bits(), NodeOutputType::F64);
    let c2 = b.build_float_const(2.0f64.to_bits(), NodeOutputType::F64);
    let sum = b.build_float_binary_op(c1, c2, FloatBinaryOp::Add, NodeOutputType::F64)?;
    b.build_return(Some(sum), &[])?;
    let g = b.build().expect("build failed: validator rejected graph");

    let m = Matcher::new(&g);
    // Ordered: correct order matches.
    assert_eq!(
        m.find_all(
            &float_add(float_const(1.0f64.to_bits()), float_const(2.0f64.to_bits()),)
                .ordered()
                .into()
        )
        .len(),
        1
    );
    // Ordered: wrong order does NOT match even though Add is commutative.
    assert!(
        m.find_all(
            &float_add(float_const(2.0f64.to_bits()), float_const(1.0f64.to_bits()),)
                .ordered()
                .into()
        )
        .is_empty()
    );
    Ok(())
}

/// `cast_to_float` pattern matches a `CastToFloat` node.
#[test]
fn cast_to_float_pattern_matches() -> ir::Result<()> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[], None, 0)?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let int_val = b.build_int_const(0x3F800000, NodeOutputType::U32);
    let cast = b.build_cast_to_float(int_val, NodeOutputType::F32);
    b.build_return(Some(cast), &[])?;
    let g = b.build().expect("build failed: validator rejected graph");

    let m = Matcher::new(&g);
    // Matches the CastToFloat node.
    let hits = m.find_all(&cast_to_float(any()));
    assert_eq!(hits.len(), 1);
    // Other unary patterns do NOT match.
    assert!(m.find_all(&int_bits_to_float(any())).is_empty());
    Ok(())
}

// ── StackStore / StackStorePhi patterns ─────────────────────────────────────

/// Builds a graph where `*(sp - 4) = 0xAB`; returns the loaded value to keep
/// the store live.  The `StackStoreDetect` pass then rewrites it into a
/// `StackStore { offset: -4 }`.
fn graph_with_stack_store() -> ir::Result<(ir::BuiltFunctionGraph, rsleigh::Vn)> {
    let sp = make_reg_vn(0x20, 4);
    let mut b = FunctionBuilder::new(vec![sp], &[], &[sp], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);
    let sp_val = b.read_variable(&sp)?;
    let four = b.build_int_const(4, NodeOutputType::U32);
    let addr = b.build_int_binary_operation(sp_val, four, IntBinaryOp::Sub, NodeOutputType::U32)?;
    let data = b.build_int_const(0xAB, NodeOutputType::U32);
    b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
    let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
    b.build_return(Some(loaded), &[])?;
    let mut fg = b.build().expect("build failed: validator rejected graph");
    let mut pipeline = opt::OptimizerPipeline::new();
    pipeline.add(opt::ConstantFold);
    pipeline.add(opt::RedundantPhis);
    pipeline.add(opt::StackStoreDetect::new(sp));
    pipeline.run(&mut fg).expect("opt pipeline should succeed");
    Ok((fg, sp))
}

#[test]
fn stack_store_matches_offset_and_data() -> ir::Result<()> {
    let (g, _sp) = graph_with_stack_store()?;
    let m = Matcher::new(&g);
    // Exact offset + exact data → match.
    let hits = m.find_all(&stack_store().offset(-4).data(int_const(0xAB)).into());
    assert_eq!(
        hits.len(),
        1,
        "expected one match for offset=-4 & data=0xAB"
    );
    // Wrong offset → no match.
    assert!(m.find_all(&stack_store().offset(0).into()).is_empty());
    // Wrong data → no match.
    assert!(
        m.find_all(&stack_store().data(int_const(0x42)).into())
            .is_empty()
    );
    // Offset-only, no data constraint → match.
    assert_eq!(m.find_all(&stack_store().offset(-4).into()).len(), 1);
    Ok(())
}

/// Builds a two-branch graph where both predecessors adjust SP differently
/// before merging and storing through the SP-phi, yielding a `StackStorePhi`
/// node with offsets `[-4, -8]`.
fn graph_with_stack_store_phi() -> ir::Result<(ir::BuiltFunctionGraph, rsleigh::Vn)> {
    let sp = make_reg_vn(0x20, 4);
    let mut b = FunctionBuilder::new(vec![sp], &[], &[sp], &[], None, 0)?;
    let entry = b.create_region()?;
    let a = b.create_region()?;
    let bb = b.create_region()?;
    let c = b.create_region()?;
    b.set_entry_region(entry)?;
    b.set_region(entry);
    let cond = b.build_boolean_const(true);
    b.build_if(cond, a, bb)?;
    b.set_region(a);
    let sp_a = b.read_variable(&sp)?;
    let four = b.build_int_const(4, NodeOutputType::U32);
    let sp_a2 = b.build_int_binary_operation(sp_a, four, IntBinaryOp::Sub, NodeOutputType::U32)?;
    b.write_variable(&sp, sp_a2)?;
    b.build_branch(c)?;
    b.set_region(bb);
    let sp_b = b.read_variable(&sp)?;
    let eight = b.build_int_const(8, NodeOutputType::U32);
    let sp_b2 = b.build_int_binary_operation(sp_b, eight, IntBinaryOp::Sub, NodeOutputType::U32)?;
    b.write_variable(&sp, sp_b2)?;
    b.build_branch(c)?;
    b.set_region(c);
    let sp_c = b.read_variable(&sp)?;
    let data = b.build_int_const(0xCC, NodeOutputType::U32);
    b.build_store(sp_c, data, rsleigh::VnSpace::RAM)?;
    let loaded = b.build_load(sp_c, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
    b.build_return(Some(loaded), &[])?;
    let mut fg = b.build().expect("build failed: validator rejected graph");
    let mut pipeline = opt::OptimizerPipeline::new();
    pipeline.add(opt::ConstantFold);
    pipeline.add(opt::RedundantPhis);
    pipeline.add(opt::StackStoreDetect::new(sp));
    pipeline.run(&mut fg).expect("opt pipeline should succeed");
    Ok((fg, sp))
}

#[test]
fn stack_store_phi_matches_offsets() -> ir::Result<()> {
    let (g, _sp) = graph_with_stack_store_phi()?;
    let m = Matcher::new(&g);
    // Exact offsets (order-independent) → match.
    assert_eq!(
        m.find_all(&stack_store_phi().offsets([-4, -8]).into())
            .len(),
        1
    );
    assert_eq!(
        m.find_all(&stack_store_phi().offsets([-8, -4]).into())
            .len(),
        1
    );
    // Wrong offsets → no match.
    assert!(
        m.find_all(&stack_store_phi().offsets([0, -4]).into())
            .is_empty()
    );
    // No offset constraint → still matches.
    assert_eq!(m.find_all(&stack_store_phi().into()).len(), 1);
    Ok(())
}

/// cdecl-style call with two pushed stack arguments.  After
/// `CallStackArgCollect` runs, the Call's inputs include the pushed values
/// as positional stack args.
fn graph_cdecl_call_with_stack_args() -> ir::Result<ir::BuiltFunctionGraph> {
    let sp = make_reg_vn(0x20, 4);
    let mut b = FunctionBuilder::new(vec![sp], &[], &[sp], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);
    let sp_v0 = b.read_variable(&sp)?;
    let four = b.build_int_const(4, NodeOutputType::U32);
    let sp_v1 = b.build_int_binary_operation(sp_v0, four, IntBinaryOp::Sub, NodeOutputType::U32)?;
    b.write_variable(&sp, sp_v1)?;
    let arg1 = b.build_int_const(22, NodeOutputType::U32);
    b.build_store(sp_v1, arg1, rsleigh::VnSpace::RAM)?;
    let sp_v2 = b.build_int_binary_operation(sp_v1, four, IntBinaryOp::Sub, NodeOutputType::U32)?;
    b.write_variable(&sp, sp_v2)?;
    let arg0 = b.build_int_const(11, NodeOutputType::U32);
    b.build_store(sp_v2, arg0, rsleigh::VnSpace::RAM)?;
    let target = b.build_int_const(0x1000, NodeOutputType::U32);
    b.build_call(target)?;
    b.build_return(None, &[])?;
    let mut fg = b.build().expect("build failed: validator rejected graph");
    let mut pipeline = opt::OptimizerPipeline::new();
    pipeline.add(opt::ConstantFold);
    pipeline.add(opt::RedundantPhis);
    pipeline.add(opt::StackStoreDetect::new(sp));
    pipeline.add_post_pass(opt::CallStackArgCollect::new(vec![0, 4, 8, 12]));
    pipeline.run(&mut fg).expect("opt pipeline should succeed");
    Ok(fg)
}

#[test]
fn call_arg_matches_stack_arg_after_collection() -> ir::Result<()> {
    let g = graph_cdecl_call_with_stack_args()?;
    let m = Matcher::new(&g);
    // arg(0) should be the pushed-last value 11, arg(1) should be 22.
    assert_eq!(m.find_all(&call().arg(0, int_const(11)).into()).len(), 1);
    assert_eq!(m.find_all(&call().arg(1, int_const(22)).into()).len(), 1);
    // Both together.
    assert_eq!(
        m.find_all(&call().arg(0, int_const(11)).arg(1, int_const(22)).into())
            .len(),
        1
    );
    // Wrong arg → no match.
    assert!(m.find_all(&call().arg(0, int_const(22)).into()).is_empty());
    Ok(())
}
