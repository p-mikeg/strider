use strider_ir::node::{NodeId, NodeKind};
use strider_ir::ReadOnlyMemory;

use crate::opt::peephole::{PeepholePass, run_peephole};
use crate::opt::pipeline::{OptimizationResult, Optimizer};

// ── LoadReadOnly optimizer ────────────────────────────────────────────────────

/// Resolves `Load` nodes with constant addresses against a
/// [`ReadOnlyMemory`] image, replacing them with the loaded constant value.
///
/// # Memory-space contract
///
/// The pass forwards the `Load`'s [`rsleigh::VnSpace`] to
/// [`ReadOnlyMemory::read`][strider_ir::ReadOnlyMemory::read] verbatim and
/// trusts the impl to discriminate.  A rom that returns `Some(_)` for an
/// unrelated space (e.g. a `Load(REGISTER, …)` request answered from rodata
/// bytes) would produce wrong constants — implementations of
/// `ReadOnlyMemory` MUST return `None` for any space they do not back.
///
/// # Endianness
///
/// [`ReadOnlyMemory::read`][strider_ir::ReadOnlyMemory::read] returns a `u64`
/// that already represents the target's *numeric* value — the impl is
/// responsible for byte-swapping according to the target's endianness
/// (see `strider_reader::ElfFileMemReader`'s `read` for an LE/BE example). This
/// pass then masks the result to the load's output type via
/// [`NodeOutputType::get_unsigned_int`][strider_ir::node::NodeOutputType::get_unsigned_int].
/// Callers must not double-swap.
///
/// Wrap a concrete memory implementation and add this optimizer to the pipeline:
///
/// ```rust
/// use strider_analyze::opt::{LoadReadOnly, OptimizerPipeline};
/// use strider_ir::ReadOnlyMemory;
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

impl<M: ReadOnlyMemory + 'static> PeepholePass for LoadReadOnly<M> {
    fn name(&self) -> &'static str {
        "LoadReadOnly"
    }

    fn matches_kind(&self, kind: &NodeKind) -> bool {
        matches!(kind, NodeKind::Load(_))
    }

    fn try_rewrite(
        &self,
        ctx: &mut crate::pattern::RewriteCtx<'_>,
        root: NodeId,
    ) -> crate::opt::Result<OptimizationResult> {
        let kind = *ctx.node_kind(root);
        let NodeKind::Load(space) = kind else {
            return Ok(OptimizationResult::NoChange);
        };

        // Load inputs: [memory_token, addr].
        let inputs = ctx.node_inputs(root);
        if inputs.len() < 2 {
            return Ok(OptimizationResult::NoChange);
        }
        let addr_input = inputs[1];
        let Some(addr) = ctx.int_const_val(addr_input) else {
            return Ok(OptimizationResult::NoChange);
        };

        // Load output: the single value output carries the loaded data type.
        let [data_out] = ctx.node_outputs_exact::<1>(root)?;
        let Some(ty) = ctx.output_kind(data_out).as_value() else {
            return Ok(OptimizationResult::NoChange);
        };
        let size = ty.byte_size();
        // `ReadOnlyMemory::read` returns `Option<u64>` — bail on
        // wider loads (U80 / U128 / U256 / U512) rather than asking
        // the impl to truncate silently into a u64.
        if size > 8 {
            return Ok(OptimizationResult::NoChange);
        }
        let Some(loaded) = self.0.read(space, addr, size) else {
            return Ok(OptimizationResult::NoChange);
        };

        let Some(masked) = ty.get_unsigned_int(u128::from(loaded)).and_then(|v| u64::try_from(v).ok()) else {
            return Ok(OptimizationResult::NoChange);
        };
        let new_out = ctx.make_int_const(masked, ty)?;
        OptimizationResult::NoChange.after_replace(ctx, data_out, new_out)
    }

    /// Replacing a `Load` with a constant exposes its consumers to
    /// `ConstantFold` in the next pipeline iteration — no value gained
    /// from re-enqueuing `Load` consumers within this pass, since they
    /// won't match `Load(_)` again.
    fn propagate_to_consumers(&self) -> bool {
        false
    }
}

impl<M: ReadOnlyMemory + 'static> Optimizer for LoadReadOnly<M> {
    fn optimize(&self, ctx: &mut crate::pattern::RewriteCtx<'_>) -> crate::opt::Result<OptimizationResult> {
        run_peephole(self, ctx)
    }
}

#[cfg(test)]
mod tests;
