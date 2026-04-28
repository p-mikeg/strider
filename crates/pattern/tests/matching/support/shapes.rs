//! Reusable pre-built graph fixtures.  A shape lives here iff at least two
//! test modules need it; single-use shapes stay inline in the test for
//! readability.

use ir::node::NodeOutputType;
use ir::{BoolBinaryOp, BuiltFunctionGraph, FloatBinaryOp, IntBinaryOp, IntCmpOp, IntUnaryOp};

use super::graph::{Tb, reg_vn, sp_vn};

// ── Minimal op-rooted graphs ─────────────────────────────────────────────────
// Each builds `return(op(5, 3))` (or similar) at U64 width.  Parameterising
// the op lets every test module drive the full op enum without open-coding
// the boilerplate.

pub fn int_bin_5_3(op: IntBinaryOp) -> BuiltFunctionGraph {
    let mut t = Tb::empty();
    let l = t.u64(5);
    let r = t.u64(3);
    let v = t.int_bin(l, r, op);
    t.ret_val(v)
}

pub fn int_bin(l: u64, r: u64, op: IntBinaryOp) -> BuiltFunctionGraph {
    let mut t = Tb::empty();
    let a = t.u64(l);
    let b = t.u64(r);
    let v = t.int_bin(a, b, op);
    t.ret_val(v)
}

pub fn int_un(v: u64, op: IntUnaryOp) -> BuiltFunctionGraph {
    let mut t = Tb::empty();
    let v = t.u64(v);
    let v = t.int_un(v, op);
    t.ret_val(v)
}

pub fn int_cmp_5_3(op: IntCmpOp) -> BuiltFunctionGraph {
    let mut t = Tb::empty();
    let l = t.u64(5);
    let r = t.u64(3);
    let c = t.int_cmp(l, r, op);
    let cast = t.as_int(c, NodeOutputType::U64);
    t.ret_val(cast)
}

pub fn bool_bin(l: bool, r: bool, op: BoolBinaryOp) -> BuiltFunctionGraph {
    let mut t = Tb::empty();
    let a = t.boolean(l);
    let b = t.boolean(r);
    let v = t.bool_bin(a, b, op);
    let as_int = t.as_int(v, NodeOutputType::U64);
    t.ret_val(as_int)
}

pub fn float_bin(l: f64, r: f64, op: FloatBinaryOp) -> BuiltFunctionGraph {
    let mut t = Tb::empty();
    let a = t.f64(l);
    let b = t.f64(r);
    let v = t.fbin(a, b, op, NodeOutputType::F64);
    let as_int = t.float_to_int(v, NodeOutputType::U64);
    t.ret_val(as_int)
}

/// `return(a + b)` — both operands are `IntConst` of type `U64`.
pub fn add_consts(a: u64, b: u64) -> BuiltFunctionGraph {
    let mut t = Tb::empty();
    let la = t.u64(a);
    let lb = t.u64(b);
    let s = t.add(la, lb);
    t.ret_val(s)
}

/// `return(((a + b) + c))` — three-deep nested add.
pub fn add_nested_3(a: u64, b: u64, c: u64) -> BuiltFunctionGraph {
    let mut t = Tb::empty();
    let la = t.u64(a);
    let lb = t.u64(b);
    let lc = t.u64(c);
    let s = t.add(la, lb);
    let s = t.add(s, lc);
    t.ret_val(s)
}

/// `call(addr)` then `return` — no args, no return value.
pub fn call_at(addr: u64) -> BuiltFunctionGraph {
    let mut t = Tb::empty();
    t.call_at(addr);
    t.ret_nothing()
}

/// `store(ram, addr=a, data=d)` then `load(ram, addr=a)` then return.
pub fn store_then_load_ram(addr: u64, data: u64) -> BuiltFunctionGraph {
    let mut t = Tb::empty();
    let a = t.u64(addr);
    let d = t.u64(data);
    t.store_ram(a, d);
    let v = t.load_ram(a, ir::node::NodeOutputType::U64);
    t.ret_val(v)
}

/// `if c == 1 { return 10 } else { return 20 }` where `c` is a u64 const
/// supplied by the caller.  Useful for If-pattern and dead-branch tests.
pub fn if_cmp_then_return(c: u64) -> BuiltFunctionGraph {
    let mut t = Tb::bare(vec![], &[], &[], &[], None, 0);
    let entry = t.region();
    let true_r = t.region();
    let false_r = t.region();
    t.set_entry(entry);

    t.enter(true_r);
    let ten = t.u64(10);
    t.fb_mut()
        .build_return(Some(ten), &[])
        .expect("build_return");

    t.enter(false_r);
    let twenty = t.u64(20);
    t.fb_mut()
        .build_return(Some(twenty), &[])
        .expect("build_return");

    t.enter(entry);
    let c_node = t.u64(c);
    let one = t.u64(1);
    let cond = t.int_cmp(c_node, one, ir::IntCmpOp::Equal);
    t.build_if(cond, true_r, false_r);
    t.finish()
}

/// Graph with a single tracked register and `return(reg)` — yields one
/// `InitialVar(reg)` node.  Returns the register so tests can construct
/// `phi_for` / `initial_var_for` patterns against it.
pub fn single_initial_var() -> (BuiltFunctionGraph, rsleigh::Vn) {
    let reg = reg_vn(0x00, 8);
    let mut t = Tb::with_vars(&[reg]);
    let v = t.read_var(&reg);
    (t.ret_val(v), reg)
}

/// Graph that, after `opt::FunctionArgDetect`, contains a single
/// `FunctionArg { Register(reg), 0 }` node.
pub fn function_arg_reg() -> (BuiltFunctionGraph, rsleigh::Vn) {
    use opt::{FunctionArgDetect, Optimizer};
    let reg = reg_vn(0x38, 8);
    let sp = sp_vn();
    let mut t = Tb::raw(vec![reg, sp], &[], &[reg], &[reg], None, 0);
    let v = t.read_var(&reg);
    let mut g = t.ret_val(v);
    FunctionArgDetect::new(vec![reg], sp, vec![])
        .optimize(&mut g.graph, g.entry)
        .expect("FunctionArgDetect");
    (g, reg)
}

/// Runs the minimal opt pipeline needed before `StackStoreDetect` can see a
/// `Store(sp ± K)` — `ConstantFold` normalises the address, `RedundantPhis`
/// removes dead joins, and `StackStoreDetect` lowers the store.
pub fn run_stack_store_pipeline(g: &mut BuiltFunctionGraph, sp: rsleigh::Vn) {
    use opt::{ConstantFold, OptimizerPipeline, RedundantPhis, StackStoreDetect};
    let mut p = OptimizerPipeline::new();
    p.add(ConstantFold);
    p.add(RedundantPhis);
    p.add(StackStoreDetect::new(sp));
    p.run(&mut g.graph, g.entry).expect("opt pipeline");
}
