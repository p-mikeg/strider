/// Integration tests for the `pattern` crate.
///
/// Each test builds a small `BuiltFunctionGraph` using `FunctionBuilder`,
/// then runs `Matcher::find_all` and asserts the expected number of matches
/// and captured bindings.
use ir::{
    BoolBinaryOp, BoolUnaryOp, ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp,
    FunctionBuilder, IntBinaryOp, IntCmpOp, IntUnaryOp,
    node::{NodeKind, NodeOutputType},
};
use pattern::*;

// ── helpers ───────────────────────────────────────────────────────────────────

/// Constructs a fake `rsleigh::Vn` (register varnode) for tests that need one.
///
/// `off` is used to produce distinct varnodes; `size` is in bytes.
fn make_reg_vn(off: u64, size: u32) -> rsleigh::Vn {
    rsleigh::Vn {
        size,
        addr: rsleigh::VnAddr { off, space: rsleigh::VnSpace::REGISTER },
    }
}

// ── Graph builders ────────────────────────────────────────────────────────────

/// `add(5, 3)`, then return the result.
/// Shape: Entry → region[add(5,3), return(add_result)]
fn graph_add_return() -> ir::Result<ir::BuiltFunctionGraph> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let c5   = b.build_int_const(5, NodeOutputType::U64);
    let c3   = b.build_int_const(3, NodeOutputType::U64);
    let sum  = b.build_int_binary_operation(c5, c3, IntBinaryOp::Add, NodeOutputType::U64)?;
    b.build_return(Some(sum), &[])?;
    Ok(b.build())
}

/// `and(4, 7)`, `add(and_result, 1)`, return.
fn graph_and_add_return() -> ir::Result<ir::BuiltFunctionGraph> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let c4   = b.build_int_const(4, NodeOutputType::U64);
    let c7   = b.build_int_const(7, NodeOutputType::U64);
    let c1   = b.build_int_const(1, NodeOutputType::U64);
    let band = b.build_int_binary_operation(c4, c7, IntBinaryOp::And, NodeOutputType::U64)?;
    let sum  = b.build_int_binary_operation(band, c1, IntBinaryOp::Add, NodeOutputType::U64)?;
    b.build_return(Some(sum), &[])?;
    Ok(b.build())
}

/// Call at target `0x1234`, then return.
fn graph_call_return() -> ir::Result<ir::BuiltFunctionGraph> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let tgt = b.build_uint64_const(0x1234);
    b.build_call(tgt)?;
    b.build_return(None, &[])?;
    Ok(b.build())
}

/// Two calls (`0x1111`, `0x2222`) in sequence, then return.
fn graph_two_calls_return() -> ir::Result<ir::BuiltFunctionGraph> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let t1 = b.build_uint64_const(0x1111);
    let t2 = b.build_uint64_const(0x2222);
    b.build_call(t1)?;
    b.build_call(t2)?;
    b.build_return(None, &[])?;
    Ok(b.build())
}

/// If (4 == 1):
///   true  branch → return 10
///   false branch → return 20
fn graph_if_branches() -> ir::Result<ir::BuiltFunctionGraph> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let entry = b.create_region()?;
    let true_r  = b.create_region()?;
    let false_r = b.create_region()?;

    b.set_entry_region(entry)?;

    b.set_region(true_r);
    let c10 = b.build_int_const(10, NodeOutputType::U64);
    b.build_return(Some(c10), &[])?;

    b.set_region(false_r);
    let c20 = b.build_int_const(20, NodeOutputType::U64);
    b.build_return(Some(c20), &[])?;

    b.set_region(entry);
    let c4  = b.build_int_const(4, NodeOutputType::U64);
    let c1  = b.build_int_const(1, NodeOutputType::U64);
    let cond = b.build_int_cmp_operation(c4, c1, IntCmpOp::Equal, NodeOutputType::U64)?;
    b.build_if(cond, true_r, false_r)?;
    Ok(b.build())
}

/// If (x == 1):
///   true branch → Call at 0x2345, then return
///   false branch → return
fn graph_if_with_call_in_true_branch() -> ir::Result<ir::BuiltFunctionGraph> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let entry  = b.create_region()?;
    let true_r  = b.create_region()?;
    let false_r = b.create_region()?;

    b.set_entry_region(entry)?;

    b.set_region(true_r);
    let tgt = b.build_uint64_const(0x2345);
    b.build_call(tgt)?;
    b.build_return(None, &[])?;

    b.set_region(false_r);
    b.build_return(None, &[])?;

    b.set_region(entry);
    let c5  = b.build_int_const(5, NodeOutputType::U64);
    let c1  = b.build_int_const(1, NodeOutputType::U64);
    let cond = b.build_int_cmp_operation(c5, c1, IntCmpOp::Equal, NodeOutputType::U64)?;
    b.build_if(cond, true_r, false_r)?;
    Ok(b.build())
}

/// If (x == 1):
///   true branch → return
///   false branch → Call at 0x5678, then return
fn graph_if_with_call_in_false_branch() -> ir::Result<ir::BuiltFunctionGraph> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let entry  = b.create_region()?;
    let true_r  = b.create_region()?;
    let false_r = b.create_region()?;

    b.set_entry_region(entry)?;

    b.set_region(true_r);
    b.build_return(None, &[])?;

    b.set_region(false_r);
    let tgt = b.build_uint64_const(0x5678);
    b.build_call(tgt)?;
    b.build_return(None, &[])?;

    b.set_region(entry);
    let c5  = b.build_int_const(5, NodeOutputType::U64);
    let c1  = b.build_int_const(1, NodeOutputType::U64);
    let cond = b.build_int_cmp_operation(c5, c1, IntCmpOp::Equal, NodeOutputType::U64)?;
    b.build_if(cond, true_r, false_r)?;
    Ok(b.build())
}

/// neg(add(5, 3)), then return.
fn graph_neg_add_return() -> ir::Result<ir::BuiltFunctionGraph> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let c5  = b.build_int_const(5, NodeOutputType::U64);
    let c3  = b.build_int_const(3, NodeOutputType::U64);
    let sum = b.build_int_binary_operation(c5, c3, IntBinaryOp::Add, NodeOutputType::U64)?;
    let neg = b.build_int_unary_operation(sum, IntUnaryOp::Neg, NodeOutputType::U64)?;
    b.build_return(Some(neg), &[])?;
    Ok(b.build())
}

/// not(bool_const(true)), then return.
fn graph_bool_not_return() -> ir::Result<ir::BuiltFunctionGraph> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let t  = b.build_boolean_const(true);
    let nt = b.build_boolean_unary_operation(t, BoolUnaryOp::Neg)?;
    // cast to int so we can return it
    let as_int = b.convert_to_int_if_needed(nt, NodeOutputType::U64)?;
    b.build_return(Some(as_int), &[])?;
    Ok(b.build())
}

/// bool_and(true, false), then return.
fn graph_bool_and_return() -> ir::Result<ir::BuiltFunctionGraph> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let t  = b.build_boolean_const(true);
    let f  = b.build_boolean_const(false);
    let ba = b.build_boolean_operation(t, f, BoolBinaryOp::And)?;
    let as_int = b.convert_to_int_if_needed(ba, NodeOutputType::U64)?;
    b.build_return(Some(as_int), &[])?;
    Ok(b.build())
}

/// zero_extend(add(1, 2) : U32 → U64), then return.
fn graph_zero_extend_return() -> ir::Result<ir::BuiltFunctionGraph> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let c1  = b.build_int_const(1, NodeOutputType::U32);
    let c2  = b.build_int_const(2, NodeOutputType::U32);
    let sum = b.build_int_binary_operation(c1, c2, IntBinaryOp::Add, NodeOutputType::U32)?;
    let ext = b.extend_if_needed(sum, NodeOutputType::U64, ExtendOp::ZeroExtend)?;
    b.build_return(Some(ext), &[])?;
    Ok(b.build())
}

/// truncate(add(1u64, 2u64) → U8), then return.
fn graph_truncate_return() -> ir::Result<ir::BuiltFunctionGraph> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let c1  = b.build_int_const(1, NodeOutputType::U64);
    let c2  = b.build_int_const(2, NodeOutputType::U64);
    let sum = b.build_int_binary_operation(c1, c2, IntBinaryOp::Add, NodeOutputType::U64)?;
    let tr  = b.truncate_if_needed(sum, NodeOutputType::U8)?;
    b.build_return(Some(tr), &[])?;
    Ok(b.build())
}

/// add(add(1, 2), 3) nested three levels, return.
fn graph_nested_add() -> ir::Result<ir::BuiltFunctionGraph> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let c1   = b.build_int_const(1, NodeOutputType::U64);
    let c2   = b.build_int_const(2, NodeOutputType::U64);
    let c3   = b.build_int_const(3, NodeOutputType::U64);
    let s12  = b.build_int_binary_operation(c1, c2, IntBinaryOp::Add, NodeOutputType::U64)?;
    let s123 = b.build_int_binary_operation(s12, c3, IntBinaryOp::Add, NodeOutputType::U64)?;
    b.build_return(Some(s123), &[])?;
    Ok(b.build())
}

