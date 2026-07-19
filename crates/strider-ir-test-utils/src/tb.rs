//! `Tb` ("test builder"), the mock-graph DSL the pattern and orchestrator
//! suites share. It owns a `FunctionBuilder` with an active entry region and
//! short helpers so tests read like pseudocode, finalising via `ret_val` /
//! `ret_nothing`.
//!
//! Anything the DSL doesn't cover (multi-region graphs, custom
//! calling-convention slots) goes through `Tb::fb_mut`.

use crate::{IrBuilderEx, RegisterSet};
use strider_ir::node::{ValueId, ValueType};
use strider_ir::{
    ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, FunctionBuilder, IRBuilderExt, IntBinaryOp,
    IntCmpOp, IntUnaryOp,
};

/// One home for the slice-to-`RegisterSet` mapping, shared by [`Tb::raw`],
/// [`Tb::bare`] and the free `crate::builder`, which differ only in what they
/// do with the result.
pub(crate) fn build_rs(
    vars: Vec<rsleigh::Vn>,
    arg_passing: &[rsleigh::Vn],
    callee_saved: &[rsleigh::Vn],
    ret_regs: &[rsleigh::Vn],
    sp: Option<rsleigh::Vn>,
    ret_stack_pop: i64,
) -> RegisterSet {
    let mut rs = vars
        .into_iter()
        .fold(RegisterSet::new(), RegisterSet::tracked);
    rs = arg_passing.iter().copied().fold(rs, RegisterSet::arg);
    rs = callee_saved
        .iter()
        .copied()
        .fold(rs, RegisterSet::callee_saved);
    rs = ret_regs.iter().copied().fold(rs, RegisterSet::ret);
    if let Some(s) = sp {
        rs = rs.stack_vn(s);
    }
    rs.ret_stack_pop(ret_stack_pop)
}

pub struct Tb {
    fb: FunctionBuilder,
}

impl Tb {
    /// No tracked variables, no calling convention, entry region active.
    pub fn empty() -> Self {
        let fb = RegisterSet::new()
            .build_fn_single_region()
            .expect("build_fn_single_region");
        Self { fb }
    }

    pub fn with_vars(vars: &[rsleigh::Vn]) -> Self {
        let rs = vars.iter().fold(RegisterSet::new(), |rs, v| rs.tracked(*v));
        let fb = rs.build_fn_single_region().expect("build_fn_single_region");
        Self { fb }
    }

    /// For fixtures needing a CC knob the positional constructors don't take,
    /// such as `stack_args`.
    pub fn from_rs(rs: RegisterSet) -> Self {
        let fb = rs.build_fn_single_region().expect("build_fn_single_region");
        Self { fb }
    }

    /// Entry region pre-created and active.
    pub fn raw(
        vars: Vec<rsleigh::Vn>,
        arg_passing: &[rsleigh::Vn],
        callee_saved: &[rsleigh::Vn],
        ret_regs: &[rsleigh::Vn],
        sp: Option<rsleigh::Vn>,
        ret_stack_pop: i64,
    ) -> Self {
        let rs = build_rs(vars, arg_passing, callee_saved, ret_regs, sp, ret_stack_pop);
        let fb = rs.build_fn_single_region().expect("build_fn_single_region");
        Self { fb }
    }

    /// Creates NO region, for multi-region graphs (branches, merges) where the
    /// caller picks the entry region.
    pub fn bare(
        vars: Vec<rsleigh::Vn>,
        arg_passing: &[rsleigh::Vn],
        callee_saved: &[rsleigh::Vn],
        ret_regs: &[rsleigh::Vn],
        sp: Option<rsleigh::Vn>,
        ret_stack_pop: i64,
    ) -> Self {
        let rs = build_rs(vars, arg_passing, callee_saved, ret_regs, sp, ret_stack_pop);
        let fb = rs.build_fn().expect("build_fn");
        Self { fb }
    }

    pub fn set_entry(&mut self, r: strider_ir::RegionId) {
        self.fb.set_entry_region_all(r).expect("set_entry_region");
        // Mirrors the lifter: arg carriers are recorded after entry setup.
        self.fb.record_register_arg_carriers();
    }

    pub fn fb_mut(&mut self) -> &mut FunctionBuilder {
        &mut self.fb
    }

    pub fn u64(&mut self, v: u64) -> ValueId {
        self.fb.build_int_const(v, ValueType::I64).unwrap()
    }
    pub fn u32(&mut self, v: u64) -> ValueId {
        self.fb.build_int_const(v, ValueType::I32).unwrap()
    }
    pub fn u8(&mut self, v: u64) -> ValueId {
        self.fb.build_int_const(v, ValueType::I8).unwrap()
    }
    pub fn int_of(&mut self, v: u64, ty: ValueType) -> ValueId {
        self.fb.build_int_const(v, ty).unwrap()
    }
    pub fn boolean(&mut self, v: bool) -> ValueId {
        self.fb.build_boolean_const(v)
    }
    pub fn f64(&mut self, v: f64) -> ValueId {
        self.fb.build_float_const(v.to_bits(), ValueType::F64)
    }
    pub fn float_bits(&mut self, bits: u64, ty: ValueType) -> ValueId {
        self.fb.build_float_const(bits, ty)
    }

