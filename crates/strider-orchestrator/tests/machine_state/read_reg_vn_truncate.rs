//! A sub-register read at container offset 0 (ARM soft-float `s0` in `d0`)
//! is truncated to its own width.

use crate::common::*;
use strider_ir::IRViewer;
use strider_ir::node::NodeKind;

/// `f32_arith`'s four float ops lift to `FloatBinaryOp`s (hardware FPU) or
/// library calls (soft-float).
fn f32_arith_graph_is_valid(function: &strider_ir::Function) {
    assert!(count_returns(function) >= 1, "f32_arith must have a Return");

    // Soft-float ABIs lower the four ops to library calls; accept either
    // FloatBinaryOp or Call nodes as evidence they lowered without a type
    // error. `FloatBinaryOp::Sub` isn't a primitive (lowered to
    // `Add(_, Neg(_))`), so the Add count subsumes subtraction.
    let float_ops = count_float_binop(function, strider_ir::FloatBinaryOp::Add)
        + count_float_binop(function, strider_ir::FloatBinaryOp::Mul)
        + count_float_binop(function, strider_ir::FloatBinaryOp::Div);
    let calls = count_calls(function);
    assert!(
        float_ops >= 1 || calls >= 1,
        "f32_arith must contain FloatBinaryOp nodes (hardware FPU) or library \
         Call nodes (soft-float); got {float_ops} float ops and {calls} calls"
    );

    // No IntBitsToFloat may have a I64 input: that would mean
    // read_reg_vn failed to truncate s0/f12 to I32.
    for nid in function.graph().all_node_ids() {
        if matches!(function.node_kind(nid), NodeKind::IntBitsToFloat) {
            let inputs: Vec<_> = function.node_inputs(nid).into_iter().collect();
            if let Some(input) = inputs.first() {
                let kind = function.value_kind(*input);
                assert_ne!(
                    kind,
                    strider_ir::node::ValueKind::Typed(strider_ir::node::ValueType::int(64)),
                    "IntBitsToFloat node received a I64 input: \
                     read_reg_vn must truncate the sub-register to its declared \
                     width (I32 for s0 / f12) before passing it to this node"
                );
            }
        }
    }
}

// An untruncated sub-register read fails IR validation here with
// "Typed(I64), expected AnyInt(I32)" from IntBitsToFloat's signature. PPC
// FPRs are natively 8 bytes, so PPC rightly produces I64 there.
#[test]
fn f32_arith_truncates_sub_register_reads() {
    let ppc = [Arch::Ppc32be, Arch::Ppc32le, Arch::Ppc64be, Arch::Ppc64le];
    for &arch in ALL_ARCHES.iter().filter(|a| !ppc.contains(a)) {
        f32_arith_graph_is_valid(&analyze(arch, "floats", "f32_arith"));
    }
}
