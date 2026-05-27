//! Thin DSL around `strider_ir::FunctionBuilder` for test graphs.
//!
//! `Tb` = "test builder".  It owns a `FunctionBuilder` with an active entry
//! region already set up, exposes short helpers for the common builder calls
//! (so tests read like pseudocode), and finalises into a `Graph`
//! via `.ret_val(v)` / `.ret_nothing()`.
//!
//! For cases the DSL doesn't cover directly (multi-region graphs, custom
//! calling-convention slots) tests reach into the underlying `FunctionBuilder`
//! via `Tb::fb_mut`.

use strider_ir::node::{NodeOutputId, NodeOutputType};
use strider_ir::{IntBinaryOp, IntCmpOp, IntUnaryOp};
use strider_ir::{ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, FunctionBuilder};
use strider_ir_test_utils::RegisterSet;

// ── Varnode helpers ───────────────────────────────────────────────────────────

pub use strider_ir_test_utils::reg_vn;
pub use strider_ir_test_utils::stack_vn_x86_64 as stack_vn;

/// Shared `RegisterSet` populater for [`Tb::raw`] and [`Tb::bare`].  Both
/// constructors take the same six DTO-style parameters and feed them
/// into `RegisterSet` field-by-field; the only difference is whether
/// the resulting builder pre-creates an entry region or not.
fn build_rs(
    vars: Vec<rsleigh::Vn>,
    arg_passing: &[rsleigh::Vn],
    callee_saved: &[rsleigh::Vn],
    ret_regs: &[rsleigh::Vn],
    sp: Option<rsleigh::Vn>,
    ret_stack_pop: i64,
) -> RegisterSet {
    let mut rs = RegisterSet::new();
    for v in vars {
        rs = rs.tracked(v);
    }
    for v in arg_passing {
        rs = rs.arg(*v);
    }
    for v in callee_saved {
        rs = rs.callee_saved(*v);
    }
    for v in ret_regs {
        rs = rs.ret(*v);
    }
    if let Some(s) = sp {
        rs = rs.stack_vn(s);
    }
    rs.ret_stack_pop(ret_stack_pop)
}

// ── Tb ────────────────────────────────────────────────────────────────────────

/// Test graph builder.  Wraps a `FunctionBuilder` with a single active entry
/// region pre-created; provides short-named helpers for common builder calls.
pub struct Tb {
    fb: FunctionBuilder,
}

impl Tb {
    /// Empty function: no tracked variables, no calling convention.  A single
    /// entry region is pre-created and set active — use [`Tb::bare`] when
    /// you need to manage regions yourself (e.g. multi-branch graphs).
    pub fn empty() -> Self {
        let fb = RegisterSet::new()
            .build_fn_single_region()
            .expect("build_fn_single_region");
        Self { fb }
    }

    /// Function with tracked variables but no calling-convention extras.
    /// An entry region is pre-created and set active.
    pub fn with_vars(vars: &[rsleigh::Vn]) -> Self {
        let mut rs = RegisterSet::new();
        for v in vars {
            rs = rs.tracked(*v);
        }
        let fb = rs.build_fn_single_region().expect("build_fn_single_region");
        Self { fb }
    }

    /// Low-level raw constructor matching `FunctionBuilder::new_raw`, with
    /// an entry region pre-created and set active.
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

    /// Low-level raw constructor that does *not* pre-create any region.  Use
    /// when you want to build a multi-region graph (branches, merges) and
    /// control entry-region selection yourself.
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

    /// Makes `r` the entry region for the function.
    pub fn set_entry(&mut self, r: strider_ir::RegionId) {
        self.fb.set_entry_region(r).expect("set_entry_region");
    }

    // ── Raw access ────────────────────────────────────────────────────────────

    pub fn fb_mut(&mut self) -> &mut FunctionBuilder {
        &mut self.fb
    }

    // ── Constant builders ─────────────────────────────────────────────────────

