//! Forwards the value of a `StackStore{offset: K}` to a subsequent
//! `Load[sp + K]` when the load's memory input traces back to that store with
//! no aliasing writes in between.  When a `MemPhi` sits between store and
//! load and every predecessor resolves to a store at the same offset, the
//! load is replaced with a synthesized [`NodeKind::Phi(None)`](strider_ir::NodeKind::Phi) sharing the
//! `MemPhi`'s phi-token.
//!
//! Must be wired into the pipeline with the calling convention's stack-pointer
//! varnode and the target's endianness (see [`StackLoadForward::new`]).

use strider_ir::node::{NodeId, NodeKind, NodeOutputId, NodeOutputKind, NodeOutputType};
use strider_target::Endianness;

use crate::opt::error::Result;
use crate::opt::pipeline::{OptimizationResult, Optimizer};
use crate::opt::sp_expr::{
    AliasStep, SpExpr, SpExprMemo, decompose_sp, ranges_disjoint, step_through_stack_store_phi,
    step_through_store,
};
use crate::opt::worklist::seeded_kind;

/// Store-to-load forwarding for SP-relative stack slots.
///
/// Runs inside the main fixed-point loop so that specializations produced by
/// `StackStoreDetect` become visible to the walker on subsequent iterations,
/// and so that forwarded constants fed into expressions are in turn
/// simplified by `ConstantFold` / `KnownBits`.
#[derive(Clone)]
pub struct StackLoadForward {
    /// Varnode for the stack pointer register (e.g. `ESP`, `RSP`, `sp`).
    pub stack_ptr_vn: rsleigh::Vn,
    /// Target endianness — controls how a narrow load from a wider store is
    /// synthesised (LE: low bytes via `Truncate`; BE: high bytes via
    /// `Truncate(ShiftRight(data, (store_size - load_size) * 8))`).
    pub endianness: Endianness,
}

impl StackLoadForward {
    /// Creates a new pass for the given stack-pointer varnode and target
    /// endianness.
    #[must_use]
    pub fn new(stack_ptr_vn: rsleigh::Vn, endianness: Endianness) -> Self {
        Self {
            stack_ptr_vn,
            endianness,
        }
    }

    /// Creates a new pass whose stack-pointer varnode is taken from `cc` and
    /// whose endianness is taken from `arch`.
    #[must_use]
    pub fn from_convention(
        cc: &strider_target::BuiltCallingConvention,
        arch: &strider_target::SleighArch,
    ) -> Self {
        Self::new(cc.stack_ptr_vn, arch.endianness())
    }
}

impl Optimizer for StackLoadForward {
    fn optimize(&self, ctx: &mut crate::pattern::RewriteCtx<'_>) -> Result<OptimizationResult> {
        let mut work = seeded_kind(ctx, |k| matches!(k, NodeKind::Load(_)));
        let mut memo: SpExprMemo = Default::default();
        let mut result = OptimizationResult::NoChange;
        while let Some(load) = work.dequeue() {
            result |= try_forward_load(ctx, load, self.stack_ptr_vn, self.endianness, &mut memo)?;
        }
        Ok(result)
    }
}

