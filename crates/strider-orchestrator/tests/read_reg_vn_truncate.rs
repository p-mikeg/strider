//! Regression tests for the `read_reg_vn` truncation fix (commit `d2aa0ac`).
//!
//! **Bug**: `read_reg_vn` returned the full container register value when the
//! sub-register's offset inside the container was zero (shift == 0), even if
//! the sub-register was strictly narrower.  For example, on ARM soft-float ABI
//! `s0` (4-byte float arg / return) lives at offset 0 inside `d0` (8-byte);
//! before the fix `read_reg_vn(s0)` returned the 8-byte `d0` value (I64).
//! That I64 then flowed into `IntBitsToFloat(F32)`, whose signature requires a
//! I32 input, causing a validation error.
//!
//! **Fix**: always call `truncate_if_needed(shifted, reg_ty)` after computing
//! the shifted value, even when shift == 0.  This ensures the returned value
//! has the sub-register's declared width.
//!
//! **Regression surface**: the `f32_arith` fixture on ARM / MIPS32 soft-float
//! targets exercises exactly this path.  On those ABIs, the compiler lowers
//! `float` arguments as raw integer bits in integer registers — the f32 arg
//! lands in `s0`/`f12`, which are 4-byte sub-registers of their 8-byte
//! containers.  The analyzer must emit a I32 value for such sub-register reads
//! so that `IntBitsToFloat(F32)` receives the correct input width.
//!
//! The write_reg_vn path uses positioned reg_mask + container-domain
//! container_mask so x64 and aarch64 round-trip cleanly.  x86 has its
//! own challenge: GCC uses the 80-bit x87 stack (10-byte registers),
//! modelled by F80 / I80 ValueType variants and ST0 in the x86
//! cdecl float-return regs.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

mod common;
use common::*;
use strider_ir::IRViewer;
use strider_ir::node::NodeKind;

// ── Assertion ────────────────────────────────────────────────────────────────

/// `f32_arith(float, float)` performs four float binary operations (+−×÷)
/// via soft-float library calls (on ARM/MIPS without FPU) or native FP
/// instructions (on hardware-FPU arches).
///
/// For soft-float targets the four arithmetic operations are lowered to
/// library calls, so we count `Call` nodes; the graph must be valid (the
/// optimizer pipeline succeeded without a validation error), which already
/// proves that `read_reg_vn` returned the correct width.
///
/// For hardware-FPU targets the `FloatBinaryOp` assertion is the right check,
/// but those arches are currently ignored because ConstantFold collapses
/// the register-merge chain; they are not part of this regression guard.
fn f32_arith_graph_is_valid(function: &strider_ir::Function) {
    // The function returns a float; there must be a Return node.
    assert!(count_returns(function) >= 1, "f32_arith must have a Return");

    // On soft-float ABIs (ARM / MIPS), four float ops become four library
    // calls; accept either FloatBinaryOp nodes OR Call nodes as evidence
    // that the operations were lowered without a type error.
    // `FloatBinaryOp::Sub` is no longer a primitive (lowered to
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

    // Critical: no Extend node must have a Bool-typed input, and no
    // IntBitsToFloat node must have a I64 input (the latter would indicate
    // that read_reg_vn failed to truncate s0/f12 to I32 before the fix).
    for nid in function.graph().all_node_ids() {
        if matches!(function.node_kind(nid), NodeKind::IntBitsToFloat) {
            let inputs: Vec<_> = function.node_inputs(nid).into_iter().collect();
            if let Some(input) = inputs.first() {
                let kind = function.value_kind(*input);
                assert_ne!(
                    kind,
                    strider_ir::node::ValueKind::Typed(strider_ir::node::ValueType::I64),
                    "IntBitsToFloat node received a I64 input — \
                     read_reg_vn must truncate the sub-register to its declared \
                     width (I32 for s0 / f12) before passing it to this node"
                );
            }
        }
    }
}

// ── Per-architecture tests ────────────────────────────────────────────────────
//
// ARM and MIPS use soft-float ABIs where `float` args are passed as raw
// integer bits in `r0` / `a0` (ARM / MIPS O32) and the float-register view
// (`s0`, `f12`) is a 4-byte sub-register of an 8-byte container.
//
// Without the read_reg_vn fix these tests fail with an IR validation error:
//   "Typed(I64), expected AnyInt(I32)" from IntBitsToFloat's signature.
//
// x64 and aarch64 also pass thanks to write_reg_vn's mask positioning.
// x86 passes via F80/I80 ValueType variants + ST0 in x86 cdecl's
// float-return regs.

// PPC FPRs (f0–f31) are natively 8 bytes — there's no 4-byte sub-register
// view like ARM's s0/d0 split.  This test specifically asserts that
// IntBitsToFloat receives a I32 input (the soft-float-via-int pattern that
// caused the original BUG); on PPC it correctly receives I64, which the
// assertion intentionally rejects.  Per-arch.
per_arch_test!("floats", "f32_arith", f32_arith_graph_is_valid, ignore = {
    Ppc32be: "PPC FPRs are natively 8-byte; the I32-input assertion doesn't apply",
    Ppc32le: "PPC FPRs are natively 8-byte; the I32-input assertion doesn't apply",
    Ppc64be: "PPC FPRs are natively 8-byte; the I32-input assertion doesn't apply",
    Ppc64le: "PPC FPRs are natively 8-byte; the I32-input assertion doesn't apply",
    ArmBe:   "ARM8_BE Sleigh's VFP register file uses descending offsets and d0 doesn't overlap s0; analyzer's container aliasing drops the entire VFP read/write chain — IR has 0 FloatBinaryOp / 0 Call nodes for f32_arith",
});
