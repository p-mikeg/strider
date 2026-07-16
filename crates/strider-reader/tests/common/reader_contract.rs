//! Backend-agnostic assertions over the `rsleigh::MemReader` and
//! `strider_reader::ReadOnlyMemory` traits.
//!
//! When a new backend (PE, Mach-O, raw blob, …) lands, its test file
//! builds the reader and calls these helpers in addition to its own
//! backend-specific assertions.

#![allow(dead_code)]

use rsleigh::{MemReader, VnAddr, VnSpace};
use strider_reader::ReadOnlyMemory;

// ── MemReader ────────────────────────────────────────────────────────────

/// Asserts that a full `buf.len()` read at `addr` succeeds and the
/// resulting bytes equal `expected`.
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

/// Asserts that a read at an unmapped address fails with an error whose
/// message identifies it as a "not mapped" failure carrying the
/// requested address in hex.
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

/// Asserts that a partial read (buf larger than region suffix) returns
/// `Ok(expected_n)`, documenting MemReader's permissive partial-read contract.
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

// ── ReadOnlyMemory ───────────────────────────────────────────────────────

/// Asserts that filling a `expected.len()`-byte buffer at `addr` succeeds
/// and yields exactly the RAW mapped bytes (no endianness swap — the
/// reader copies bytes verbatim; decode is the optimizer's job).
pub(crate) fn assert_readonly_reads(r: &impl ReadOnlyMemory, addr: u64, expected: &[u8]) {
    let mut buf = vec![0u8; expected.len()];
    r.read(addr, &mut buf).expect("ReadOnlyMemory::read");
    assert_eq!(&buf[..], expected, "ReadOnlyMemory raw bytes");
}

/// Asserts that filling a `len`-byte buffer at `addr` errors (any byte in
/// the range is unmapped — the all-or-nothing contract).
pub(crate) fn assert_readonly_errors(r: &impl ReadOnlyMemory, addr: u64, len: usize) {
    let mut buf = vec![0u8; len];
    assert!(
        r.read(addr, &mut buf).is_err(),
        "ReadOnlyMemory::read must error for an unmapped/short range",
    );
}
