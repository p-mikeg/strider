use strider_ir::node::ValueType;
use strider_ir::{ExtendOp, IRViewer};
use strider_pattern::matcher::CastMask;
use strider_pattern::*;

use super::support::{Tb, assertions as a, reg_vn};

// `extend_if_needed` / `truncate_if_needed` / `convert_to_*` fold on const
// inputs instead of emitting a cast node, so the helpers below thread their
// value through an `Add` to keep the operand non-const.

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

#[test]
fn zero_extend_matches() {
    let mut t = Tb::empty();
    let s = non_const_u32(&mut t, 1, 2);
    let x = t.zext_to(s, ValueType::I64);
    let function = t.ret_val(x);
    a::matches(&function, int_zero_extend(anything()).into_pattern(), 1);
}

#[test]
fn sign_extend_matches() {
    let mut t = Tb::empty();
    let s = non_const_u32(&mut t, 1, 2);
    let x = t.sext_to(s, ValueType::I64);
    let function = t.ret_val(x);
    a::matches(&function, int_sign_extend(anything()).into_pattern(), 1);
}

#[test]
fn extend_op_variant_matches_zero_and_sign() {
    let mut t = Tb::empty();
    let s = non_const_u32(&mut t, 1, 2);
    let x = t.zext_to(s, ValueType::I64);
    let function = t.ret_val(x);
    a::matches(
        &function,
        int_extend(ExtendOp::ZeroExtend, anything()).into_pattern(),
        1,
    );
    a::none(
        &function,
        int_extend(ExtendOp::SignExtend, anything()).into_pattern(),
    );

    let mut t = Tb::empty();
    let s = non_const_u32(&mut t, 1, 2);
    let x = t.sext_to(s, ValueType::I64);
    let function = t.ret_val(x);
    a::matches(
        &function,
        int_extend(ExtendOp::SignExtend, anything()).into_pattern(),
        1,
    );
    a::none(
        &function,
        int_extend(ExtendOp::ZeroExtend, anything()).into_pattern(),
    );
}

#[test]
fn truncate_matches() {
    let mut t = Tb::empty();
    let s = non_const_u64(&mut t, 0xAABBCCDD, 1);
    let x = t.trunc_to(s, ValueType::I8);
    let function = t.ret_val(x);
    a::matches(&function, int_truncate(anything()).into_pattern(), 1);
}

#[test]
fn extend_then_truncate_chain_matches() {
    let mut t = Tb::empty();
    let s = non_const_u32(&mut t, 1, 2);
    let ext = t.zext_to(s, ValueType::I64);
    let tr = t.trunc_to(ext, ValueType::I8);
    let function = t.ret_val(tr);

    a::matches(&function, int_truncate(anything()).into_pattern(), 1);
    a::matches(
        &function,
        int_truncate(int_zero_extend(anything())).into_pattern(),
        1,
    );
}

#[test]
fn int_to_float_matches() {
    let mut t = Tb::empty();
    let v = t.u64(42);
    let f = t.int_to_float(v, ValueType::F64);
    let as_int = t.float_to_int(f, ValueType::I64);
    let function = t.ret_val(as_int);
    a::matches(&function, int_to_float(anything()).into_pattern(), 1);
}

#[test]
fn float_to_int_matches() {
    let mut t = Tb::empty();
    let v = t.f64(1.5);
    let i = t.float_to_int(v, ValueType::I64);
    let function = t.ret_val(i);
    a::matches(&function, float_to_int(anything()).into_pattern(), 1);
}

#[test]
fn float_to_float_matches() {
    let mut t = Tb::empty();
    let v = t.f64(1.0);
    let f = t.float_to_float(v, ValueType::F32);
    let ff = t.float_to_float(f, ValueType::F64);
    let as_int = t.float_to_int(ff, ValueType::I64);
    let function = t.ret_val(as_int);
    // Two FloatToFloat nodes in the graph.
    a::matches(&function, float_to_float(anything()).into_pattern(), 2);
}