    pub fn u64(&mut self, v: u64) -> NodeOutputId {
        self.fb.build_int_const(v, NodeOutputType::I64).unwrap()
    }
    pub fn u32(&mut self, v: u64) -> NodeOutputId {
        self.fb.build_int_const(v, NodeOutputType::I32).unwrap()
    }
    pub fn u8(&mut self, v: u64) -> NodeOutputId {
        self.fb.build_int_const(v, NodeOutputType::I8).unwrap()
    }
    pub fn int_of(&mut self, v: u64, ty: NodeOutputType) -> NodeOutputId {
        self.fb.build_int_const(v, ty).unwrap()
    }
    pub fn boolean(&mut self, v: bool) -> NodeOutputId {
        self.fb.build_boolean_const(v)
    }
    pub fn f64(&mut self, v: f64) -> NodeOutputId {
        self.fb.build_float_const(v.to_bits(), NodeOutputType::F64)
    }
    pub fn f32(&mut self, v: f32) -> NodeOutputId {
        self.fb
            .build_float_const(v.to_bits() as u64, NodeOutputType::F32)
    }
    pub fn float_bits(&mut self, bits: u64, ty: NodeOutputType) -> NodeOutputId {
        self.fb.build_float_const(bits, ty)
    }

    // ── Integer ops ───────────────────────────────────────────────────────────

    pub fn add(&mut self, l: NodeOutputId, r: NodeOutputId) -> NodeOutputId {
        self.int_bin(l, r, IntBinaryOp::Add)
    }
    /// Builds the canonical lowered shape for `l - r`: `Add(l, Neg(r))`.
    /// `IntBinaryOp::Sub` is not a primitive; pcode-lift produces this shape.
    pub fn sub(&mut self, l: NodeOutputId, r: NodeOutputId) -> NodeOutputId {
        let neg = self.int_un(r, IntUnaryOp::Neg);
        self.int_bin(l, neg, IntBinaryOp::Add)
    }
    pub fn mul(&mut self, l: NodeOutputId, r: NodeOutputId) -> NodeOutputId {
        self.int_bin(l, r, IntBinaryOp::Mul)
    }
    pub fn band(&mut self, l: NodeOutputId, r: NodeOutputId) -> NodeOutputId {
        self.int_bin(l, r, IntBinaryOp::And)
    }
    pub fn bxor(&mut self, l: NodeOutputId, r: NodeOutputId) -> NodeOutputId {
        self.int_bin(l, r, IntBinaryOp::Xor)
    }
    pub fn bor(&mut self, l: NodeOutputId, r: NodeOutputId) -> NodeOutputId {
        self.int_bin(l, r, IntBinaryOp::Or)
    }
    pub fn shl(&mut self, l: NodeOutputId, r: NodeOutputId) -> NodeOutputId {
        self.int_bin(l, r, IntBinaryOp::ShiftLeft)
    }
    pub fn int_bin(&mut self, l: NodeOutputId, r: NodeOutputId, op: IntBinaryOp) -> NodeOutputId {
        self.int_bin_at(l, r, op, NodeOutputType::I64)
    }
    pub fn int_bin_at(
        &mut self,
        l: NodeOutputId,
        r: NodeOutputId,
        op: IntBinaryOp,
        ty: NodeOutputType,
    ) -> NodeOutputId {
        self.fb
            .build_int_binary_operation(l, r, op, ty)
            .expect("int_binary_operation")
    }
    pub fn neg(&mut self, v: NodeOutputId) -> NodeOutputId {
        self.int_un(v, IntUnaryOp::BitNot)
    }
    pub fn int_un(&mut self, v: NodeOutputId, op: IntUnaryOp) -> NodeOutputId {
        self.fb
            .build_int_unary_operation(v, op, NodeOutputType::I64)
            .expect("int_unary_operation")
    }
    pub fn int_un_at(
        &mut self,
        v: NodeOutputId,
        op: IntUnaryOp,
        ty: NodeOutputType,
    ) -> NodeOutputId {
        self.fb
            .build_int_unary_operation(v, op, ty)
            .expect("int_unary_operation")
    }
    pub fn int_cmp(&mut self, l: NodeOutputId, r: NodeOutputId, op: IntCmpOp) -> NodeOutputId {
        self.fb
            .build_int_cmp_operation(l, r, op, NodeOutputType::I64)
            .expect("int_cmp_operation")
    }
    pub fn int_cmp_at(
        &mut self,
        l: NodeOutputId,
        r: NodeOutputId,
        op: IntCmpOp,
        ty: NodeOutputType,
    ) -> NodeOutputId {
        self.fb
            .build_int_cmp_operation(l, r, op, ty)
            .expect("int_cmp_operation")
    }
    pub fn popcount(&mut self, v: NodeOutputId) -> NodeOutputId {
        self.fb
            .build_popcount(v, NodeOutputType::I64)
            .expect("popcount")
    }
    pub fn lzcount(&mut self, v: NodeOutputId) -> NodeOutputId {
        self.fb
            .build_lzcount(v, NodeOutputType::I64)
            .expect("lzcount")
    }

    // ── Boolean ops ───────────────────────────────────────────────────────────