/// Tries to forward a single `Load[sp + K]` to the value of a matching
/// upstream `StackStore{offset: K}`.  Returns `Changed` iff the load's uses
/// were rewired.
fn try_forward_load(
    ctx: &mut crate::pattern::RewriteCtx<'_>,
    load: NodeId,
    sp_vn: rsleigh::Vn,
    endianness: Endianness,
    memo: &mut SpExprMemo,
) -> Result<OptimizationResult> {
    // Load inputs: [memory, addr].
    let [mem, addr] = ctx.node_inputs_exact::<2>(load)?;
    let [load_out] = ctx.node_outputs_exact::<1>(load)?;
    let Some(load_ty) = ctx.output_kind(load_out).as_value() else {
        return Ok(OptimizationResult::NoChange);
    };

    let mut visiting: entity_utils::DenseEntitySet<strider_ir::node::NodeId> = entity_utils::DenseEntitySet::new();
    let Some(SpExpr::Terminal { base: _, offset }) =
        decompose_sp(ctx.graph_ref(), addr, sp_vn, memo, &mut visiting)
    else {
        return Ok(OptimizationResult::NoChange);
    };

    let load_size = load_ty.byte_size() as i64;
    // Two-phase walk: probe is read-only and decides whether forwarding
    // can succeed; only on full success does realize commit fresh nodes
    // (Truncate / ShiftRight / ValuePhi) to the graph. This prevents
    // partial walks that fail downstream from leaving orphan nodes in
    // the arena.
    let mut visited: entity_utils::DenseEntitySet<NodeOutputId> = entity_utils::DenseEntitySet::new();
    let Some(shape) = probe(
        ctx,
        mem,
        offset,
        load_size,
        load_ty,
        sp_vn,
        memo,
        &mut visited,
    ) else {
        return Ok(OptimizationResult::NoChange);
    };
    let forwarded = realize(ctx, shape, load_ty, endianness, load)?;

    // Absorb the rewritten Load's asm-fingerprint into the forwarded
    // producer.  `realize` may have returned an existing-attributed node
    // (when the value comes straight from a StackStore's data slot) or
    // freshly synthesised one (Truncate / ShiftRight / ValuePhi).  When
    // `realize` synthesises multi-node chains (BE narrow path emits
    // `Truncate(ShiftRight(...))`), each intermediate node carries the
    // attribution via `create_node_attributed(..., &[load])` inside
    // `realize`; the call below covers the outermost-only LE narrow
    // and Existing cases.
    let forwarded_node = ctx.get_node_from_output(forwarded);
    ctx.extend_asm_fingerprint_from(forwarded_node, load);
    let changed = ctx.replace_all_uses(load_out, forwarded)?;
    if changed {
        ctx.detach_node_inputs(load);
    }
    Ok(OptimizationResult::from_changed(changed))
}

/// Description of how to materialize a forwarded value.  Built by
/// [`probe`] (which is read-only) and consumed by [`realize`] (which is
/// the only function that creates fresh IR nodes for forwarding).  Splitting
/// the walk this way prevents a partial probe — one that succeeds for some
/// MemPhi predecessors and fails for others — from leaking orphan nodes
/// (`Truncate`, `ShiftRight`, `ValuePhi`) into the graph arena.
enum ResolveShape {
    /// The forwarded value is an existing graph output and no new IR is
    /// needed.
    Existing(NodeOutputId),
    /// Narrow-load-from-wider-store at a matching offset.  `realize`
    /// synthesizes `Truncate(data)` (LE) or `Truncate(ShiftRight(data, k))`
    /// (BE) using `data_ty` to size the shift.
    Narrow {
        data: NodeOutputId,
        data_ty: strider_ir::node::NodeOutputType,
    },
    /// MemPhi resolution.  `realize` recursively materializes each
    /// predecessor first; if every predecessor materializes to the same
    /// `NodeOutputId` it returns that one without creating a `ValuePhi`,
    /// otherwise it creates a `ValuePhi { phi_token, vals... }`.
    Phi {
        phi_token: NodeOutputId,
        preds: Vec<ResolveShape>,
    },
}

