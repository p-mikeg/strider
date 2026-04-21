/// Integration tests for the `pattern` crate.
///
/// Each test builds a small `BuiltFunctionGraph` using `FunctionBuilder`,
/// then runs `Matcher::find_all` and asserts the expected number of matches
/// and captured bindings.
use ir::{
    BoolBinaryOp, BoolUnaryOp, ExtendOp, FunctionBuilder, IntBinaryOp, IntCmpOp, IntUnaryOp,
    node::{NodeKind, NodeOutputType},
};
use pattern::*;

// ── helpers ───────────────────────────────────────────────────────────────────

/// Constructs a fake `rsleigh::Vn` (register varnode) for tests that need one.
///
/// `off` is used to produce distinct varnodes; `size` is in bytes.
pub(crate) fn make_reg_vn(off: u64, size: u32) -> rsleigh::Vn {
    rsleigh::Vn {
        size,
        addr: rsleigh::VnAddr {
            off,
            space: rsleigh::VnSpace::REGISTER,
        },
    }
}

// ── Graph builders ────────────────────────────────────────────────────────────

/// `add(5, 3)`, then return the result.
/// Shape: Entry → region[add(5,3), return(add_result)]
pub(crate) fn graph_add_return() -> ir::Result<ir::BuiltFunctionGraph> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[], None, 0)?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let c5 = b.build_int_const(5, NodeOutputType::U64);
    let c3 = b.build_int_const(3, NodeOutputType::U64);
    let sum = b.build_int_binary_operation(c5, c3, IntBinaryOp::Add, NodeOutputType::U64)?;
    b.build_return(Some(sum), &[])?;
    b.build()
}

/// `and(4, 7)`, `add(and_result, 1)`, return.
pub(crate) fn graph_and_add_return() -> ir::Result<ir::BuiltFunctionGraph> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[], None, 0)?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let c4 = b.build_int_const(4, NodeOutputType::U64);
    let c7 = b.build_int_const(7, NodeOutputType::U64);
    let c1 = b.build_int_const(1, NodeOutputType::U64);
    let band = b.build_int_binary_operation(c4, c7, IntBinaryOp::And, NodeOutputType::U64)?;
    let sum = b.build_int_binary_operation(band, c1, IntBinaryOp::Add, NodeOutputType::U64)?;
    b.build_return(Some(sum), &[])?;
    b.build()
}

/// Call at target `0x1234`, then return.
pub(crate) fn graph_call_return() -> ir::Result<ir::BuiltFunctionGraph> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[], None, 0)?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let tgt = b.build_uint64_const(0x1234);
    b.build_call(tgt)?;
    b.build_return(None, &[])?;
    b.build()
}

/// Two calls (`0x1111`, `0x2222`) in sequence, then return.
pub(crate) fn graph_two_calls_return() -> ir::Result<ir::BuiltFunctionGraph> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[], None, 0)?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let t1 = b.build_uint64_const(0x1111);
    let t2 = b.build_uint64_const(0x2222);
    b.build_call(t1)?;
    b.build_call(t2)?;
    b.build_return(None, &[])?;
    b.build()
}

/// If (4 == 1):
///   true  branch → return 10
///   false branch → return 20
pub(crate) fn graph_if_branches() -> ir::Result<ir::BuiltFunctionGraph> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[], None, 0)?;
    let entry = b.create_region()?;
    let true_r = b.create_region()?;
    let false_r = b.create_region()?;

    b.set_entry_region(entry)?;

    b.set_region(true_r);
    let c10 = b.build_int_const(10, NodeOutputType::U64);
    b.build_return(Some(c10), &[])?;

    b.set_region(false_r);
    let c20 = b.build_int_const(20, NodeOutputType::U64);
    b.build_return(Some(c20), &[])?;

    b.set_region(entry);
    let c4 = b.build_int_const(4, NodeOutputType::U64);
    let c1 = b.build_int_const(1, NodeOutputType::U64);
    let cond = b.build_int_cmp_operation(c4, c1, IntCmpOp::Equal, NodeOutputType::U64)?;
    b.build_if(cond, true_r, false_r)?;
    b.build()
}

