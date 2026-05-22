use std::sync::Arc;

use strider_ir::node::{NodeId, NodeKind};
use strider_ir::ReadOnlyMemory;

use crate::opt::peephole::{PeepholePass, impl_optimizer_from_peephole};
use crate::opt::pipeline::OptimizationResult;

// ── LoadReadOnly optimizer ────────────────────────────────────────────────────

/// Resolves `Load` nodes with constant addresses against a
/// [`ReadOnlyMemory`] image, replacing them with the loaded constant value.
///
/// # Memory-space contract
///
/// `ReadOnlyMemory` only models RAM.  The pass gates on
/// `Load(VnSpace::RAM)` at the call site and never asks the rom about
/// REGISTER / CONST / UNIQUE / OTHER spaces (those Load nodes are
/// folded by varnode aliasing or constant propagation before reaching
/// this pass).
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
/// use std::sync::Arc;
/// use strider_analyze::opt::{LoadReadOnly, OptimizerPipeline};
/// use strider_ir::ReadOnlyMemory;
///
/// struct MyRom;
/// impl ReadOnlyMemory for MyRom {
///     fn read(&self, _addr: u64, _size: usize) -> Option<u64> {
///         None
///     }
/// }
///
/// let mut pipeline = OptimizerPipeline::new();
/// pipeline.add(LoadReadOnly::new(Arc::new(MyRom)));
/// ```
pub struct LoadReadOnly {
    rom: Arc<dyn ReadOnlyMemory>,
}

impl LoadReadOnly {
    /// Construct a `LoadReadOnly` pass over an `Arc`-shared rom.  The
    /// pipeline holds `Box<dyn Optimizer>` and the production callers
    /// (orchestrator, cfg::options) already carry the rom as
    /// `Arc<dyn ReadOnlyMemory>`, so taking an `Arc` here makes the
    /// construction a no-op clone rather than a deep copy.
    pub fn new(rom: Arc<dyn ReadOnlyMemory>) -> Self {
        Self { rom }
    }
}

impl PeepholePass for LoadReadOnly {
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
        // `ReadOnlyMemory` only ever models RAM — REGISTER / CONST /
        // UNIQUE / OTHER Load nodes are folded by varnode aliasing or
        // constant propagation before reaching this pass.  Gate the
        // call so a misrouted non-RAM Load doesn't ask the rom for
        // bytes outside its semantic domain.
        if space != rsleigh::VnSpace::RAM {
            return Ok(OptimizationResult::NoChange);
        }

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
        let Some(loaded) = self.rom.read(addr, size) else {
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

impl_optimizer_from_peephole!(LoadReadOnly);

#[cfg(test)]
mod tests;