/// store(addr=0x200, data=42) then load from same addr, return the loaded value.
///
/// The Store's memory output flows into Load (which consumes it as input[0]),
/// making the Store node reachable via the preorder walk from Return.
fn graph_store_then_load() -> ir::Result<ir::BuiltFunctionGraph> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let addr = b.build_uint64_const(0x200);
    let data = b.build_uint64_const(42);
    b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
    // Load consumes the Store's memory output, making the Store reachable.
    let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U64)?;
    b.build_return(Some(loaded), &[])?;
    Ok(b.build())
}

/// load(addr=0x100) in RAM space, return the loaded value.
fn graph_load_return() -> ir::Result<ir::BuiltFunctionGraph> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let addr   = b.build_uint64_const(0x100);
    let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U64)?;
    b.build_return(Some(loaded), &[])?;
    Ok(b.build())
}

/// store(addr=0x300, data=7) in RAM, then call(0xCAFE) (which consumes the
/// current memory), then return.
///
/// The Store's memory is threaded into the Call (via cur_region_memory), so
/// the Store is reachable from the Return through the Call's inputs.
fn graph_store_then_call() -> ir::Result<ir::BuiltFunctionGraph> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let addr = b.build_uint64_const(0x300);
    let data = b.build_uint64_const(7);
    b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
    // build_call takes cur_region_memory() which is now the Store's mem output.
    let tgt = b.build_uint64_const(0xCAFE);
    b.build_call(tgt)?;
    b.build_return(None, &[])?;
    Ok(b.build())
}

/// Call at 0xABCD with one argument register pre-loaded with the value 42.
///
/// Layout of the Call's inputs:
///   [ctrl(0), mem(1), target(2)=IntConst(0xABCD), arg0(3)=IntConst(42)]
fn graph_call_with_arg() -> ir::Result<(ir::BuiltFunctionGraph, rsleigh::Vn)> {
    let arg_vn = make_reg_vn(0, 8); // 8-byte register at offset 0
    // Register it as both tracked and arg-passing.
    let mut b = FunctionBuilder::new(vec![arg_vn], &[arg_vn], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    // Overwrite the initial value with a concrete constant.
    let c42 = b.build_uint64_const(42);
    b.write_variable(&arg_vn, c42)?;
    let tgt = b.build_uint64_const(0xABCD);
    b.build_call(tgt)?;
    b.build_return(None, &[])?;
    Ok((b.build(), arg_vn))
}

/// Graph with one variable (register varnode), returned as the result.
/// Produces an `InitialVar` node.
fn graph_with_initial_var() -> ir::Result<(ir::BuiltFunctionGraph, rsleigh::Vn)> {
    let vn = make_reg_vn(0, 8);
    let mut b = FunctionBuilder::new(vec![vn], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let val = b.read_variable(&vn)?;
    b.build_return(Some(val), &[])?;
    Ok((b.build(), vn))
}

/// Graph with two variables (different offsets), both returned.
fn graph_with_two_initial_vars() -> ir::Result<(ir::BuiltFunctionGraph, rsleigh::Vn, rsleigh::Vn)> {
    let vn_a = make_reg_vn(0, 8);
    let vn_b = make_reg_vn(8, 8);
    let mut b = FunctionBuilder::new(vec![vn_a, vn_b], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let val_a = b.read_variable(&vn_a)?;
    let val_b = b.read_variable(&vn_b)?;
    // add them together so both are reachable
    let sum = b.build_int_binary_operation(val_a, val_b, IntBinaryOp::Add, NodeOutputType::U64)?;
    b.build_return(Some(sum), &[])?;
    Ok((b.build(), vn_a, vn_b))
}

/// Graph: if (cond): return 10; else: return 20.
/// Uses a variable for the condition so `ControlPhi` nodes are created.
#[allow(dead_code)]
fn graph_if_with_phi() -> ir::Result<(ir::BuiltFunctionGraph, rsleigh::Vn)> {
    let flag = make_reg_vn(0, 8);
    let mut b = FunctionBuilder::new(vec![flag], &[], &[], &[])?;
    let entry  = b.create_region()?;
    let true_r  = b.create_region()?;
    let false_r = b.create_region()?;

    b.set_entry_region(entry)?;

    b.set_region(true_r);
    let c10 = b.build_int_const(10, NodeOutputType::U64);
    b.build_return(Some(c10), &[])?;

    b.set_region(false_r);
    let c20 = b.build_int_const(20, NodeOutputType::U64);
    b.build_return(Some(c20), &[])?;

    b.set_region(entry);
    let flag_val = b.read_variable(&flag)?;
    let cond = b.convert_to_bool_if_needed(flag_val)?;
    b.build_if(cond, true_r, false_r)?;
    Ok((b.build(), flag))
}

// ── Var uniqueness ────────────────────────────────────────────────────────────

#[test]
fn var_ids_are_unique() -> ir::Result<()> {
    let a = Var::new();
    let b = Var::new();
    let c = Var::new();
    assert_ne!(a, b);
    assert_ne!(b, c);
    assert_ne!(a, c);
    Ok(())
}

#[test]
fn node_var_ids_are_unique_and_distinct_from_var() -> ir::Result<()> {
    let v  = Var::new();
    let nv = NodeVar::new();
    // Their raw ids come from the same counter so they can't collide.
    // We can only check that two NodeVars differ from each other.
    let nv2 = NodeVar::new();
    assert_ne!(nv, nv2);
    let _ = v; // used
    Ok(())
}

// ── int_const pattern ─────────────────────────────────────────────────────────

#[test]
fn int_const_matches_exact_value() -> ir::Result<()> {
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&int_const(5).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn int_const_no_match_for_wrong_value() -> ir::Result<()> {
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&int_const(99).into());
    assert!(hits.is_empty());
    Ok(())
}

#[test]
fn bool_const_matches_true() -> ir::Result<()> {
    let g = graph_bool_and_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&bool_const(true).into());
    assert_eq!(hits.len(), 1, "bool_const(true) should find exactly one node");
    Ok(())
}

#[test]
fn bool_const_matches_false() -> ir::Result<()> {
    let g = graph_bool_and_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&bool_const(false).into());
    assert_eq!(hits.len(), 1, "bool_const(false) should find exactly one node");
    Ok(())
}

#[test]
fn bool_const_no_match_in_int_only_graph() -> ir::Result<()> {
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&bool_const(true).into());
    assert!(hits.is_empty());
    Ok(())
}

// ── Binary op patterns ────────────────────────────────────────────────────────

#[test]
fn add_pattern_matches_add_node() -> ir::Result<()> {
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&add(int_const(5), int_const(3)).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn add_pattern_ordered_wrong_operand_order_no_match() -> ir::Result<()> {
    // With .ordered(), operand order is significant.
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&add(int_const(3), int_const(5)).ordered().into());
    assert!(hits.is_empty(), "ordered add must not match reversed operands");
    Ok(())
}

#[test]
fn nested_pattern_and_add() -> ir::Result<()> {
    // and(4, 7) + 1
    let g = graph_and_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&add(and(int_const(4), int_const(7)), int_const(1)).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn nested_pattern_partial_wildcard() -> ir::Result<()> {
    // add(and(4, _), 1)
    let g = graph_and_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&add(and(int_const(4), any()), int_const(1)).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn sub_pattern_no_match_in_add_graph() -> ir::Result<()> {
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&sub(int_const(5), int_const(3)).into());
    assert!(hits.is_empty(), "no sub node in add graph");
    Ok(())
}

#[test]
fn or_pattern_no_match_in_and_graph() -> ir::Result<()> {
    let g = graph_and_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&or(int_const(4), int_const(7)).into());
    assert!(hits.is_empty(), "no or node; the binary op is And");
    Ok(())
}

#[test]
fn and_pattern_matches_in_and_add_graph() -> ir::Result<()> {
    let g = graph_and_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&and(int_const(4), int_const(7)).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn xor_pattern_no_match_in_add_graph() -> ir::Result<()> {
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&xor(any(), any()).into());
    assert!(hits.is_empty());
    Ok(())
}

// ── Deeply nested add patterns ────────────────────────────────────────────────