/// If (x == 1):
///   true branch → Call at 0x2345, then return
///   false branch → return
pub(crate) fn graph_if_with_call_in_true_branch() -> ir::Result<ir::BuiltFunctionGraph> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[], None, 0)?;
    let entry = b.create_region()?;
    let true_r = b.create_region()?;
    let false_r = b.create_region()?;

    b.set_entry_region(entry)?;

    b.set_region(true_r);
    let tgt = b.build_uint64_const(0x2345);
    b.build_call(tgt)?;
    b.build_return(None, &[])?;

    b.set_region(false_r);
    b.build_return(None, &[])?;

    b.set_region(entry);
    let c5 = b.build_int_const(5, NodeOutputType::U64);
    let c1 = b.build_int_const(1, NodeOutputType::U64);
    let cond = b.build_int_cmp_operation(c5, c1, IntCmpOp::Equal, NodeOutputType::U64)?;
    b.build_if(cond, true_r, false_r)?;
    b.build()
}

/// If (x == 1):
///   true branch → return
///   false branch → Call at 0x5678, then return
pub(crate) fn graph_if_with_call_in_false_branch() -> ir::Result<ir::BuiltFunctionGraph> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[], None, 0)?;
    let entry = b.create_region()?;
    let true_r = b.create_region()?;
    let false_r = b.create_region()?;

    b.set_entry_region(entry)?;

    b.set_region(true_r);
    b.build_return(None, &[])?;

    b.set_region(false_r);
    let tgt = b.build_uint64_const(0x5678);
    b.build_call(tgt)?;
    b.build_return(None, &[])?;

    b.set_region(entry);
    let c5 = b.build_int_const(5, NodeOutputType::U64);
    let c1 = b.build_int_const(1, NodeOutputType::U64);
    let cond = b.build_int_cmp_operation(c5, c1, IntCmpOp::Equal, NodeOutputType::U64)?;
    b.build_if(cond, true_r, false_r)?;
    b.build()
}

/// neg(add(5, 3)), then return.
pub(crate) fn graph_neg_add_return() -> ir::Result<ir::BuiltFunctionGraph> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[], None, 0)?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let c5 = b.build_int_const(5, NodeOutputType::U64);
    let c3 = b.build_int_const(3, NodeOutputType::U64);
    let sum = b.build_int_binary_operation(c5, c3, IntBinaryOp::Add, NodeOutputType::U64)?;
    let neg = b.build_int_unary_operation(sum, IntUnaryOp::Neg, NodeOutputType::U64)?;
    b.build_return(Some(neg), &[])?;
    b.build()
}

/// not(bool_const(true)), then return.
pub(crate) fn graph_bool_not_return() -> ir::Result<ir::BuiltFunctionGraph> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[], None, 0)?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let t = b.build_boolean_const(true);
    let nt = b.build_boolean_unary_operation(t, BoolUnaryOp::Neg)?;
    // cast to int so we can return it
    let as_int = b.convert_to_int_if_needed(nt, NodeOutputType::U64)?;
    b.build_return(Some(as_int), &[])?;
    b.build()
}

/// bool_and(true, false), then return.
pub(crate) fn graph_bool_and_return() -> ir::Result<ir::BuiltFunctionGraph> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[], None, 0)?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let t = b.build_boolean_const(true);
    let f = b.build_boolean_const(false);
    let ba = b.build_boolean_operation(t, f, BoolBinaryOp::And)?;
    let as_int = b.convert_to_int_if_needed(ba, NodeOutputType::U64)?;
    b.build_return(Some(as_int), &[])?;
    b.build()
}

/// zero_extend(add(1, 2) : U32 → U64), then return.
pub(crate) fn graph_zero_extend_return() -> ir::Result<ir::BuiltFunctionGraph> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[], None, 0)?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let c1 = b.build_int_const(1, NodeOutputType::U32);
    let c2 = b.build_int_const(2, NodeOutputType::U32);
    let sum = b.build_int_binary_operation(c1, c2, IntBinaryOp::Add, NodeOutputType::U32)?;
    let ext = b.extend_if_needed(sum, NodeOutputType::U64, ExtendOp::ZeroExtend)?;
    b.build_return(Some(ext), &[])?;
    b.build()
}

