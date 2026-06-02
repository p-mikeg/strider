use strider_ir::node::{NodeId, NodeKind};
use strider_ir::ReadOnlyMemory;

use crate::opt::error::Result;
use crate::opt::pipeline::{OptCtx, OptimizationResult, Optimizer};

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
/// [`ReadOnlyMemory::read`][strider_ir::ReadOnlyMemory::read] fills a
/// caller buffer with RAW bytes — it does NOT decode.  This pass decodes
/// those bytes into an integer per the target byte order carried on
/// [`OptCtx::endianness`] (via
/// [`Endianness::read_uint`][strider_target::Endianness::read_uint]),
/// then masks the result to the load's output type via
/// [`NodeOutputType::get_unsigned_int`][strider_ir::node::NodeOutputType::get_unsigned_int].
/// The orchestrator populates the context endianness from the run's
/// `SleighArch`.
///
/// # Rom plumbing
///
/// The rom image is no longer stored on the pass: it flows through the
/// per-run [`OptCtx`] threaded by [`crate::opt::OptimizerPipeline::run`].
/// When `ctx.rom` is `None` the pass short-circuits to
/// [`OptimizationResult::NoChange`] — this is the canonical "no rom
/// configured" path (`strider.run(..., rom=None)`).  The orchestrator
/// constructs the `OptCtx` from `RunConfig::rom`; ad-hoc callers
/// driving the pipeline directly construct one via
/// [`OptCtx::with_rom`].
///
/// ```rust
/// use strider_analyze::opt::{LoadReadOnly, OptCtx, OptimizerPipeline};
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
/// let ctx = OptCtx::with_rom(&rom);
/// # let _ = (pipeline, ctx);
/// ```
#[derive(Clone, Copy)]
pub struct LoadReadOnly;

impl Optimizer for LoadReadOnly {
    fn apply(
        &self,
        rctx: &mut strider_pattern::RewriteCtx<'_>,
        ctx: &OptCtx<'_>,
    ) -> Result<OptimizationResult> {
        let Some(rom) = ctx.rom else {
            // No rom configured — nothing to fold.
            return Ok(OptimizationResult::NoChange);
        };
        // Snapshot the reachable nodes up front: `walk()` only needs the
        // immutable view, and the borrow ends (the `Vec` is owned) before
        // the per-node folding loop takes `rctx` mutably.  `ctx` here is the
        // read-only `OptCtx` (carrying the rom) — `rctx` is the shared
        // rewrite ctx.
        let nodes: Vec<NodeId> = rctx.walk().collect();
        let mut overall = OptimizationResult::NoChange;
        for node_id in nodes {
            // Gate on Load(RAM) — REGISTER / CONST / UNIQUE / OTHER
            // Load nodes are folded by varnode aliasing or constant
            // propagation before reaching this pass.
            let NodeKind::Load(space) = *rctx.node_kind(node_id) else {
                continue;
            };
            if space != rsleigh::VnSpace::RAM {
                continue;
            }
            if try_fold_const_load_at(rctx, node_id, rom, ctx.endianness)? {
                overall = OptimizationResult::Changed;
            }
        }
        Ok(overall)
    }
}

/// Attempts to fold the `Load` node at `node_id` against `rom`,
/// rewriting its single value output to an `IntConst` when the load's
/// address is constant and the rom can resolve the bytes.  Returns
/// `Ok(true)` iff a rewrite fired.
///
/// Shared core of [`LoadReadOnly::optimize`] and the cfg-time
/// indirect-resolver's per-site load-folding loop.  Callers MUST have
/// already established that `node_id` is a `Load` node (the helper
/// short-circuits to `Ok(false)` for non-Load kinds or non-RAM spaces,
/// but exercising it on every reachable node would be wasteful).
///
/// Absorbs the rewritten Load's asm-fingerprint into the new
/// `IntConst` so the always-on Layer-C fingerprint check sees a
/// non-empty fingerprint on the freshly-introduced constant even when
/// the cache-hit dedup path returns an existing node.
///
/// # Errors
///
/// Returns the first error reported by `Graph::make_int_const` or
/// `Graph::replace_all_uses` — both are structural by-construction
/// invariants in production, surfaced as `Err` for defensive
/// completeness.
pub(crate) fn try_fold_const_load_at(
    ctx: &mut strider_pattern::RewriteCtx<'_>,
    node_id: NodeId,
    rom: &dyn ReadOnlyMemory,
    endianness: strider_target::Endianness,
) -> Result<bool> {
    // Defensive: callers may dispatch on the node kind themselves; the
    // double-check is cheap and keeps the helper safe to use on raw
    // node ids.
    let NodeKind::Load(space) = *ctx.node_kind(node_id) else {
        return Ok(false);
    };
    if space != rsleigh::VnSpace::RAM {
        return Ok(false);
    }
    // Load inputs: [memory_token, addr].
    let inputs = ctx.node_inputs(node_id);
    if inputs.len() < 2 {
        return Ok(false);
    }
    let addr_input = inputs[1];
    let Some(addr) = ctx.int_const_val(addr_input) else {
        return Ok(false);
    };
    // Load output: the single value output carries the loaded data type.
    let [data_out] = ctx.node_outputs_exact::<1>(node_id)?;
    let Some(ty) = ctx.output_kind(data_out).as_value() else {
        return Ok(false);
    };
    let size = ty.byte_size();
    // Bail on wider loads (I80 / I128 / I256 / I512): the decode below
    // tops out at a 8-byte raw word, matching the historic `u64`-width
    // fold.  Wider rodata loads are left for a future pass rather than
    // silently truncated.
    if size > 8 {
        return Ok(false);
    }
    // Read the RAW bytes (the reader no longer decodes), then decode to
    // an integer per the context's target endianness.  Fill-or-error:
    // a partial/unmapped range errors and we leave the Load intact.
    let mut bytes = [0u8; 8];
    if rom.read(addr, &mut bytes[..size]).is_err() {
        return Ok(false);
    }
    let loaded = endianness.read_uint(&bytes[..size]);
    let Some(masked) = ty.get_unsigned_int(loaded) else {
        return Ok(false);
    };
    let new_out = ctx.make_int_const(masked, ty)?;
    // `replace_value` absorbs the rewritten Load's asm-fingerprint into the
    // new IntConst and redirects all uses (single SSoT for the pair).
    ctx.replace_value(data_out, new_out)
}

#[cfg(test)]
mod tests;
