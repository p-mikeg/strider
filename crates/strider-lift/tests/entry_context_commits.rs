//! Re-analysing one entry must not grow the engine's context commit log: past
//! `rsleigh::MAX_CONTEXT_COMMITS` the log is dropped and a clone of the engine
//! decodes every alternate-ISA address in the base ISA.

use rsleigh::mem_readers::BufMemReader;
use strider_lift::lift::Lifter;
use strider_target::SleighArch;

/// One `build_cfg` per iteration, more iterations than the budget, so a single
/// commit per call would exhaust it.
const CALLS: usize = rsleigh::MAX_CONTEXT_COMMITS + 200;

#[test]
fn repeating_one_entry_does_not_exhaust_the_context_commit_log() {
    let arch = SleighArch::arm();
    let mut bytes = vec![0u8; 0x40];
    bytes[0x00..0x02].copy_from_slice(&[0x70, 0x47]); // 0x1000: bx lr (Thumb)
    bytes[0x10..0x14].copy_from_slice(&[0x1e, 0xff, 0x2f, 0xe1]); // 0x1010: bx lr (ARM)
    let mut lifter = Lifter::new(
        arch,
        rsleigh::Sleigh::new(
            arch.sla_spec(),
            arch.pspec(),
            BufMemReader::new(bytes, 0x1000),
        )
        .expect("create Sleigh"),
    )
    .unwrap();
    // What a clone of the engine decodes the Thumb entry as, which is what
    // `pcode_at` answers with.
    let thumb_decode = |l: &Lifter<BufMemReader<Vec<u8>>>| {
        format!(
            "{:?}",
            l.sleigh().clone().lift_one(0x1000).expect("lift_one")
        )
    };

    for entry in [0x1001u64, 0x1010] {
        lifter
            .build_cfg(entry.into(), &Default::default(), &Default::default())
            .expect("build_cfg");
        let decoded = thumb_decode(&lifter);
        for _ in 0..CALLS {
            lifter
                .build_cfg(entry.into(), &Default::default(), &Default::default())
                .expect("build_cfg");
        }
        assert!(
            lifter.sleigh().clone_would_replay_context(),
            "{CALLS} analyses of {entry:#x} exhausted the commit log",
        );
        assert_eq!(thumb_decode(&lifter), decoded, "entry {entry:#x}");
    }
}