/// Iterative read-only walk of the memory chain backward from `mem`
/// looking for a provable source of the bytes
/// `[offset, offset + load_size)` at type `load_ty`.  Mirrors the
/// structure of the previous recursive `probe` but is stack-safe at
/// any memory-chain depth (a long sequence of disjoint StackStores
/// or non-aliasing Stores no longer burns one stack frame per node).
///
/// **Frame model:**
/// * Linear walks (StackStore offset-disjoint, Store passthrough) are
///   a tight inner loop with zero heap allocation.
/// * MemPhi (the only multi-pred case) pushes a continuation frame
///   storing the partial `collected: Vec<ResolveShape>`; after each
///   predecessor's sub-walk returns a result, the continuation
///   pushes it into `collected` and either chains the next pred or
///   assembles the final `ResolveShape::Phi`.
///
/// Returns `None` if forwarding cannot be proven.
// Eight arguments are the minimum needed to thread cycle-guards, the SP
// decomposition memo, and the search-target byte range through the probe;
// bundling them into a context struct would just add indirection without
// clarifying the call sites.
#[allow(clippy::too_many_arguments)]
fn probe(
    ctx: &crate::pattern::RewriteCtx<'_>,
    initial_mem: NodeOutputId,
    offset: i64,
    load_size: i64,
    load_ty: strider_ir::node::NodeOutputType,
    sp_vn: rsleigh::Vn,
    memo: &mut SpExprMemo,
    visited: &mut entity_utils::DenseEntitySet<NodeOutputId>,
) -> Option<ResolveShape> {
    struct PhiFrame {
        phi_node: NodeId,
        phi_token: NodeOutputId,
        // Number of mem-predecessor slots; mem preds are at slots
        // 1..total_preds + 1 of `phi_node`'s inputs (slot 0 is the phi token).
        total_preds: usize,
        // Number of preds whose results are already in `collected`.
        // Equivalently the index of the next mem-pred slot to process,
        // offset by one (so the slot to read is `done_count + 1`).
        done_count: usize,
        collected: Vec<ResolveShape>,
    }

    let mut work: Vec<PhiFrame> = Vec::new();
    let mut mem = initial_mem;

    'outer: loop {
        // Linear walk inside one path — no heap allocation on any
        // chain of disjoint StackStores or non-aliasing Stores.
        let inner_result: Option<ResolveShape> = loop {
            let node = ctx.get_node_from_output(mem);
            match *ctx.node_kind(node) {
                NodeKind::StackStore {
                    offset: k,
                    space: _,
                } => {
                    let inputs = ctx.node_inputs(node);
                    if inputs.len() < 3 {
                        break None;
                    }
                    let data = inputs[2];
                    let Some(data_ty) = ctx.output_kind(data).as_value() else {
                        break None;
                    };
                    let store_size = data_ty.byte_size() as i64;
                    if k == offset {
                        if data_ty == load_ty {
                            break Some(ResolveShape::Existing(data));
                        } else if data_ty.is_integer()
                            && load_ty.is_integer()
                            && load_ty.byte_size() < data_ty.byte_size()
                        {
                            break Some(ResolveShape::Narrow { data, data_ty });
                        } else {
                            break None;
                        }
                    } else if ranges_disjoint(k, store_size, offset, load_size) {
                        mem = inputs[0];
                        continue;
                    } else {
                        break None;
                    }
                }
                NodeKind::Store(_) => {
                    match step_through_store(ctx.graph_ref(), node, sp_vn, memo, offset, load_size) {
                        AliasStep::MayAlias => break None,
                        AliasStep::PassThrough { prev_mem } => {
                            mem = prev_mem;
                            continue;
                        }
                    }
                }
                NodeKind::StackStorePhi { .. } => {
                    // A `StackStorePhi` records every per-predecessor stack
                    // offset on `Graph::stack_phi_offsets`; if every offset
                    // is provably disjoint from `(offset, load_size)`, the
                    // phi is a transparent step.  Mirrors the equivalent
                    // arm in `function_args::mem_chain_is_dirty`.  Without
                    // this, `Load[sp+K]` whose memory chain passes through
                    // a stack-store phi could never be forwarded — round
                    // fix.
                    match step_through_stack_store_phi(ctx.graph_ref(), node, offset, load_size) {
                        AliasStep::MayAlias => break None,
                        AliasStep::PassThrough { prev_mem } => {
                            mem = prev_mem;
                            continue;
                        }
                    }
                }
                NodeKind::MemPhi => {
                    // Cycle guard: loop-header MemPhis feed their own
                    // region indirectly.  Guard only at MemPhi
                    // boundaries — other memory nodes walk backward to
                    // strictly earlier producers and cannot cycle on
                    // their own.
                    if !visited.insert(mem) {
                        break None;
                    }
                    // MemPhi inputs: [phi_token, mem_pred_0, mem_pred_1, ...].
                    let inputs = ctx.node_inputs(node);
                    if inputs.len() < 2 {
                        break None;
                    }
                    let phi_token = inputs[0];
                    let total_preds = inputs.len() - 1;
                    let first_pred = inputs[1];
                    work.push(PhiFrame {
                        phi_node: node,
                        phi_token,
                        total_preds,
                        done_count: 0,
                        collected: Vec::with_capacity(total_preds),
                    });
                    mem = first_pred;
                    continue;
                }
                _ => break None,
            }
        };

        // Feed the inner result to the topmost PhiFrame, if any.
        let mut last_result = inner_result;
        loop {
            let Some(top) = work.last_mut() else {
                return last_result;
            };
            let Some(shape) = last_result else {
                // This MemPhi fails closed; pop and propagate.
                work.pop();
                last_result = None;
                continue;
            };
            top.collected.push(shape);
            top.done_count += 1;
            if top.done_count >= top.total_preds {
                // All preds collected; assemble the Phi shape.
                // The `last_mut` above proved the stack non-empty;
                // `pop` is therefore guaranteed to return Some.  We
                // still handle None conservatively to honour the
                // no-panic discipline (a later refactor that
                // mutates `work` between these two calls would
                // otherwise silently miscompile rather than panic).
                let Some(frame) = work.pop() else {
                    last_result = None;
                    return last_result;
                };
                last_result = Some(ResolveShape::Phi {
                    phi_token: frame.phi_token,
                    preds: frame.collected,
                });
                continue;
            }
            // Schedule the next pred.  Re-fetch inputs from the
            // graph (cheap, slice access) rather than caching the
            // pred list on the frame — saves a Vec allocation per
            // MemPhi.
            let next_slot = top.done_count + 1;
            let phi_inputs = ctx.node_inputs(top.phi_node);
            let next_mem = phi_inputs[next_slot];
            mem = next_mem;
            continue 'outer;
        }
    }
}