/// truncate(add(1u64, 2u64) → U8), then return.
pub(crate) fn graph_truncate_return() -> ir::Result<ir::BuiltFunctionGraph> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[], None, 0)?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let c1 = b.build_int_const(1, NodeOutputType::U64);
    let c2 = b.build_int_const(2, NodeOutputType::U64);
    let sum = b.build_int_binary_operation(c1, c2, IntBinaryOp::Add, NodeOutputType::U64)?;
    let tr = b.truncate_if_needed(sum, NodeOutputType::U8)?;
    b.build_return(Some(tr), &[])?;
    b.build()
}

/// add(add(1, 2), 3) nested three levels, return.
pub(crate) fn graph_nested_add() -> ir::Result<ir::BuiltFunctionGraph> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[], None, 0)?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let c1 = b.build_int_const(1, NodeOutputType::U64);
    let c2 = b.build_int_const(2, NodeOutputType::U64);
    let c3 = b.build_int_const(3, NodeOutputType::U64);
    let s12 = b.build_int_binary_operation(c1, c2, IntBinaryOp::Add, NodeOutputType::U64)?;
    let s123 = b.build_int_binary_operation(s12, c3, IntBinaryOp::Add, NodeOutputType::U64)?;
    b.build_return(Some(s123), &[])?;
    b.build()
}

/// store(addr=0x200, data=42) then load from same addr, return the loaded value.
///
/// The Store's memory output flows into Load (which consumes it as input[0]),
/// making the Store node reachable via the preorder walk from Return.
pub(crate) fn graph_store_then_load() -> ir::Result<ir::BuiltFunctionGraph> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[], None, 0)?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let addr = b.build_uint64_const(0x200);
    let data = b.build_uint64_const(42);
    b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
    // Load consumes the Store's memory output, making the Store reachable.
    let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U64)?;
    b.build_return(Some(loaded), &[])?;
    b.build()
}

/// load(addr=0x100) in RAM space, return the loaded value.
pub(crate) fn graph_load_return() -> ir::Result<ir::BuiltFunctionGraph> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[], None, 0)?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let addr = b.build_uint64_const(0x100);
    let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U64)?;
    b.build_return(Some(loaded), &[])?;
    b.build()
}

/// store(addr=0x300, data=7) in RAM, then call(0xCAFE) (which consumes the
/// current memory), then return.
///
/// The Store's memory is threaded into the Call (via cur_region_memory), so
/// the Store is reachable from the Return through the Call's inputs.
pub(crate) fn graph_store_then_call() -> ir::Result<ir::BuiltFunctionGraph> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[], None, 0)?;
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
    b.build()
}

/// Call at 0xABCD with one argument register pre-loaded with the value 42.
///
/// Layout of the Call's inputs:
///   [ctrl(0), mem(1), target(2)=IntConst(0xABCD), arg0(3)=IntConst(42)]
pub(crate) fn graph_call_with_arg() -> ir::Result<(ir::BuiltFunctionGraph, rsleigh::Vn)> {
    let arg_vn = make_reg_vn(0, 8); // 8-byte register at offset 0
    // Register it as both tracked and arg-passing.
    let mut b = FunctionBuilder::new(vec![arg_vn], &[arg_vn], &[], &[], None, 0)?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    // Overwrite the initial value with a concrete constant.
    let c42 = b.build_uint64_const(42);
    b.write_variable(&arg_vn, c42)?;
    let tgt = b.build_uint64_const(0xABCD);
    b.build_call(tgt)?;
    b.build_return(None, &[])?;
    Ok((b.build()?, arg_vn))
}

