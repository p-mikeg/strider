//! `strider.disassemble` / `strider.disassemble_addrs` — turn machine
//! addresses back into human-readable instruction text.
//!
//! These close the "audit trail" loop: `Analysis.fingerprint(node)`
//! hands back the machine-instruction *addresses* that explain a
//! matched value; these functions decode those addresses so the user
//! never has to leave strider for objdump.
//!
//! Both build exactly ONE `rsleigh::Sleigh` from the arch + the
//! `MemoryMap`'s `Send + Sync` reader snapshot, then decode through it:
//! `disassemble` walks `count` instructions sequentially from a start
//! address (advancing by each instruction's machine byte length —
//! Sleigh's `lift_one` is `&mut self` and carries context-register
//! state, so sequential decoding within one run is required), while
//! `disassemble_addrs` decodes a SET of (possibly non-sequential)
//! addresses, one instruction each, reusing the single Sleigh.

use pyo3::prelude::*;

use crate::arch::PySleighArch;
use crate::errors::into_strider_err;
use crate::reader::{PyMemoryMap, PyMemoryMapReader};

/// Build one `Sleigh` over the memory map's `Send + Sync` reader
/// snapshot.  Returns a typed `StriderError` on a Sleigh-construction
/// failure rather than panicking across the FFI boundary.
fn build_sleigh(
    arch: &PySleighArch,
    mem: &PyMemoryMap,
) -> PyResult<rsleigh::Sleigh<PyMemoryMapReader>> {
    rsleigh::Sleigh::new(arch.inner.sla_spec(), arch.inner.pspec(), mem.reader_view())
        .map_err(|e| into_strider_err(anyhow::anyhow!("Sleigh::new failed: {e:?}")))
}

/// Decode the single machine instruction at `addr` through `sleigh`,
/// returning `(text, machine_insn_len)`.
///
/// The text is the lifted instruction's pcode rendered via each
/// `rsleigh::Insn`'s `Display` impl, joined with `"; "`.  A machine
/// instruction that lifts to zero pcode ops (e.g. a NOP on some Sleigh
/// specs) yields an empty text but still advances by its byte length.
fn lift_one_text(
    sleigh: &mut rsleigh::Sleigh<PyMemoryMapReader>,
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

/// Disassemble `count` machine instructions starting at `addr`,
/// returning a list of `(insn_addr, text)` tuples in address order.
///
/// Builds one `Sleigh` for `arch` over `mem` and decodes
/// sequentially, advancing by each instruction's machine byte length.
/// `text` is the lifted pcode for that instruction (one or more
/// `rsleigh::Insn`s rendered via `Display`, joined with `"; "`).
///
/// Raises `StriderError` on a Sleigh-construction or lift failure
/// (e.g. `addr` is unmapped or a zero-length instruction would loop).
#[pyfunction]
#[pyo3(signature = (arch, mem, addr, count = 1))]
pub fn disassemble(
    arch: &PySleighArch,
    mem: &PyMemoryMap,
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

/// Disassemble a SET of (possibly non-sequential) machine addresses,
/// one instruction each, returning a list of `(addr, text)` tuples in
/// the order of `addrs`.
///
/// Builds the `Sleigh` only ONCE and decodes one machine instruction
/// per supplied address — this is the path
/// `Analysis.fingerprint_text` uses to render a node's fingerprint
/// addresses without paying the Sleigh-construction cost per address.
///
/// Raises `StriderError` on a Sleigh-construction or lift failure.
#[pyfunction]
pub fn disassemble_addrs(
    arch: &PySleighArch,
    mem: &PyMemoryMap,
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
    m.add_function(wrap_pyfunction!(disassemble, m)?)?;
    m.add_function(wrap_pyfunction!(disassemble_addrs, m)?)?;
    Ok(())
}
