use pyo3::prelude::*;

use crate::errors::into_strider_err;

/// The p-code text and byte length of the instruction at `addr`. One with no
/// ops (`endbr64`) yields empty text.
pub(crate) fn lift_one_text<R: rsleigh::MemReader>(
    sleigh: &mut rsleigh::Sleigh<R>,
    space_ids: &rsleigh::SpaceIds,
    addr: u64,
) -> PyResult<(String, usize)> {
    let lift = sleigh
        .lift_one(addr)
        .map_err(|e| into_strider_err(anyhow::anyhow!("lift_one at {addr:#x} failed: {e:#}")))?;
    Ok((pcode_text(&lift.insns, space_ids), lift.machine_insn_len))
}

/// The ops joined with `"; "`.
pub(crate) fn pcode_text<'a>(
    insns: impl IntoIterator<Item = &'a rsleigh::Insn>,
    space_ids: &rsleigh::SpaceIds,
) -> String {
    insns
        .into_iter()
        .map(|insn| strider_cfg::insn_plain_text(insn, space_ids).to_string())
        .collect::<Vec<_>>()
        .join("; ")
}