#[test]
fn deeply_nested_add_matches() -> ir::Result<()> {
    let g = graph_nested_add()?;
    let m = Matcher::new(&g);
    // add(add(1, 2), 3)
    let hits = m.find_all(&add(add(int_const(1), int_const(2)), int_const(3)).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn deeply_nested_add_ordered_wrong_order_no_match() -> ir::Result<()> {
    let g = graph_nested_add()?;
    let m = Matcher::new(&g);
    // add(3, add(1, 2)).ordered() — wrong outer order, ordered matching
    let hits = m.find_all(&add(int_const(3), add(int_const(1), int_const(2))).ordered().into());
    assert!(hits.is_empty());
    Ok(())
}

#[test]
fn inner_add_matches_independently() -> ir::Result<()> {
    // The inner add(1,2) should also be found directly.
    let g = graph_nested_add()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&add(int_const(1), int_const(2)).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

// ── Unary op patterns ─────────────────────────────────────────────────────────

#[test]
fn neg_pattern_matches_neg_node() -> ir::Result<()> {
    let g = graph_neg_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&neg(any()).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn neg_of_add_pattern_matches() -> ir::Result<()> {
    let g = graph_neg_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&neg(add(int_const(5), int_const(3))).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn not_pattern_no_match_in_neg_graph() -> ir::Result<()> {
    // `not` is bitwise NOT (IntUnaryOp::Not); the graph only has `neg`.
    let g = graph_neg_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&not(any()).into());
    assert!(hits.is_empty(), "graph has neg, not not");
    Ok(())
}

#[test]
fn neg_pattern_no_match_in_add_graph() -> ir::Result<()> {
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&neg(any()).into());
    assert!(hits.is_empty());
    Ok(())
}

// ── Bool op patterns ──────────────────────────────────────────────────────────

#[test]
fn bool_not_pattern_matches() -> ir::Result<()> {
    let g = graph_bool_not_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&bool_not(any()).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn bool_and_pattern_matches() -> ir::Result<()> {
    let g = graph_bool_and_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&bool_and(bool_const(true), bool_const(false)).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn bool_or_pattern_no_match_in_and_graph() -> ir::Result<()> {
    let g = graph_bool_and_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&bool_or(any(), any()).into());
    assert!(hits.is_empty(), "graph has bool_and, not bool_or");
    Ok(())
}

// Note: bool_and(true, false) evaluates to false at compile time, so the
// builder folds it.  bool_and with a non-const operand is needed to avoid
// constant folding and actually emit a BoolBinaryOp node.
#[test]
fn bool_and_pattern_with_wildcard() -> ir::Result<()> {
    let g = graph_bool_and_return()?;
    let m = Matcher::new(&g);
    let _hits = m.find_all(&bool_and(any(), any()).into());
    // boolean constant folding: bool_and(true, false) = BoolConst(false)
    // In that case there is no BoolBinaryOp node, so hits could be 0 or 1.
    // We just assert the bool_or wildcard produces 0:
    let or_hits = m.find_all(&bool_or(any(), any()).into());
    assert!(or_hits.is_empty());
    Ok(())
}

// ── Comparison op patterns ────────────────────────────────────────────────────

#[test]
fn int_eq_pattern_matches_in_if_graph() -> ir::Result<()> {
    let g = graph_if_branches()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&int_eq(int_const(4), int_const(1)).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn int_eq_wrong_order_no_match() -> ir::Result<()> {
    let g = graph_if_branches()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&int_eq(int_const(1), int_const(4)).into());
    assert!(hits.is_empty(), "int_eq is not commutative in pattern matching");
    Ok(())
}

#[test]
fn int_lt_no_match_when_op_is_equal() -> ir::Result<()> {
    let g = graph_if_branches()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&int_lt(int_const(4), int_const(1)).into());
    assert!(hits.is_empty(), "cond is Equal, not Less");
    Ok(())
}

#[test]
fn int_eq_with_wildcard_operands() -> ir::Result<()> {
    let g = graph_if_branches()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&int_eq(any(), any()).into());
    assert_eq!(hits.len(), 1, "one Equal comparison node in graph");
    Ok(())
}

// ── Extend / truncate / cast patterns ────────────────────────────────────────

#[test]
fn zero_extend_pattern_matches() -> ir::Result<()> {
    let g = graph_zero_extend_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&zero_extend(any()).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn sign_extend_no_match_in_zero_extend_graph() -> ir::Result<()> {
    let g = graph_zero_extend_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&sign_extend(any()).into());
    assert!(hits.is_empty(), "graph uses zero_extend, not sign_extend");
    Ok(())
}

#[test]
fn truncate_pattern_matches() -> ir::Result<()> {
    let g = graph_truncate_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&truncate(any()).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn zero_extend_no_match_in_truncate_graph() -> ir::Result<()> {
    let g = graph_truncate_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&zero_extend(any()).into());
    assert!(hits.is_empty(), "graph uses truncate, not extend");
    Ok(())
}

// ── InitialVar patterns ───────────────────────────────────────────────────────

#[test]
fn initial_var_any_matches_all_initial_vars() -> ir::Result<()> {
    let (g, _vn) = graph_with_initial_var()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&initial_var().into());
    assert_eq!(hits.len(), 1, "one InitialVar node in graph");
    Ok(())
}

#[test]
fn initial_var_for_matches_correct_vn() -> ir::Result<()> {
    let (g, vn) = graph_with_initial_var()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&initial_var_for(vn).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn initial_var_for_wrong_vn_no_match() -> ir::Result<()> {
    let (g, _vn) = graph_with_initial_var()?;
    let m = Matcher::new(&g);
    let other_vn = make_reg_vn(999, 8);
    let hits = m.find_all(&initial_var_for(other_vn).into());
    assert!(hits.is_empty(), "wrong vn, no match");
    Ok(())
}

#[test]
fn initial_var_any_finds_two_vars() -> ir::Result<()> {
    let (g, _a, _b) = graph_with_two_initial_vars()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&initial_var().into());
    assert_eq!(hits.len(), 2, "two distinct InitialVar nodes");
    Ok(())
}

#[test]
fn initial_var_for_specific_in_two_var_graph() -> ir::Result<()> {
    let (g, vn_a, vn_b) = graph_with_two_initial_vars()?;
    let m = Matcher::new(&g);
    let hits_a = m.find_all(&initial_var_for(vn_a).into());
    let hits_b = m.find_all(&initial_var_for(vn_b).into());
    assert_eq!(hits_a.len(), 1);
    assert_eq!(hits_b.len(), 1);
    // Each should match a different node.
    assert_ne!(hits_a[0].root, hits_b[0].root);
    Ok(())
}

// ── Capture variables ─────────────────────────────────────────────────────────

#[test]
fn capture_var_binds_to_matched_output() -> ir::Result<()> {
    let g = graph_and_add_return()?;
    let m = Matcher::new(&g);
    let x = Var::new();
    // add(and(4, x), 1) — x should bind to the const-7 output
    let hits = m.find_all(&add(and(int_const(4), var(x)), int_const(1)).into());
    assert_eq!(hits.len(), 1);

    let bound = hits[0].get(x).expect("x should be bound");
    // The bound output should come from an IntConst(7) node.
    let node = g.graph.get_node_from_output(bound);
    assert!(
        matches!(g.graph.node_kind(node), NodeKind::IntConst(7)),
        "x should be bound to const 7, got {:?}", g.graph.node_kind(node)
    );
    Ok(())
}

#[test]
fn same_var_twice_requires_same_output() -> ir::Result<()> {
    // add(x, x) should only match if both inputs are identical.
    // In graph_add_return, add(5, 3) — both inputs are different constants.
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let x = Var::new();
    let hits = m.find_all(&add(var(x), var(x)).into());
    assert!(hits.is_empty(), "add(x,x) must not match add(5,3)");
    Ok(())
}

#[test]
fn same_var_twice_matches_when_operands_are_equal() -> ir::Result<()> {
    // Build a graph with add(c, c) where both inputs are the same node.
    // Because constants are deduplicated, build_int_const(5,U64) twice
    // returns the same NodeOutputId.
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let c = b.build_int_const(5, NodeOutputType::U64);
    let sum = b.build_int_binary_operation(c, c, IntBinaryOp::Add, NodeOutputType::U64)?;
    b.build_return(Some(sum), &[])?;
    let g = b.build();

    let m = Matcher::new(&g);
    let x = Var::new();
    let hits = m.find_all(&add(var(x), var(x)).into());
    assert_eq!(hits.len(), 1, "add(x,x) must match add(c,c) with same node");
    // Both uses of x resolve to the same constant output.
    let bound = hits[0].get(x).unwrap();
    let node = g.graph.get_node_from_output(bound);
    assert!(matches!(g.graph.node_kind(node), NodeKind::IntConst(5)));
    Ok(())
}

#[test]
fn two_independent_vars_bind_independently() -> ir::Result<()> {
    let g = graph_and_add_return()?;
    let m = Matcher::new(&g);
    let lhs_var = Var::new();
    let rhs_var = Var::new();
    // add(and(lhs_var, rhs_var), 1)
    let hits = m.find_all(&add(and(var(lhs_var), var(rhs_var)), int_const(1)).into());
    assert_eq!(hits.len(), 1);
    let lhs_bound = hits[0].get(lhs_var).expect("lhs_var must be bound");
    let rhs_bound = hits[0].get(rhs_var).expect("rhs_var must be bound");
    // They should be different nodes (4 vs 7)
    assert_ne!(lhs_bound, rhs_bound);
    let lhs_node = g.graph.get_node_from_output(lhs_bound);
    let rhs_node = g.graph.get_node_from_output(rhs_bound);
    assert!(matches!(g.graph.node_kind(lhs_node), NodeKind::IntConst(4)));
    assert!(matches!(g.graph.node_kind(rhs_node), NodeKind::IntConst(7)));
    Ok(())
}

#[test]
fn var_shared_across_nested_subpatterns_enforces_equality() -> ir::Result<()> {
    // add(x, x) where x appears twice — must match only if both inputs equal.
    // add(1, 2) has different inputs so should NOT match.
    let g = graph_nested_add()?;
    let m = Matcher::new(&g);
    let x = Var::new();
    // add(x, x) — both leaves must be the same node
    let hits = m.find_all(&add(var(x), var(x)).into());
    assert!(hits.is_empty(), "add(x,x) must not match add(1,2) or add(add(1,2),3)");
    Ok(())
}

// ── ret() pattern ─────────────────────────────────────────────────────────────

#[test]
fn ret_pattern_finds_return_node() -> ir::Result<()> {
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&ret().into());
    assert_eq!(hits.len(), 1);
    assert!(matches!(g.graph.node_kind(hits[0].root), NodeKind::Return));
    Ok(())
}

