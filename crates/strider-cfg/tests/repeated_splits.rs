//! Splitting one long region again and again must cost memory in proportion
//! to the instructions it holds, whichever order the splits arrive in.

use rsleigh::mem_readers::BufMemReader;
use strider_cfg::{Builder, CfgOptions, RegionInstruction};
use strider_target::SleighArch;

const BASE: u64 = 0x10_0000;

/// x86-64: `k` `jne rel32` whose targets name body labels, then an
/// `m`-instruction straight-line body of `inc eax`, then `ret`. The
/// fall-through chain decodes the body as one region first; with `descending`
/// labels the LIFO work queue then pops the taken targets lowest first.
fn image(k: usize, m: usize, descending: bool) -> Vec<u8> {
    let head = k * 6;
    let stride = m / k;
    let mut bytes = Vec::with_capacity(head + m * 2 + 1);
    for i in 0..k {
        let label = if descending { k - 1 - i } else { i };
        let target = head + label * stride * 2;
        let next = (i + 1) * 6;
        let rel = i32::try_from(target as i64 - next as i64).expect("rel32");
        bytes.extend_from_slice(&[0x0f, 0x85]);
        bytes.extend_from_slice(&rel.to_le_bytes());
    }
    for _ in 0..m {
        bytes.extend_from_slice(&[0xff, 0xc0]);
    }
    bytes.push(0xc3);
    bytes
}

/// Bytes of instruction storage the built CFG's regions hold.
fn held_bytes(k: usize, m: usize, descending: bool) -> usize {
    let arch = SleighArch::x86_64();
    let mut sleigh = rsleigh::Sleigh::new(
        arch.sla_spec(),
        arch.pspec(),
        BufMemReader::new(image(k, m, descending), BASE),
    )
    .expect("sleigh");
    let cfg = Builder::for_arch(&arch, &mut sleigh, BASE, &CfgOptions::default())
        .build()
        .expect("build");
    assert!(
        cfg.regions().count() >= 2 * k,
        "every label splits the body"
    );
    cfg.regions()
        .map(|r| r.insns.capacity() * std::mem::size_of::<RegionInstruction>())
        .sum()
}

#[test]
fn lowest_first_splits_hold_memory_linear_in_the_region() {
    let small = held_bytes(50, 500, true);
    let large = held_bytes(200, 2_000, true);
    // 4x the input; a tail copy per split that the first half keeps the
    // capacity of grows 16x.
    let ratio = large as f64 / small as f64;
    assert!(ratio < 6.0, "4x input grew held storage {ratio:.1}x");
    let other_order = held_bytes(200, 2_000, false);
    assert!(
        large < 2 * other_order,
        "lowest-first holds {large} bytes, highest-first {other_order}"
    );
}
