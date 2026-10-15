//! The engine's flowing context is a per-address fact, and `Sleigh` keeps it
//! across `build_cfg` calls. So a decode must be a function of THIS build: a
//! `globalset` an earlier build committed at an address this one decodes may
//! not change the instruction, and this build's own commit may not be lost.
//!
//! ARM is the only family that commits at an address another instruction
//! decodes in, and its IT block is the shape that makes it observable:
//! `condit` decides whether the bytes after an `IT` are predicated.
//!
//! `SleighArch::flow_commit_vars` names what is re-derived, each with the
//! flow class that decides what it is erased TO: the pspec default for
//! `noflow` `condit`, the predecessor's value for flowing `TEEMode`. The
//! cross-build half of that is pinned through the production `Lifter` by
//! `reused_lifter_rederives_flow_context`.

use rsleigh::mem_readers::BufMemReader;
use strider_cfg::{Builder, Cfg, CfgOptions, FlowVars};
use strider_target::SleighArch;

const BASE: u64 = 0x1000;

/// One Sleigh engine, so successive builds share the context database exactly
/// as a caller sweeping many functions from one handle does.
struct Engine {
    sleigh: rsleigh::Sleigh<BufMemReader<Vec<u8>>>,
    arch: SleighArch,
    flow: FlowVars,
    transients: Vec<(&'static str, u32)>,
    /// The pspec defaults, captured before any build committed anything.
    defaults: strider_cfg::FlowContext,
    /// What `Lifter::build_cfg` lends the builder so it can re-derive a
    /// committed per-address context from this build's own decode.
    flow_commit_defaults: Vec<(&'static str, strider_target::FlowCommit, u32)>,
}

impl Engine {
    fn thumb(bytes: Vec<u8>) -> Self {
        let arch = SleighArch::arm_thumb();
        let sleigh = rsleigh::Sleigh::new(
            arch.sla_spec(),
            arch.pspec(),
            BufMemReader::new(bytes, BASE),
        )
        .expect("create Sleigh");
        let flow = FlowVars::discover(&sleigh).expect("flow vars");
        let transients: Vec<(&'static str, u32)> = arch
            .transient_decode_vars()
            .iter()
            .map(|name| (*name, sleigh.get_context_at(0, name).expect("default")))
            .collect();
        let defaults = flow.snapshot(&sleigh, BASE);
        let flow_commit_defaults: Vec<(&'static str, strider_target::FlowCommit, u32)> = arch
            .flow_commit_vars()
            .iter()
            .map(|&(name, flow)| (name, flow, sleigh.get_context_at(0, name).expect("default")))
            .collect();
        Self {
            sleigh,
            arch,
            flow,
            transients,
            defaults,
            flow_commit_defaults,
        }
    }

    fn build(&mut self, entry: u64, size: u64) -> Cfg {
        // The part of `Lifter::build_cfg` these tests need: without the entry
        // pin the baseline is whatever an earlier build committed there, and
        // every decode downstream inherits it.
        //
        // NOT the whole of it. Production also masks the Thumb bit off the
        // entry and force-writes the ISA-mode var from `entry_mode_context`,
        // which would decode these even entry addresses as ARM. They decode
        // as Thumb because `ARMCortex.pspec` pins `TMode=1`. Production's
        // entry contract is covered by `Lifter`'s own tests instead.
        self.flow
            .pin_at(&mut self.sleigh, entry, &self.defaults)
            .expect("pin entry defaults");
        let mode = self.flow.snapshot(&self.sleigh, entry);
        let opts = CfgOptions {
            fn_max_size: Some(size),
            ..CfgOptions::default()
        };
        Builder::for_arch(&self.arch, &mut self.sleigh, entry, &opts)
            .with_flow_context(&self.flow, mode)
            .with_transient_defaults(&self.transients)
            .with_flow_commit_defaults(&self.flow_commit_defaults)
            .build()
            .expect("build")
    }
}

/// Every p-code opcode any region decoded at `addr`.
fn opcodes_at(cfg: &Cfg, addr: u64) -> Vec<rsleigh::Opcode> {
    cfg.regions()
        .flat_map(|r| r.insns.iter())
        .filter(|i| i.addr.machine_addr.addr == addr)
        .map(|i| i.insn.opcode)
        .collect()
}

/// `itt eq ; movs r0,#0 ; movs r0,#0 ; bx lr`. The `IT` commits `condit` at
/// each following address, so a build entered at 0x1000 predicates 0x1002 and
/// 0x1004 -- and leaves those commits in the engine.
const IT_BLOCK: [u8; 8] = [0x04, 0xbf, 0x00, 0x20, 0x00, 0x20, 0x70, 0x47];

/// A build that never decodes the `IT` must not inherit its commits. Before
/// the per-instruction verification, the second build read the first's
/// `condit` at 0x1004 and decoded a bare `movs` as a predicated one, inventing
/// a CBRANCH and dropping the flag writes, with every report channel empty.
#[test]
fn an_earlier_builds_commit_does_not_change_this_builds_decode() {
    let mut eng = Engine::thumb(IT_BLOCK.to_vec());

    // Entered at 0x1002, so 0x1000's `IT` is never decoded by this build.
    let fresh = eng.build(0x1002, 6);
    let want = opcodes_at(&fresh, 0x1004);

    // Now prime the engine by building from the `IT` itself, then repeat.
    let mut eng = Engine::thumb(IT_BLOCK.to_vec());
    let primed = eng.build(0x1000, 8);
    assert!(
        !opcodes_at(&primed, 0x1004).is_empty(),
        "the priming build must actually decode 0x1004",
    );
    let after = eng.build(0x1002, 6);

    assert_eq!(
        opcodes_at(&after, 0x1004),
        want,
        "0x1004 decoded differently because an earlier build on this engine \
         committed `condit` there",
    );
}

/// The converse: this build's OWN commit must survive. A zero-p-code-op
/// instruction never reaches the per-op loop, so recording `inst_next` there
/// missed `enterx`, and the fallback then overwrote its `TEEMode` commit.
///
/// `enterx` (0xf3bf 0x8f1f) commits `TEEMode=1` at 0x1004, under which
/// 0xc201 is the zero-op ThumbEE `hb #1` rather than a `stmia` with a STORE.
#[test]
fn a_zero_pcode_op_instructions_own_commit_is_not_overwritten() {
    let code = vec![0xbf, 0xf3, 0x1f, 0x8f, 0x01, 0xc2, 0x70, 0x47];
    let mut eng = Engine::thumb(code);
    let cfg = eng.build(0x1000, 8);
    // Positively: a region owns 0x1004 and spans its two bytes while
    // recording no p-code op, which is what a zero-op ThumbEE `hb` looks
    // like. Asserting only the absence of a STORE would also pass if 0x1004
    // stopped being decoded at all.
    let at = cfg
        .regions()
        .find(|r| r.start_addr.machine_addr.addr == 0x1004)
        .expect("a region starts at 0x1004");
    assert_eq!(
        (at.insns.len(), at.empty_span_len),
        (0, 2),
        "0x1004 is not the zero-op ThumbEE `hb`: {:?}",
        opcodes_at(&cfg, 0x1004),
    );
    assert!(
        !opcodes_at(&cfg, 0x1004).contains(&rsleigh::Opcode::Store),
        "0x1004 decoded as a plain-Thumb store, so `enterx`'s own TEEMode \
         commit was lost: {:?}",
        opcodes_at(&cfg, 0x1004),
    );
}

/// Two edges reaching one address with different `condit` is not a leak but a
/// genuine ambiguity: the first decode owns the bytes and the loser's path
/// gets the wrong stream. ARM calls a branch into an IT block UNPREDICTABLE,
/// so the honest answer is to report rather than to pick.
#[test]
#[ignore = "KNOWN GAP: two edges reaching one address with different IT state \
is decided by work-queue order and not reported. Widening the clash check \
beyond the ISA-mode var flags ordinary IT-block functions as possibly \
incomplete (`ittt lt ; ldmialt sp!,{r7,lr} ; blt.w ; pop {r7,pc}` is a \
genuine complete return), so the report needs the same per-address split."]
fn an_edge_into_an_it_block_is_reported_not_silently_decided() {
    // 0x1000 beq 0x1010 ; padding ; 0x100c itt eq ; two in-block movs ; bx lr.
    let mut code = vec![0u8; 0x14];
    code[0x00..0x02].copy_from_slice(&[0x06, 0xd0]);
    for off in (0x02..0x0c).step_by(2) {
        code[off..off + 2].copy_from_slice(&[0x00, 0x21]);
    }
    code[0x0c..0x0e].copy_from_slice(&[0x04, 0xbf]);
    code[0x0e..0x10].copy_from_slice(&[0x00, 0x20]);
    code[0x10..0x12].copy_from_slice(&[0x00, 0x20]);
    code[0x12..0x14].copy_from_slice(&[0x70, 0x47]);

    let mut eng = Engine::thumb(code);
    let cfg = eng.build(0x1000, 0x14);
    let clashes: Vec<u64> = cfg
        .isa_mode_conflicts()
        .iter()
        .map(|a| a.machine_addr.addr)
        .collect();
    assert!(
        clashes.contains(&0x1010),
        "the external edge into the IT block decodes in a context the \
         in-block path disagrees with, so it must be reported; got {clashes:?}",
    );
}