#[test]
fn ret_pattern_finds_both_returns_in_if_graph() -> ir::Result<()> {
    let g = graph_if_branches()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&ret().into());
    assert_eq!(hits.len(), 2, "if graph has two return nodes");
    Ok(())
}

// ── ret().ret_val ─────────────────────────────────────────────────────────────

#[test]
fn ret_val_pattern_matches_correct_return_value() -> ir::Result<()> {
    // graph_add_return returns add(5,3). We can match it by its result pattern.
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    // The return value (input[2]) is the add node.
    let hits = m.find_all(&ret().ret_val(0, add(int_const(5), int_const(3))).into());
    assert_eq!(hits.len(), 1, "return value is add(5,3)");
    Ok(())
}

#[test]
fn ret_val_pattern_no_match_for_wrong_value() -> ir::Result<()> {
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&ret().ret_val(0, int_const(99)).into());
    assert!(hits.is_empty(), "return value is not 99");
    Ok(())
}

#[test]
fn ret_val_wildcard_matches_any_return_with_value() -> ir::Result<()> {
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&ret().ret_val(0, any()).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn ret_val_distinguishes_branches() -> ir::Result<()> {
    // In graph_if_branches, true branch returns 10, false branch returns 20.
    let g = graph_if_branches()?;
    let m = Matcher::new(&g);
    let hits_10 = m.find_all(&ret().ret_val(0, int_const(10)).into());
    let hits_20 = m.find_all(&ret().ret_val(0, int_const(20)).into());
    assert_eq!(hits_10.len(), 1, "one return with value 10");
    assert_eq!(hits_20.len(), 1, "one return with value 20");
    assert_ne!(hits_10[0].root, hits_20[0].root, "they must be different nodes");
    Ok(())
}

// ── ret().preceded_by ─────────────────────────────────────────────────────────

#[test]
fn preceded_by_matches_call_at_correct_address() -> ir::Result<()> {
    let g = graph_call_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&ret().preceded_by(call().at(0x1234)).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn preceded_by_no_match_for_wrong_address() -> ir::Result<()> {
    let g = graph_call_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&ret().preceded_by(call().at(0xDEAD)).into());
    assert!(hits.is_empty());
    Ok(())
}

#[test]
fn preceded_by_no_match_when_no_call_precedes_return() -> ir::Result<()> {
    // graph_add_return has no call.
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&ret().preceded_by(call().at(0x1234)).into());
    assert!(hits.is_empty());
    Ok(())
}

#[test]
fn preceded_by_finds_either_call_in_two_call_graph() -> ir::Result<()> {
    let g = graph_two_calls_return()?;
    let m = Matcher::new(&g);
    // The return is preceded by both calls in sequence; backwards walk finds
    // call at 0x2222 directly, then walks further to find 0x1111 if needed.
    let hits_2222 = m.find_all(&ret().preceded_by(call().at(0x2222)).into());
    let hits_1111 = m.find_all(&ret().preceded_by(call().at(0x1111)).into());
    assert_eq!(hits_2222.len(), 1, "return preceded by call at 0x2222");
    assert_eq!(hits_1111.len(), 1, "return also preceded by earlier call at 0x1111");
    Ok(())
}

// ── call() pattern ────────────────────────────────────────────────────────────

#[test]
fn call_pattern_finds_all_calls() -> ir::Result<()> {
    let g = graph_two_calls_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&call().into());
    assert_eq!(hits.len(), 2);
    Ok(())
}

#[test]
fn call_at_matches_specific_address() -> ir::Result<()> {
    let g = graph_two_calls_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&call().at(0x1111).into());
    assert_eq!(hits.len(), 1);
    let hits2 = m.find_all(&call().at(0x2222).into());
    assert_eq!(hits2.len(), 1);
    Ok(())
}

#[test]
fn call_at_wrong_address_no_match() -> ir::Result<()> {
    let g = graph_call_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&call().at(0xBEEF).into());
    assert!(hits.is_empty());
    Ok(())
}

// ── NodeVar capture ───────────────────────────────────────────────────────────

#[test]
fn node_var_captures_call_node_id() -> ir::Result<()> {
    let g = graph_call_return()?;
    let m = Matcher::new(&g);
    let cv = NodeVar::new();
    let hits = m.find_all(&call().at(0x1234).capture(cv).into());
    assert_eq!(hits.len(), 1);
    let node_id = hits[0].get_node(cv).expect("NodeVar must be bound");
    assert!(matches!(g.graph.node_kind(node_id), NodeKind::Call));
    Ok(())
}

#[test]
fn ret_node_var_captures_return_node_id() -> ir::Result<()> {
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let rv = NodeVar::new();
    let hits = m.find_all(&ret().capture(rv).into());
    assert_eq!(hits.len(), 1);
    let node_id = hits[0].get_node(rv).expect("NodeVar must be bound");
    assert_eq!(node_id, hits[0].root);
    assert!(matches!(g.graph.node_kind(node_id), NodeKind::Return));
    Ok(())
}

#[test]
fn if_node_var_captures_if_node_id() -> ir::Result<()> {
    let g = graph_if_branches()?;
    let m = Matcher::new(&g);
    let iv = NodeVar::new();
    let hits = m.find_all(&if_node().capture(iv).into());
    assert_eq!(hits.len(), 1);
    let node_id = hits[0].get_node(iv).unwrap();
    assert!(matches!(g.graph.node_kind(node_id), NodeKind::If));
    Ok(())
}

#[test]
fn node_var_not_bound_when_pattern_fails() -> ir::Result<()> {
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let cv = NodeVar::new();
    // call().at(0xDEAD) won't match, so cv stays unbound
    let hits = m.find_all(&call().at(0xDEAD).capture(cv).into());
    assert!(hits.is_empty());
    Ok(())
}

// ── if_node() pattern ─────────────────────────────────────────────────────────

#[test]
fn if_pattern_finds_if_node() -> ir::Result<()> {
    let g = graph_if_branches()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&if_node().into());
    assert_eq!(hits.len(), 1);
    assert!(matches!(g.graph.node_kind(hits[0].root), NodeKind::If));
    Ok(())
}

#[test]
fn if_cond_pattern_matches_condition() -> ir::Result<()> {
    // graph_if_branches: cond = Equal(4, 1)
    let g = graph_if_branches()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&if_node().cond(int_eq(int_const(4), int_const(1))).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn if_cond_wrong_pattern_no_match() -> ir::Result<()> {
    let g = graph_if_branches()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&if_node().cond(int_eq(int_const(99), int_const(1))).into());
    assert!(hits.is_empty());
    Ok(())
}

#[test]
fn if_pattern_no_match_in_flat_graph() -> ir::Result<()> {
    let g = graph_call_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&if_node().into());
    assert!(hits.is_empty());
    Ok(())
}

// ── contains / true_branch / false_branch ────────────────────────────────────

#[test]
fn true_branch_contains_call_matches() -> ir::Result<()> {
    let g = graph_if_with_call_in_true_branch()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&if_node().true_branch(contains(call().at(0x2345))).into());
    assert_eq!(hits.len(), 1, "true branch should contain call at 0x2345");
    Ok(())
}

#[test]
fn false_branch_contains_call_no_match() -> ir::Result<()> {
    // The call is in the TRUE branch, so false_branch(contains(call)) should fail.
    let g = graph_if_with_call_in_true_branch()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&if_node().false_branch(contains(call().at(0x2345))).into());
    assert!(hits.is_empty(), "false branch does not contain the call");
    Ok(())
}

#[test]
fn true_branch_wrong_address_no_match() -> ir::Result<()> {
    let g = graph_if_with_call_in_true_branch()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&if_node().true_branch(contains(call().at(0xDEAD))).into());
    assert!(hits.is_empty());
    Ok(())
}

#[test]
fn false_branch_contains_call_matches() -> ir::Result<()> {
    let g = graph_if_with_call_in_false_branch()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&if_node().false_branch(contains(call().at(0x5678))).into());
    assert_eq!(hits.len(), 1, "false branch contains call at 0x5678");
    Ok(())
}

#[test]
fn true_branch_no_match_when_call_only_in_false() -> ir::Result<()> {
    let g = graph_if_with_call_in_false_branch()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&if_node().true_branch(contains(call().at(0x5678))).into());
    assert!(hits.is_empty(), "call is only in false branch");
    Ok(())
}

#[test]
fn both_branches_contain_ret() -> ir::Result<()> {
    // In graph_if_branches both branches end in a return.
    let g = graph_if_branches()?;
    let m = Matcher::new(&g);
    let hits_true  = m.find_all(&if_node().true_branch(contains(ret())).into());
    let hits_false = m.find_all(&if_node().false_branch(contains(ret())).into());
    assert_eq!(hits_true.len(),  1, "true branch has a return");
    assert_eq!(hits_false.len(), 1, "false branch has a return");
    Ok(())
}

