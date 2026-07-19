#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use dot::{DotStyle, GraphDot};
use rsleigh::Sleigh;
use rsleigh::mem_readers::BufMemReader;
use strider_cfg::{Builder, Cfg, CfgOptions};
use strider_target::SleighArch;

type TestReader = BufMemReader<Vec<u8>>;

fn build_from_bytes(bytes: Vec<u8>, start: u64) -> (Cfg, Sleigh<TestReader>) {
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, start);
    let mut sleigh = Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("sleigh");
    let cfg = Builder::for_arch(&arch, &mut sleigh, start, &CfgOptions::default())
        .build()
        .expect("Builder::build");
    (cfg, sleigh)
}

fn dot_source(cfg: &Cfg, sleigh: &Sleigh<TestReader>) -> String {
    GraphDot::new(cfg.dot_dumper(sleigh), DotStyle::dark())
        .as_dot()
        .expect("dot rendering must not fail")
}

#[test]
fn dot_output_non_empty_for_linear_function() {
    // `add eax, ebx; ret`: a linear single-region body.
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
    // A conditional split:
    //   0x1000: xor eax, eax   (2 bytes)
    //   0x1002: je 0x1006      (2 bytes; taken when ZF==1)
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
    // `xor eax, eax; xor eax, eax; jmp -4`: two regions, the second
    // branching back to itself as a loop back-edge.
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
    // Every region must emit exactly one `Instruction(addr=...)` header.
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

/// Pins the per-instruction line to rsleigh's `InsnCtxFmt`, not `{:?}` on the
/// opcode.  Visible difference: `InsnCtxFmt` puts a SPACE after the opcode
/// (`IntAdd RAX, RBX, RCX`) where the hand-rolled form put a comma.
///
/// Operand ordering is deliberately not pinned.
#[test]
fn dot_output_uses_rsleigh_insn_ctx_fmt() {
    // `add eax, ebx; ret`: an `IntAdd` with a register operand.
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
