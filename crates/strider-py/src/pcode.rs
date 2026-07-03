//! Shared p-code rendering helper.
//!
//! p-code has two homes on the Python surface: `Cfg.pcode_at` /
//! `Cfg.fingerprint_pcode` (`cfg.rs` — an exact LOOKUP against an
//! already-built CFG's stored decodes) and `Lifter.pcode_at` (an
//! entry-relative linear sweep — `strider_cls.rs`).  Both render a
//! decoded `rsleigh::Insn` (or a joined run of them) the same way, via
//! [`lift_one_text`], which is the one place that rendering lives.

use pyo3::prelude::*;

use crate::errors::into_strider_err;

/// Lift the single machine instruction at `addr` through `sleigh`,
/// returning `(text, machine_insn_len)`.
///
/// The text is the instruction's lifted p-code rendered via each
/// `rsleigh::Insn`'s `Display` impl, joined with `"; "`.  A machine
/// instruction that lifts to zero p-code ops (e.g. `endbr64`) yields an
/// empty text but still advances by its byte length.
///
/// Generic over the reader so `PyLifter::pcode_at` (`strider_cls.rs`)
/// can reuse this exact rendering over its own `Sleigh<AnyMemReader>`
/// clone instead of duplicating the insns-to-text join.
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
