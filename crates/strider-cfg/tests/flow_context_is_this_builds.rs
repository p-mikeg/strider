//! The engine's flowing context is a per-address fact, and `Sleigh` keeps it
//! across `build_cfg` calls. So a decode must be a function of THIS build: a
//! `globalset` an earlier build committed at an address this one decodes may
//! not change the instruction, and this build's own commit may not be lost.
//!
//! ARM is the only family that commits at an address another instruction
//! decodes in, and its IT block is the shape that makes it observable:
//! `condit` decides whether the bytes after an `IT` are predicated.
//!
//! `SleighArch::flow_commit_vars` names what is re-derived; the sla's flow
//! class decides what each is erased TO: the pspec default for `noflow`
//! `condit`, the predecessor's value for flowing `TEEMode`. The same through
//! `Lifter` is pinned by `reused_lifter_rederives_flow_context`.

use rsleigh::mem_readers::BufMemReader;
use strider_cfg::{Cfg, CfgOptions, DecodeEngine};
use strider_target::SleighArch;

const BASE: u64 = 0x1000;

type Engine = DecodeEngine<BufMemReader<Vec<u8>>>;

/// One engine, so successive builds share the context database exactly as a
/// caller sweeping many functions from one handle does.
fn thumb(bytes: Vec<u8>) -> Engine {
    let arch = SleighArch::arm_thumb();
    DecodeEngine::new(arch, BufMemReader::new(bytes, BASE)).expect("decode engine")
}