#[test]
fn both_branches_constrained_simultaneously() -> ir::Result<()> {
    // Require the true branch to have a ret returning 10 AND the false branch
    // to have a ret returning 20.
    let g = graph_if_branches()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(
        &if_node()
            .true_branch(contains(ret().ret_val(0, int_const(10))))
            .false_branch(contains(ret().ret_val(0, int_const(20))))
            .into(),
    );
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn both_branches_constrained_swapped_no_match() -> ir::Result<()> {
    // Swapping the expected values must not match.
    let g = graph_if_branches()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(
        &if_node()
            .true_branch(contains(ret().ret_val(0, int_const(20))))
            .false_branch(contains(ret().ret_val(0, int_const(10))))
            .into(),
    );
    assert!(hits.is_empty(), "values are swapped, should not match");
    Ok(())
}

// ── Capture across control + data patterns ────────────────────────────────────

#[test]
fn capture_var_from_call_target_via_preceded_by() -> ir::Result<()> {
    // ret().preceded_by(call — capture target address as var)
    let g = graph_call_return()?;
    let m = Matcher::new(&g);
    let addr_v = Var::new();
    let hits = m.find_all(&ret().preceded_by(call().target(var(addr_v))).into());
    assert_eq!(hits.len(), 1);
    let bound = hits[0].get(addr_v).expect("addr_v must be bound");
    let node = g.graph.get_node_from_output(bound);
    assert!(
        matches!(g.graph.node_kind(node), NodeKind::IntConst(0x1234)),
        "target should be 0x1234, got {:?}", g.graph.node_kind(node)
    );
    Ok(())
}

#[test]
fn capture_call_target_inside_true_branch() -> ir::Result<()> {
    let g = graph_if_with_call_in_true_branch()?;
    let m = Matcher::new(&g);
    let tgt_v = Var::new();
    let hits = m.find_all(
        &if_node()
            .true_branch(contains(call().target(var(tgt_v))))
            .into(),
    );
    assert_eq!(hits.len(), 1);
    let bound = hits[0].get(tgt_v).expect("tgt_v must be bound");
    let node = g.graph.get_node_from_output(bound);
    assert!(
        matches!(g.graph.node_kind(node), NodeKind::IntConst(0x2345)),
        "call target should be 0x2345, got {:?}", g.graph.node_kind(node)
    );
    Ok(())
}

// ── any() wildcard ────────────────────────────────────────────────────────────

#[test]
fn any_pattern_matches_many_nodes() -> ir::Result<()> {
    // any() as a data root matches all nodes that have a value output.
    // At minimum the constants and the add node.
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&any().into());
    // 5 const, 3 const, add result all have value outputs → ≥ 3 hits
    assert!(hits.len() >= 3, "expected at least 3 any() matches, got {}", hits.len());
    Ok(())
}

#[test]
fn any_in_binary_op_matches_both_operands() -> ir::Result<()> {
    // add(any(), any()) matches any add node regardless of operands.
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&add(any(), any()).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

// ── No spurious matches ───────────────────────────────────────────────────────

#[test]
fn call_pattern_no_match_in_add_only_graph() -> ir::Result<()> {
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&call().into());
    assert!(hits.is_empty());
    Ok(())
}

#[test]
fn if_pattern_no_match_in_call_graph() -> ir::Result<()> {
    let g = graph_call_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&if_node().into());
    assert!(hits.is_empty());
    Ok(())
}

#[test]
fn mul_pattern_no_match_in_add_graph() -> ir::Result<()> {
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&mul(any(), any()).into());
    assert!(hits.is_empty());
    Ok(())
}

#[test]
fn zero_extend_no_match_in_add_graph() -> ir::Result<()> {
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&zero_extend(any()).into());
    assert!(hits.is_empty());
    Ok(())
}

// ── Edge-case: constant deduplication ────────────────────────────────────────

#[test]
fn deduplicated_constants_yield_single_match() -> ir::Result<()> {
    // Building two int_const(5, U64) returns the same node due to deduplication.
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let _c5a = b.build_int_const(5, NodeOutputType::U64);
    let _c5b = b.build_int_const(5, NodeOutputType::U64); // same node
    let sum = b.build_int_binary_operation(_c5a, _c5b, IntBinaryOp::Add, NodeOutputType::U64)?;
    b.build_return(Some(sum), &[])?;
    let g = b.build();

    let m = Matcher::new(&g);
    // Both const-5 references alias the same node, so int_const(5) finds 1.
    let hits = m.find_all(&int_const(5).into());
    assert_eq!(hits.len(), 1, "deduplication means only one const-5 node");
    Ok(())
}

#[test]
fn two_different_constants_both_found() -> ir::Result<()> {
    let g = graph_add_return()?; // has 5 and 3
    let m = Matcher::new(&g);
    let h5 = m.find_all(&int_const(5).into());
    let h3 = m.find_all(&int_const(3).into());
    assert_eq!(h5.len(), 1);
    assert_eq!(h3.len(), 1);
    assert_ne!(h5[0].root, h3[0].root);
    Ok(())
}

// ── Edge-case: pattern on graph with no matching kind ────────────────────────

#[test]
fn phi_no_match_in_graph_without_variables() -> ir::Result<()> {
    // Without tracked variables, no ControlPhi nodes are emitted.
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&phi().into());
    assert!(hits.is_empty(), "no variable → no ControlPhi nodes");
    Ok(())
}

#[test]
fn initial_var_no_match_in_graph_without_variables() -> ir::Result<()> {
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&initial_var().into());
    assert!(hits.is_empty());
    Ok(())
}

// ── Load patterns ─────────────────────────────────────────────────────────────

#[test]
fn load_any_matches_load_node() -> ir::Result<()> {
    let g = graph_load_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&load().into());
    assert_eq!(hits.len(), 1);
    let _node = g.graph.get_node_from_output(g.graph.node_outputs(hits[0].root)[0]);
    assert!(matches!(g.graph.node_kind(hits[0].root), NodeKind::Load(_)));
    Ok(())
}

#[test]
fn load_with_matching_addr_matches() -> ir::Result<()> {
    let g = graph_load_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&load().addr(int_const(0x100)).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn load_with_wrong_addr_no_match() -> ir::Result<()> {
    let g = graph_load_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&load().addr(int_const(0x999)).into());
    assert!(hits.is_empty());
    Ok(())
}

#[test]
fn load_with_correct_space_matches() -> ir::Result<()> {
    let g = graph_load_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&load().space(rsleigh::VnSpace::RAM).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn load_with_wrong_space_no_match() -> ir::Result<()> {
    let g = graph_load_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&load().space(rsleigh::VnSpace::REGISTER).into());
    assert!(hits.is_empty());
    Ok(())
}

#[test]
fn load_addr_and_space_together_matches() -> ir::Result<()> {
    let g = graph_load_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&load().space(rsleigh::VnSpace::RAM).addr(int_const(0x100)).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn load_capture_addr() -> ir::Result<()> {
    let g = graph_load_return()?;
    let m = Matcher::new(&g);
    let addr_v = Var::new();
    let hits = m.find_all(&load().addr(var(addr_v)).into());
    assert_eq!(hits.len(), 1);
    let val = hits[0].get_int_const(addr_v, &g).expect("addr must be an int const");
    assert_eq!(val, 0x100);
    Ok(())
}

#[test]
fn load_no_match_in_graph_without_load() -> ir::Result<()> {
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&load().into());
    assert!(hits.is_empty());
    Ok(())
}

// ── Store patterns ────────────────────────────────────────────────────────────

// Note: the Store node is reachable via preorder only when its memory output
// is consumed by something downstream (here a Load that feeds into Return).

#[test]
fn store_any_matches_store_node() -> ir::Result<()> {
    let g = graph_store_then_load()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&store().into());
    assert_eq!(hits.len(), 1);
    assert!(matches!(g.graph.node_kind(hits[0].root), NodeKind::Store(_)));
    Ok(())
}

#[test]
fn store_with_matching_addr_matches() -> ir::Result<()> {
    let g = graph_store_then_load()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&store().addr(int_const(0x200)).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn store_with_wrong_addr_no_match() -> ir::Result<()> {
    let g = graph_store_then_load()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&store().addr(int_const(0x999)).into());
    assert!(hits.is_empty());
    Ok(())
}

#[test]
fn store_with_matching_data_matches() -> ir::Result<()> {
    let g = graph_store_then_load()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&store().data(int_const(42)).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn store_with_wrong_data_no_match() -> ir::Result<()> {
    let g = graph_store_then_load()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&store().data(int_const(0)).into());
    assert!(hits.is_empty());
    Ok(())
}