/// Graph with one variable (register varnode), returned as the result.
/// Produces an `InitialVar` node.
pub(crate) fn graph_with_initial_var() -> ir::Result<(ir::BuiltFunctionGraph, rsleigh::Vn)> {
    let vn = make_reg_vn(0, 8);
    let mut b = FunctionBuilder::new(vec![vn], &[], &[], &[], None, 0)?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let val = b.read_variable(&vn)?;
    b.build_return(Some(val), &[])?;
    Ok((b.build()?, vn))
}

/// Graph with two variables (different offsets), both returned.
pub(crate) fn graph_with_two_initial_vars()
-> ir::Result<(ir::BuiltFunctionGraph, rsleigh::Vn, rsleigh::Vn)> {
    let vn_a = make_reg_vn(0, 8);
    let vn_b = make_reg_vn(8, 8);
    let mut b = FunctionBuilder::new(vec![vn_a, vn_b], &[], &[], &[], None, 0)?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let val_a = b.read_variable(&vn_a)?;
    let val_b = b.read_variable(&vn_b)?;
    // add them together so both are reachable
    let sum = b.build_int_binary_operation(val_a, val_b, IntBinaryOp::Add, NodeOutputType::U64)?;
    b.build_return(Some(sum), &[])?;
    Ok((b.build()?, vn_a, vn_b))
}

/// Graph: if (cond): return 10; else: return 20.
/// Uses a variable for the condition so `ControlPhi` nodes are created.
#[allow(dead_code)]
pub(crate) fn graph_if_with_phi() -> ir::Result<(ir::BuiltFunctionGraph, rsleigh::Vn)> {
    let flag = make_reg_vn(0, 8);
    let mut b = FunctionBuilder::new(vec![flag], &[], &[], &[], None, 0)?;
    let entry = b.create_region()?;
    let true_r = b.create_region()?;
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
    Ok((b.build()?, flag))
}

/// Graph: `add(5, 3)` — note: 5 is lhs, 3 is rhs.
pub(crate) fn graph_add_5_3() -> ir::Result<ir::BuiltFunctionGraph> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[], None, 0)?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let c5 = b.build_int_const(5, NodeOutputType::U64);
    let c3 = b.build_int_const(3, NodeOutputType::U64);
    let sum = b.build_int_binary_operation(c5, c3, IntBinaryOp::Add, NodeOutputType::U64)?;
    b.build_return(Some(sum), &[])?;
    b.build()
}

/// Graph: `sub(5, 3)`.
pub(crate) fn graph_sub_5_3() -> ir::Result<ir::BuiltFunctionGraph> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[], None, 0)?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let c5 = b.build_int_const(5, NodeOutputType::U64);
    let c3 = b.build_int_const(3, NodeOutputType::U64);
    let diff = b.build_int_binary_operation(c5, c3, IntBinaryOp::Sub, NodeOutputType::U64)?;
    b.build_return(Some(diff), &[])?;
    b.build()
}

/// Returns the graph from `graph_add_return`, plus the NodeId of the Add node.
pub(crate) fn add_node_in_add_graph(g: &ir::BuiltFunctionGraph) -> ir::node::NodeId {
    g.preorder()
        .find(|&n| matches!(g.graph.node_kind(n), NodeKind::IntBinaryOp(IntBinaryOp::Add)))
        .expect("add node must exist")
}

/// Build a small graph that returns `bool_const(true)` cast to int.
pub(crate) fn graph_bool_const_return() -> ir::Result<ir::BuiltFunctionGraph> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[], None, 0)?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    let bc = b.build_boolean_const(true);
    let as_int = b.convert_to_int_if_needed(bc, ir::node::NodeOutputType::U64)?;
    b.build_return(Some(as_int), &[])?;
    b.build()
}

pub(crate) fn resolve_int_const(
    g: &ir::BuiltFunctionGraph,
    bindings: &Bindings,
    v: Var,
) -> Option<u64> {
    let o = bindings.get(v)?;
    let n = g.graph.get_node_from_output(o);
    if let NodeKind::IntConst(val) = *g.graph.node_kind(n) {
        Some(val)
    } else {
        None
    }
}

