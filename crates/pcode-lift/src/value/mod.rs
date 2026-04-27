//! Per-opcode-family value lifters.
//!
//! Each submodule provides one or more handlers that map a specific
//! pcode opcode (or family of related opcodes) onto IR builder calls.
//! The top-level dispatch lives in [`lift`].

use crate::{Result, ValueLifter};

/// Dispatches `insn` to the appropriate per-opcode handler.
///
/// Returns `Ok(true)` when the opcode is value-producing and was
/// lifted; `Ok(false)` when the opcode is a control-flow / call /
/// store op the caller must handle itself.
pub(crate) fn lift<R: rsleigh::MemReader>(
    _lifter: &mut ValueLifter<'_, R>,
    _insn: &rsleigh::Insn,
) -> Result<bool> {
    // Skeleton — handlers fill in this match in subsequent commits as
    // the per-opcode-family files move over.
    Ok(false)
}
