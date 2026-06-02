//! Width-changing and type-converting cast patterns.
//!
//! Covers: `zero_extend`, `sign_extend`, `extend(ExtendOp::…)`, `truncate`,
//! `cast_to_float`, `int_to_float`, `float_to_int`, `float_to_float`,
//! `int_bits_to_float`, `float_bits_to_int`.
//!
//! Because most cast producers are introduced implicitly by coercion helpers
//! on `FunctionBuilder`, tests use those helpers to create the target nodes
//! and then match against them with the corresponding pattern constructor.

use strider_pattern::*;
use strider_pattern::matcher::CastMask;
use strider_ir::ExtendOp;
use strider_ir::node::ValueType;

use super::support::{Tb, assertions as a, reg_vn};

// Note on constant folding: `extend_if_needed`, `truncate_if_needed`, and
// `convert_to_*` on `IntConst` / `BoolConst` inputs immediately fold to a
// new const rather than emitting an Extend / Truncate / CastTo* node.  To
// exercise the cast nodes themselves each helper below threads the value
// through an `Add` of two constants so the operand is non-const.

fn non_const_u32(t: &mut Tb, a_v: u64, b_v: u64) -> strider_ir::node::ValueId {
    let a_ = t.u32(a_v);
    let b_ = t.u32(b_v);
    t.int_bin_at(a_, b_, strider_ir::IntBinaryOp::Add, ValueType::I32)
}

fn non_const_u64(t: &mut Tb, a_v: u64, b_v: u64) -> strider_ir::node::ValueId {
    let a_ = t.u64(a_v);
    let b_ = t.u64(b_v);
    t.add(a_, b_)
}

// ── Zero / sign extend, truncate ─────────────────────────────────────────────

#[test]
fn zero_extend_matches() {
    let mut t = Tb::empty();
    let s = non_const_u32(&mut t, 1, 2);
    let x = t.zext_to(s, ValueType::I64);
    let function = t.ret_val(x);
    a::matches(&function, zero_extend(any()).into_pattern(), 1);
}

#[test]
fn sign_extend_matches() {
    let mut t = Tb::empty();
    let s = non_const_u32(&mut t, 1, 2);
    let x = t.sext_to(s, ValueType::I64);
    let function = t.ret_val(x);
    a::matches(&function, sign_extend(any()).into_pattern(), 1);
}

#[test]
fn extend_op_variant_matches_zero_and_sign() {
    // Zero-extend graph.
    let mut t = Tb::empty();
    let s = non_const_u32(&mut t, 1, 2);
    let x = t.zext_to(s, ValueType::I64);
    let function = t.ret_val(x);
    a::matches(&function, extend(ExtendOp::ZeroExtend, any()).into_pattern(), 1);
    a::none(&function, extend(ExtendOp::SignExtend, any()).into_pattern());

    // Sign-extend graph.
    let mut t = Tb::empty();
    let s = non_const_u32(&mut t, 1, 2);
    let x = t.sext_to(s, ValueType::I64);
    let function = t.ret_val(x);
    a::matches(&function, extend(ExtendOp::SignExtend, any()).into_pattern(), 1);
    a::none(&function, extend(ExtendOp::ZeroExtend, any()).into_pattern());
}

#[test]
fn truncate_matches() {
    let mut t = Tb::empty();
    let s = non_const_u64(&mut t, 0xAABBCCDD, 1);
    let x = t.trunc_to(s, ValueType::I8);
    let function = t.ret_val(x);
    a::matches(&function, truncate(any()).into_pattern(), 1);
}

#[test]
fn extend_then_truncate_chain_matches() {
    // Non-const I32 → I64 (zero-extend) → I8 (truncate).
    let mut t = Tb::empty();
    let s = non_const_u32(&mut t, 1, 2);
    let ext = t.zext_to(s, ValueType::I64);
    let tr = t.trunc_to(ext, ValueType::I8);
    let function = t.ret_val(tr);

    a::matches(&function, truncate(any()).into_pattern(), 1);
    a::matches(&function, truncate(zero_extend(any())).into_pattern(), 1);
}

// (The `CastToFloat` matching test was removed with the node kind: an
// int→float cast is now `IntBitsToFloat`, covered by
// `int_bits_to_float_matches` below.)

// ── Int ↔ Float conversions ──────────────────────────────────────────────────

#[test]
fn int_to_float_matches() {
    let mut t = Tb::empty();
    let v = t.u64(42);
    let f = t.int_to_float(v, ValueType::F64);
    let as_int = t.float_to_int(f, ValueType::I64);
    let function = t.ret_val(as_int);
    a::matches(&function, int_to_float(any()).into_pattern(), 1);
}