/// Materializes a [`ResolveShape`] into a concrete `NodeOutputId`,
/// creating any new IR nodes (`Truncate`, `ShiftRight`, `ValuePhi`) only
/// once the entire shape is known.  The dedup of identical predecessor
/// values for `Phi` happens here as well: if every realized predecessor
/// shares the same output id, no `ValuePhi` is created.
///
/// `Result<_, _>` is needed only because `make_int_const` can fail when
/// the IR rejects the requested constant; structurally the realization
/// is a deterministic walk over the shape tree.
fn realize(
    ctx: &mut crate::pattern::RewriteCtx<'_>,
    shape: ResolveShape,
    load_ty: strider_ir::node::NodeOutputType,
    endianness: Endianness,
    load: strider_ir::node::NodeId,
) -> crate::opt::Result<NodeOutputId> {
    match shape {
        ResolveShape::Existing(out) => Ok(out),
        ResolveShape::Narrow { data, data_ty } => {
            // - LE: load bytes are the low `load_size` bytes of the stored
            //   value → `Truncate(data)`.
            // - BE: load bytes are the high `load_size` bytes →
            //   `Truncate(ShiftRight(data, (store_size - load_size) * 8))`.
            //   `ShiftRight` is the *logical* right-shift (zero-fill), the
            //   correct synthesis since we want the high bytes positioned
            //   in the low end before truncating.
            //
            // Use `create_node_attributed(..., &[load])` for every
            // freshly-synthesised node so the asm-fingerprint contract
            // holds at every intermediate node — not just the outermost.
            // The caller in `try_forward_load` only absorbs into the
            // returned outermost node, so a plain `create_node` would
            // leave the BE-path `ShiftRight` node reachable with an
            // empty fingerprint.
            let shifted = match endianness {
                Endianness::Little => data,
                Endianness::Big => {
                    let shift_bits =
                        ((data_ty.byte_size() - load_ty.byte_size()) as u64) * 8;
                    // `make_int_const` does NOT stamp asm-fingerprints (it's
                    // the low-level `Graph` method, not the `FunctionBuilder`
                    // one).  Build the IntConst via `create_node_attributed`
                    // so the freshly-introduced constant inherits the
                    // rewritten load's fingerprint — otherwise the Layer-C
                    // always-on check trips on the BE narrow-shift constant
                    // (e.g. `IntConst(32)` for a U64→U32 narrow on aarch64be).
                    let shift_const_node = ctx.create_node_attributed(
                        NodeKind::IntConst(u128::from(shift_bits) & data_ty.bit_mask_u128()),
                        [],
                        [NodeOutputKind::OutputType(data_ty)],
                        &[load],
                    );
                    let [shift_const] = ctx.node_outputs_exact::<1>(shift_const_node)?;
                    let shr = ctx.create_node_attributed(
                        NodeKind::IntBinaryOp(strider_ir::IntBinaryOp::ShiftRight),
                        [data, shift_const],
                        [NodeOutputKind::OutputType(data_ty)],
                        &[load],
                    );
                    let [out] = ctx.node_outputs_exact::<1>(shr)?;
                    out
                }
            };
            let trunc = ctx.create_node_attributed(
                NodeKind::Truncate,
                [shifted],
                [NodeOutputKind::OutputType(load_ty)],
                &[load],
            );
            let [out] = ctx.node_outputs_exact::<1>(trunc)?;
            Ok(out)
        }
        ResolveShape::Phi { phi_token, preds } => {
            let mut resolved: Vec<NodeOutputId> = Vec::with_capacity(preds.len());
            for p in preds {
                resolved.push(realize(ctx, p, load_ty, endianness, load)?);
            }
            // Dedup: if all per-predecessor results coincide, skip the
            // ValuePhi — returning the common value keeps the graph
            // smaller and exposes it to later passes more cleanly.
            // `windows(2).all` is vacuously true for len < 2, but `probe`
            // already rejects MemPhi with fewer than 2 mem predecessors,
            // so `resolved.first()` is the actual emptiness guard here.
            if let Some(&first) = resolved.first()
                && resolved.windows(2).all(|w| w[0] == w[1])
            {
                return Ok(first);
            }
            let value_phi = ctx.create_node_attributed(
                NodeKind::Phi(None),
                std::iter::once(phi_token).chain(resolved),
                [NodeOutputKind::OutputType(load_ty)],
                &[load],
            );
            let [out] = ctx.node_outputs_exact::<1>(value_phi)?;
            Ok(out)
        }
    }
}


