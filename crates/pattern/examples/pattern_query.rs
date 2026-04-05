//! Demonstrates how to use the `pattern` crate to query an IR graph.
//!
//! Run with:
//!   cargo run --example pattern_query -p pattern
//!
//! The example builds several small IR graphs by hand (no binary required),
//! then shows a variety of pattern queries and how to read the results.

use ir::{
    BoolBinaryOp, ExtendOp, FunctionBuilder, IntBinaryOp, IntCmpOp,
    node::NodeOutputType,
};
use pattern::*;

fn main() {
    example_arithmetic();
    example_calls_and_returns();
    example_if_branches();
    example_captures();
    example_load_store();
    example_initial_vars();
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn separator(title: &str) {
    println!("\n=== {title} ===");
}

// ── Example 1: arithmetic queries ────────────────────────────────────────────

fn example_arithmetic() {
    separator("Arithmetic patterns");

    // Build: add(and(4, 7), 1), return result
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[]);
    let r = b.create_region();
    b.set_entry_region(r);
    b.set_region(r);
    let c4  = b.build_int_const(4, NodeOutputType::U64);
    let c7  = b.build_int_const(7, NodeOutputType::U64);
    let c1  = b.build_int_const(1, NodeOutputType::U64);
    let band = b.build_int_binary_operation(c4, c7, IntBinaryOp::And, NodeOutputType::U64);
    let sum  = b.build_int_binary_operation(band, c1, IntBinaryOp::Add, NodeOutputType::U64);
    b.build_return(Some(sum), &[]);
    let graph = b.build();

    let m = Matcher::new(&graph);

    // ── Query 1: find any Add node ────────────────────────────────────────────
    let hits = m.find_all(&add(any(), any()).into());
    println!("add(_, _) matches: {}", hits.len()); // 1

    // ── Query 2: find the specific pattern add(and(4, 7), 1) ─────────────────
    let hits = m.find_all(&add(and(int_const(4), int_const(7)), int_const(1)).into());
    println!("add(and(4, 7), 1) matches: {}", hits.len()); // 1

    // ── Query 3: wrong operand order — patterns are not commutative ───────────
    let hits = m.find_all(&add(int_const(1), and(int_const(4), int_const(7))).into());
    println!("add(1, and(4,7)) matches (wrong order): {}", hits.len()); // 0

    // ── Query 4: capture a sub-expression with a Var ─────────────────────────
    let rhs = Var::new();
    let hits = m.find_all(&add(any(), var(rhs)).into());
    if let Some(hit) = hits.first() {
        let bound = hit.get(rhs).unwrap();
        let node  = graph.graph.get_node_from_output(bound);
        println!("rhs of add is: {:?}", graph.graph.node_kind(node)); // IntConst(1)
    }

    // ── Query 5: extend / truncate ────────────────────────────────────────────
    let mut b2 = FunctionBuilder::new(vec![], &[], &[], &[]);
    let r2 = b2.create_region();
    b2.set_entry_region(r2);
    b2.set_region(r2);
    let small = b2.build_int_const(42, NodeOutputType::U32);
    let inner = b2.build_int_binary_operation(small, small, IntBinaryOp::Add, NodeOutputType::U32);
    let ext   = b2.extend_if_needed(inner, NodeOutputType::U64, ExtendOp::ZeroExtend);
    b2.build_return(Some(ext), &[]);
    let g2 = b2.build();
    let m2 = Matcher::new(&g2);

    println!("zero_extend(_) matches: {}", m2.find_all(&zero_extend(any()).into()).len()); // 1
    println!("sign_extend(_) matches: {}", m2.find_all(&sign_extend(any()).into()).len()); // 0

    // ── Query 6: bool operations ──────────────────────────────────────────────
    let mut b3 = FunctionBuilder::new(vec![], &[], &[], &[]);
    let r3 = b3.create_region();
    b3.set_entry_region(r3);
    b3.set_region(r3);
    let t = b3.build_boolean_const(true);
    let f = b3.build_boolean_const(false);
    // bool_and(true, false) constant-folds to BoolConst(false), so use
    // a non-const path to get an actual BoolBinaryOp node.
    let v1 = b3.build_int_const(5, NodeOutputType::U64);
    let v2 = b3.build_int_const(3, NodeOutputType::U64);
    let cmp = b3.build_int_cmp_operation(v1, v2, IntCmpOp::Less, NodeOutputType::U64);
    let not_cmp = b3.build_boolean_unary_operation(cmp, ir::BoolUnaryOp::Neg);
    let bor = b3.build_boolean_operation(t, f, BoolBinaryOp::Or);
    let not_cmp_int = b3.convert_to_int_if_needed(not_cmp, NodeOutputType::U64);
    let bor_int     = b3.convert_to_int_if_needed(bor, NodeOutputType::U64);
    let res = b3.build_int_binary_operation(not_cmp_int, bor_int, IntBinaryOp::Add, NodeOutputType::U64);
    b3.build_return(Some(res), &[]);
    let g3 = b3.build();
    let m3 = Matcher::new(&g3);
    println!("bool_not(_) matches: {}", m3.find_all(&bool_not(any()).into()).len()); // 1
    println!("bool_or(_, _) matches: {}", m3.find_all(&bool_or(any(), any()).into()).len()); // 1
}