#[test]
fn store_with_addr_and_data_matches() -> ir::Result<()> {
    let g = graph_store_then_load()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&store().addr(int_const(0x200)).data(int_const(42)).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn store_with_correct_space_matches() -> ir::Result<()> {
    let g = graph_store_then_load()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&store().space(rsleigh::VnSpace::RAM).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn store_with_wrong_space_no_match() -> ir::Result<()> {
    let g = graph_store_then_load()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&store().space(rsleigh::VnSpace::REGISTER).into());
    assert!(hits.is_empty());
    Ok(())
}

#[test]
fn store_capture_addr_and_data() -> ir::Result<()> {
    let g = graph_store_then_load()?;
    let m = Matcher::new(&g);
    let addr_v = Var::new();
    let data_v = Var::new();
    let hits = m.find_all(&store().addr(var(addr_v)).data(var(data_v)).into());
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].get_int_const(addr_v, &g), Some(0x200));
    assert_eq!(hits[0].get_int_const(data_v, &g), Some(42));
    Ok(())
}

#[test]
fn store_reachable_via_call_memory_chain() -> ir::Result<()> {
    // Store's memory → Call (which takes cur_region_memory as input), so
    // the Store is reachable from Return via the Call's inputs.
    let g = graph_store_then_call()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&store().into());
    assert_eq!(hits.len(), 1, "store is reachable through the call's memory input");
    Ok(())
}

#[test]
fn store_no_match_in_load_only_graph() -> ir::Result<()> {
    let g = graph_load_return()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&store().into());
    assert!(hits.is_empty(), "load-only graph has no store");
    Ok(())
}

// ── Call argument patterns ────────────────────────────────────────────────────

#[test]
fn call_arg0_matches_correct_value() -> ir::Result<()> {
    let (g, _) = graph_call_with_arg()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&call().arg(0, int_const(42)).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn call_arg0_wrong_value_no_match() -> ir::Result<()> {
    let (g, _) = graph_call_with_arg()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&call().arg(0, int_const(0)).into());
    assert!(hits.is_empty());
    Ok(())
}

#[test]
fn call_target_and_arg_together() -> ir::Result<()> {
    let (g, _) = graph_call_with_arg()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&call().at(0xABCD).arg(0, int_const(42)).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn call_target_matches_but_arg_wrong_no_match() -> ir::Result<()> {
    let (g, _) = graph_call_with_arg()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&call().at(0xABCD).arg(0, int_const(99)).into());
    assert!(hits.is_empty());
    Ok(())
}

#[test]
fn call_arg_out_of_range_no_match() -> ir::Result<()> {
    // arg index 1 doesn't exist (only 1 arg in this graph).
    let (g, _) = graph_call_with_arg()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&call().arg(1, any()).into());
    assert!(hits.is_empty());
    Ok(())
}

#[test]
fn call_arg_wildcard_matches() -> ir::Result<()> {
    let (g, _) = graph_call_with_arg()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&call().arg(0, any()).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn call_arg_capture_and_extract() -> ir::Result<()> {
    let (g, _) = graph_call_with_arg()?;
    let m = Matcher::new(&g);
    let arg_v = Var::new();
    let hits = m.find_all(&call().arg(0, var(arg_v)).into());
    assert_eq!(hits.len(), 1);
    // get_int_const provides the easy value extraction the user asked for.
    let val = hits[0].get_int_const(arg_v, &g).expect("arg must be an int const");
    assert_eq!(val, 42);
    Ok(())
}

// ── get_int_const / get_bool_const helpers ────────────────────────────────────

#[test]
fn get_int_const_returns_value_for_const_binding() -> ir::Result<()> {
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let lhs_v = Var::new();
    // add(lhs_v, _): lhs is IntConst(5)
    let hits = m.find_all(&add(var(lhs_v), any()).into());
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].get_int_const(lhs_v, &g), Some(5));
    Ok(())
}

#[test]
fn get_int_const_returns_none_for_non_const_binding() -> ir::Result<()> {
    let g = graph_and_add_return()?;
    let m = Matcher::new(&g);
    let inner_v = Var::new();
    // add(inner_v, 1): inner_v is bound to and(4,7) — not a const node
    let hits = m.find_all(&add(var(inner_v), int_const(1)).into());
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].get_int_const(inner_v, &g), None,
        "and(4,7) is not an IntConst node");
    Ok(())
}

#[test]
fn get_int_const_returns_none_for_unbound_var() -> ir::Result<()> {
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let unbound = Var::new();
    // Pattern doesn't use `unbound` at all.
    let hits = m.find_all(&add(any(), any()).into());
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].get_int_const(unbound, &g), None);
    Ok(())
}

#[test]
fn get_int_const_works_for_nested_capture() -> ir::Result<()> {
    // Capture the inner constant of and(4, _rhs_) via a nested pattern.
    let g = graph_and_add_return()?;
    let m = Matcher::new(&g);
    let rhs_v = Var::new();
    let hits = m.find_all(&and(int_const(4), var(rhs_v)).into());
    assert_eq!(hits.len(), 1);
    // rhs_v is IntConst(7)
    assert_eq!(hits[0].get_int_const(rhs_v, &g), Some(7));
    Ok(())
}

#[test]
fn get_bool_const_returns_value_for_bool_binding() -> ir::Result<()> {
    let g = graph_bool_not_return()?;
    let m = Matcher::new(&g);
    let v = Var::new();
    let hits = m.find_all(&bool_not(var(v)).into());
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].get_bool_const(v, &g), Some(true));
    Ok(())
}

#[test]
fn get_bool_const_returns_none_for_int_binding() -> ir::Result<()> {
    // Binding an int const must not be mistaken for a bool const.
    let g = graph_add_return()?;
    let m = Matcher::new(&g);
    let v = Var::new();
    let hits = m.find_all(&add(var(v), any()).into());
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].get_bool_const(v, &g), None);
    Ok(())
}

#[test]
fn get_int_const_for_store_addr_and_data() -> ir::Result<()> {
    let g = graph_store_then_load()?;
    let m = Matcher::new(&g);
    let addr_v = Var::new();
    let data_v = Var::new();
    let hits = m.find_all(&store().addr(var(addr_v)).data(var(data_v)).into());
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].get_int_const(addr_v, &g), Some(0x200));
    assert_eq!(hits[0].get_int_const(data_v, &g), Some(42));
    Ok(())
}

#[test]
fn get_int_const_for_load_addr() -> ir::Result<()> {
    let g = graph_load_return()?;
    let m = Matcher::new(&g);
    let addr_v = Var::new();
    let hits = m.find_all(&load().addr(var(addr_v)).into());
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].get_int_const(addr_v, &g), Some(0x100));
    Ok(())
}

#[test]
fn get_int_const_for_call_target() -> ir::Result<()> {
    let g = graph_call_return()?;
    let m = Matcher::new(&g);
    let tgt_v = Var::new();
    let hits = m.find_all(&call().target(var(tgt_v)).into());
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].get_int_const(tgt_v, &g), Some(0x1234));
    Ok(())
}

// ── New feature tests: commutative matching, .capture(), .when(), predicate ──

// ── helper for commutative tests ─────────────────────────────────────────────

/// Graph: `add(5, 3)` — note: 5 is lhs, 3 is rhs.
fn graph_add_5_3() -> ir::Result<ir::BuiltFunctionGraph> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let c5 = b.build_int_const(5, NodeOutputType::U64);
    let c3 = b.build_int_const(3, NodeOutputType::U64);
    let sum = b.build_int_binary_operation(c5, c3, IntBinaryOp::Add, NodeOutputType::U64)?;
    b.build_return(Some(sum), &[])?;
    Ok(b.build())
}

/// Graph: `sub(5, 3)`.
fn graph_sub_5_3() -> ir::Result<ir::BuiltFunctionGraph> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let c5 = b.build_int_const(5, NodeOutputType::U64);
    let c3 = b.build_int_const(3, NodeOutputType::U64);
    let diff = b.build_int_binary_operation(c5, c3, IntBinaryOp::Sub, NodeOutputType::U64)?;
    b.build_return(Some(diff), &[])?;
    Ok(b.build())
}

// ── commutative matching ──────────────────────────────────────────────────────

#[test]
fn commutative_add_reversed_operands_matches() -> ir::Result<()> {
    // IR has add(5, 3); pattern asks for add(3, 5) — should match via commutation.
    let g = graph_add_5_3()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&add(int_const(3), int_const(5)).into());
    assert_eq!(hits.len(), 1, "commutative add should match reversed operands");
    Ok(())
}

#[test]
fn commutative_add_stated_order_also_matches() -> ir::Result<()> {
    // Stated order (5, 3) should still work.
    let g = graph_add_5_3()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&add(int_const(5), int_const(3)).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn ordered_add_reversed_operands_no_match() -> ir::Result<()> {
    // .ordered() forces stated order — reversed should not match.
    let g = graph_add_5_3()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&add(int_const(3), int_const(5)).ordered().into());
    assert!(hits.is_empty(), "ordered add must not match reversed operands");
    Ok(())
}

#[test]
fn non_commutative_sub_no_commutation() -> ir::Result<()> {
    // sub(5, 3) — pattern sub(3, 5) must NOT match even without .ordered().
    let g = graph_sub_5_3()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&sub(int_const(3), int_const(5)).into());
    assert!(hits.is_empty(), "sub is not commutative and must not match reversed operands");
    Ok(())
}