#[test]
fn float_to_int_matches() {
    let mut t = Tb::empty();
    let v = t.f64(1.5);
    let i = t.float_to_int(v, ValueType::I64);
    let function = t.ret_val(i);
    a::matches(&function, float_to_int(any()).into_pattern(), 1);
}

#[test]
fn float_to_float_matches() {
    let mut t = Tb::empty();
    let v = t.f64(1.0);
    let f = t.float_to_float(v, ValueType::F32);
    let ff = t.float_to_float(f, ValueType::F64);
    let as_int = t.float_to_int(ff, ValueType::I64);
    let function = t.ret_val(as_int);
    // There are two FloatToFloat nodes.
    a::matches(&function, float_to_float(any()).into_pattern(), 2);
}

#[test]
fn int_bits_to_float_matches() {
    let mut t = Tb::empty();
    // `build_int_bits_to_float` on a const folds immediately; use a
    // non-const input (an Add) so a real IntBitsToFloat node is emitted.
    let a_ = t.u64(1);
    let b_ = t.u64(2);
    let s = t.add(a_, b_);
    let f = t.int_bits_to_float(s, ValueType::F64);
    let as_int = t.float_to_int(f, ValueType::I64);
    let function = t.ret_val(as_int);
    a::matches(&function, int_bits_to_float(any()).into_pattern(), 1);
}

#[test]
fn float_bits_to_int_matches() {
    let mut t = Tb::empty();
    let fa = t.f64(1.0);
    let fb = t.f64(2.0);
    let s = t.fbin(fa, fb, strider_ir::FloatBinaryOp::Add, ValueType::F64);
    let i = t.float_bits_to_int(s, ValueType::I64);
    let function = t.ret_val(i);
    a::matches(&function, float_bits_to_int(any()).into_pattern(), 1);
}

// ── Cross-kind rejection ─────────────────────────────────────────────────────

#[test]
fn cast_patterns_are_kind_sensitive() {
    // Graph has a ZeroExtend; patterns for unrelated casts must not match.
    let mut t = Tb::empty();
    let v = t.u32(1);
    let x = t.zext_to(v, ValueType::I64);
    let function = t.ret_val(x);

    a::none(&function, truncate(any()).into_pattern());
    a::none(&function, int_to_float(any()).into_pattern());
    a::none(&function, int_bits_to_float(any()).into_pattern());
}

// ── ignore_casts_mask walk-through (mask lives on the Pattern) ─────────────────

/// `Add(IntConst(5), ZeroExtend(reg))` at I64 where the extend's input is a
/// 4-byte tracked register read (so the IR builder does not fold the cast).
fn add_with_zext_reg_operand() -> strider_ir::Function {
    let vn = reg_vn(0x40, 4); // 4-byte register varnode → I32
    let mut t = Tb::with_vars(&[vn]);
    let x32 = t.read_var(&vn);
    let zx = t.zext_to(x32, ValueType::I64);
    let five = t.u64(5);
    let sum = t.add(five, zx);
    t.ret_val(sum)
}

/// A strict `int_const` sub-pattern does NOT walk through a ZeroExtend: the
/// walk-through fallback only engages on a *kind-mismatch* against the
/// cast's *input*, and that input is a register read (not an IntConst).
#[test]
fn ignore_casts_mask_does_not_spuriously_match_strict_const() {
    let function = add_with_zext_reg_operand();
    let pat_strict = add(int_const(5u128), any_int_const()).into_pattern();
    // No mask: the IntConst sub-pattern kind-mismatches the ZeroExtend.
    a::none(&function, pat_strict);

    // With ZERO_EXTEND the matcher unwraps the cast and retries against the
    // register read — still not an IntConst, so the strict pattern fails.
    let pat_walk = add(int_const(5u128), any_int_const())
        .into_pattern()
        .ignore_casts_mask(CastMask::ZERO_EXTEND);
    a::none(&function, pat_walk);
}

/// With `CastMask::ZERO_EXTEND` set on the Pattern, `add(int_const(5),
/// var(c))` still matches once (the direct producer `var(c)` accepts the
/// ZeroExtend output before the walk-through fallback engages).
#[test]
fn ignore_casts_mask_zero_extend_matches_var_capture() {
    let function = add_with_zext_reg_operand();
    let c = Capture::new();
    let pat = add(int_const(5u128), var(c))
        .into_pattern()
        .ignore_casts_mask(CastMask::ZERO_EXTEND);
    let m = a::unique(&function, pat);
    let value = m.value(c).expect("c must bind under walk-through");
    let node = function.producer(value);
    assert!(
        matches!(
            function.node_kind(node),
            strider_ir::node::NodeKind::Extend(ExtendOp::ZeroExtend)
        ),
        "var(c) accepts the direct ZeroExtend producer, got {:?}",
        function.node_kind(node)
    );
}