// ── Public helper for the indirect-branch classifier ──────
//
// `try_forward_load` rewrites the load by bottoming-out the memory chain at
// a `StackStore` and re-using its data slot.  When the load address has a
// concrete SP-relative offset, that's straightforward.  But the 
// computed-goto-via-stack-array shape has a *symbolic* offset
// (`sp + base + idx*stride`) — the per-i target lives at offset
// `base + i*stride` for i in [0, N), bounded by KnownBits.
//
// The indirect-branch classifier needs to enumerate per-i values without rewriting
// the load (no IR primitive expresses "value depends on idx" without a
// `ControlState` for ValuePhi to bind to).  This helper exposes the
// `StackStore`-chain walk as a pub function: given a memory chain root
// and a concrete offset, return the `NodeOutputId` of the value stored
// there (or `None` when the chain has no matching store, has an aliasing
// intermediate, or terminates at `InitialMemory`).
//
// SOUNDNESS — same algorithm as [`probe`]'s `StackStore` / `Store`
// arms, restricted to the no-MemPhi case (the classifier asks one
// concrete offset at a time):
//   * `StackStore { offset == requested }` with matching value type:
//     return the stored `data` output.  This is sound because no later
//     write can have aliased the slot — we walked here from the load's
//     memory input through strictly-earlier stores, and the offset
//     equality check is exact (StackStoreDetect tagged it).
//   * `StackStore` at a different offset: skip iff the byte ranges are
//     provably disjoint (`ranges_disjoint`); recurse on the prior
//     memory.
//   * `Store(_)` (raw, non-StackStore): probe its address.  If it's
//     not SP-rooted (`decompose_sp` returns `None`), it cannot alias
//     a stack slot; recurse.  If it IS SP-rooted (`Terminal`), recurse
//     iff disjoint.  `SpExpr::Phi` (SP through a phi) is conservatively
//     treated as aliasing → bail.
//   * `MemPhi`: cross-region join.  This helper does NOT recurse
//     across MemPhi (returns `None`) — the case is single-
//     region (the prologue stores and the dispatch load live in the
//     same region) and the classifier asks one offset at a time, so
//     the "all preds agree" reasoning the existing `probe` does for
//     ValuePhi synthesis is unnecessary here.  Future extension:
//     handle MemPhi by recursing into preds and requiring all to
//     return the same `NodeOutputId`.
//   * `InitialMemory` / anything else: return `None`.
//
// Type strictness: the helper returns `None` if the StackStore's value
// type doesn't equal `value_type` exactly.  Narrow-load-from-wider-store
// (which `probe` handles via `ResolveShape::Narrow`) is intentionally
// NOT implemented here — the classifier only consumes IntConst targets,
// and a Truncate(IntConst) folds to IntConst via ConstantFold, so the
// narrow case shows up as a wide-typed IntConst-valued store that the
// classifier can read directly.