/// The Thumb function at `addr`, entered as production enters one: with the
/// Thumb bit set.
fn build(eng: &mut Engine, addr: u64, size: u64) -> Cfg {
    let opts = CfgOptions {
        bound: strider_cfg::FunctionBound::size(size),
        ..CfgOptions::default()
    };
    eng.build_cfg((addr | 1).into(), &opts).expect("build")
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
/// a CBRANCH and dropping the flag writes, with nothing reported incomplete.
#[test]
fn an_earlier_builds_commit_does_not_change_this_builds_decode() {
    let mut eng = thumb(IT_BLOCK.to_vec());

    // Entered at 0x1002, so 0x1000's `IT` is never decoded by this build.
    let fresh = build(&mut eng, 0x1002, 6);
    let want = opcodes_at(&fresh, 0x1004);

    // Now prime the engine by building from the `IT` itself, then repeat.
    let mut eng = thumb(IT_BLOCK.to_vec());
    let primed = build(&mut eng, 0x1000, 8);
    assert!(
        !opcodes_at(&primed, 0x1004).is_empty(),
        "the priming build must actually decode 0x1004",
    );
    let after = build(&mut eng, 0x1002, 6);

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
    let mut eng = thumb(code);
    let cfg = build(&mut eng, 0x1000, 8);
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

    let mut eng = thumb(code);
    let cfg = build(&mut eng, 0x1000, 0x14);
    let clashes: Vec<u64> = cfg
        .incomplete()
        .of(strider_cfg::Reason::IsaModeConflict)
        .map(|a| a.machine_addr.addr)
        .collect();
    assert!(
        clashes.contains(&0x1010),
        "the external edge into the IT block decodes in a context the \
         in-block path disagrees with, so it must be reported; got {clashes:?}",
    );
}

/// A direct edge whose target is no `inst_next` this build recorded: every
/// forward `b`, and every conditional whose fall-through terminates first.
/// `enqueue` cannot snapshot such an address, so it rebuilds the context from
/// the function mode and the parent's ISA-mode bit.
///
/// `TEEMode` is FLOWING, so the live ThumbEE paint has to come with it. Left
/// out, it degraded to the entry default AND `pin_at` wrote that default over
/// the live paint, decoding 0xc201 at the target as a plain-Thumb
/// `stmia r2!,{r0}` and inventing a `Store`, with nothing reported incomplete.
///
/// `enterx` commits `TEEMode=1` at 0x1004, so the `b.n` region decodes in
/// ThumbEE; 0x1008 is no `inst_next` (0x1004 and 0x1006 are), so it takes the
/// fallback arm.
#[test]
fn a_forward_branch_carries_the_live_thumbee_mode() {
    let code = vec![
        0xbf, 0xf3, 0x1f, 0x8f, // 0x1000: enterx -> TEEMode=1 at 0x1004
        0x00, 0xe0, // 0x1004: b.n 0x1008
        0x00, 0x00, // 0x1006: never decoded
        0x01, 0xc2, // 0x1008: ThumbEE `hb #1`, zero-op
        0x70, 0x47, // 0x100a: bx lr
    ];
    let mut eng = thumb(code);
    let cfg = build(&mut eng, 0x1000, 12);

    // Silent: the wrong decode is a clean answer, so only the shape says so.
    assert!(
        cfg.incomplete().is_empty(),
        "premise: nothing here is reported incomplete; got {:?}",
        cfg.incomplete(),
    );
    let at = cfg
        .regions()
        .find(|r| r.start_addr.machine_addr.addr == 0x1008)
        .expect("a region starts at the branch target 0x1008");
    assert_eq!(
        (at.insns.len(), at.empty_span_len),
        (0, 2),
        "0x1008 is not the zero-op ThumbEE `hb`: {:?}",
        opcodes_at(&cfg, 0x1008),
    );
    assert!(
        !opcodes_at(&cfg, 0x1008).contains(&rsleigh::Opcode::Store),
        "the branch target decoded as a plain-Thumb store, so the live \
         ThumbEE mode did not cross the forward edge: {:?}",
        opcodes_at(&cfg, 0x1008),
    );
}

/// The mirror: a plain-Thumb region must not hand its target a mode it is not
/// in. Entered at 0x1004 the `enterx` is never decoded, so 0x1008 is the
/// `stmia` with its `Store` -- on an engine whose earlier build painted
/// `TEEMode=1` across those very bytes.
#[test]
fn a_plain_thumb_forward_branch_does_not_acquire_thumbee() {
    let code = vec![
        0xbf, 0xf3, 0x1f, 0x8f, // 0x1000: enterx -> TEEMode=1 at 0x1004
        0x00, 0xe0, // 0x1004: b.n 0x1008
        0x00, 0x00, // 0x1006: never decoded
        0x01, 0xc2, // 0x1008: plain-Thumb `stmia r2!,{r0}`
        0x70, 0x47, // 0x100a: bx lr
    ];
    let mut eng = thumb(code);
    let thumbee = build(&mut eng, 0x1000, 12);
    assert!(
        !opcodes_at(&thumbee, 0x1008).contains(&rsleigh::Opcode::Store),
        "premise: under ThumbEE the same bytes carry no store",
    );

    let plain = build(&mut eng, 0x1004, 8);
    assert!(
        opcodes_at(&plain, 0x1008).contains(&rsleigh::Opcode::Store),
        "the branch target lost its store, so a ThumbEE mode this build is \
         not in reached it: {:?}",
        opcodes_at(&plain, 0x1008),
    );
}

/// The IT-block state must keep degrading. `condit` is `noflow`: carrying the
/// value live at the branch would predicate the target, which ARM calls
/// UNPREDICTABLE, and a `cond_shft` the next instruction shifts again can land
/// on a `cond_full` no constructor covers and fail the decode outright.
///
/// `itt eq` predicates 0x1002, whose `b.n` lowers to a skip-CBRANCH plus the
/// branch itself: the branch's own region re-lifts 0x1002, so the live IT state
/// is what sits at its last decoded instruction.
#[test]
fn a_forward_branch_does_not_carry_the_it_state() {
    let code = vec![
        0x04, 0xbf, // 0x1000: itt eq
        0x02, 0xe0, // 0x1002: beq 0x100a
        0x70, 0x47, // 0x1004: bxeq lr
        0x70, 0x47, // 0x1006: bx lr
        0x00, 0x00, // 0x1008: never decoded
        0x00, 0x20, // 0x100a: movs r0, #0
        0x70, 0x47, // 0x100c: bx lr
    ];
    let mut eng = thumb(code.clone());
    let reached = build(&mut eng, 0x1000, 14);

    // The same instruction decoded with no IT block in front of it.
    let mut eng = thumb(code);
    let bare = build(&mut eng, 0x100a, 4);
    assert!(
        !opcodes_at(&bare, 0x100a).is_empty(),
        "premise: 0x100a decodes on its own",
    );
    assert_eq!(
        opcodes_at(&reached, 0x100a),
        opcodes_at(&bare, 0x100a),
        "the branch target was predicated by the IT state live at the branch",
    );
}