#[test]
fn int_bits_to_float_matches() {
    let mut t = Tb::empty();
    // A const input folds, so feed an Add to emit a real IntBitsToFloat.
    let a_ = t.u64(1);
    let b_ = t.u64(2);
    let s = t.add(a_, b_);
    let f = t.int_bits_to_float(s, ValueType::F64);
    let as_int = t.float_to_int(f, ValueType::I64);
    let function = t.ret_val(as_int);
    a::matches(&function, int_bits_to_float(anything()).into_pattern(), 1);
}

#[test]
fn float_bits_to_int_matches() {
    let mut t = Tb::empty();
    let fa = t.f64(1.0);
    let fb = t.f64(2.0);
    let s = t.fbin(fa, fb, strider_ir::FloatBinaryOp::Add, ValueType::F64);
    let i = t.float_bits_to_int(s, ValueType::I64);
    let function = t.ret_val(i);
    a::matches(&function, float_bits_to_int(anything()).into_pattern(), 1);
}

#[test]
fn cast_patterns_are_kind_sensitive() {
    let mut t = Tb::empty();
    let v = t.u32(1);
    let x = t.zext_to(v, ValueType::I64);
    let function = t.ret_val(x);

    a::none(&function, int_truncate(anything()).into_pattern());
    a::none(&function, int_to_float(anything()).into_pattern());
    a::none(&function, int_bits_to_float(anything()).into_pattern());
}

/// `Add(IntConst(5), ZeroExtend(reg))` at I64 where the extend's input is a
/// 4-byte tracked register read (so the IR builder does not fold the cast).
fn add_with_zext_reg_operand() -> strider_ir::Function {
    let vn = reg_vn(0x40, 4); // 4-byte register varnode, so I32
    let mut t = Tb::with_vars(&[vn]);
    let x32 = t.read_var(&vn);
    let zx = t.zext_to(x32, ValueType::I64);
    let five = t.u64(5);
    let sum = t.add(five, zx);
    t.ret_val(sum)
}

/// A strict `int_const` sub-pattern does NOT walk through a ZeroExtend: the
/// walk-through fallback engages only on a kind-mismatch against the cast's
/// input, and that input is a register read.
#[test]
fn ignore_casts_mask_does_not_spuriously_match_strict_const() {
    let function = add_with_zext_reg_operand();
    let pat_strict = int_add(int_const(5u128), any_int_const()).into_pattern();
    // No mask: the IntConst sub-pattern kind-mismatches the ZeroExtend.
    a::none(&function, pat_strict);

    // With ZERO_EXTEND the matcher unwraps the cast and retries against the
    // register read, still not an IntConst.
    let pat_walk = int_add(int_const(5u128), any_int_const())
        .into_pattern()
        .ignore_casts_mask(CastMask::ZERO_EXTEND);
    a::none(&function, pat_walk);
}

/// `var(c)` accepts the ZeroExtend output as a direct producer, so the match
/// lands before the walk-through fallback ever engages.
#[test]
fn ignore_casts_mask_zero_extend_matches_var_capture() {
    let function = add_with_zext_reg_operand();
    let c = Capture::new();
    let pat = int_add(int_const(5u128), var(c))
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

/// The cast walk-through is an unbounded tail-loop: 32 alternating Truncate /
/// ZeroExtend casts between the `Add` and the `Mul` are skipped in one fallback
/// step. The `int_const` operand matches directly, since an active cast mask
/// never perturbs a leaf that already matches.
#[test]
fn deep_alternating_cast_chain_walked_through() {
    let function = {
        let mut t = Tb::empty();
        let two = t.u64(2);
        let three = t.u64(3);
        let mut v = t.mul(two, three);
        // 16 x (Truncate I64 to I32, ZeroExtend back) = 32 cast nodes.
        for _ in 0..16 {
            v = t.trunc_to(v, ValueType::I32);
            v = t.zext_to(v, ValueType::I64);
        }
        let four = t.u64(4);
        let total = t.add(v, four);
        t.ret_val(total)
    };

    // Without a cast mask the chain is opaque.
    a::none(
        &function,
        int_add(int_mul(anything(), anything()), int_const(4u128)).into_pattern(),
    );

    // With ignore_casts the matcher reaches the Mul through all 32 casts.
    a::matches(
        &function,
        int_add(
            int_mul(int_const(2u128), int_const(3u128)),
            int_const(4u128),
        )
        .into_pattern()
        .ignore_casts(),
        1,
    );
}