    // Booleans are 1-bit (`I1`) integers: a boolean binary op is an
    // `IntBinaryOp` (`And` / `Or` / `Xor`) at `I1`, and a logical NOT is an
    // `IntUnaryOp::BitNot` at `I1`.
    pub fn bool_bin(
        &mut self,
        l: NodeOutputId,
        r: NodeOutputId,
        op: IntBinaryOp,
    ) -> NodeOutputId {
        self.fb
            .build_int_binary_operation(l, r, op, NodeOutputType::I1)
            .expect("boolean_operation")
    }
    pub fn bool_un(&mut self, v: NodeOutputId, op: IntUnaryOp) -> NodeOutputId {
        self.fb
            .build_int_unary_operation(v, op, NodeOutputType::I1)
            .expect("boolean_unary_operation")
    }

    // ── Float ops ─────────────────────────────────────────────────────────────

    pub fn fbin(
        &mut self,
        l: NodeOutputId,
        r: NodeOutputId,
        op: FloatBinaryOp,
        ty: NodeOutputType,
    ) -> NodeOutputId {
        self.fb
            .build_float_binary_op(l, r, op, ty)
            .expect("float_binary_op")
    }
    pub fn fun(&mut self, v: NodeOutputId, op: FloatUnaryOp, ty: NodeOutputType) -> NodeOutputId {
        self.fb
            .build_float_unary_op(v, op, ty)
            .expect("float_unary_op")
    }
    pub fn fcmp(&mut self, l: NodeOutputId, r: NodeOutputId, op: FloatCmpOp) -> NodeOutputId {
        self.fb.build_float_cmp_op(l, r, op).expect("float_cmp_op")
    }
    pub fn int_to_float(&mut self, v: NodeOutputId, ty: NodeOutputType) -> NodeOutputId {
        self.fb.build_int_to_float(v, ty).expect("int_to_float")
    }
    pub fn float_to_int(&mut self, v: NodeOutputId, ty: NodeOutputType) -> NodeOutputId {
        self.fb.build_float_to_int(v, ty).expect("float_to_int")
    }
    pub fn float_to_float(&mut self, v: NodeOutputId, ty: NodeOutputType) -> NodeOutputId {
        self.fb.build_float_to_float(v, ty).expect("float_to_float")
    }
    pub fn int_bits_to_float(&mut self, v: NodeOutputId, ty: NodeOutputType) -> NodeOutputId {
        self.fb
            .build_int_bits_to_float(v, ty)
            .expect("int_bits_to_float")
    }
    pub fn float_bits_to_int(&mut self, v: NodeOutputId, ty: NodeOutputType) -> NodeOutputId {
        self.fb
            .build_float_bits_to_int(v, ty)
            .expect("float_bits_to_int")
    }
    pub fn cast_to_float(&mut self, v: NodeOutputId, ty: NodeOutputType) -> NodeOutputId {
        self.fb.build_cast_to_float(v, ty)
    }

    // ── Casts / coercions ─────────────────────────────────────────────────────

    pub fn zext_to(&mut self, v: NodeOutputId, ty: NodeOutputType) -> NodeOutputId {
        self.fb
            .extend_if_needed(v, ty, ExtendOp::ZeroExtend)
            .expect("zero_extend")
    }
    pub fn sext_to(&mut self, v: NodeOutputId, ty: NodeOutputType) -> NodeOutputId {
        self.fb
            .extend_if_needed(v, ty, ExtendOp::SignExtend)
            .expect("sign_extend")
    }
    pub fn trunc_to(&mut self, v: NodeOutputId, ty: NodeOutputType) -> NodeOutputId {
        self.fb.truncate_if_needed(v, ty).expect("truncate")
    }
    pub fn as_bool(&mut self, v: NodeOutputId) -> NodeOutputId {
        // Booleans are 1-bit (`I1`) integers; coerce to that width.
        self.fb
            .convert_to_int_if_needed(v, NodeOutputType::I1)
            .expect("as_bool")
    }
    pub fn as_int(&mut self, v: NodeOutputId, ty: NodeOutputType) -> NodeOutputId {
        self.fb.convert_to_int_if_needed(v, ty).expect("as_int")
    }

    // ── Memory ────────────────────────────────────────────────────────────────