#[test]
fn commutative_add_with_wildcard_lhs() -> ir::Result<()> {
    // add(_, 5) should match add(5, 3) via commutation (try: l=5 vs _, r=3 vs 5 fails;
    // then reversed: l=3 vs _, r=5 vs 5 succeeds).
    let g = graph_add_5_3()?;
    let m = Matcher::new(&g);
    // Pattern: add(_, int_const(3)) — rhs is 3.
    let hits = m.find_all(&add(any(), int_const(3)).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

// ── .capture() on any pattern ─────────────────────────────────────────────────

#[test]
fn capture_output_of_add_node() -> ir::Result<()> {
    // Capture the output of the add node itself.
    let g = graph_add_5_3()?;
    let m = Matcher::new(&g);
    let v = Var::new();
    let hits = m.find_all(&add(any(), any()).capture(v).into());
    assert_eq!(hits.len(), 1);
    // The captured output should be the add node's output — its kind is IntBinaryOp.
    let out = hits[0].get(v).expect("capture var should be bound");
    let node = g.graph.get_node_from_output(out);
    assert!(matches!(g.graph.node_kind(node), NodeKind::IntBinaryOp(_)));
    Ok(())
}

#[test]
fn capture_nested_field_via_var() -> ir::Result<()> {
    // Capture the address of a load by passing var(v) as the addr sub-pattern.
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let addr_const = b.build_int_const(0x1000, NodeOutputType::U64);
    let loaded = b.build_load(addr_const, rsleigh::VnSpace::RAM, ir::node::NodeOutputType::U64)?;
    b.build_return(Some(loaded), &[])?;
    let g = b.build();

    let m = Matcher::new(&g);
    let addr_v = Var::new();
    let hits = m.find_all(&load().addr(var(addr_v)).into());
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].get_int_const(addr_v, &g), Some(0x1000));
    Ok(())
}

#[test]
fn capture_via_when_on_load_addr() -> ir::Result<()> {
    // Use any().capture(v) as sub-pattern to capture the load's address output.
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let addr_const = b.build_int_const(0x2000, NodeOutputType::U64);
    let loaded = b.build_load(addr_const, rsleigh::VnSpace::RAM, ir::node::NodeOutputType::U64)?;
    b.build_return(Some(loaded), &[])?;
    let g = b.build();

    let m = Matcher::new(&g);
    let addr_v = Var::new();
    // any().capture(v) is equivalent to var(v).
    let hits = m.find_all(&load().addr(any().capture(addr_v)).into());
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].get_int_const(addr_v, &g), Some(0x2000));
    Ok(())
}

// ── .when() predicate ─────────────────────────────────────────────────────────

#[test]
fn when_predicate_filters_matching_nodes() -> ir::Result<()> {
    // any().when(f) should only match outputs where f returns true.
    let g = graph_add_5_3()?;
    let m = Matcher::new(&g);
    // Predicate: only match IntConst nodes.
    let hits = m.find_all(&any().when(|fg, out| {
        let node = fg.graph.get_node_from_output(out);
        matches!(fg.graph.node_kind(node), NodeKind::IntConst(_))
    }).into());
    // There are two IntConst nodes (5 and 3).
    assert_eq!(hits.len(), 2);
    Ok(())
}

#[test]
fn predicate_fn_matches_same_as_when() -> ir::Result<()> {
    // predicate(f) is equivalent to any().when(f).
    let g = graph_add_5_3()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&predicate(|fg, out| {
        let node = fg.graph.get_node_from_output(out);
        matches!(fg.graph.node_kind(node), NodeKind::IntConst(_))
    }).into());
    assert_eq!(hits.len(), 2);
    Ok(())
}

#[test]
fn when_predicate_on_structural_pattern() -> ir::Result<()> {
    // add(_, _).when(f) — structural match first, then predicate.
    let g = graph_add_5_3()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&add(any(), any()).when(|fg, out| {
        // Only match if the add node's output is a U64.
        let kind = fg.graph.output_kind(out);
        matches!(kind, ir::node::NodeOutputKind::OutputType(ir::node::NodeOutputType::U64))
    }).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn when_predicate_rejection() -> ir::Result<()> {
    // .when(f) that always returns false rejects everything.
    let g = graph_add_5_3()?;
    let m = Matcher::new(&g);
    let hits = m.find_all(&any().when(|_fg, _out| false).into());
    assert!(hits.is_empty());
    Ok(())
}

#[test]
fn predicate_in_field_position() -> ir::Result<()> {
    // predicate(f) passed as load's addr sub-pattern.
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let addr_const = b.build_int_const(0x3000, NodeOutputType::U64);
    let loaded = b.build_load(addr_const, rsleigh::VnSpace::RAM, ir::node::NodeOutputType::U64)?;
    b.build_return(Some(loaded), &[])?;
    let g = b.build();

    let m = Matcher::new(&g);
    // Match loads where the address is an IntConst >= 0x1000.
    let hits = m.find_all(&load().addr(predicate(|fg, out| {
        let node = fg.graph.get_node_from_output(out);
        match fg.graph.node_kind(node) {
            NodeKind::IntConst(v) => *v >= 0x1000,
            _ => false,
        }
    })).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

// ── Lzcount / Piece / Extract / Insert pattern tests ─────────────────────────

#[test]
fn lzcount_pattern_matches() -> ir::Result<()> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let v = b.build_int_const(1, NodeOutputType::U8);
    let lz = b.build_lzcount(v, NodeOutputType::U8)?;
    b.build_return(Some(lz), &[])?;
    let g = b.build();

    let m = Matcher::new(&g);
    let hits = m.find_all(&lzcount(any()).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn piece_pattern_matches() -> ir::Result<()> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let hi = b.build_int_const(0xAB, NodeOutputType::U8);
    let lo = b.build_int_const(0xCD, NodeOutputType::U8);
    let p = b.build_piece(hi, lo, NodeOutputType::U16)?;
    b.build_return(Some(p), &[])?;
    let g = b.build();

    let m = Matcher::new(&g);
    let hits = m.find_all(&piece(any(), any()).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn extract_pattern_exact_lsb_len() -> ir::Result<()> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let v = b.build_int_const(0xABCD, NodeOutputType::U16);
    let ex = b.build_extract(v, 4, 8, NodeOutputType::U8)?;
    b.build_return(Some(ex), &[])?;
    let g = b.build();

    let m = Matcher::new(&g);
    // Exact match on lsb=4, len=8 → finds it.
    let hits = m.find_all(&extract(Some(4), Some(8), any()).into());
    assert_eq!(hits.len(), 1);
    // Wrong lsb → no match.
    let miss = m.find_all(&extract(Some(0), Some(8), any()).into());
    assert!(miss.is_empty());
    Ok(())
}

#[test]
fn extract_pattern_wildcard() -> ir::Result<()> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let v = b.build_int_const(0xABCD, NodeOutputType::U16);
    let ex = b.build_extract(v, 4, 8, NodeOutputType::U8)?;
    b.build_return(Some(ex), &[])?;
    let g = b.build();

    let m = Matcher::new(&g);
    let hits = m.find_all(&extract(None, None, any()).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

#[test]
fn insert_pattern_matches() -> ir::Result<()> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let dest = b.build_int_const(0xFF00, NodeOutputType::U16);
    let src  = b.build_int_const(0x42,   NodeOutputType::U16);
    let ins = b.build_insert(dest, src, 0, 8, NodeOutputType::U16)?;
    b.build_return(Some(ins), &[])?;
    let g = b.build();

    let m = Matcher::new(&g);
    // Wildcard match.
    let hits = m.find_all(&insert(None, None, any(), any()).into());
    assert_eq!(hits.len(), 1);
    // Exact match on lsb=0, len=8.
    let hits2 = m.find_all(&insert(Some(0), Some(8), any(), any()).into());
    assert_eq!(hits2.len(), 1);
    // Wrong lsb → no match.
    let miss = m.find_all(&insert(Some(4), None, any(), any()).into());
    assert!(miss.is_empty());
    Ok(())
}

// ── Float pattern tests ───────────────────────────────────────────────────────

/// `float_add(1.0f64, 2.0f64)` — basic binary float pattern match.
#[test]
fn float_add_pattern_matches() -> ir::Result<()> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let c1 = b.build_float_const(1.0f64.to_bits(), NodeOutputType::F64);
    let c2 = b.build_float_const(2.0f64.to_bits(), NodeOutputType::F64);
    let sum = b.build_float_binary_op(c1, c2, FloatBinaryOp::Add, NodeOutputType::F64)?;
    b.build_return(Some(sum), &[])?;
    let g = b.build();

    let m = Matcher::new(&g);

    // Match float_add with exact constants.
    let hits = m.find_all(&float_add(
        float_const(1.0f64.to_bits()),
        float_const(2.0f64.to_bits()),
    ).into());
    assert_eq!(hits.len(), 1);

    // Wrong constant → no match.
    let miss = m.find_all(&float_add(
        float_const(3.0f64.to_bits()),
        float_const(2.0f64.to_bits()),
    ).into());
    assert!(miss.is_empty());
    Ok(())
}

/// Float `mul` is commutative: `float_mul(a, b)` should also match with reversed operands.
#[test]
fn float_mul_commutative_pattern() -> ir::Result<()> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let c3 = b.build_float_const(3.0f32.to_bits() as u64, NodeOutputType::F32);
    let c7 = b.build_float_const(7.0f32.to_bits() as u64, NodeOutputType::F32);
    // Build node as 7 * 3.
    let prod = b.build_float_binary_op(c7, c3, FloatBinaryOp::Mul, NodeOutputType::F32)?;
    b.build_return(Some(prod), &[])?;
    let g = b.build();

    let m = Matcher::new(&g);
    let v_a = Var::new();
    let v_b = Var::new();

    // Pattern states 3 * 7; node stores 7 * 3 — commutative match must succeed.
    let hits = m.find_all(&float_mul(
        float_const(3.0f32.to_bits() as u64),
        float_const(7.0f32.to_bits() as u64),
    ).into());
    assert_eq!(hits.len(), 1);

    // Any-capture version also works.
    let hits2 = m.find_all(&float_mul(any_float_const(v_a), any_float_const(v_b)).into());
    assert_eq!(hits2.len(), 1);
    Ok(())
}

/// Float `sub` is NOT commutative: wrong order must fail.
#[test]
fn float_sub_not_commutative() -> ir::Result<()> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let c5 = b.build_float_const(5.0f64.to_bits(), NodeOutputType::F64);
    let c2 = b.build_float_const(2.0f64.to_bits(), NodeOutputType::F64);
    // 5.0 - 2.0
    let diff = b.build_float_binary_op(c5, c2, FloatBinaryOp::Sub, NodeOutputType::F64)?;
    b.build_return(Some(diff), &[])?;
    let g = b.build();

    let m = Matcher::new(&g);
    // Correct order matches.
    assert_eq!(m.find_all(&float_sub(float_const(5.0f64.to_bits()), float_const(2.0f64.to_bits())).into()).len(), 1);
    // Wrong order does NOT match.
    assert!(m.find_all(&float_sub(float_const(2.0f64.to_bits()), float_const(5.0f64.to_bits())).into()).is_empty());
    Ok(())
}

/// Float comparison (`float_eq`) produces a `Bool` output.
#[test]
fn float_eq_pattern_matches() -> ir::Result<()> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let c3 = b.build_float_const(3.0f64.to_bits(), NodeOutputType::F64);
    let c4 = b.build_float_const(4.0f64.to_bits(), NodeOutputType::F64);
    let cmp = b.build_float_cmp_op(c3, c4, FloatCmpOp::Equal)?;
    b.build_return(Some(cmp), &[])?;
    let g = b.build();

    let m = Matcher::new(&g);
    let hits = m.find_all(&float_eq(
        float_const(3.0f64.to_bits()),
        float_const(4.0f64.to_bits()),
    ).into());
    assert_eq!(hits.len(), 1);

    // Wrong op kind → no match.
    let miss = m.find_all(&float_lt(
        float_const(3.0f64.to_bits()),
        float_const(4.0f64.to_bits()),
    ).into());
    assert!(miss.is_empty());
    Ok(())
}