    pub fn add(&mut self, l: ValueId, r: ValueId) -> ValueId {
        self.int_bin(l, r, IntBinaryOp::Add)
    }
    /// `Add(l, Neg(r))`: there is no `IntBinaryOp::Sub`, and this is the shape
    /// pcode-lift produces.
    pub fn sub(&mut self, l: ValueId, r: ValueId) -> ValueId {
        self.fb
            .build_sub_as_add_neg(l, r, ValueType::I64)
            .expect("sub_as_add_neg")
    }
    pub fn mul(&mut self, l: ValueId, r: ValueId) -> ValueId {
        self.int_bin(l, r, IntBinaryOp::Mul)
    }
    pub fn bxor(&mut self, l: ValueId, r: ValueId) -> ValueId {
        self.int_bin(l, r, IntBinaryOp::Xor)
    }
    pub fn bor(&mut self, l: ValueId, r: ValueId) -> ValueId {
        self.int_bin(l, r, IntBinaryOp::Or)
    }
    pub fn int_bin(&mut self, l: ValueId, r: ValueId, op: IntBinaryOp) -> ValueId {
        self.int_bin_at(l, r, op, ValueType::I64)
    }
    pub fn int_bin_at(
        &mut self,
        l: ValueId,
        r: ValueId,
        op: IntBinaryOp,
        ty: ValueType,
    ) -> ValueId {
        self.fb
            .build_int_binary_operation(l, r, op, ty)
            .expect("int_binary_operation")
    }
    /// Bitwise complement `~v`, which the IR spells `Xor(v, all_ones)`; there
    /// is no BitNot unary op.
    pub fn bit_not_at(&mut self, v: ValueId, ty: ValueType) -> ValueId {
        let all_ones = self.fb.build_int_const(u128::MAX, ty).expect("all_ones");
        self.fb
            .build_int_binary_operation(v, all_ones, IntBinaryOp::Xor, ty)
            .expect("bit_not as xor")
    }
    pub fn int_un(&mut self, v: ValueId, op: IntUnaryOp) -> ValueId {
        self.fb
            .build_int_unary_operation(v, op, ValueType::I64)
            .expect("int_unary_operation")
    }
    pub fn int_cmp(&mut self, l: ValueId, r: ValueId, op: IntCmpOp) -> ValueId {
        self.fb
            .build_int_cmp_operation(l, r, op, ValueType::I64)
            .expect("int_cmp_operation")
    }
    pub fn popcount(&mut self, v: ValueId) -> ValueId {
        self.fb.build_popcount(v, ValueType::I64).expect("popcount")
    }
    pub fn lzcount(&mut self, v: ValueId) -> ValueId {
        self.fb.build_lzcount(v, ValueType::I64).expect("lzcount")
    }

    // Booleans are 1-bit (`I1`) integers, so a boolean binary op is just an
    // `IntBinaryOp` (`And` / `Or` / `Xor`) at `I1`.
    pub fn bool_bin(&mut self, l: ValueId, r: ValueId, op: IntBinaryOp) -> ValueId {
        self.fb
            .build_int_binary_operation(l, r, op, ValueType::I1)
            .expect("boolean_operation")
    }
    /// Logical NOT of an I1 value: `Xor(v, IntConst(1)):I1`. `Neg`, the only
    /// `IntUnaryOp`, is meaningless at I1 and is never the right tool here.
    pub fn bool_not(&mut self, v: ValueId) -> ValueId {
        self.bit_not_at(v, ValueType::I1)
    }

    pub fn fbin(&mut self, l: ValueId, r: ValueId, op: FloatBinaryOp, ty: ValueType) -> ValueId {
        self.fb
            .build_float_binary_op(l, r, op, ty)
            .expect("float_binary_op")
    }
    pub fn fun(&mut self, v: ValueId, op: FloatUnaryOp, ty: ValueType) -> ValueId {
        self.fb
            .build_float_unary_op(v, op, ty)
            .expect("float_unary_op")
    }
    pub fn fcmp(&mut self, l: ValueId, r: ValueId, op: FloatCmpOp) -> ValueId {
        self.fb.build_float_cmp_op(l, r, op).expect("float_cmp_op")
    }
    pub fn int_to_float(&mut self, v: ValueId, ty: ValueType) -> ValueId {
        self.fb.build_int_to_float(v, ty).expect("int_to_float")
    }
    pub fn float_to_int(&mut self, v: ValueId, ty: ValueType) -> ValueId {
        self.fb.build_float_to_int(v, ty).expect("float_to_int")
    }
    pub fn float_to_float(&mut self, v: ValueId, ty: ValueType) -> ValueId {
        self.fb.build_float_to_float(v, ty).expect("float_to_float")
    }
    pub fn int_bits_to_float(&mut self, v: ValueId, ty: ValueType) -> ValueId {
        self.fb
            .build_int_bits_to_float(v, ty)
            .expect("int_bits_to_float")
    }
    pub fn float_bits_to_int(&mut self, v: ValueId, ty: ValueType) -> ValueId {
        self.fb
            .build_float_bits_to_int(v, ty)
            .expect("float_bits_to_int")
    }