// ── Example 2: call and return patterns ──────────────────────────────────────

fn example_calls_and_returns() {
    separator("Call and return patterns");

    // Build: call(0x1000), call(0x2000), return
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[]);
    let r = b.create_region();
    b.set_entry_region(r);
    b.set_region(r);
    let t1 = b.build_uint64_const(0x1000);
    let t2 = b.build_uint64_const(0x2000);
    b.build_call(t1);
    b.build_call(t2);
    b.build_return(None, &[]);
    let graph = b.build();
    let m = Matcher::new(&graph);

    // ── Find all calls ────────────────────────────────────────────────────────
    println!("Total call nodes: {}", m.find_all(&call().into()).len()); // 2

    // ── Match a specific call address ─────────────────────────────────────────
    println!("call at 0x1000: {}", m.find_all(&call().at(0x1000).into()).len()); // 1
    println!("call at 0xDEAD: {}", m.find_all(&call().at(0xDEAD).into()).len()); // 0

    // ── Capture the call node id ──────────────────────────────────────────────
    let cv = NodeVar::new();
    let hits = m.find_all(&call().at(0x2000).capture(cv).into());
    if let Some(hit) = hits.first() {
        let nid = hit.get_node(cv).unwrap();
        println!("Captured call node kind: {:?}", graph.graph.node_kind(nid)); // Call
    }

    // ── preceded_by: find a return that follows a specific call ───────────────
    println!("ret preceded by call(0x2000): {}",
        m.find_all(&ret().preceded_by(call().at(0x2000)).into()).len()); // 1

    // preceded_by walks the full backward chain, so earlier calls are found too
    println!("ret preceded by call(0x1000): {}",
        m.find_all(&ret().preceded_by(call().at(0x1000)).into()).len()); // 1

    println!("ret preceded by call(0xDEAD): {}",
        m.find_all(&ret().preceded_by(call().at(0xDEAD)).into()).len()); // 0

    // ── Capture the call target address via a Var ─────────────────────────────
    let addr = Var::new();
    let rv    = NodeVar::new();
    let hits = m.find_all(&ret().preceded_by(call().target(var(addr)).capture(rv)).into());
    if let Some(hit) = hits.first() {
        let tgt_out  = hit.get(addr).unwrap();
        let tgt_node = graph.graph.get_node_from_output(tgt_out);
        println!("Target of call before return: {:?}", graph.graph.node_kind(tgt_node));
        // Will print IntConst(0x2000) since 0x2000 is immediately before return
    }
}

// ── Example 3: conditional branches ──────────────────────────────────────────

