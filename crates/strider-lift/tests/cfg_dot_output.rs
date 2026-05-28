#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Smoke tests for [`Cfg::dot_dumper`] — the CFG-level DOT renderer
//! used by the orchestrator's `cfg.html` debug output.  Ported from
//! the pre-rewrite `crates/cfg/tests/dot_dumper.rs` suite.  Uses
//! synthetic x86_64 byte sequences (mirroring
//! `cfg_build_end_to_end.rs`) so no ELF fixture-build dependency.
//!
//! Coverage:
//! - Non-empty output with the per-region label header.
//! - Conditional-branch edge labelling (`if-true`/`if-false`) + dashed style.
//! - Solid style on a back-edge (loop) unconditional edge.
//! - Per-region label count matches `graph.node_count()`.
//! - Per-insn line uses rsleigh's `InsnCtxFmt` (space-separated opcode
//!   + operands), not the hand-rolled `<Opcode>, <Reg>` form.

use dot::{DotStyle, GraphDot};
use rsleigh::mem_readers::BufMemReader;
use rsleigh::Sleigh;
use strider_lift::cfg::{Builder, Cfg, OptionsBuilder};
use strider_target::SleighArch;

type TestReader = BufMemReader<Vec<u8>>;

fn build_from_bytes(bytes: Vec<u8>, start: u64) -> (Cfg, Sleigh<TestReader>) {
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, start);
    let sleigh = Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("sleigh");
    Builder::for_arch(&arch, sleigh, start, OptionsBuilder::new().build())
        .build()
        .expect("Builder::build")
}

fn dot_source(cfg: &Cfg, sleigh: &Sleigh<TestReader>) -> String {
    GraphDot::new(cfg.dot_dumper(sleigh), DotStyle::dark())
        .as_dot()
        .expect("dot rendering must not fail")
}

#[test]
fn dot_output_non_empty_for_linear_function() {
    // `add eax, ebx; ret` — a linear single-region body.
    let (cfg, sleigh) = build_from_bytes(vec![0x01, 0xd8, 0xc3], 0x1000);
    let s = dot_source(&cfg, &sleigh);
    assert!(!s.is_empty(), "DOT output must not be empty");
    assert!(
        s.contains("Instruction(addr="),
        "node label must appear in DOT output"
    );
}

#[test]
fn dot_output_for_conditional_function_contains_if_case_edges_and_dashed_style() {
    // `xor eax, eax; je +2; xor eax, eax; ret` — a conditional split:
    //   0x1000: xor eax, eax   (2 bytes)
    //   0x1002: je 0x1006      (2 bytes; ZF==1 → taken)
    //   0x1004: xor eax, eax   (2 bytes; fall-through path)
    //   0x1006: ret            (1 byte; taken target)
    let bytes = vec![0x31, 0xc0, 0x74, 0x02, 0x31, 0xc0, 0xc3];
    let (cfg, sleigh) = build_from_bytes(bytes, 0x1000);
    let s = dot_source(&cfg, &sleigh);
    assert!(
        s.contains("if-true") || s.contains("if-false"),
        "a conditional function's DOT output must label its branch edges"
    );
    assert!(
        s.contains("dashed"),
        "conditional-branch edges must render with dashed style"
    );
}

#[test]
fn dot_output_for_loop_contains_solid_unconditional_edges() {
    // `xor eax, eax; xor eax, eax; jmp -4` — a 2-region body whose
    // second half branches back to itself (a back-edge loop).  Same
    // byte sequence as `cfg_build_end_to_end.rs`'s
    // `split_first_half_becomes_fallthrough_second_half_branch`.
    let bytes = vec![0x31, 0xc0, 0x31, 0xc0, 0xeb, 0xfc];
    let (cfg, sleigh) = build_from_bytes(bytes, 0x1000);
    let s = dot_source(&cfg, &sleigh);
    assert!(
        s.contains("solid"),
        "a looping CFG's unconditional edges (incl. the back-edge) render solid"
    );
}

#[test]
fn dot_output_mentions_every_region() {
    // Use the same conditional shape as the IfCase test.  Every region
    // must emit exactly one `Instruction(addr=...)` label header.
    let bytes = vec![0x31, 0xc0, 0x74, 0x02, 0x31, 0xc0, 0xc3];
    let (cfg, sleigh) = build_from_bytes(bytes, 0x1000);
    let s = dot_source(&cfg, &sleigh);
    let expected = cfg.region_graph().node_count();
    let actual = s.matches("Instruction(addr=").count();
    assert_eq!(
        actual, expected,
        "every region must emit exactly one Instruction(addr=...) label"
    );
}

/// Pins that the per-instruction line uses rsleigh's
/// [`rsleigh::ctx_fmt::InsnCtxFmt`] formatter, not `{:?}` on the
/// opcode.  The user-visible difference: `InsnCtxFmt` separates the
/// opcode from its first operand with a *space* (`IntAdd RAX, RBX,
/// RCX`); the hand-rolled `"{:?}, {}"` form emitted a *comma* after
/// the opcode.
///
/// Operand *ordering* isn't pinned (rsleigh 4.0.0 puts the output
/// varnode first, before the inputs).
#[test]
fn dot_output_uses_rsleigh_insn_ctx_fmt() {
    // `add eax, ebx; ret` — `IntAdd` opcode with at least one register
    // operand.
    let bytes = vec![0x01, 0xd8, 0xc3];
    let (cfg, sleigh) = build_from_bytes(bytes, 0x1000);
    let s = dot_source(&cfg, &sleigh);
    assert!(
        s.contains("IntAdd R") || s.contains("IntAdd E"),
        "expected `IntAdd R<...>` or `IntAdd E<...>` (InsnCtxFmt \
         spelling) in the dot source; got:\n{s}",
    );
    assert!(
        !s.contains("IntAdd, R") && !s.contains("IntAdd, E"),
        "the hand-rolled `<Opcode>, <Reg>` spelling must not appear; \
         the cfg dot dumper should delegate to InsnCtxFmt.\n\nfull dot:\n{s}",
    );
}
