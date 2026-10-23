//! A reused `DecodeEngine` builds an entry as a fresh one does, though
//! `Sleigh` keeps every context commit an earlier build made.

use rsleigh::mem_readers::BufMemReader;
use strider_cfg::{Cfg, CfgOptions, DecodeEngine, RegionTerminator};
use strider_target::SleighArch;

type Engine = DecodeEngine<BufMemReader<Vec<u8>>>;

/// `(start, (opcode, machine length) per op, terminator)` per region, by start.
type Shape = Vec<(u64, Vec<(rsleigh::Opcode, u32)>, RegionTerminator)>;

fn shape(cfg: &Cfg) -> Shape {
    let mut out: Shape = cfg
        .regions()
        .map(|r| {
            (
                r.start_addr.machine_addr.addr,
                r.insns.iter().map(|i| (i.insn.opcode, i.len)).collect(),
                r.terminator.clone(),
            )
        })
        .collect();
    out.sort_by_key(|(start, ..)| *start);
    out
}

fn thumb_engine(code: &[u8]) -> Engine {
    let arch = SleighArch::arm_thumb();
    DecodeEngine::new(arch, BufMemReader::new(code.to_vec(), 0x1000)).expect("decode engine")
}

/// Thumb `itt eq ; movs r0,#0 ; movs r0,#0 ; bx lr`: the `IT` commits `condit`
/// at the two instructions after it. A build entering past the `IT`, at the
/// first `movs`, must not meet the commits a priming build left there.
#[test]
fn a_reused_engine_builds_an_entry_as_a_fresh_one_does() {
    const IT_BLOCK: [u8; 8] = [0x04, 0xbf, 0x00, 0x20, 0x00, 0x20, 0x70, 0x47];
    let opts = CfgOptions::default();
    let fresh = shape(
        &thumb_engine(&IT_BLOCK)
            .build_cfg(0x1003u64.into(), &opts)
            .expect("fresh build"),
    );
    let mut engine = thumb_engine(&IT_BLOCK);
    let primed = engine
        .build_cfg(0x1001u64.into(), &opts)
        .expect("priming build");
    assert!(
        primed.regions().count() > 1,
        "premise: the predicated movs split the priming build"
    );
    let again = shape(
        &engine
            .build_cfg(0x1003u64.into(), &opts)
            .expect("reused build"),
    );
    assert_eq!(again, fresh);
}

/// Thumb `movs r0,#0 ; bx lr` at 0x1000, ARM `bx lr` at 0x1004: one engine
/// alternating between the two decodes each in the mode its entry's low bit
/// names, as a fresh engine does.
#[test]
fn an_engine_alternating_isa_modes_builds_each_entry_as_a_fresh_one_does() {
    const MIXED: [u8; 8] = [0x00, 0x20, 0x70, 0x47, 0x1e, 0xff, 0x2f, 0xe1];
    let opts = CfgOptions::default();
    let fresh = |entry: u64| {
        shape(
            &thumb_engine(&MIXED)
                .build_cfg(entry.into(), &opts)
                .expect("fresh build"),
        )
    };
    let mut engine = thumb_engine(&MIXED);
    let thumb = shape(&engine.build_cfg(0x1001u64.into(), &opts).expect("Thumb"));
    let arm = shape(&engine.build_cfg(0x1004u64.into(), &opts).expect("ARM"));
    let thumb_again = shape(
        &engine
            .build_cfg(0x1001u64.into(), &opts)
            .expect("Thumb again"),
    );
    // The even entry decodes ARM `bx lr`, though this pspec's default is Thumb.
    let [(start, ops, terminator)] = arm.as_slice() else {
        panic!("one ARM region: {arm:?}");
    };
    assert_eq!((*start, terminator), (0x1004, &RegionTerminator::Return));
    assert!(ops.iter().all(|&(_, len)| len == 4), "ARM ops: {ops:?}");
    assert_eq!(arm, fresh(0x1004));
    assert_eq!(thumb, fresh(0x1001));
    assert_eq!(thumb_again, thumb);
}