fn example_if_branches() {
    separator("If / branch patterns");

    // Build:
    //   if (5 == 1):
    //     true branch  → call(0xAAAA), return
    //     false branch → return
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[]);
    let entry  = b.create_region();
    let true_r  = b.create_region();
    let false_r = b.create_region();

    b.set_entry_region(entry);

    b.set_region(true_r);
    let tgt = b.build_uint64_const(0xAAAA);
    b.build_call(tgt);
    b.build_return(None, &[]);

    b.set_region(false_r);
    b.build_return(None, &[]);

    b.set_region(entry);
    let c5   = b.build_int_const(5, NodeOutputType::U64);
    let c1   = b.build_int_const(1, NodeOutputType::U64);
    let cond = b.build_int_cmp_operation(c5, c1, IntCmpOp::Equal, NodeOutputType::U64);
    b.build_if(cond, true_r, false_r);
    let graph = b.build();
    let m = Matcher::new(&graph);

    // ── Match any If node ─────────────────────────────────────────────────────
    println!("if_node() matches: {}", m.find_all(&if_node().into()).len()); // 1

    // ── Match If with a specific condition ────────────────────────────────────
    let hits = m.find_all(&if_node().cond(int_eq(int_const(5), int_const(1))).into());
    println!("if(5 == 1) matches: {}", hits.len()); // 1

    let no_match = m.find_all(&if_node().cond(int_eq(int_const(99), int_const(1))).into());
    println!("if(99 == 1) matches: {}", no_match.len()); // 0

    // ── Check what is in each branch ─────────────────────────────────────────
    let true_has_call = m.find_all(
        &if_node().true_branch(contains(call().at(0xAAAA))).into()
    );
    println!("true branch contains call(0xAAAA): {}", true_has_call.len()); // 1

    let false_has_call = m.find_all(
        &if_node().false_branch(contains(call().at(0xAAAA))).into()
    );
    println!("false branch contains call(0xAAAA): {}", false_has_call.len()); // 0

    // ── Capture the If node id ────────────────────────────────────────────────
    let iv = NodeVar::new();
    let hits = m.find_all(&if_node().capture(iv).into());
    if let Some(hit) = hits.first() {
        println!("If node kind: {:?}", graph.graph.node_kind(hit.get_node(iv).unwrap()));
    }
}

// ── Example 4: capture variables ─────────────────────────────────────────────

fn example_captures() {
    separator("Capture variables");

    // Build: if (x == 0): return 10; else: return 20;
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[]);
    let entry  = b.create_region();
    let true_r  = b.create_region();
    let false_r = b.create_region();

    b.set_entry_region(entry);

    b.set_region(true_r);
    let c10 = b.build_int_const(10, NodeOutputType::U64);
    b.build_return(Some(c10), &[]);

    b.set_region(false_r);
    let c20 = b.build_int_const(20, NodeOutputType::U64);
    b.build_return(Some(c20), &[]);

    b.set_region(entry);
    let cx = b.build_int_const(0, NodeOutputType::U64);
    let cy = b.build_int_const(1, NodeOutputType::U64);
    let cond = b.build_int_cmp_operation(cx, cy, IntCmpOp::Equal, NodeOutputType::U64);
    b.build_if(cond, true_r, false_r);
    let graph = b.build();
    let m = Matcher::new(&graph);

    // ── Capture both operands of the condition ────────────────────────────────
    let lhs_v = Var::new();
    let rhs_v = Var::new();
    let hits  = m.find_all(&if_node().cond(int_eq(var(lhs_v), var(rhs_v))).into());
    if let Some(hit) = hits.first() {
        let lhs_node = graph.graph.get_node_from_output(hit.get(lhs_v).unwrap());
        let rhs_node = graph.graph.get_node_from_output(hit.get(rhs_v).unwrap());
        println!("Condition lhs: {:?}", graph.graph.node_kind(lhs_node)); // IntConst(0)
        println!("Condition rhs: {:?}", graph.graph.node_kind(rhs_node)); // IntConst(1)
    }

    // ── Match a return with a specific value and capture it ───────────────────
    let val_v = Var::new();
    let ret_v = NodeVar::new();
    let hits  = m.find_all(&ret().ret_val(0, var(val_v)).capture(ret_v).into());
    println!("Returns with a value: {}", hits.len()); // 2 (one per branch)
    for hit in &hits {
        let val_node = graph.graph.get_node_from_output(hit.get(val_v).unwrap());
        let ret_node = hit.get_node(ret_v).unwrap();
        println!(
            "  Return {:?} returns {:?}",
            ret_node,
            graph.graph.node_kind(val_node),
        );
    }

    // ── Enforce equality: same var used twice ─────────────────────────────────
    let x = Var::new();
    // add(x, x) only matches nodes where both inputs are the *same* output.
    let mut b2 = FunctionBuilder::new(vec![], &[], &[], &[]);
    let r2 = b2.create_region();
    b2.set_entry_region(r2);
    b2.set_region(r2);
    let c5   = b2.build_int_const(5, NodeOutputType::U64);
    let self_add = b2.build_int_binary_operation(c5, c5, IntBinaryOp::Add, NodeOutputType::U64);
    b2.build_return(Some(self_add), &[]);
    let g2 = b2.build();
    let m2 = Matcher::new(&g2);
    println!("add(x, x) on add(5, 5): {}", m2.find_all(&add(var(x), var(x)).into()).len()); // 1
    println!("add(x, x) on add(5, 3): {}", m.find_all(&add(var(x), var(x)).into()).len());  // 0
}

