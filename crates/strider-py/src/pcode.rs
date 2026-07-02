//! `strider.pcode_at` / `strider.pcode_at_addrs` — lift machine
//! addresses to their p-code semantics.
//!
//! These close the "audit trail" loop: `Node.fingerprint()` (also
//! reachable via `Match.asm_fingerprint(key)`) hands back the machine-
//! instruction *addresses* that explain a matched value; these functions lift those
//! addresses to p-code so the user can read the semantics that produced
//! a match without leaving strider.  rsleigh is a p-code lifter — the
//! returned `text` is the lifted semantics, NOT native assembly
//! mnemonics.
//!
//! Both build exactly ONE `rsleigh::Sleigh` from the arch + the
//! `BufferReader`'s `Send + Sync` reader snapshot, then lift through it:
//! `pcode_at` walks `count` instructions sequentially from a start
//! address (advancing by each instruction's machine byte length —
//! Sleigh's `lift_one` is `&mut self` and carries context-register
//! state, so sequential decoding within one run is required), while
//! `pcode_at_addrs` lifts a SET of (possibly non-sequential)
//! addresses, one instruction each, reusing the single Sleigh.

use pyo3::prelude::*;

use crate::arch::PySleighArch;
use crate::errors::into_strider_err;
use crate::reader::{PyBufferReader, PyBufferReaderView};

/// Build one `Sleigh` over the buffer reader's `Send + Sync` reader
/// snapshot.  Returns a typed `StriderError` on a Sleigh-construction
/// failure rather than panicking across the FFI boundary.
fn build_sleigh(
    arch: &PySleighArch,
    mem: &PyBufferReader,
) -> PyResult<rsleigh::Sleigh<PyBufferReaderView>> {
    rsleigh::Sleigh::new(arch.inner.sla_spec(), arch.inner.pspec(), mem.reader_view())
        .map_err(|e| into_strider_err(anyhow::anyhow!("Sleigh::new failed: {e:?}")))
}

/// Lift the single machine instruction at `addr` through `sleigh`,
/// returning `(text, machine_insn_len)`.
///
/// The text is the instruction's lifted p-code rendered via each
/// `rsleigh::Insn`'s `Display` impl, joined with `"; "`.  A machine
/// instruction that lifts to zero p-code ops (e.g. `endbr64`) yields an
/// empty text but still advances by its byte length.
///
/// Generic over the reader so `PyLifter::fingerprint_pcode`
/// (`strider_cls.rs`) can reuse this exact rendering over its own
/// `Sleigh<AnyMemReader>` clone instead of duplicating the
/// insns-to-text join.
pub(crate) fn lift_one_text<R: rsleigh::MemReader>(
    sleigh: &mut rsleigh::Sleigh<R>,
    addr: u64,
) -> PyResult<(String, usize)> {
    let lift = sleigh
        .lift_one(addr)
        .map_err(|e| into_strider_err(anyhow::anyhow!("lift_one at {addr:#x} failed: {e:?}")))?;
    let text = lift
        .insns
        .iter()
        .map(|insn| insn.to_string())
        .collect::<Vec<_>>()
        .join("; ");
    Ok((text, lift.machine_insn_len))
}

/// Lift the p-code of `count` machine instructions starting at `addr`,
/// returning a list of `(insn_addr, text)` tuples in address order.
///
/// Builds one `Sleigh` for `arch` over `mem` and decodes
/// sequentially, advancing by each instruction's machine byte length.
/// `text` is the instruction's lifted p-code ops (one or more
/// `rsleigh::Insn`s rendered via `Display`, joined with `"; "`, empty
/// for ops like `endbr64` that lift to no p-code).  rsleigh is a p-code
/// lifter — this is the lifted semantics, NOT native assembly
/// mnemonics.
///
/// Raises `StriderError` on a Sleigh-construction or lift failure
/// (e.g. `addr` is unmapped or a zero-length instruction would loop).
#[pyfunction]
#[pyo3(signature = (arch, mem, addr, count = 1))]
pub fn pcode_at(
    arch: &PySleighArch,
    mem: &PyBufferReader,
    addr: u64,
    count: usize,
) -> PyResult<Vec<(u64, String)>> {
    let mut sleigh = build_sleigh(arch, mem)?;
    let mut out = Vec::with_capacity(count);
    let mut cur = addr;
    for _ in 0..count {
        let (text, len) = lift_one_text(&mut sleigh, cur)?;
        out.push((cur, text));
        if len == 0 {
            return Err(into_strider_err(anyhow::anyhow!(
                "lift_one at {cur:#x} reported a zero-length machine instruction; \
                 cannot advance to the next instruction"
            )));
        }
        cur = cur.checked_add(len as u64).ok_or_else(|| {
            into_strider_err(anyhow::anyhow!(
                "machine-address overflow advancing past {cur:#x}"
            ))
        })?;
    }
    Ok(out)
}

/// Lift the p-code of a SET of (possibly non-sequential) machine
/// addresses, one instruction each, returning a list of `(addr, text)`
/// tuples in the order of `addrs`.
///
/// Builds the `Sleigh` only ONCE and lifts one machine instruction per
/// supplied address — the same shape `Lifter.fingerprint_pcode`
/// (`strider_cls.rs`) uses over its own `Sleigh<AnyMemReader>` clone to
/// render a node's fingerprint addresses without paying the
/// Sleigh-construction cost per address. `text` is the lifted p-code
/// (empty for ops like `endbr64` that lift to no p-code).  rsleigh is a
/// p-code lifter — this is the lifted semantics, NOT native assembly
/// mnemonics.
///
/// Raises `StriderError` on a Sleigh-construction or lift failure.
#[pyfunction]
pub fn pcode_at_addrs(
    arch: &PySleighArch,
    mem: &PyBufferReader,
    addrs: Vec<u64>,
) -> PyResult<Vec<(u64, String)>> {
    let mut sleigh = build_sleigh(arch, mem)?;
    let mut out = Vec::with_capacity(addrs.len());
    for addr in addrs {
        let (text, _len) = lift_one_text(&mut sleigh, addr)?;
        out.push((addr, text));
    }
    Ok(out)
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(pcode_at, m)?)?;
    m.add_function(wrap_pyfunction!(pcode_at_addrs, m)?)?;
    Ok(())
}
