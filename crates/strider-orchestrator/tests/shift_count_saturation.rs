//! A shift count wider than the output must SATURATE, not truncate.
//!
//! P-code tests the full count against `8 * sizeout`, so truncating
//! `0x1_0000_0000` to `I32` yields 0 and the shift silently does nothing.
//! x86 SIMD shift-by-register is exactly this shape: a 4-byte lane shifted by
//! the 8-byte count the ISA reads from `SRC[63:0]`.

use rsleigh::Sleigh;
use rsleigh::mem_readers::BufMemReader;
use strider_ir::IRViewer;
use strider_target::{CallingConvention, SleighArch};

/// The same image served as read-only memory, so the operands fold.
struct ByteRom(Vec<u8>);

impl strider_orchestrator::opt::ReadOnlyMemory for ByteRom {
    fn read(&self, addr: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        let off = usize::try_from(
            addr.checked_sub(BASE)
                .ok_or_else(|| anyhow::anyhow!("address {addr:#x} below the image base"))?,
        )?;
        let end = off
            .checked_add(buf.len())
            .filter(|&e| e <= self.0.len())
            .ok_or_else(|| anyhow::anyhow!("read past the image"))?;
        buf.copy_from_slice(&self.0[off..end]);
        Ok(())
    }
}

const BASE: u64 = 0x1000;

/// `movdqa xmm0,[rip+0x18]` / `movdqa xmm1,[rip+0x20]` / `psrad xmm0,xmm1` /
/// `movd eax,xmm0` / `ret`, then the two operands.
fn image() -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&[0x66, 0x0f, 0x6f, 0x05]);
    v.extend_from_slice(&0x18u32.to_le_bytes());
    v.extend_from_slice(&[0x66, 0x0f, 0x6f, 0x0d]);
    v.extend_from_slice(&0x20u32.to_le_bytes());
    v.extend_from_slice(&[0x66, 0x0f, 0xe2, 0xc1]);
    v.extend_from_slice(&[0x66, 0x0f, 0x7e, 0xc0]);
    v.push(0xc3);
    v.resize(0x20, 0);
    // lane 0 = 0x8000_0000, the sign bit set.
    v.extend_from_slice(&0x8000_0000u32.to_le_bytes());
    v.resize(0x30, 0);
    // count = 2^32, far past the 32-bit lane width.
    v.extend_from_slice(&(1u128 << 32).to_le_bytes());
    v
}

#[test]
fn an_over_wide_shift_count_saturates_rather_than_truncating() {
    let arch = SleighArch::x86_64();
    let bytes = image();
    let sleigh = Sleigh::new(
        arch.sla_spec(),
        arch.pspec(),
        BufMemReader::new(bytes.clone(), BASE),
    )
    .expect("sleigh");
    let rom: Box<dyn strider_orchestrator::opt::ReadOnlyMemory> = Box::new(ByteRom(bytes));
    let mut strider = strider_orchestrator::Strider::new(arch, sleigh, Some(rom)).expect("new");
    let cc = CallingConvention::x86_64_systemv()
        .build(strider.sleigh_regs())
        .expect("cc");
    let out = strider
        .analyze(BASE, &cc, &Default::default(), &Default::default(), None)
        .expect("analyze");

    let folded: Vec<u128> = out
        .function
        .graph()
        .all_node_ids()
        .filter_map(|n| {
            out.function
                .node_outputs(n)
                .first()
                .and_then(|&v| out.function.int_const_u128(v))
        })
        .collect();
    // `psrad` fills each doubleword with the sign bit once the count reaches
    // the lane width. Truncating the count to 0 would leave 0x8000_0000.
    assert!(
        folded.contains(&0xFFFF_FFFF),
        "expected the sign-filled result, got {folded:x?}"
    );
}
