//! `read_reg_vn` truncates a sub-register read to the sub-register's declared
//! width even when its offset inside the container is zero (shift == 0), i.e.
//! it always calls `truncate_if_needed(shifted, reg_ty)`.
//!
//! ARM soft-float ABI `s0` (4-byte float arg/return) lives at offset 0 inside
//! the 8-byte `d0`. An untruncated read yields the 8-byte `d0` value (I64),
//! which flows into `IntBitsToFloat(F32)`, whose signature requires an I32
//! input, and fails validation.
//!
//! The surface is the `f32_arith` fixture on ARM / MIPS32 soft-float targets,
//! where the compiler lowers `float` args as raw integer bits in integer
//! registers and the float-register view (`s0`/`f12`) is a 4-byte
//! sub-register of an 8-byte container.
//!
//! `write_reg_vn` uses positioned reg_mask + container-domain
//! container_mask so x64 and aarch64 round-trip cleanly. x86 goes through
//! the 80-bit x87 stack instead (F80/I80 ValueType, ST0 in the x86 cdecl
//! float-return regs).

mod common;
use common::*;
use strider_ir::IRViewer;
use strider_ir::node::NodeKind;

/// `f32_arith(float, float)` performs four float binary ops (+-x/) via
/// soft-float library calls (ARM/MIPS without FPU) or native FP
/// instructions (hardware-FPU arches). Hardware-FPU arches aren't part of
/// this guard: ConstantFold collapses the register-merge chain before the
/// FloatBinaryOp assertion would apply.
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
                    strider_ir::node::ValueKind::Typed(strider_ir::node::ValueType::I64),
                    "IntBitsToFloat node received a I64 input: \
                     read_reg_vn must truncate the sub-register to its declared \
                     width (I32 for s0 / f12) before passing it to this node"
                );
            }
        }
    }
}

// An untruncated sub-register read fails IR validation here with
// "Typed(I64), expected AnyInt(I32)" from IntBitsToFloat's signature.

// PPC FPRs (f0-f31) are natively 8 bytes, the whole register being the only
// view of it, so the I32-input assertion doesn't apply: PPC correctly produces
// I64 there, which the assertion would reject.
per_arch_test!("floats", "f32_arith", f32_arith_graph_is_valid, ignore = {
    Ppc32be: "PPC FPRs are natively 8-byte; the I32-input assertion doesn't apply",
    Ppc32le: "PPC FPRs are natively 8-byte; the I32-input assertion doesn't apply",
    Ppc64be: "PPC FPRs are natively 8-byte; the I32-input assertion doesn't apply",
    Ppc64le: "PPC FPRs are natively 8-byte; the I32-input assertion doesn't apply",
});