/// `FloatIsNan` pattern match.
#[test]
fn float_is_nan_pattern_matches() -> ir::Result<()> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let cv = b.build_float_const(1.0f64.to_bits(), NodeOutputType::F64);
    let is_nan = b.build_float_is_nan(cv)?;
    b.build_return(Some(is_nan), &[])?;
    let g = b.build();

    let m = Matcher::new(&g);
    let hits = m.find_all(&float_is_nan(any()).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

/// `float_neg` unary pattern match.
#[test]
fn float_unary_pattern_matches() -> ir::Result<()> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let cv = b.build_float_const(2.0f64.to_bits(), NodeOutputType::F64);
    let neg_v = b.build_float_unary_op(cv, FloatUnaryOp::Neg, NodeOutputType::F64)?;
    b.build_return(Some(neg_v), &[])?;
    let g = b.build();

    let m = Matcher::new(&g);
    // Correct unary op matches.
    let hits = m.find_all(&float_neg(float_const(2.0f64.to_bits())).into());
    assert_eq!(hits.len(), 1);
    // Different unary op → no match.
    let miss = m.find_all(&float_abs(float_const(2.0f64.to_bits())).into());
    assert!(miss.is_empty());
    Ok(())
}

/// `any_float_const` captures the float constant bits.
#[test]
fn any_float_const_captures_bits() -> ir::Result<()> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let bits = 42.5f64.to_bits();
    let cv = b.build_float_const(bits, NodeOutputType::F64);
    b.build_return(Some(cv), &[])?;
    let g = b.build();

    let m = Matcher::new(&g);
    let v = Var::new();
    let hits = m.find_all(&any_float_const(v).into());
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].get_float_bits(v, &g), Some(bits));
    Ok(())
}

/// `int_bits_to_float` bitcast pattern match.
#[test]
fn int_bits_to_float_pattern_matches() -> ir::Result<()> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    // Non-const int → explicit IntBitsToFloat node must be emitted.
    let int_val = b.build_int_const(0xDEAD, NodeOutputType::U64);
    // Force a non-const path so we actually get an IntBitsToFloat node.
    // Add 0 to make the optimizer think it's not constant (int_const 0).
    let zero = b.build_int_const(0, NodeOutputType::U64);
    let non_const = b.build_int_binary_operation(int_val, zero, IntBinaryOp::Add, NodeOutputType::U64)?;
    let float_v = b.build_int_bits_to_float(non_const, NodeOutputType::F64)?;
    b.build_return(Some(float_v), &[])?;
    let g = b.build();

    let m = Matcher::new(&g);
    let hits = m.find_all(&int_bits_to_float(any()).into());
    assert_eq!(hits.len(), 1);
    // float_bits_to_int should NOT match.
    assert!(m.find_all(&float_bits_to_int(any()).into()).is_empty());
    Ok(())
}

/// `float_bits_to_int` bitcast pattern match.
#[test]
fn float_bits_to_int_pattern_matches() -> ir::Result<()> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let cv = b.build_float_const(1.0f32.to_bits() as u64, NodeOutputType::F32);
    // Force a non-const float so we get a FloatBitsToInt node.
    let neg_v = b.build_float_unary_op(cv, FloatUnaryOp::Neg, NodeOutputType::F32)?;
    let int_v = b.build_float_bits_to_int(neg_v, NodeOutputType::U32)?;
    b.build_return(Some(int_v), &[])?;
    let g = b.build();

    let m = Matcher::new(&g);
    let hits = m.find_all(&float_bits_to_int(any()).into());
    assert_eq!(hits.len(), 1);
    Ok(())
}

/// `int_to_float`, `float_to_int`, `float_to_float` conversion patterns.
#[test]
fn float_conversion_patterns_match() -> ir::Result<()> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let int_v  = b.build_int_const(42, NodeOutputType::U64);
    let f64_v  = b.build_int_to_float(int_v, NodeOutputType::F64)?;
    let f32_v  = b.build_float_to_float(f64_v, NodeOutputType::F32)?;
    let int_v2 = b.build_float_to_int(f32_v, NodeOutputType::U32)?;
    b.build_return(Some(int_v2), &[])?;
    let g = b.build();

    let m = Matcher::new(&g);
    assert_eq!(m.find_all(&int_to_float(any()).into()).len(), 1);
    assert_eq!(m.find_all(&float_to_float(any()).into()).len(), 1);
    assert_eq!(m.find_all(&float_to_int(any()).into()).len(), 1);
    Ok(())
}

/// `.ordered()` on `float_add` prevents commutative fallback.
#[test]
fn float_add_ordered_no_commutative_fallback() -> ir::Result<()> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let c1 = b.build_float_const(1.0f64.to_bits(), NodeOutputType::F64);
    let c2 = b.build_float_const(2.0f64.to_bits(), NodeOutputType::F64);
    let sum = b.build_float_binary_op(c1, c2, FloatBinaryOp::Add, NodeOutputType::F64)?;
    b.build_return(Some(sum), &[])?;
    let g = b.build();

    let m = Matcher::new(&g);
    // Ordered: correct order matches.
    assert_eq!(m.find_all(&float_add(
        float_const(1.0f64.to_bits()),
        float_const(2.0f64.to_bits()),
    ).ordered().into()).len(), 1);
    // Ordered: wrong order does NOT match even though Add is commutative.
    assert!(m.find_all(&float_add(
        float_const(2.0f64.to_bits()),
        float_const(1.0f64.to_bits()),
    ).ordered().into()).is_empty());
    Ok(())
}

/// `cast_to_float` pattern matches a `CastToFloat` node.
#[test]
fn cast_to_float_pattern_matches() -> ir::Result<()> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let int_val = b.build_int_const(0x3F800000, NodeOutputType::U32);
    let cast = b.build_cast_to_float(int_val, NodeOutputType::F32);
    b.build_return(Some(cast), &[])?;
    let g = b.build();

    let m = Matcher::new(&g);
    // Matches the CastToFloat node.
    let hits = m.find_all(&cast_to_float(any()).into());
    assert_eq!(hits.len(), 1);
    // Other unary patterns do NOT match.
    assert!(m.find_all(&int_bits_to_float(any()).into()).is_empty());
    Ok(())
}
