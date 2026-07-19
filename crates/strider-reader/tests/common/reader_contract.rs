//! Backend-agnostic assertions over `rsleigh::MemReader` and
//! `strider_reader::ReadOnlyMemory`. A new backend (PE, Mach-O, raw blob, ...)
//! calls these alongside its own backend-specific assertions.

#![allow(dead_code)]

use rsleigh::{MemReader, VnAddr, VnSpace};
use strider_reader::ReadOnlyMemory;

pub(crate) fn assert_mem_reader_reads<R>(r: &R, addr: u64, expected: &[u8])
where
    R: MemReader,
    R::Err: std::fmt::Debug,
{
    let mut buf = vec![0u8; expected.len()];
    let n = r
        .read(
            VnAddr {
                off: addr,
                space: VnSpace::RAM,
            },
            &mut buf,
        )
        .expect("MemReader::read");
    assert_eq!(
        n,
        expected.len(),
        "expected full read of {} bytes",
        expected.len()
    );
    assert_eq!(
        &buf[..],
        expected,
        "MemReader read returned unexpected bytes"
    );
}

/// The error message must name the failure and carry the requested address in
/// hex.
pub(crate) fn assert_mem_reader_unmapped_is_not_mapped_error<R>(r: &R, addr: u64)
where
    R: MemReader,
    R::Err: std::fmt::Display,
{
    let mut buf = [0u8; 1];
    let err = r
        .read(
            VnAddr {
                off: addr,
                space: VnSpace::RAM,
            },
            &mut buf,
        )
        .expect_err("read at unmapped addr must error");
    let msg = err.to_string();
    let expected_addr = format!("{addr:#x}");
    assert!(
        msg.contains("is not mapped") && msg.contains(&expected_addr),
        "expected `not mapped` error for {expected_addr}, got: {err}",
    );
}

/// A buffer larger than the region suffix returns `Ok(expected_n)`: MemReader
/// permits partial reads.
pub(crate) fn assert_mem_reader_partial_read_ok<R>(
    r: &R,
    addr: u64,
    buf_len: usize,
    expected_n: usize,
) where
    R: MemReader,
    R::Err: std::fmt::Debug,
{
    assert!(expected_n <= buf_len);
    let mut buf = vec![0u8; buf_len];
    let n = r
        .read(
            VnAddr {
                off: addr,
                space: VnSpace::RAM,
            },
            &mut buf,
        )
        .expect("MemReader partial read");
    assert_eq!(n, expected_n, "partial read length");
}

/// The bytes must arrive raw, with no endianness swap: the reader copies
/// verbatim and decoding is the optimizer's job.
pub(crate) fn assert_readonly_reads(r: &impl ReadOnlyMemory, addr: u64, expected: &[u8]) {
    let mut buf = vec![0u8; expected.len()];
    r.read(addr, &mut buf).expect("ReadOnlyMemory::read");
    assert_eq!(&buf[..], expected, "ReadOnlyMemory raw bytes");
}

/// All-or-nothing: any unmapped byte in the range must error.
pub(crate) fn assert_readonly_errors(r: &impl ReadOnlyMemory, addr: u64, len: usize) {
    let mut buf = vec![0u8; len];
    assert!(
        r.read(addr, &mut buf).is_err(),
        "ReadOnlyMemory::read must error for an unmapped/short range",
    );
}