// ── Example 5: load / store / call-arg patterns ──────────────────────────────

fn example_load_store() {
    separator("Load / Store / Call-arg patterns");

    // ── Load ──────────────────────────────────────────────────────────────────
    // Build: load(0x100, RAM), return loaded value
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[]);
    let r = b.create_region();
    b.set_entry_region(r);
    b.set_region(r);
    let addr   = b.build_uint64_const(0x100);
    let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, ir::node::NodeOutputType::U64);
    b.build_return(Some(loaded), &[]);
    let g_load = b.build();
    let m = Matcher::new(&g_load);

    println!("load() matches: {}", m.find_all(&load().into()).len()); // 1
    println!("load(RAM) matches: {}", m.find_all(&load().space(rsleigh::VnSpace::RAM).into()).len()); // 1
    println!("load(REGISTER) matches: {}", m.find_all(&load().space(rsleigh::VnSpace::REGISTER).into()).len()); // 0
    println!("load addr=0x100: {}", m.find_all(&load().addr(int_const(0x100)).into()).len()); // 1
    println!("load addr=0x999: {}", m.find_all(&load().addr(int_const(0x999)).into()).len()); // 0

    // Capture the address and extract the constant value directly.
    let addr_v = Var::new();
    let hits = m.find_all(&load().addr(var(addr_v)).into());
    if let Some(hit) = hits.first() {
        // get_int_const is the easy way to get the constant without digging into node kinds.
        let addr_val = hit.get_int_const(addr_v, &g_load).expect("address is an int const");
        println!("Captured load address: 0x{addr_val:X}"); // 0x100
    }

    // ── Store ─────────────────────────────────────────────────────────────────
    // Build: store(addr=0x200, data=42, RAM), then load to make store reachable,
    //        return the loaded value.
    let mut b2 = FunctionBuilder::new(vec![], &[], &[], &[]);
    let r2 = b2.create_region();
    b2.set_entry_region(r2);
    b2.set_region(r2);
    let addr2  = b2.build_uint64_const(0x200);
    let data2  = b2.build_uint64_const(42);
    b2.build_store(addr2, data2, rsleigh::VnSpace::RAM);
    // A load immediately after consumes the store's memory output, making it
    // reachable from Return via the preorder walk.
    let loaded2 = b2.build_load(addr2, rsleigh::VnSpace::RAM, ir::node::NodeOutputType::U64);
    b2.build_return(Some(loaded2), &[]);
    let g_store = b2.build();
    let m2 = Matcher::new(&g_store);

    println!("store() matches: {}", m2.find_all(&store().into()).len()); // 1
    println!("store(addr=0x200) matches: {}", m2.find_all(&store().addr(int_const(0x200)).into()).len()); // 1
    println!("store(data=42) matches: {}", m2.find_all(&store().data(int_const(42)).into()).len()); // 1
    println!("store(data=0) matches: {}", m2.find_all(&store().data(int_const(0)).into()).len()); // 0

    // Capture both addr and data at once, then read values directly.
    let addr_v2 = Var::new();
    let data_v2 = Var::new();
    let hits2 = m2.find_all(&store().addr(var(addr_v2)).data(var(data_v2)).into());
    if let Some(hit) = hits2.first() {
        println!(
            "Captured store: addr=0x{:X}, data={}",
            hit.get_int_const(addr_v2, &g_store).unwrap(),
            hit.get_int_const(data_v2, &g_store).unwrap(),
        ); // addr=0x200, data=42
    }

    // ── Call arguments ────────────────────────────────────────────────────────
    // Build: load a constant into arg register, call(0xABCD)
    let arg_vn = rsleigh::Vn {
        size: 8,
        addr: rsleigh::VnAddr { off: 0, space: rsleigh::VnSpace::REGISTER },
    };
    let mut b3 = FunctionBuilder::new(vec![arg_vn], &[arg_vn], &[], &[]);
    let r3 = b3.create_region();
    b3.set_entry_region(r3);
    b3.set_region(r3);
    let c_arg = b3.build_uint64_const(42);
    b3.write_variable(&arg_vn, c_arg);
    let tgt3 = b3.build_uint64_const(0xABCD);
    b3.build_call(tgt3);
    b3.build_return(None, &[]);
    let g_call = b3.build();
    let m3 = Matcher::new(&g_call);

    println!("call().arg(0, int_const(42)) matches: {}",
        m3.find_all(&call().arg(0, int_const(42)).into()).len()); // 1
    println!("call().arg(0, int_const(99)) matches: {}",
        m3.find_all(&call().arg(0, int_const(99)).into()).len()); // 0

    // Capture the call argument and extract the value with get_int_const.
    let arg_v = Var::new();
    let hits3 = m3.find_all(&call().arg(0, var(arg_v)).into());
    if let Some(hit) = hits3.first() {
        let arg_val = hit.get_int_const(arg_v, &g_call).unwrap();
        println!("Captured call arg0: {arg_val}"); // 42
    }
}

