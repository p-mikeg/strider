//! MMX `psubb` subtracts all eight byte lanes, where the register form
//! skipped lane 1. The expected value was also obtained by running the same
//! bytes natively on x86-64.

use crate::common;

const BASE: u64 = 0x1000;
/// The minuend; the subtrahend sits at [`B`].
const A: usize = 0x40;
const B: usize = 0x60;

enum Part {
    Bytes(&'static [u8]),
    /// Bytes before a rip-relative disp32 that reaches the given offset.
    Rip(&'static [u8], usize),
}

fn image(parts: &[Part], data: &[u8]) -> Vec<u8> {
    let mut code = Vec::new();
    for part in parts {
        match part {
            Part::Bytes(b) => code.extend_from_slice(b),
            Part::Rip(head, target) => {
                code.extend_from_slice(head);
                let next = code.len() + 4;
                let disp = i32::try_from(*target).unwrap() - i32::try_from(next).unwrap();
                code.extend_from_slice(&disp.to_le_bytes());
            }
        }
    }
    code.push(0xc3);
    assert!(code.len() <= A, "code overruns the operand pool");
    code.resize(A, 0);
    code.extend_from_slice(data);
    code
}

fn rax(parts: &[Part], data: &[u8]) -> u64 {
    common::cone::returned_rax(image(parts, data), BASE)
}

/// `movq mm0, [A]` / `psubb mm0, mm1|[B]` / `movq rax, mm0` / `emms`: every
/// byte lane subtracts, lane 1 included.
#[test]
fn mmx_psubb_subtracts_every_byte_lane() {
    const LOAD_MM0: Part = Part::Rip(&[0x0f, 0x6f, 0x05], A);
    const OUT: Part = Part::Bytes(&[0x48, 0x0f, 0x7e, 0xc0, 0x0f, 0x77]);
    let mut data = vec![0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80];
    data.resize(B - A, 0);
    data.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
    let want = 0x7869_5a4b_3c2d_1e0f;
    let reg = [
        LOAD_MM0,
        Part::Rip(&[0x0f, 0x6f, 0x0d], B),
        Part::Bytes(&[0x0f, 0xf8, 0xc1]),
        OUT,
    ];
    assert_eq!(rax(&reg, &data), want, "psubb mm0, mm1");
    let mem = [LOAD_MM0, Part::Rip(&[0x0f, 0xf8, 0x05], B), OUT];
    assert_eq!(rax(&mem, &data), want, "psubb mm0, [B]");
}
