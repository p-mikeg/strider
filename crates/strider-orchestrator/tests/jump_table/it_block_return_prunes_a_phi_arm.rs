//! A Thumb-2 `tbb` whose index is a phi of an IT block, from clang -O2
//! -mthumb (armv7a), `sw_nested` of a stripped binary:
//!
//! ```text
//! 20414: cmp   r0, #5
//! 20416: itt   hi
//! 20418: movhi r0, #7
//! 2041a: bxhi  lr
//! 2041c: tbb   [pc, r0]      ; cells at 0x20420: 03 0c 10 15 1b 20
//! ```
//!
//! The IT block lifts as two diamonds on `5 < r0`, the first merging
//! `phi(r0, 7)`. Only the `r0 <= 5` arm reaches the `tbb`, so the index is
//! `[0, 5]`; the `7` arm would read the next instruction's bytes as cells and
//! seat a literal-pool word and the next function.

use strider_ir_test_utils::MockRom;

use crate::common;

const BASE: u64 = 0x20414;

/// `sw_nested`, its literal pool and the next function, `sw_long`.
const CODE: &[&str] = &[
    "052884bf07207047dfe800f0030c10151b20062924bf002070471248784450f8",
    "210070470b487844006870470a48784400680230704709487844006800ebc000",
    "70470748784400680338704701fb01f0704700bfbe120100b6120100ac120100",
    "a012010078fdfeffc11c07290bd84d20dfe801f0040c12171d220b2712487844",
    "0068013070474ff0ff3070470f487844006800eb400070470d48784400680738",
    "70470c487844006880f0090070470a4878440068800070470848784400684010",
    "70470520704700bf66120100561201004a12010040120100341201002a120100",
];

#[test]
fn an_arm_the_dispatch_cannot_reach_bounds_nothing() {
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
        [0x20426, 0x20438, 0x20440, 0x2044a, 0x20456, 0x20460],
        "{arms:#x?}"
    );
    assert!(r.incomplete().is_empty(), "{}", r.incomplete());
}
