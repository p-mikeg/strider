//! End-to-end smoke check for [`strider_orchestrator::opt::FlagCmpCanonicalize`].
//!
//! Lifts a hand-encoded AArch64 `cmp w0, #5; b.eq +8; ret; ret` byte
//! sequence through the full strider + opt pipeline and asserts that
//! the resulting `If` node's cond is a direct `IntCmpOp::Equal` —
//! i.e. the rule fired against real Sleigh-lifted IR (not a synthetic
//! `FunctionBuilder` fixture).
//!
//! Mirrors `tests/common/indirect_resolve_helpers::run_pipeline_x86_64`
//! shape but targets AArch64 and asserts on the post-pipeline cond
//! kind.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use rsleigh::Sleigh;
use strider_ir::IRViewer;
use rsleigh::mem_readers::BufMemReader;
use strider_ir::Function;
use strider_ir::node::{NodeId, NodeKind};
use strider_cfg::Builder;
use strider_cfg::CfgOptions;
use strider_orchestrator::LiftDriver;
use strider_target::{CallingConvention, SleighArch};

mod common;

/// Lift the supplied bytes starting at `0x1000` and run the full
/// strider optimiser pipeline (which now includes
/// `FlagCmpCanonicalize`).  Returns the post-pipeline graph.
fn lift(arch: SleighArch, cc: CallingConvention, bytes: Vec<u8>) -> Function {
    let base = 0x1000u64;
    let reader = BufMemReader::new(bytes, base);
    let mut sleigh = Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("create sleigh");
    let opts = CfgOptions::default();
    let cfg = Builder::for_arch(&arch, &mut sleigh, base, &opts)
        .build()
        .expect("cfg build");

    let regs = arch.probe_regs().expect("probe regs");
    let strider = LiftDriver::new(arch, regs, cc).expect("LiftDriver::new");
    let outcome = strider.analyze_cfg(&cfg, &sleigh).expect("analyze_cfg");
    let mut function = outcome.function;

    let p = strider.build_optimizer_pipeline();
    p.run(&mut function, &mut strider_orchestrator::opt::OptCtx::empty())
        .expect("optimizer pipeline");
    function
}

/// Build the AArch64 byte sequence:
///
/// ```text
/// 0x1000: cmp w0, #5      (subs wzr, w0, #5)  — 0x7100141F
/// 0x1004: b.eq +8         (skip the next ret)  — 0x54000040
/// 0x1008: ret                                    — 0xD65F03C0
/// 0x100C: ret                                    — 0xD65F03C0
/// ```
fn aarch64_cmp_eq_branch_bytes() -> Vec<u8> {
    let mut out = Vec::with_capacity(16);
    let cmp: u32 = 0x7100_141F; // cmp w0, #5
    let beq: u32 = 0x5400_0040; // b.eq +8 (imm19 = 2)
    let ret: u32 = 0xD65F_03C0; // ret (= ret x30)
    out.extend_from_slice(&cmp.to_le_bytes());
    out.extend_from_slice(&beq.to_le_bytes());
    out.extend_from_slice(&ret.to_le_bytes());
    out.extend_from_slice(&ret.to_le_bytes());
    out
}

/// Returns the producer-`NodeKind` of `if_node`'s cond input.
fn if_cond_kind(function: &Function, if_node: NodeId) -> NodeKind {
    let [_ctrl, cond_value] = function
        .graph()
        .node_inputs_exact::<2>(if_node)
        .expect("If has 2 inputs");
    *function.node_kind(function.producer(cond_value))
}

#[test]
fn aarch64_b_eq_after_pipeline_has_direct_int_cmp_cond() {
    let function = lift(
        SleighArch::aarch64(),
        CallingConvention::aarch64_aapcs64().unwrap(),
        aarch64_cmp_eq_branch_bytes(),
    );
    let if_node = common::find_unique_if(&function);
    assert_eq!(
        if_cond_kind(&function, if_node),
        NodeKind::IntCmpOp(strider_ir::IntCmpOp::Equal),
        "AArch64 b.eq should canonicalise to IntCmpOp::Equal",
    );
}

/// `cmp rax, rbx; je +1; ret; ret` — x86_64.
///
/// ```text
/// 0x1000: cmp rax, rbx     48 39 D8       (3 bytes)
/// 0x1003: je  +1           74 01          (2 bytes; target = 0x1006)
/// 0x1005: ret              C3             (1 byte; fall-through path)
/// 0x1006: ret              C3             (1 byte; je-taken path)
/// ```
fn x86_64_cmp_je_branch_bytes() -> Vec<u8> {
    vec![0x48, 0x39, 0xD8, 0x74, 0x01, 0xC3, 0xC3]
}

#[test]
fn x86_64_je_after_pipeline_has_direct_int_cmp_cond() {
    let function = lift(
        SleighArch::x86_64(),
        CallingConvention::x86_64_systemv().unwrap(),
        x86_64_cmp_je_branch_bytes(),
    );
    let if_node = common::find_unique_if(&function);
    assert_eq!(
        if_cond_kind(&function, if_node),
        NodeKind::IntCmpOp(strider_ir::IntCmpOp::Equal),
        "x86_64 JE should canonicalise to IntCmpOp::Equal",
    );
}

/// ARM Thumb `cmp r0, r1; beq +0; bx lr; bx lr`.
///
/// ```text
/// 0x1000: cmp r0, r1       88 42          (2 bytes)
/// 0x1002: beq +0           00 D0          (2 bytes; target = PC+4 = 0x1006)
/// 0x1004: bx lr            70 47          (2 bytes; fall-through path)
/// 0x1006: bx lr            70 47          (2 bytes; beq-taken path)
/// ```
fn thumb_cmp_beq_branch_bytes() -> Vec<u8> {
    vec![0x88, 0x42, 0x00, 0xD0, 0x70, 0x47, 0x70, 0x47]
}

#[test]
fn arm_thumb_beq_after_pipeline_has_direct_int_cmp_cond() {
    let function = lift(
        SleighArch::arm_thumb(),
        CallingConvention::arm_aapcs().unwrap(),
        thumb_cmp_beq_branch_bytes(),
    );
    let if_node = common::find_unique_if(&function);
    assert_eq!(
        if_cond_kind(&function, if_node),
        NodeKind::IntCmpOp(strider_ir::IntCmpOp::Equal),
        "Thumb BEQ should canonicalise to IntCmpOp::Equal (Thumb's IntNotEqual(ZR, 0) leaf must reduce too)",
    );
}