// ── Example 6: initial variable queries ───────────────────────────────────────

fn example_initial_vars() {
    separator("InitialVar patterns");

    // Simulate a register varnode (8-byte, offset 0 in register space).
    let rax_vn = rsleigh::Vn {
        size: 8,
        addr: rsleigh::VnAddr { off: 0, space: rsleigh::VnSpace::REGISTER },
    };
    let rbx_vn = rsleigh::Vn {
        size: 8,
        addr: rsleigh::VnAddr { off: 8, space: rsleigh::VnSpace::REGISTER },
    };

    // Build: rax + rbx, return result
    let mut b = FunctionBuilder::new(vec![rax_vn, rbx_vn], &[], &[], &[]);
    let r = b.create_region();
    b.set_entry_region(r);
    b.set_region(r);
    let rax_val = b.read_variable(&rax_vn);
    let rbx_val = b.read_variable(&rbx_vn);
    let sum = b.build_int_binary_operation(rax_val, rbx_val, IntBinaryOp::Add, NodeOutputType::U64);
    b.build_return(Some(sum), &[]);
    let graph = b.build();
    let m = Matcher::new(&graph);

    // ── initial_var() matches any initial-variable node ───────────────────────
    println!("initial_var() matches: {}", m.find_all(&initial_var().into()).len()); // 2

    // ── initial_var_for(vn) matches only the named varnode ────────────────────
    println!("initial_var_for(rax): {}", m.find_all(&initial_var_for(rax_vn).into()).len()); // 1
    println!("initial_var_for(rbx): {}", m.find_all(&initial_var_for(rbx_vn).into()).len()); // 1

    // ── Match an add of two values ────────────────────────────────────────────
    // In a single-region graph, `read_variable` returns a ControlSelector
    // (phi-like) node rather than the InitialVar directly.  The ControlSelector
    // holds the InitialVar as one of its inputs.
    let lhs_v = Var::new();
    let rhs_v = Var::new();
    let hits  = m.find_all(&add(var(lhs_v), var(rhs_v)).into());
    println!("add of any two values: {}", hits.len()); // 1
    if let Some(hit) = hits.first() {
        let lhs = graph.graph.get_node_from_output(hit.get(lhs_v).unwrap());
        let rhs = graph.graph.get_node_from_output(hit.get(rhs_v).unwrap());
        // Both inputs are ControlSelector(vn) phi nodes wrapping the InitialVar.
        println!(
            "  lhs kind: {:?}", graph.graph.node_kind(lhs),
        );
        println!(
            "  rhs kind: {:?}", graph.graph.node_kind(rhs),
        );
    }

    // ── Match "add where lhs comes from rax's selector" ──────────────────────
    // Use selector_for() to match the ControlSelector phi node for rax.
    let hits = m.find_all(&add(selector_for(rax_vn).into(), any()).into());
    println!("add(rax_selector, _): {}", hits.len()); // 1

    let hits_wrong = m.find_all(&add(selector_for(rbx_vn).into(), any()).into());
    println!("add(rbx_selector, _): {}", hits_wrong.len()); // 0 — rax is lhs, rbx is rhs

    // initial_var_for matches the deeper InitialVar node itself, which is an
    // input *inside* the selector — not the direct input to the add.
    let hits_iv = m.find_all(&add(initial_var_for(rax_vn), any()).into());
    println!("add(rax_initial_var_direct, _): {}", hits_iv.len()); // 0 (selector wraps it)
}
