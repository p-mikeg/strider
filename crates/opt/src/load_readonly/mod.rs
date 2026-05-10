use ir::node::NodeKind;
use reader::ReadOnlyMemory;

use crate::pipeline::{OptimizationResult, Optimizer};
use crate::worklist::WorkSet;

// ── LoadReadOnly optimizer ────────────────────────────────────────────────────

/// Resolves `Load` nodes with constant addresses against a
/// [`ReadOnlyMemory`] image, replacing them with the loaded constant value.
///
/// # Memory-space contract
///
/// The pass forwards the `Load`'s [`rsleigh::VnSpace`] to
/// [`ReadOnlyMemory::read`][reader::ReadOnlyMemory::read] verbatim and trusts
/// the impl to discriminate.  A rom that returns `Some(_)` for an unrelated
/// space (e.g. a `Load(REGISTER, …)` request answered from rodata bytes)
/// would produce wrong constants — implementations of `ReadOnlyMemory` MUST
/// return `None` for any space they do not back.
///
/// # Endianness
///
/// [`ReadOnlyMemory::read`][reader::ReadOnlyMemory::read] returns a `u64`
/// that already represents the target's *numeric* value — the impl is
/// responsible for byte-swapping according to the target's endianness
/// (see `reader::ElfFileMemReader`'s `read` for an LE/BE example). This
/// pass then masks the result to the load's output type via
/// [`NodeOutputType::get_unsigned_int`][ir::node::NodeOutputType::get_unsigned_int].
/// Callers must not double-swap.
///
/// Wrap a concrete memory implementation and add this optimizer to the pipeline:
///
/// ```rust
/// use opt::{LoadReadOnly, OptimizerPipeline};
/// use reader::ReadOnlyMemory;
///
/// struct MyRom;
/// impl ReadOnlyMemory for MyRom {
///     fn read(&self, _space: rsleigh::VnSpace, _addr: u64, _size: usize) -> Option<u64> {
///         None
///     }
/// }
///
/// let mut pipeline = OptimizerPipeline::new();
/// pipeline.add(LoadReadOnly(MyRom));
/// ```
pub struct LoadReadOnly<M>(pub M);

impl<M: ReadOnlyMemory + 'static> Optimizer for LoadReadOnly<M> {
    fn optimize(&self, ctx: &mut pattern::RewriteCtx<'_>) -> crate::Result<OptimizationResult> {
        // Only Load nodes are candidates — kind-filter at the iterator
        // level rather than collecting all N reachable nodes and
        // skipping non-Loads in the body.
        let mut work = WorkSet::seeded_kind(ctx, |k| matches!(k, NodeKind::Load(_)));
        let mut result = OptimizationResult::NoChange;

        while let Some(node_id) = work.pop() {
            let kind = *ctx.graph.node_kind(node_id);
            let NodeKind::Load(space) = kind else {
                continue;
            };

            // Load inputs: [memory_token, addr].
            let inputs = ctx.graph.node_inputs(node_id);
            if inputs.len() < 2 {
                continue;
            }
            let addr_input = inputs[1];
            let Some(addr) = ctx.graph.int_const_val(addr_input) else {
                continue;
            };

            // Load output: the single value output carries the loaded data type.
            let [data_out] = ctx.graph.node_outputs_exact::<1>(node_id)?;
            let Some(ty) = ctx.graph.output_kind(data_out).as_value() else {
                continue;
            };
            let size = ty.byte_size();
            // `ReadOnlyMemory::read` returns `Option<u64>` — bail on
            // wider loads (U80 / U128 / U256 / U512) rather than asking
            // the impl to truncate silently into a u64.
            if size > 8 {
                continue;
            }
            let Some(loaded) = self.0.read(space, addr, size) else {
                continue;
            };

            let Some(masked) = ty.get_unsigned_int(u128::from(loaded)).and_then(|v| u64::try_from(v).ok()) else {
                continue;
            };
            let new_out = ctx.graph.make_int_const(masked, ty)?;
            result = result.after_replace(ctx, data_out, new_out);
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests;
