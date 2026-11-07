//! A run of x86 prefixes before a `nopw` with a disp32 decodes past Sleigh's
//! 16-byte fetch (the unread displacement still counts). Such bytes are not
//! code: a branch to them is reported and the entry fails cleanly.

use rsleigh::mem_readers::BufMemReader;
use strider_cfg::{Cfg, CfgOptions, DecodeEngine, PcodeInsnAddr, Reason};
use strider_target::SleighArch;

/// 17 and 19 bytes long.
const PREFIX_RUNS: [usize; 2] = [9, 11];

fn overlong(prefixes: usize) -> Vec<u8> {
    let mut bytes = vec![0x66; prefixes];
    bytes.extend_from_slice(&[0x0f, 0x1f, 0x84, 0, 0, 0, 0, 0, 0xc3]);
    bytes
}

fn build(arch: &SleighArch, mut bytes: Vec<u8>) -> anyhow::Result<Cfg> {
    bytes.resize(bytes.len() + 32, 0);
    let opts = CfgOptions {
        bound: strider_cfg::FunctionBound::size(bytes.len() as u64),
        ..CfgOptions::default()
    };
    DecodeEngine::new(*arch, BufMemReader::new(bytes, 0))
        .expect("decode engine")
        .build_cfg(0.into(), &opts)
}

#[test]
fn a_branch_to_an_overlong_decode_is_an_undecodable_target() {
    for arch in [SleighArch::x86_64(), SleighArch::x86()] {
        for prefixes in PREFIX_RUNS {
            // je 3 ; ret ; <overlong>
            let mut bytes = vec![0x74, 0x01, 0xc3];
            bytes.extend(overlong(prefixes));
            let cfg = build(&arch, bytes).expect("a branch into non-code still builds");
            assert_eq!(
                cfg.incomplete()
                    .of(Reason::UndecodableTarget)
                    .collect::<Vec<_>>(),
                [PcodeInsnAddr::at_machine_start(3)],
                "{prefixes} prefixes",
            );
        }
    }
}

#[test]
fn an_overlong_decode_at_the_entry_is_an_error() {
    for arch in [SleighArch::x86_64(), SleighArch::x86()] {
        for prefixes in PREFIX_RUNS {
            assert!(
                build(&arch, overlong(prefixes)).is_err(),
                "{prefixes} prefixes"
            );
        }
    }
}