    pub fn store_ram(&mut self, addr: NodeOutputId, data: NodeOutputId) {
        self.fb
            .build_store(addr, data, rsleigh::VnSpace::RAM)
            .expect("store");
    }
    pub fn store_in(&mut self, addr: NodeOutputId, data: NodeOutputId, space: rsleigh::VnSpace) {
        self.fb.build_store(addr, data, space).expect("store");
    }
    pub fn load_ram(&mut self, addr: NodeOutputId, ty: NodeOutputType) -> NodeOutputId {
        self.fb
            .build_load(addr, rsleigh::VnSpace::RAM, ty)
            .expect("load")
    }
    pub fn load_in(
        &mut self,
        addr: NodeOutputId,
        space: rsleigh::VnSpace,
        ty: NodeOutputType,
    ) -> NodeOutputId {
        self.fb.build_load(addr, space, ty).expect("load")
    }

    // ── Control / calls ───────────────────────────────────────────────────────

    pub fn call_at(&mut self, addr: u64) {
        let tgt = self.u64(addr);
        self.fb.build_call(tgt).expect("call");
    }
    pub fn call_addr(&mut self, addr: NodeOutputId) {
        self.fb.build_call(addr).expect("call");
    }

    /// Emits a `CallOther(user_op_id)` node via the modeled API.
    /// Returns the ret-value output when `ret_ty` is `Some`.
    pub fn call_other(
        &mut self,
        name: &str,
        user_op_id: u64,
        args: &[NodeOutputId],
        ret_ty: Option<NodeOutputType>,
        implicit_reads: &[NodeOutputId],
        implicit_writes: &[rsleigh::Vn],
    ) -> Option<NodeOutputId> {
        let implicit_write_kinds: Vec<strider_ir::node::NodeOutputKind> = implicit_writes
            .iter()
            .map(|vn| {
                strider_ir::node::NodeOutputKind::OutputType(
                    strider_ir::ValueType::int_for_byte_size(vn.size).expect("vn size -> output type"),
                )
            })
            .collect();
        let (_node, value, _clobber_outs) = self
            .fb
            .build_call_other_modeled(
                user_op_id,
                name,
                args,
                ret_ty,
                implicit_reads,
                implicit_writes,
                &implicit_write_kinds,
            )
            .expect("call_other");
        value
    }

    // ── Variables ─────────────────────────────────────────────────────────────

    pub fn read_var(&mut self, vn: &rsleigh::Vn) -> NodeOutputId {
        self.fb.read_variable(vn).expect("read_variable")
    }
    pub fn write_var(&mut self, vn: &rsleigh::Vn, v: NodeOutputId) {
        self.fb.write_variable(vn, v).expect("write_variable")
    }

    // ── Regions / branches ────────────────────────────────────────────────────

    pub fn region(&mut self) -> strider_ir::RegionId {
        self.fb.create_region().expect("create_region")
    }
    pub fn enter(&mut self, r: strider_ir::RegionId) {
        self.fb.set_region(r);
    }
    pub fn branch(&mut self, dst: strider_ir::RegionId) {
        self.fb.build_branch(dst).expect("build_branch");
    }
    pub fn build_if(&mut self, cond: NodeOutputId, t: strider_ir::RegionId, f: strider_ir::RegionId) {
        self.fb.build_if(cond, t, f).expect("build_if");
    }

    // ── Finalisation ──────────────────────────────────────────────────────────

    /// Emits `Return(v)` in the current region and finalises the graph.
    pub fn ret_val(mut self, v: NodeOutputId) -> strider_ir::Function {
        self.fb.build_return(Some(v), &[]).expect("build_return");
        self.fb.build().expect("FunctionBuilder::build (validator)")
    }

    /// `return(IntConst(v) : I64)` — convenience for the one-constant graph.
    pub fn ret_const(mut self, v: u64) -> strider_ir::Function {
        let c = self.u64(v);
        self.ret_val(c)
    }

    /// Emits `Return()` with no data value and finalises the graph.
    pub fn ret_nothing(mut self) -> strider_ir::Function {
        self.fb.build_return(None, &[]).expect("build_return");
        self.fb.build().expect("FunctionBuilder::build (validator)")
    }

    /// Emits `Return()` in the current region, plus return registers, and
    /// finalises the graph.
    pub fn ret_regs(mut self, regs: &[rsleigh::Vn]) -> strider_ir::Function {
        self.fb.build_return(None, regs).expect("build_return");
        self.fb.build().expect("FunctionBuilder::build (validator)")
    }

    /// Finalises the graph without emitting any extra instructions — caller
    /// has already emitted the terminator(s) themselves.
    pub fn finish(self) -> strider_ir::Function {
        self.fb.build().expect("FunctionBuilder::build (validator)")
    }
}
