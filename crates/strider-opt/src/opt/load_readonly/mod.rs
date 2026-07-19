use strider_ir::node::{NodeId, NodeKind};
use strider_ir::{IRBuilderExt, IRViewer, ReadOnlyMemory};

use crate::error::Result;
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
/// [`ReadOnlyMemory::read`][strider_ir::ReadOnlyMemory::read] fills the
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
/// pipeline.add(LoadReadOnly);
/// let rom = MyRom;
/// let edit = OptCtx::new(Some(&rom));
/// # let _ = (pipeline, edit);
/// ```
#[derive(Clone, Copy)]
pub struct LoadReadOnly;

impl crate::peephole::PeepholePass for LoadReadOnly {
    // Each Load folds independently against the rom, so the driver's RPO seed
    // order does not affect the outcome.
    fn matches_kind(&self, kind: &NodeKind) -> bool {
        matches!(kind, NodeKind::Load(s) if *s == rsleigh::VnSpace::RAM)
    }

    // A folded `Load` becomes a constant, never another load's operand.
    fn propagate_to_consumers(&self) -> bool {
        false
    }

    fn try_rewrite(
        &self,
        edit: &mut crate::EditFunction<'_>,
        opt_ctx: &mut OptCtx<'_>,
        root: NodeId,
    ) -> Result<crate::peephole::PeepholeRewrite> {
        let Some(rom) = opt_ctx.rom else {
            return Ok(crate::peephole::PeepholeRewrite::NoChange);
        };
        Ok(crate::peephole::PeepholeRewrite::from_changed(
            try_fold_const_load_at(edit, root, rom)?,
        ))
    }
}

/// `node_id` MUST be a `Load(VnSpace::RAM)`.
fn try_fold_const_load_at(
    edit: &mut crate::EditFunction<'_>,
    node_id: NodeId,
    rom: &dyn ReadOnlyMemory,
) -> Result<bool> {
    let (data_value, ty) = edit.single_value_output(node_id)?;
    let resolve = |v| edit.function().int_const_u128(v);
    let Some(masked) =
        crate::const_eval::eval_node_const(edit.function(), data_value, &resolve, Some(rom))
    else {
        return Ok(false);
    };
    let new_value = edit.build_int_const(masked, ty)?;
    // The address cone justifies which byte run was read and is cascade-culled
    // once the Load is replaced, so absorb its fingerprint first.
    let addr_value = edit.load_addr(node_id);
    edit.absorb_fingerprint(new_value, addr_value);
    edit.replace_value(data_value, new_value)
}

#[cfg(test)]
mod tests;
