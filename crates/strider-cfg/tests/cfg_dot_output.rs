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

/// Pins the per-instruction line to rsleigh's `InsnCtxFmt`, which puts a SPACE
/// after the opcode (`IntAdd RAX, RBX, RCX`). The assertion covers the opcode
/// and that spacing, leaving operand order free.
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

/// `mov [rsp], rax; mov rbx, [rsp]; ret`: one STORE and one LOAD.
const LOAD_STORE: [u8; 9] = [0x48, 0x89, 0x04, 0x24, 0x48, 0x8b, 0x1c, 0x24, 0xc3];

/// A LOAD / STORE space id is the host address of the engine's `AddrSpace`, so
/// printed raw it differs between two engines over the same bytes.
#[test]
fn dot_output_names_load_store_space_and_is_identical_across_engines() {
    let (cfg_a, sleigh_a) = build_from_bytes(LOAD_STORE.to_vec(), 0x1000);
    let (cfg_b, sleigh_b) = build_from_bytes(LOAD_STORE.to_vec(), 0x1000);
    let a = dot_source(&cfg_a, &sleigh_a);
    let b = dot_source(&cfg_b, &sleigh_b);
    assert_eq!(a, b, "two engines rendered the same bytes differently");
    assert!(a.contains("Store ram, "), "STORE space not named:\n{a}");
    assert!(a.contains(", ram, "), "LOAD space not named:\n{a}");
}

/// The explorer renders through a decoder of its own thread, whose `AddrSpace`
/// addresses are not the ones the CFG's space ids carry.
#[test]
fn dot_output_names_load_store_space_through_another_engine() {
    let (cfg, sleigh) = build_from_bytes(LOAD_STORE.to_vec(), 0x1000);
    let (_, other) = build_from_bytes(LOAD_STORE.to_vec(), 0x1000);
    assert_eq!(dot_source(&cfg, &other), dot_source(&cfg, &sleigh));
}

#[test]
fn insn_text_spells_load_store_space_without_a_host_address() {
    let (cfg, sleigh) = build_from_bytes(LOAD_STORE.to_vec(), 0x1000);
    let regs = sleigh.regs().expect("regs");
    let insns: Vec<_> = cfg.regions().flat_map(|r| &r.insns).collect();
    let text = |f: &dyn Fn(&rsleigh::Insn) -> String| {
        insns
            .iter()
            .map(|ri| f(&ri.insn))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let plain = text(&|i| strider_cfg::insn_plain_text(i, cfg.space_ids()).to_string());
    assert!(plain.contains("Store r, "), "{plain}");
    let foreign = rsleigh::SpaceIds::default();
    let foreign_ctx = text(&|i| strider_cfg::insn_text(i, &foreign, &sleigh, &regs).to_string());
    assert!(
        foreign_ctx.contains("Store <foreign space>, "),
        "{foreign_ctx}"
    );
}
