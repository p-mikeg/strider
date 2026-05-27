//! Width-changing and type-converting cast patterns.
//!
//! Covers: `zero_extend`, `sign_extend`, `extend(ExtendOp::…)`, `truncate`,
//! `cast_to_float`, `int_to_float`, `float_to_int`, `float_to_float`,
//! `int_bits_to_float`, `float_bits_to_int`.
//!
//! Because most cast producers are introduced implicitly by coercion helpers
//! on `FunctionBuilder`, tests use those helpers to create the target nodes
//! and then match against them with the corresponding pattern constructor.

use strider_analyze::pattern::*;
use strider_ir::ExtendOp;
use strider_ir::node::NodeOutputType;

use super::support::{Tb, assertions as a};

// Note on constant folding: `extend_if_needed`, `truncate_if_needed`, and
// `convert_to_*` on `IntConst` / `BoolConst` inputs immediately fold to a
// new const rather than emitting an Extend / Truncate / CastTo* node.  To
// exercise the cast nodes themselves each helper below threads the value
// through an `Add` of two constants so the operand is non-const.

fn non_const_u32(t: &mut Tb, a_v: u64, b_v: u64) -> strider_ir::node::NodeOutputId {
    let a_ = t.u32(a_v);
    let b_ = t.u32(b_v);
    t.int_bin_at(a_, b_, strider_ir::IntBinaryOp::Add, NodeOutputType::I32)
}

fn non_const_u64(t: &mut Tb, a_v: u64, b_v: u64) -> strider_ir::node::NodeOutputId {
    let a_ = t.u64(a_v);
    let b_ = t.u64(b_v);
    t.add(a_, b_)
}

// ── Zero / sign extend, truncate ─────────────────────────────────────────────

#[test]
fn zero_extend_matches() {
    let mut t = Tb::empty();
    let s = non_const_u32(&mut t, 1, 2);
    let x = t.zext_to(s, NodeOutputType::I64);
    let function = t.ret_val(x);
    a::matches(&function, zero_extend(any()), 1);
}

#[test]
fn sign_extend_matches() {
    let mut t = Tb::empty();
    let s = non_const_u32(&mut t, 1, 2);
    let x = t.sext_to(s, NodeOutputType::I64);
    let function = t.ret_val(x);
    a::matches(&function, sign_extend(any()), 1);
}

#[test]
fn extend_op_variant_matches_zero_and_sign() {
    // Zero-extend graph.
    let mut t = Tb::empty();
    let s = non_const_u32(&mut t, 1, 2);
    let x = t.zext_to(s, NodeOutputType::I64);
    let function = t.ret_val(x);
    a::matches(&function, extend(ExtendOp::ZeroExtend, any()), 1);
    a::none(&function, extend(ExtendOp::SignExtend, any()));

    // Sign-extend graph.
    let mut t = Tb::empty();
    let s = non_const_u32(&mut t, 1, 2);
    let x = t.sext_to(s, NodeOutputType::I64);
    let function = t.ret_val(x);
    a::matches(&function, extend(ExtendOp::SignExtend, any()), 1);
    a::none(&function, extend(ExtendOp::ZeroExtend, any()));
}

#[test]
fn truncate_matches() {
    let mut t = Tb::empty();
    let s = non_const_u64(&mut t, 0xAABBCCDD, 1);
    let x = t.trunc_to(s, NodeOutputType::I8);
    let function = t.ret_val(x);
    a::matches(&function, truncate(any()), 1);
}

#[test]
fn extend_then_truncate_chain_matches() {
    // Non-const I32 → I64 (zero-extend) → I8 (truncate).
    let mut t = Tb::empty();
    let s = non_const_u32(&mut t, 1, 2);
    let ext = t.zext_to(s, NodeOutputType::I64);
    let tr = t.trunc_to(ext, NodeOutputType::I8);
    let function = t.ret_val(tr);

    a::matches(&function, truncate(any()), 1);
    a::matches(&function, truncate(zero_extend(any())), 1);
}

// (The `CastToFloat` matching test was removed with the node kind: an
// int→float cast is now `IntBitsToFloat`, covered by
// `int_bits_to_float_matches` below.)

// ── Int ↔ Float conversions ──────────────────────────────────────────────────

#[test]
fn int_to_float_matches() {
    let mut t = Tb::empty();
    let v = t.u64(42);
    let f = t.int_to_float(v, NodeOutputType::F64);
    let as_int = t.float_to_int(f, NodeOutputType::I64);
    let function = t.ret_val(as_int);
    a::matches(&function, int_to_float(any()), 1);
}

#[test]
fn float_to_int_matches() {
    let mut t = Tb::empty();
    let v = t.f64(1.5);
    let i = t.float_to_int(v, NodeOutputType::I64);
    let function = t.ret_val(i);
    a::matches(&function, float_to_int(any()), 1);
}

#[test]
fn float_to_float_matches() {
    let mut t = Tb::empty();
    let v = t.f64(1.0);
    let f = t.float_to_float(v, NodeOutputType::F32);
    let ff = t.float_to_float(f, NodeOutputType::F64);
    let as_int = t.float_to_int(ff, NodeOutputType::I64);
    let function = t.ret_val(as_int);
    // There are two FloatToFloat nodes.
    a::matches(&function, float_to_float(any()), 2);
}

#[test]
fn int_bits_to_float_matches() {
    let mut t = Tb::empty();
    // `build_int_bits_to_float` on a const folds immediately; use a
    // non-const input (an Add) so a real IntBitsToFloat node is emitted.
    let a_ = t.u64(1);
    let b_ = t.u64(2);
    let s = t.add(a_, b_);
    let f = t.int_bits_to_float(s, NodeOutputType::F64);
    let as_int = t.float_to_int(f, NodeOutputType::I64);
    let function = t.ret_val(as_int);
    a::matches(&function, int_bits_to_float(any()), 1);
}

#[test]
fn float_bits_to_int_matches() {
    let mut t = Tb::empty();
    let fa = t.f64(1.0);
    let fb = t.f64(2.0);
    let s = t.fbin(fa, fb, strider_ir::FloatBinaryOp::Add, NodeOutputType::F64);
    let i = t.float_bits_to_int(s, NodeOutputType::I64);
    let function = t.ret_val(i);
    a::matches(&function, float_bits_to_int(any()), 1);
}

// ── Cross-kind rejection ─────────────────────────────────────────────────────

#[test]
fn cast_patterns_are_kind_sensitive() {
    // Graph has a ZeroExtend; patterns for unrelated casts must not match.
    let mut t = Tb::empty();
    let v = t.u32(1);
    let x = t.zext_to(v, NodeOutputType::I64);
    let function = t.ret_val(x);

    a::none(&function, truncate(any()));
    a::none(&function, int_to_float(any()));
    a::none(&function, int_bits_to_float(any()));
}