/// Per-call memo for `find_stack_stored_value_at_offset`, keyed on
/// `(memory_token, offset, value_type)`.  Threaded through the
/// indirect-branch classifier loops so repeated lookups across
/// enumerated jump-table indices share their walks.
pub type StackStoredValueMemo =
    rustc_hash::FxHashMap<(NodeOutputId, i64, NodeOutputType), Option<NodeOutputId>>;

/// Walks the memory chain backward from `mem` looking for a
/// [`NodeKind::StackStore`] at SP-relative offset `offset` whose stored
/// value has type `value_type`.  Returns the stored value's output id
/// on success, or `None` when no matching store dominates the chain.
///
/// See the module-level "Public helper for the indirect-branch
/// classifier" notes for the soundness rules.
///
/// # Parameters
///
/// - `graph` — the IR graph to walk (read-only).
/// - `mem` — the chain root (typically a Load's memory-input slot).
/// - `offset` — the SP-relative offset of the requested slot.
/// - `value_type` — the expected stored value's type.  Mismatched
///   types return `None` (no Truncate / ShiftRight synthesis here).
/// - `sp_vn` — the calling convention's stack-pointer varnode (used
///   to interpret raw `Store(_)` addresses; matches the pass's
///   [`StackLoadForward::stack_ptr_vn`] field).
/// - `sp_memo` — a per-call SP-decomposition memo.  Reuse the same memo
///   across multiple calls for the same graph to amortise the cost
///   of decomposing repeated SP expressions.
/// - `walk_memo` — a per-call result memo keyed on `(mem, offset,
///   value_type)`.  Reuse it across multiple per-index lookups in the
///   indirect-branch classifier so shared chain prefixes pay O(1) per node.
#[must_use]
pub(crate) fn find_stack_stored_value_at_offset(
    graph: &strider_ir::Graph,
    mem: NodeOutputId,
    offset: i64,
    value_type: NodeOutputType,
    sp_vn: rsleigh::Vn,
    sp_memo: &mut SpExprMemo,
    walk_memo: &mut StackStoredValueMemo,
) -> Option<NodeOutputId> {
    // Iterative form (was recursive — see scale.md A1).  Walks the
    // memory-chain backward via StackStore.inputs[0] or
    // Store-passthrough's prev_mem.  Stack-safe at any chain depth.
    //
    // Visited stack records every `mem` node we passed through so we
    // can populate `walk_memo` for ALL of them once the terminal
    // result is known — preserves the prior memoisation behaviour
    // where every revisited prefix saved its result.
    let load_size = value_type.byte_size() as i64;
    let mut visited: Vec<(NodeOutputId, i64, NodeOutputType)> = Vec::new();
    let mut cur_mem = mem;

    let result: Option<NodeOutputId> = loop {
        let key = (cur_mem, offset, value_type);
        if let Some(&cached) = walk_memo.get(&key) {
            break cached;
        }
        visited.push(key);
        let node = graph.get_node_from_output(cur_mem);
        match *graph.node_kind(node) {
            NodeKind::StackStore { offset: k, space: _ } => {
                let inputs = graph.node_inputs(node);
                if inputs.len() < 3 {
                    break None;
                }
                let data = inputs[2];
                let data_ty = graph.output_kind(data).as_value();
                match data_ty {
                    None => break None,
                    Some(data_ty) if k == offset => {
                        if data_ty == value_type {
                            break Some(data);
                        }
                        break None;
                    }
                    Some(data_ty) => {
                        let store_size = data_ty.byte_size() as i64;
                        if ranges_disjoint(k, store_size, offset, load_size) {
                            cur_mem = inputs[0];
                            continue;
                        }
                        break None;
                    }
                }
            }
            NodeKind::Store(_) => {
                match step_through_store(graph, node, sp_vn, sp_memo, offset, load_size) {
                    AliasStep::MayAlias => break None,
                    AliasStep::PassThrough { prev_mem } => {
                        cur_mem = prev_mem;
                        continue;
                    }
                }
            }
            // MemPhi / InitialMemory / anything else: bail.  See module
            // notes for why MemPhi handling is intentionally future work.
            _ => break None,
        }
    };

    // Memoise every prefix on the way back so future queries reuse work.
    for key in visited {
        walk_memo.insert(key, result);
    }
    result
}

#[cfg(test)]
mod tests;
