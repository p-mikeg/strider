//! Backend-agnostic assertions over the `rsleigh::MemReader` and
//! `reader::ReadOnlyMemory` traits.
//!
//! When a new backend (PE, Mach-O, raw blob, …) lands, its test file
//! builds the reader and calls these helpers in addition to its own
//! backend-specific assertions.

#![allow(dead_code)]

use reader::{Error, ErrorKind, ReadOnlyMemory};
use rsleigh::{MemReader, VnAddr, VnSpace};

// ── MemReader ────────────────────────────────────────────────────────────

/// Asserts that a full `buf.len()` read at `addr` succeeds and the
/// resulting bytes equal `expected`.
pub fn assert_mem_reader_reads<R>(r: &R, addr: u64, expected: &[u8])
where
    R: MemReader,
    R::Err: std::fmt::Debug,
{
    let mut buf = vec![0u8; expected.len()];
    let n = r
        .read(VnAddr { off: addr, space: VnSpace::RAM }, &mut buf)
        .expect("MemReader::read");
    assert_eq!(n, expected.len(), "expected full read of {} bytes", expected.len());
    assert_eq!(&buf[..], expected, "MemReader read returned unexpected bytes");
}

/// Asserts that a read at an unmapped address fails with
/// `ErrorKind::NotMapped(addr)`.
pub fn assert_mem_reader_unmapped_is_not_mapped_error<R>(r: &R, addr: u64)
where
    R: MemReader<Err = Error>,
{
    let mut buf = [0u8; 1];
    let err = r
        .read(VnAddr { off: addr, space: VnSpace::RAM }, &mut buf)
        .expect_err("read at unmapped addr must error");
    match err.kind() {
        ErrorKind::NotMapped(got) => {
            assert_eq!(*got, addr, "NotMapped should carry the requested addr")
        }
        other => panic!("expected NotMapped({addr:#x}), got {other:?}"),
    }
}

/// Asserts that a partial read (buf larger than region suffix) returns
/// `Ok(expected_n)`, documenting MemReader's permissive partial-read contract.
pub fn assert_mem_reader_partial_read_ok<R>(r: &R, addr: u64, buf_len: usize, expected_n: usize)
where
    R: MemReader,
    R::Err: std::fmt::Debug,
{
    assert!(expected_n <= buf_len);
    let mut buf = vec![0u8; buf_len];
    let n = r
        .read(VnAddr { off: addr, space: VnSpace::RAM }, &mut buf)
        .expect("MemReader partial read");
    assert_eq!(n, expected_n, "partial read length");
}

// ── ReadOnlyMemory ───────────────────────────────────────────────────────

pub fn assert_readonly_reads(
    r: &impl ReadOnlyMemory,
    space: VnSpace,
    addr: u64,
    size: usize,
    expected: u64,
) {
    assert_eq!(r.read(space, addr, size), Some(expected), "ReadOnlyMemory::read");
}

pub fn assert_readonly_returns_none(
    r: &impl ReadOnlyMemory,
    space: VnSpace,
    addr: u64,
    size: usize,
) {
    assert_eq!(r.read(space, addr, size), None);
}

/// Exercises the trait's rule that non-RAM spaces always return None.
/// Caller supplies any mapped address; only the space varies.
pub fn assert_readonly_rejects_non_ram_spaces(r: &impl ReadOnlyMemory, mapped_addr: u64) {
    for space in [VnSpace::REGISTER, VnSpace::UNIQUE, VnSpace::CONST] {
        assert_eq!(
            r.read(space, mapped_addr, 4),
            None,
            "space {space:?} must be rejected",
        );
    }
}

/// Exercises the trait's rule that `size == 0` and `size > 8` always return None.
pub fn assert_readonly_rejects_bad_sizes(r: &impl ReadOnlyMemory, mapped_addr: u64) {
    assert_eq!(r.read(VnSpace::RAM, mapped_addr, 0), None, "size=0 must be rejected");
    assert_eq!(r.read(VnSpace::RAM, mapped_addr, 9), None, "size=9 must be rejected");
}
