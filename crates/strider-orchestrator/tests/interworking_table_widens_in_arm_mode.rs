//! An ARM `ldr pc, [pc, r1, lsl #2]` interworking table whose first round
//! resolves to two ARM arms, and whose loop closure widens it to two more arms
//! that are THUMB (`word | 1`). The widening is re-derived through the seated
//! `Switch`, which has to carry the dispatch's ISA-mode input for those arms
//! to decode as Thumb.
//!
//! ```text
//! 10058 iw_loop2: and r1, r0, #1
//! 1005c jl_disp:  cmp r1, #3 ; bhi jl_def
//! 10064           ldr pc, [pc, r1, lsl #2]
//! 1006c           .word jl_a0, jl_a1, jl_t2+1, jl_t3+1
//! 1007c jl_a0:    mov r1, #2 ; b jl_disp
//! 10084 jl_a1:    mov r1, #3 ; b jl_disp
//! 1008c jl_def:   mvn r0, #0 ; bx lr
//! 10094 jl_t2:    (thumb) movs r0, #2 ; bx lr
//! 10098 jl_t3:    (thumb) movs r0, #3 ; bx lr
//! 1009c           (arm) bx lr
//! ```
mod common;

use strider_orchestrator::LiftOptions;
use strider_orchestrator::opt::OptOptions;

const BASE: u64 = 0x10054;
const TEXT: &[u8] = &[
    0x1e, 0xff, 0x2f, 0xe1, 0x01, 0x10, 0x00, 0xe2, 0x03, 0x00, 0x51, 0xe3, 0x09, 0x00, 0x00, 0x8a,
    0x01, 0xf1, 0x9f, 0xe7, 0x00, 0xf0, 0x20, 0xe3, 0x7c, 0x00, 0x01, 0x00, 0x84, 0x00, 0x01, 0x00,
    0x95, 0x00, 0x01, 0x00, 0x99, 0x00, 0x01, 0x00, 0x02, 0x10, 0xa0, 0xe3, 0xf5, 0xff, 0xff, 0xea,
    0x03, 0x10, 0xa0, 0xe3, 0xf3, 0xff, 0xff, 0xea, 0x00, 0x00, 0xe0, 0xe3, 0x1e, 0xff, 0x2f, 0xe1,
    0x02, 0x20, 0x70, 0x47, 0x03, 0x20, 0x70, 0x47, 0x1e, 0xff, 0x2f, 0xe1,
];

struct Rom;
impl strider_orchestrator::opt::ReadOnlyMemory for Rom {
    fn read(&self, addr: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        let off = usize::try_from(
            addr.checked_sub(BASE)
                .ok_or_else(|| anyhow::anyhow!("low"))?,
        )?;
        let src = TEXT
            .get(off..off + buf.len())
            .ok_or_else(|| anyhow::anyhow!("high"))?;
        buf.copy_from_slice(src);
        Ok(())
    }
}

/// The machine enters 0x10094 / 0x10098 in Thumb, since their table words are
/// odd. Seated without that mode they decode as ARM (`ldrbmi r2, [r0, -r2]!`).
#[test]
fn widened_thumb_arms_keep_their_mode() {
    let mut bytes = TEXT.to_vec();
    bytes.extend(std::iter::repeat_n(0u8, 64));
    let (mut s, cc) =
        common::strider_over_bytes(common::Arch::Arm, bytes, BASE, Some(Box::new(Rom)));
    let r = s
        .analyze(
            0x10058,
            &cc,
            &LiftOptions::default(),
            &OptOptions::default(),
            None,
        )
        .expect("analyze");
    let mut arms: Vec<(u64, Option<bool>)> = r
        .cfg
        .regions()
        .filter_map(|reg| match &reg.terminator {
            strider_cfg::RegionTerminator::Switch { targets, .. } => Some(targets.clone()),
            _ => None,
        })
        .flatten()
        .map(|t| (t.addr, t.isa_bit))
        .collect();
    arms.sort_unstable();
    assert_eq!(
        arms,
        [
            (0x1007c, Some(false)),
            (0x10084, Some(false)),
            (0x10094, Some(true)),
            (0x10098, Some(true)),
        ]
    );
    assert!(
        r.unresolved_indirect_branches.is_empty() && r.unverified_seeded_sites.is_empty(),
        "unresolved {:?}, unverified {:?}",
        r.unresolved_indirect_branches,
        r.unverified_seeded_sites
    );
}
