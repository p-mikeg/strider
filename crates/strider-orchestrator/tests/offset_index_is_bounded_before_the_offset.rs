//! A table index bounded only by SIGNED guards on the value after an offset,
//! so nothing the classifier reads bounds the index itself:
//!
//! ```text
//! 40100d presub: movzx eax, dil ; and eax, 7 ; sub eax, 2
//! 401017         test eax, eax  ; js  def        ; eax >= 0
//! 40101b         cmp eax, 4     ; jg  def        ; eax <= 4
//! 401020         movsxd rax, eax
//! 401023         jmp [0x402010 + rax*8]
//! 40102a t0 .. 401042 t4   (the five feasible arms)
//! 401048 x1, 40104e x2     (table[-2], table[-1])
//! 401054 x3                (table[5], table[6])
//! 40105a def
//! ```
//!
//! The `and eax, 7` operand spans 0..7, but the index is that minus two. Only
//! t0..t4 are reachable; enumerating the operand as if it were the index seats
//! x1, x2 and x3 too.
mod common;

use strider_orchestrator::LiftOptions;
use strider_orchestrator::opt::OptOptions;

const TEXT_BASE: u64 = 0x401000;
const TEXT: &[u8] = &[
    0xc3, 0xb8, 0x6f, 0x00, 0x00, 0x00, 0xc3, 0xb8, 0xde, 0x00, 0x00, 0x00, 0xc3, 0x40, 0x0f, 0xb6,
    0xc7, 0x83, 0xe0, 0x07, 0x83, 0xe8, 0x02, 0x85, 0xc0, 0x78, 0x3f, 0x83, 0xf8, 0x04, 0x7f, 0x3a,
    0x48, 0x63, 0xc0, 0xff, 0x24, 0xc5, 0x10, 0x20, 0x40, 0x00, 0xb8, 0x00, 0x00, 0x00, 0x00, 0xc3,
    0xb8, 0x01, 0x00, 0x00, 0x00, 0xc3, 0xb8, 0x02, 0x00, 0x00, 0x00, 0xc3, 0xb8, 0x03, 0x00, 0x00,
    0x00, 0xc3, 0xb8, 0x04, 0x00, 0x00, 0x00, 0xc3, 0xb8, 0x5b, 0x00, 0x00, 0x00, 0xc3, 0xb8, 0x5c,
    0x00, 0x00, 0x00, 0xc3, 0xb8, 0x5d, 0x00, 0x00, 0x00, 0xc3, 0xb8, 0xff, 0xff, 0xff, 0xff, 0xc3,
];
const RODATA_BASE: u64 = 0x402000;
const RODATA: &[u8] = &[
    0x48, 0x10, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x4e, 0x10, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x2a, 0x10, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x30, 0x10, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x36, 0x10, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x3c, 0x10, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x42, 0x10, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x54, 0x10, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x54, 0x10, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00,
];

struct Rom;
impl strider_orchestrator::opt::ReadOnlyMemory for Rom {
    fn read(&self, addr: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        for (base, bytes) in [(TEXT_BASE, TEXT), (RODATA_BASE, RODATA)] {
            if let Some(off) = addr.checked_sub(base)
                && let Ok(off) = usize::try_from(off)
                && let Some(src) = bytes.get(off..off + buf.len())
            {
                buf.copy_from_slice(src);
                return Ok(());
            }
        }
        anyhow::bail!("unmapped {addr:#x}")
    }
}

#[test]
fn an_operand_below_an_offset_is_not_enumerated_as_the_index() {
    let mut bytes = TEXT.to_vec();
    bytes.extend(std::iter::repeat_n(0xccu8, 32));
    let (mut s, cc) =
        common::strider_over_bytes(common::Arch::X64, bytes, TEXT_BASE, Some(Box::new(Rom)));
    let r = s
        .analyze(
            0x40100d,
            &cc,
            &LiftOptions::default(),
            &OptOptions::default(),
            None,
        )
        .expect("analyze");
    let arms: Vec<u64> = r
        .cfg
        .regions()
        .filter_map(|reg| match &reg.terminator {
            strider_cfg::RegionTerminator::Switch { targets, .. } => Some(targets.clone()),
            _ => None,
        })
        .flatten()
        .map(|t| t.addr)
        .collect();
    for infeasible in [0x401048, 0x40104e, 0x401054] {
        assert!(
            !arms.contains(&infeasible),
            "arm {infeasible:#x} is a table slot no index reaches; arms {arms:x?}"
        );
    }
    assert!(
        arms.is_empty() || arms.len() == 5,
        "either the five feasible arms or none: {arms:x?}"
    );
    if arms.is_empty() {
        assert!(
            !r.unresolved_indirect_branches.is_empty(),
            "an unseated dispatch is reported"
        );
    }
}
