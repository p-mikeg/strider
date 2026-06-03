use strider_ir::node::{NodeId, NodeKind};
use strider_ir::ReadOnlyMemory;

use crate::opt::OptRewrite;
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
/// [`ValueType::get_unsigned_int`][strider_ir::node::ValueType::get_unsigned_int].
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
        // Snapshot the reachable `Load(RAM)` nodes up front in global
        // reverse-post-order: the RPO borrow only needs the immutable
        // view, and it ends (the `Vec` is owned) before the per-node
        // folding loop takes `rctx` mutably.  The reachable SET matches
        // `walk()`; only the ORDER is canonicalised.  `ctx` here is the
        // read-only `OptCtx` (carrying the rom) — `rctx` is the shared
        // rewrite ctx.  The filter gates on `Load(RAM)` directly:
        // REGISTER / CONST / UNIQUE / OTHER Load nodes are folded by
        // varnode aliasing or constant propagation before reaching this
        // pass and `ReadOnlyMemory` only models RAM.
        let nodes: Vec<NodeId> = rctx
            .rpo_filter(|k| matches!(k, NodeKind::Load(s) if *s == rsleigh::VnSpace::RAM))
            .collect();
        // SSoT: decode the rom bytes with the function's own endianness.
        let endianness = rctx.function_ref().endianness();
        let mut overall = OptimizationResult::NoChange;
        for node_id in nodes {
            if try_fold_const_load_at(rctx, node_id, rom, endianness)? {
                overall = OptimizationResult::Changed;
            }
        }
        Ok(overall)
    }
}

/// Attempts to fold the `Load(RAM)` node at `node_id` against `rom`,
/// rewriting its single value output to an `IntConst` when the load's
/// address is constant and the rom can resolve the bytes.  Returns
/// `Ok(true)` iff a rewrite fired.
///
/// `node_id` MUST be a `Load(VnSpace::RAM)` node — the sole caller
/// ([`LoadReadOnly::apply`]) filters to that kind/space before calling,
/// and the address read below relies on the `Load` two-input arity
/// invariant.
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
    // Load inputs: [memory_token, addr] — exactly 2 once the kind is
    // established (validated structural invariant).
    let addr_value = ctx.graph_ref().node_inputs_exact::<2>(node_id)?[1];
    let Some(addr) = ctx.graph_ref().int_const_val(addr_value) else {
        return Ok(false);
    };
    // Load output: the single value output always carries the loaded data
    // type, and `Load` is integer-only (validated signature —
    // `outputs: [INT_VAL]`, an `AnyInt` slot).  A non-value / non-integer
    // here means malformed IR, not a fold we should silently skip.
    let [data_value] = ctx.node_outputs_exact::<1>(node_id)?;
    let ty = ctx
        .value_kind(data_value)
        .as_value()
        .expect("Load output is a value");
    let size = ty.byte_size();
    // Bail on wider-than-I128 loads (I256 / I512): the decode below tops
    // out at a 16-byte raw word — the full width of the `u128` carrier
    // that `IntConst` / `Endianness::read_uint` use.  Loads up to 16
    // bytes (I8…I128, including the x87 10-byte I80) fold; wider rodata
    // loads are left for a future pass rather than silently truncated.
    if size > 16 {
        return Ok(false);
    }
    // Read the RAW bytes (the reader no longer decodes), then decode to
    // an integer per the context's target endianness.  Fill-or-error:
    // a partial/unmapped range errors and we leave the Load intact.
    let mut bytes = [0u8; 16];
    if rom.read(addr, &mut bytes[..size]).is_err() {
        return Ok(false);
    }
    let loaded = endianness.read_uint(&bytes[..size]);
    // `ty` is an integer type (checked above), so the mask is infallible —
    // a `None` here would mean `Load` produced a float output, which the
    // validator forbids.
    let masked = ty
        .get_unsigned_int(loaded)
        .expect("Load output type is integer");
    let new_value = ctx.make_int_const(masked, ty)?;
    // `replace_value` absorbs the rewritten Load's asm-fingerprint into the
    // new IntConst and redirects all uses (single SSoT for the pair).
    ctx.replace_value(data_value, new_value)
}

#[cfg(test)]
mod tests;
