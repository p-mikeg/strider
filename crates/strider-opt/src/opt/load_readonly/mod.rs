use strider_ir::IRBuilderExt;
use strider_ir::IRViewer;
use strider_ir::ReadOnlyMemory;
use strider_ir::node::{NodeId, NodeKind};

use crate::error::Result;
use crate::pipeline::{OptCtx, OptimizationResult, Optimizer};

// ── LoadReadOnly optimizer ────────────────────────────────────────────────────

/// Resolves `Load` nodes with constant addresses against a
/// [`ReadOnlyMemory`] image, replacing them with the loaded constant value.
///
/// # Immutability contract
///
/// The fold does NOT consult the load's memory-token chain — it replaces
/// the load with the bytes the rom resolves and trusts that those bytes
/// are runtime-immutable.  The supplied [`ReadOnlyMemory`] rom MUST
/// therefore contain ONLY runtime-immutable memory (code / `.rodata`,
/// never writable `.data` / `.got` / `.data.rel.ro` / stack): a
/// writable global that is stored and later reloaded would otherwise
/// fold to its stale file-initial value, discarding the store.  See the
/// [`ReadOnlyMemory`][strider_ir::ReadOnlyMemory] trait docs.
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
/// those bytes into an integer per the target byte order read from the
/// function under analysis (`Function::endianness`, the single source of
/// truth, via
/// [`Endianness::read_uint`][strider_target::Endianness::read_uint]),
/// then masks the result to the load's output type via
/// [`ValueType::get_unsigned_int`][strider_ir::node::ValueType::get_unsigned_int].
///
/// # Rom plumbing
///
/// The rom image is no longer stored on the pass: it flows through the
/// per-run [`OptCtx`] threaded by [`crate::OptimizerPipeline::run`].
/// When `ctx.rom` is `None` the pass short-circuits to
/// [`OptimizationResult::NoChange`] — this is the canonical "no rom
/// configured" path (`strider.run(..., rom=None)`).  The orchestrator
/// constructs the `OptCtx` rom from the analysis driver; ad-hoc callers
/// driving the pipeline directly construct one via
/// [`OptCtx::new`].
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
/// let ctx = OptCtx::new(Some(&rom));
/// # let _ = (pipeline, ctx);
/// ```
#[derive(Clone, Copy)]
pub struct LoadReadOnly;

impl Optimizer for LoadReadOnly {
    fn apply(
        &self,
        edit: &mut crate::EditFunction<'_>,
        ctx: &mut OptCtx<'_>,
    ) -> Result<OptimizationResult> {
        let Some(rom) = ctx.rom else {
            // No rom configured — nothing to fold.
            return Ok(OptimizationResult::NoChange);
        };
        // Snapshot the live `Load(RAM)` nodes up front: the borrow only needs
        // the immutable view, and it ends (the `Vec` is owned) before the
        // per-node folding loop takes `edit` mutably.  Each Load folds
        // INDEPENDENTLY against the read-only rom, so processing order does
        // not affect the outcome — iterate the cached live set directly
        // (`live_of_kind`, no graph walk) rather than canonicalising to RPO.
        // `ctx` here is the read-only `OptCtx` (carrying the rom) — `edit` is
        // the shared rewrite ctx.  The filter gates on `Load(RAM)` directly:
        // REGISTER / CONST / UNIQUE / OTHER Load nodes are folded by
        // varnode aliasing or constant propagation before reaching this
        // pass and `ReadOnlyMemory` only models RAM.
        let nodes: Vec<NodeId> = edit
            .live_of_kind(|k| matches!(k, NodeKind::Load(s) if *s == rsleigh::VnSpace::RAM))
            .collect();
        let mut overall = OptimizationResult::NoChange;
        for node_id in nodes {
            if try_fold_const_load_at(edit, node_id, rom)? {
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
/// Returns the first error reported by `build_int_const` or
/// `Graph::replace_all_uses` — both are structural by-construction
/// invariants in production, surfaced as `Err` for defensive
/// completeness.
pub(crate) fn try_fold_const_load_at(
    ctx: &mut crate::EditFunction<'_>,
    node_id: NodeId,
    rom: &dyn ReadOnlyMemory,
) -> Result<bool> {
    // SSoT: fold this Load via the shared const-eval utility (constant address
    // → ROM decode), so the decode logic is not duplicated in the jump-table
    // evaluator.
    let endianness = ctx.function().endianness();
    let [data_value] = ctx.node_outputs_exact::<1>(node_id)?;
    let resolve = |v| ctx.function().int_const_u128(v);
    let Some(masked) =
        crate::const_eval::eval_node_const(ctx.function(), data_value, &resolve, Some(rom), endianness)
    else {
        return Ok(false);
    };
    let ty = ctx
        .value_type_opt(data_value)
        .expect("Load output is a value");
    let new_value = ctx.build_int_const(masked, ty)?;
    // The loaded constant is justified by the load ADDRESS — *which* byte run
    // got read depends entirely on the address cone, which is about to be
    // cascade-culled once the Load is replaced.  Absorb the address producer's
    // asm-fingerprint into the new IntConst (the proof of why this value was
    // read) before the `replace_value` below removes it.  Over-tainting is
    // intentional — the fingerprint is a generous superset proof aid.
    let addr_value = ctx.load_addr(node_id);
    let addr_producer = ctx.producer(addr_value);
    let new_producer = ctx.producer(new_value);
    ctx.function_mut()
        .extend_asm_fingerprint_from(new_producer, addr_producer);
    // `replace_value` absorbs the rewritten Load's asm-fingerprint into the
    // new IntConst and redirects all uses (single SSoT for the pair).
    ctx.replace_value(data_value, new_value)
}

#[cfg(test)]
mod tests;
