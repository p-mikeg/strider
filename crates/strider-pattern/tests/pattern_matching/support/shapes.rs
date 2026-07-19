//! Reusable graph fixtures. A shape lives here iff two or more test modules
//! need it; single-use shapes stay inline in the test.

use strider_ir::node::ValueType;
use strider_ir::{FloatBinaryOp, Function, IntBinaryOp, IntCmpOp, IntUnaryOp};

use strider_ir_test_utils::{Tb, reg_vn, stack_vn_x86_64 as stack_vn};

// Op-rooted graphs, parameterised so a test can drive the whole op enum.

pub(crate) fn int_bin_5_3(op: IntBinaryOp) -> Function {
    let mut t = Tb::empty();
    let l = t.u64(5);
    let r = t.u64(3);
    let v = t.int_bin(l, r, op);
    t.ret_val(v)
}

pub(crate) fn int_bin(l: u64, r: u64, op: IntBinaryOp) -> Function {
    let mut t = Tb::empty();
    let a = t.u64(l);
    let b = t.u64(r);
    let v = t.int_bin(a, b, op);
    t.ret_val(v)
}

pub(crate) fn int_un(v: u64, op: IntUnaryOp) -> Function {
    let mut t = Tb::empty();
    let v = t.u64(v);
    let v = t.int_un(v, op);
    t.ret_val(v)
}

pub(crate) fn int_cmp_5_3(op: IntCmpOp) -> Function {
    let mut t = Tb::empty();
    let l = t.u64(5);
    let r = t.u64(3);
    let c = t.int_cmp(l, r, op);
    let cast = t.as_int(c, ValueType::I64);
    t.ret_val(cast)
}

/// `return(5 <= 3)` in the lowered `not(Less(3, 5))` form the pcode lift
/// produces for `IntLessEqual`. Args swapped, result negated.
pub(crate) fn int_le_lowered_5_3() -> Function {
    let mut t = Tb::empty();
    let l = t.u64(5);
    let r = t.u64(3);
    let lt = t.int_cmp(r, l, IntCmpOp::Less);
    let neg = t.bool_not(lt);
    let cast = t.as_int(neg, ValueType::I64);
    t.ret_val(cast)
}

/// Signed analogue of [`int_le_lowered_5_3`], over `Sless`.
pub(crate) fn int_sle_lowered_5_3() -> Function {
    let mut t = Tb::empty();
    let l = t.u64(5);
    let r = t.u64(3);
    let lt = t.int_cmp(r, l, IntCmpOp::Sless);
    let neg = t.bool_not(lt);
    let cast = t.as_int(neg, ValueType::I64);
    t.ret_val(cast)
}

pub(crate) fn bool_bin(l: bool, r: bool, op: IntBinaryOp) -> Function {
    let mut t = Tb::empty();
    let a = t.boolean(l);
    let b = t.boolean(r);
    let v = t.bool_bin(a, b, op);
    let as_int = t.as_int(v, ValueType::I64);
    t.ret_val(as_int)
}

pub(crate) fn float_bin(l: f64, r: f64, op: FloatBinaryOp) -> Function {
    let mut t = Tb::empty();
    let a = t.f64(l);
    let b = t.f64(r);
    let v = t.fbin(a, b, op, ValueType::F64);
    let as_int = t.float_to_int(v, ValueType::I64);
    t.ret_val(as_int)
}

/// `return(a + b)`, both operands `IntConst` at `I64`.
pub(crate) fn add_consts(a: u64, b: u64) -> Function {
    let mut t = Tb::empty();
    let la = t.u64(a);
    let lb = t.u64(b);
    let s = t.add(la, lb);
    t.ret_val(s)
}

/// `return((a + b) + c)`.
pub(crate) fn add_nested_3(a: u64, b: u64, c: u64) -> Function {
    let mut t = Tb::empty();
    let la = t.u64(a);
    let lb = t.u64(b);
    let lc = t.u64(c);
    let s = t.add(la, lb);
    let s = t.add(s, lc);
    t.ret_val(s)
}

/// `call(addr)` then `return`; no args, no return value.
pub(crate) fn call_at(addr: u64) -> Function {
    let mut t = Tb::empty();
    t.call_at(addr);
    t.ret_nothing()
}

/// `store(ram, addr=a, data=d)` then `load(ram, addr=a)` then return.
pub(crate) fn store_then_load_ram(addr: u64, data: u64) -> Function {
    let mut t = Tb::empty();
    let a = t.u64(addr);
    let d = t.u64(data);
    t.store_ram(a, d);
    let v = t.load_ram(a, ValueType::I64);
    t.ret_val(v)
}

/// `if c == 1 { return 10 } else { return 20 }`, `c` a caller-supplied const.
pub(crate) fn if_cmp_then_return(c: u64) -> Function {
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
    let cond = t.int_cmp(c_node, one, IntCmpOp::Equal);
    t.build_if(cond, true_r, false_r);
    t.finish()
}

/// Compiler-inverted [`if_cmp_then_return`]: same source program, but the IR
/// wraps the cond in `Not(..)` and swaps the branches, giving the literal
/// shape `if !(c == 1) { return 20 } else { return 10 }`.
pub(crate) fn if_cmp_then_return_inverted(c: u64) -> Function {
    let mut t = Tb::bare(vec![], &[], &[], &[], None, 0);
    let entry = t.region();
    let true_r = t.region();
    let false_r = t.region();
    t.set_entry(entry);

    t.enter(true_r);
    let twenty = t.u64(20);
    t.fb_mut()
        .build_return(Some(twenty), &[])
        .expect("build_return");

    t.enter(false_r);
    let ten = t.u64(10);
    t.fb_mut()
        .build_return(Some(ten), &[])
        .expect("build_return");

    t.enter(entry);
    let c_node = t.u64(c);
    let one = t.u64(1);
    let inner = t.int_cmp(c_node, one, IntCmpOp::Equal);
    let cond = t.bool_not(inner);
    t.build_if(cond, true_r, false_r);
    t.finish()
}

/// `return(reg)` over one tracked register, yielding a single
/// `InitialVar(reg)`. Returns the register so tests can build `phi_for` /
/// `initial_var_for` patterns against it.
pub(crate) fn single_initial_var() -> (Function, rsleigh::Vn) {
    let reg = reg_vn(0x00, 8);
    let mut t = Tb::with_vars(&[reg]);
    let v = t.read_var(&reg);
    (t.ret_val(v), reg)
}

// `function_arg_reg` lives in the strider-orchestrator copy of this file: it
// needs `strider_opt::FunctionArgDetect` to populate `arg_index_to_values`,
// and strider-pattern can't depend on strider-orchestrator without inverting
// the crate graph. Its one consumer (`pattern_matching/ssa.rs`) is over there
// too.
