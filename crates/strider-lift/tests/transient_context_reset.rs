//! A `noflow` context var that changes the decode must not survive from one
//! function to the next on a reused engine.

use rsleigh::mem_readers::BufMemReader;
use strider_lift::lift::Lifter;
use strider_target::SleighArch;

/// `mov lr, pc` commits ARM's `LRset` at the NEXT address, and `ARMinstructions
/// .sinc` picks `call [pc]` over `goto [pc]` for a `bx` under it. `LRset` is
/// `noflow`, so it is outside the flowing set `reset_at` restores: analysing the
/// function at 0x1000 first must not turn 0x1004's `bx r0` into a call when
/// 0x1004 is later analysed as its own cold entry.
#[test]
fn a_noflow_decode_var_does_not_leak_into_the_next_function() {
    let arch = SleighArch::arm();
    let mut bytes = vec![0u8; 0x40];
    bytes[0x00..0x0c].copy_from_slice(&[
        0x0f, 0xe0, 0xa0, 0xe1, // 0x1000: mov lr, pc
        0x10, 0xff, 0x2f, 0xe1, // 0x1004: bx r0
        0x1e, 0xff, 0x2f, 0xe1, // 0x1008: bx lr
    ]);
    let sleigh = |b: Vec<u8>| {
        rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), BufMemReader::new(b, 0x1000))
            .expect("create Sleigh")
    };
    let terminators = |lifter: &mut Lifter<BufMemReader<Vec<u8>>>, entry: u64| -> Vec<String> {
        let cfg = lifter
            .build_cfg(entry.into(), &Default::default(), &Default::default())
            .expect("build_cfg");
        cfg.regions()
            .map(|r| format!("{:?}", r.terminator))
            .collect()
    };

    let mut cold = Lifter::new(arch, sleigh(bytes.clone())).unwrap();
    let alone = terminators(&mut cold, 0x1004);
    assert!(
        alone
            .iter()
            .any(|t| t.starts_with("UnresolvedIndirectBranch")),
        "a cold `bx r0` is an indirect branch; got {alone:?}",
    );

    let mut reused = Lifter::new(arch, sleigh(bytes)).unwrap();
    let _ = terminators(&mut reused, 0x1000);
    assert_eq!(
        terminators(&mut reused, 0x1004),
        alone,
        "the previous function's `mov lr,pc` must not change this decode",
    );
}
