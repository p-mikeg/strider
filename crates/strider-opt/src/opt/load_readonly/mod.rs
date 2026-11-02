use strider_ir::node::{NodeId, NodeKind, ValueId};
use strider_ir::{IRBuilderExt, IRViewer, ReadOnlyMemory};

use crate::Result;
use crate::pipeline::OptCtx;

/// Resolves `Load` nodes with constant addresses against a
/// [`ReadOnlyMemory`] image, replacing them with the loaded constant value.
///
/// # Immutability contract
///
/// The fold never consults the load's memory-token chain, so the supplied
/// rom MUST map only runtime-immutable memory (code, `.rodata`; never
/// `.data` / `.got` / `.data.rel.ro` / stack).  A writable global that is
/// stored then reloaded would otherwise fold to its stale file-initial
/// value, discarding the store.  Only `Load(VnSpace::RAM)` is folded.
///
/// # Endianness
///
/// [`ReadOnlyMemory::read`] fills the
/// buffer with RAW bytes and does not decode.  This pass decodes them per
/// `Function::endianness`, then masks to the load's output type.
///
/// A `None` rom on the [`OptCtx`] folds nothing.
///
/// ```rust
/// use strider_opt::{LoadReadOnly, OptCtx, OptimizerPipeline};
/// use strider_ir::ReadOnlyMemory;
///
/// struct MyRom;
/// impl ReadOnlyMemory for MyRom {
///     fn read(&self, _addr: u64, _buf: &mut [u8]) -> anyhow::Result<()> {
///         anyhow::bail!("unmapped")
///     }
/// }
///
/// let mut pipeline = OptimizerPipeline::new();
/// pipeline.add(&LoadReadOnly);
/// let rom = MyRom;
/// let edit = OptCtx::new(Some(&rom));
/// # let _ = (pipeline, edit);
/// ```
pub struct LoadReadOnly;

// Each Load folds independently against the rom, and the driver's RPO puts the
// inner load of a `Load(Load(c))` before the outer one, so no consumer needs
// re-enqueueing.
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
    // The address cone justifies which byte run was read and is cascade-culled
    // once the Load is replaced, so absorb its fingerprint first.
    edit.absorb_fingerprint(new_value, addr_value);
    edit.replace_value(data_value, new_value);
    Ok(())
}

#[cfg(test)]
mod tests;
