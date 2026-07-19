use pyo3::prelude::*;

use crate::errors::into_strider_err;

/// Lift the machine instruction at `addr`, returning
/// `(text, machine_insn_len)`.
///
/// An instruction that lifts to zero p-code ops (`endbr64`, say) yields empty
/// text but still advances by its byte length.
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
