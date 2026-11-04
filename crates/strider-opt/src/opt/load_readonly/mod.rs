use strider_ir::node::{NodeId, NodeKind, ValueId};
use strider_ir::{IRBuilderExt, IRViewer, ReadOnlyMemory};

use crate::Result;
use crate::pipeline::OptCtx;

/// Folds a RAM `Load` at a constant address to the integer the
/// [`OptCtx`]'s [`ReadOnlyMemory`] holds there, decoded per
/// `Function::endianness`.
///
/// The fold ignores the memory chain, so the rom must map only memory that
/// is immutable at runtime (code, `.rodata`; never `.data`, `.got` or the
/// stack).
pub struct LoadReadOnly;

// RPO folds the inner load of `Load(Load(c))` first, so nothing re-enqueues.
impl crate::peephole::PeepholePass for LoadReadOnly {
    type Sweep = ();

    fn matches_kind(&self, kind: &NodeKind) -> bool {
        matches!(kind, NodeKind::Load(s) if *s == rsleigh::VnSpace::RAM)
    }

    fn try_rewrite(
        &self,
        _sweep: &mut (),
        edit: &mut crate::EditFunction<'_>,
        opt_ctx: &OptCtx<'_>,
        root: NodeId,
    ) -> Result<Option<ValueId>> {
        if let Some(rom) = opt_ctx.rom {
            try_fold_const_load_at(edit, root, rom)?;
        }
        Ok(None)
    }
}

/// `node_id` MUST be a `Load(VnSpace::RAM)`.
fn try_fold_const_load_at(
    edit: &mut crate::EditFunction<'_>,
    node_id: NodeId,
    rom: &dyn ReadOnlyMemory,
) -> Result<()> {
    let (data_value, ty) = edit.single_value_output(node_id)?;
    let addr_value = edit.load_addr(node_id);
    let Some(masked) = edit
        .int_const(addr_value)
        .and_then(|addr| u64::try_from(addr).ok())
        .and_then(|addr| {
            crate::const_eval::read_rom_const(rom, addr, ty, edit.function().endianness())
        })
    else {
        return Ok(());
    };
    let new_value = edit.build_int_const(masked, ty)?;
    // The address is the fold's proof.
    edit.absorb_fingerprint(new_value, addr_value);
    edit.replace_value(data_value, new_value);
    Ok(())
}

#[cfg(test)]
mod tests;
