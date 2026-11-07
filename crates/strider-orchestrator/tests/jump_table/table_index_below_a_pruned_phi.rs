//! A Thumb-2 `tbb` whose index is a field of a cell read through a phi the
//! dispatch narrows, from a clang -O2 Thumb kernel,
//! `snd_pcm_format_set_silence`:
//!
//! ```text
//! 8067ae9a: cmp   r0, #52
//! 8067ae9c: itt   hi
//! 8067ae9e: mvnhi r0, #21
//! 8067aea2: pophi {r7, pc}
//!           ...                   ; r3 = fmts + 12 * r0
//! 8067aeb8: ldrb  r0, [r3, #1]
//!           ...
//! 8067aece: add.w ip, r3, r0, lsr #3   ; r3 = -2
//! 8067aed4: cmp.w ip, #6
//! 8067aed8: bhi   8067af4e
//! 8067aeda: tbb   [pc, ip]       ; cells: 04 0b 26 38 38 38 2d
//! ```
//!
//! Only the `r0 <= 52` arm of the IT block's phi reaches the `tbb`, which
//! bounds the cell address too. The cell is not ROM, so the dispatch cannot
//! fold over that address; the guarded field `ip` still bounds the table.

use strider_ir_test_utils::MockRom;

use crate::common;

const BASE: u64 = 0x8067_ae98;

/// The function's 0xb8 bytes.
const CODE: &[&str] = &[
    "80b5342884bf6ff0150080bd002a31d041f6646300eb4000c8f2870303eb8003",
    "587848b303f1040e092828d3db78012b25d06ff0010303ebd00c0020bcf1060f",
    "39d8dfe80cf0040b263838382d00bef8003021f8023b013afbd12ce09ef80200",
    "013a8870bef8000021f8030b4ff00000f4d120e0002080bd6ff0150080bd5043",
    "9ef80030c208084619465ef18dfb002080bddef8003041f8043b013afbd10ae0",
    "def800c0def80430c1f800c0013a4b6001f10801f8d180bd",
];

#[test]
fn a_field_guard_bounds_the_table_when_the_cell_does_not_fold() {
    let code = common::hex(&CODE.concat());
    let r = common::analyze_bytes(
        common::Arch::ArmThumb,
        code.clone(),
        BASE,
        Some(Box::new(MockRom::raw_bytes(BASE, code))),
        BASE | 1,
    );
    let mut arms: Vec<u64> = r
        .cfg
        .regions()
        .filter_map(|reg| match &reg.terminator {
            strider_cfg::RegionTerminator::Switch { targets, .. } => Some(targets.clone()),
            _ => None,
        })
        .flatten()
        .map(|t| t.addr)
        .collect();
    arms.sort_unstable();
    assert_eq!(
        arms,
        [
            0x8067_aee6,
            0x8067_aef4,
            0x8067_af2a,
            0x8067_af38,
            0x8067_af4e
        ],
        "{arms:#x?}"
    );
    assert!(r.incomplete().is_empty(), "{}", r.incomplete());
}