    pub fn zext_to(&mut self, v: ValueId, ty: ValueType) -> ValueId {
        self.fb
            .extend_if_needed(v, ty, ExtendOp::ZeroExtend)
            .expect("zero_extend")
    }
    pub fn sext_to(&mut self, v: ValueId, ty: ValueType) -> ValueId {
        self.fb
            .extend_if_needed(v, ty, ExtendOp::SignExtend)
            .expect("sign_extend")
    }
    pub fn trunc_to(&mut self, v: ValueId, ty: ValueType) -> ValueId {
        self.fb.truncate_if_needed(v, ty).expect("truncate")
    }
    pub fn as_int(&mut self, v: ValueId, ty: ValueType) -> ValueId {
        self.fb.convert_to_int_if_needed(v, ty).expect("as_int")
    }

    pub fn store_ram(&mut self, addr: ValueId, data: ValueId) {
        self.fb
            .build_store(addr, data, rsleigh::VnSpace::RAM)
            .expect("store");
    }
    pub fn load_ram(&mut self, addr: ValueId, ty: ValueType) -> ValueId {
        self.fb
            .build_load(addr, rsleigh::VnSpace::RAM, ty)
            .expect("load")
    }

    pub fn call_at(&mut self, addr: u64) {
        let tgt = self.u64(addr);
        self.fb.build_call_cc(tgt, None).expect("call");
    }
    /// Yields the ret-value output when `output_vn` is `Some`. The builder
    /// itself reads `implicit_read_vns` and emits one clobber per
    /// `implicit_write_vns` entry.
    pub fn call_other(
        &mut self,
        name: &str,
        user_op_id: u64,
        args: &[ValueId],
        output_vn: Option<rsleigh::Vn>,
        implicit_read_vns: &[rsleigh::Vn],
        implicit_write_vns: &[rsleigh::Vn],
    ) -> Option<ValueId> {
        let abi = strider_target::BuiltCallOtherAbi {
            implicit_reads: implicit_read_vns.to_vec(),
            implicit_writes: implicit_write_vns.to_vec(),
            clobbers_memory: false,
        };
        let (_node, result) = self
            .fb
            .build_call_other_abi(user_op_id, name, args, &abi, output_vn, false)
            .expect("call_other");
        result
    }

    pub fn read_var(&mut self, vn: &rsleigh::Vn) -> ValueId {
        self.fb.read_variable(vn).expect("read_variable")
    }
    pub fn write_var(&mut self, vn: &rsleigh::Vn, v: ValueId) {
        self.fb.write_variable(vn, v).expect("write_variable")
    }

    pub fn region(&mut self) -> strider_ir::RegionId {
        self.fb.create_region_all().expect("create_region")
    }
    pub fn enter(&mut self, r: strider_ir::RegionId) {
        self.fb.set_region(r);
    }
    pub fn branch(&mut self, dst: strider_ir::RegionId) {
        self.fb.build_branch(dst).expect("build_branch");
    }
    pub fn build_if(&mut self, cond: ValueId, t: strider_ir::RegionId, f: strider_ir::RegionId) {
        self.fb.build_if(cond, t, f).expect("build_if");
    }

    pub fn ret_val(mut self, v: ValueId) -> strider_ir::Function {
        self.fb.build_return(Some(v), &[]).expect("build_return");
        self.fb.build().expect("FunctionBuilder::build (validator)")
    }

    /// `return(IntConst(v) : I64)`, for the one-constant graph.
    pub fn ret_const(mut self, v: u64) -> strider_ir::Function {
        let c = self.u64(v);
        self.ret_val(c)
    }

    pub fn ret_nothing(mut self) -> strider_ir::Function {
        self.fb.build_return(None, &[]).expect("build_return");
        self.fb.build().expect("FunctionBuilder::build (validator)")
    }

    /// Emits nothing extra; the caller has already built its terminators.
    pub fn finish(self) -> strider_ir::Function {
        self.fb.build().expect("FunctionBuilder::build (validator)")
    }
}
