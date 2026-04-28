//! Forwards the value of a `StackStore{offset: K}` to a subsequent
//! `Load[sp + K]` when the load's memory input traces back to that store with
//! no aliasing writes in between.  When a `MemPhi` sits between store and
//! load and every predecessor resolves to a store at the same offset, the
//! load is replaced with a synthesized [`NodeKind::ValuePhi`] sharing the
//! `MemPhi`'s phi-token.
//!
//! Must be wired into the pipeline with the calling convention's stack-pointer
//! varnode and the target's endianness (see [`StackLoadForward::new`]).

use ir::BuiltFunctionGraph;
use ir::node::{NodeId, NodeKind, NodeOutputId, NodeOutputKind};
use target::Endianness;

use crate::error::Result;
use crate::pipeline::{OptimizationResult, Optimizer};
use crate::sp_expr::{SpExpr, SpExprMemo, decompose_sp, ranges_disjoint};

/// Store-to-load forwarding for SP-relative stack slots.
///
/// Runs inside the main fixed-point loop so that specializations produced by
/// `StackStoreDetect` become visible to the walker on subsequent iterations,
/// and so that forwarded constants fed into expressions are in turn
/// simplified by `ConstantFold` / `KnownBits`.
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
        cc: &target::BuiltCallingConvention,
        arch: &target::SleighArch,
    ) -> Self {
        Self::new(cc.stack_ptr_vn, arch.endianness)
    }
}

impl Optimizer for StackLoadForward {
    fn optimize(
        &self,
        graph: &mut ir::Graph,
        entry: ir::node::NodeId,
    ) -> Result<OptimizationResult> {
        // F2 bridge: opt's pass internals still operate on `&mut BuiltFunctionGraph`
        // via helper functions and the `pattern` crate's rewrite machinery.
        // `with_built` wraps the caller's `(&mut Graph, NodeId)` into a
        // temporary `BuiltFunctionGraph` for the duration of the pass.
        crate::pipeline::with_built(graph, entry, |function| self.optimize_built(function))
    }
}

impl StackLoadForward {
    fn optimize_built(&self, function: &mut BuiltFunctionGraph) -> Result<OptimizationResult> {
        let loads: Vec<NodeId> = function
            .preorder()
            .filter(|&n| matches!(function.graph.node_kind(n), NodeKind::Load(_)))
            .collect();
        let mut memo: SpExprMemo = Default::default();
        let mut result = OptimizationResult::NoChange;
        for load in loads {
            result |= try_forward_load(function, load, self.stack_ptr_vn, self.endianness, &mut memo)?;
        }
        Ok(result)
    }
}

/// Tries to forward a single `Load[sp + K]` to the value of a matching
/// upstream `StackStore{offset: K}`.  Returns `Changed` iff the load's uses
/// were rewired.
fn try_forward_load(
    fg: &mut BuiltFunctionGraph,
    load: NodeId,
    sp_vn: rsleigh::Vn,
    endianness: Endianness,
    memo: &mut SpExprMemo,
) -> Result<OptimizationResult> {
    // Load inputs: [memory, addr].
    let [mem, addr] = fg.graph.node_inputs_exact::<2>(load)?;
    let [load_out] = fg.graph.node_outputs_exact::<1>(load)?;
    let Some(load_ty) = fg.graph.output_kind(load_out).as_value() else {
        return Ok(OptimizationResult::NoChange);
    };

    let mut visiting = rustc_hash::FxHashSet::default();
    let Some(SpExpr::Terminal { base: _, offset }) =
        decompose_sp(&fg.graph, addr, sp_vn, memo, &mut visiting)
    else {
        return Ok(OptimizationResult::NoChange);
    };

    let load_size = load_ty.byte_size() as i64;
    // Two-phase walk: probe is read-only and decides whether forwarding
    // can succeed; only on full success does realize commit fresh nodes
    // (Truncate / ShiftRight / ValuePhi) to the graph. This prevents
    // partial walks that fail downstream from leaving orphan nodes in
    // the arena.
    let mut visited = rustc_hash::FxHashSet::default();
    let Some(shape) = probe(
        fg,
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
    let forwarded = realize(fg, shape, load_ty, endianness)?;

    let changed = fg.replace_all_uses(load_out, forwarded)?;
    if changed {
        fg.graph.detach_node_inputs(load);
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
        data_ty: ir::node::NodeOutputType,
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

/// Read-only walk of the memory chain backward from `mem` looking for a
/// provable source of the bytes `[offset, offset + load_size)` at type
/// `load_ty`.  Mirrors the structure of the previous `resolve` but does
/// not touch `fg.graph`; on success, returns a [`ResolveShape`] tree that
/// [`realize`] can turn into IR nodes.  Returns `None` if forwarding
/// cannot be proven.
// Eight arguments are the minimum needed to thread cycle-guards, the SP
// decomposition memo, and the search-target byte range through a recursive
// memory-chain probe; bundling them into a context struct would just add
// indirection without clarifying the call sites.
#[allow(clippy::too_many_arguments)]
fn probe(
    fg: &BuiltFunctionGraph,
    mem: NodeOutputId,
    offset: i64,
    load_size: i64,
    load_ty: ir::node::NodeOutputType,
    sp_vn: rsleigh::Vn,
    memo: &mut SpExprMemo,
    visited: &mut rustc_hash::FxHashSet<NodeOutputId>,
) -> Option<ResolveShape> {
    let node = fg.graph.get_node_from_output(mem);
    match *fg.graph.node_kind(node) {
        NodeKind::StackStore {
            offset: k,
            space: _,
        } => {
            // StackStore inputs: [MEM, SP, DATA].
            let inputs = fg.graph.node_inputs(node);
            if inputs.len() < 3 {
                return None;
            }
            let data = inputs[2];
            let data_ty = fg.graph.output_kind(data).as_value()?;
            let store_size = data_ty.byte_size() as i64;
            if k == offset {
                if data_ty == load_ty {
                    Some(ResolveShape::Existing(data))
                } else if data_ty.is_integer()
                    && load_ty.is_integer()
                    && load_ty.byte_size() < data_ty.byte_size()
                {
                    Some(ResolveShape::Narrow { data, data_ty })
                } else {
                    None
                }
            } else if ranges_disjoint(k, store_size, offset, load_size) {
                let prev_mem = inputs[0];
                probe(fg, prev_mem, offset, load_size, load_ty, sp_vn, memo, visited)
            } else {
                None
            }
        }
        // BUG-28 cause #2 also affects this pass: a non-aliasing `Store`
        // (a write to global / heap memory, which `StackStoreDetect`
        // didn't rewrite to `StackStore` because its address didn't
        // resolve to `sp + K`) on the memory chain previously
        // terminated forwarding.  Mirror `CallStackArgCollect`'s
        // resilience: probe the address, and if it provably is NOT
        // `sp + K` aliasing the load's byte range, walk through.
        NodeKind::Store(_) => {
            // Store inputs: [MEM, ADDR, DATA].
            let inputs = fg.graph.node_inputs(node);
            if inputs.len() < 3 {
                return None;
            }
            let addr = inputs[1];
            let mut sp_visiting = rustc_hash::FxHashSet::default();
            match decompose_sp(&fg.graph, addr, sp_vn, memo, &mut sp_visiting) {
                None => {
                    // Address is not SP-rooted: provably non-aliasing
                    // with the stack-arg byte range.  Continue.
                    let prev_mem = inputs[0];
                    probe(fg, prev_mem, offset, load_size, load_ty, sp_vn, memo, visited)
                }
                Some(SpExpr::Terminal { base: _, offset: store_off }) => {
                    // SP-rooted: only continue if the byte ranges are
                    // provably disjoint.  Store size taken from the
                    // value's declared NodeOutputType; the fallback
                    // (`i64::MAX`) is the soundness-preserving answer
                    // — it forces `ranges_disjoint` to return false
                    // and we conservatively terminate.  In valid IR
                    // a Store's DATA slot is value-typed by signature
                    // so the fallback is unreachable; the branch
                    // exists only as a defensive guardrail.
                    let data = inputs[2];
                    let store_size = fg
                        .graph
                        .output_kind(data)
                        .as_value()
                        .map_or(i64::MAX, |t| t.byte_size() as i64);
                    if ranges_disjoint(store_off, store_size, offset, load_size) {
                        let prev_mem = inputs[0];
                        probe(fg, prev_mem, offset, load_size, load_ty, sp_vn, memo, visited)
                    } else {
                        None
                    }
                }
                // SpExpr::Phi (SP-rooted but flowing through a phi
                // join): conservatively terminate, matching
                // CallStackArgCollect's posture — handling phi-of-SP
                // would require per-pred range analysis.
                Some(SpExpr::Phi { .. }) => None,
            }
        }
        NodeKind::MemPhi => {
            // Cycle guard: loop-header MemPhis feed their own region
            // indirectly.  Guard only at MemPhi boundaries — other memory
            // nodes walk backward to strictly earlier producers and cannot
            // cycle on their own, and guarding them would prevent sibling
            // branches from re-reaching a shared upstream node.
            if !visited.insert(mem) {
                return None;
            }
            // MemPhi inputs: [phi_token, mem_pred_0, mem_pred_1, ...].
            let inputs = fg.graph.node_inputs(node);
            if inputs.len() < 2 {
                return None;
            }
            let phi_token = inputs[0];
            let mut preds: Vec<ResolveShape> = Vec::with_capacity(inputs.len() - 1);
            for pred_mem in inputs.into_iter().skip(1) {
                preds.push(probe(fg, pred_mem, offset, load_size, load_ty, sp_vn, memo, visited)?);
            }
            Some(ResolveShape::Phi { phi_token, preds })
        }
        _ => None,
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
    fg: &mut BuiltFunctionGraph,
    shape: ResolveShape,
    load_ty: ir::node::NodeOutputType,
    endianness: Endianness,
) -> crate::Result<NodeOutputId> {
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
            let shifted = match endianness {
                Endianness::Little => data,
                Endianness::Big => {
                    let shift_bits =
                        ((data_ty.byte_size() - load_ty.byte_size()) as u64) * 8;
                    let shift_const = fg.make_int_const(shift_bits, data_ty)?;
                    let shr = fg.graph.create_node(
                        NodeKind::IntBinaryOp(ir::IntBinaryOp::ShiftRight),
                        [data, shift_const],
                        [NodeOutputKind::OutputType(data_ty)],
                    );
                    let [out] = fg.graph.node_outputs_exact::<1>(shr)?;
                    out
                }
            };
            let trunc = fg.graph.create_node(
                NodeKind::Truncate,
                [shifted],
                [NodeOutputKind::OutputType(load_ty)],
            );
            let [out] = fg.graph.node_outputs_exact::<1>(trunc)?;
            Ok(out)
        }
        ResolveShape::Phi { phi_token, preds } => {
            let mut resolved: Vec<NodeOutputId> = Vec::with_capacity(preds.len());
            for p in preds {
                resolved.push(realize(fg, p, load_ty, endianness)?);
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
            let value_phi = fg.graph.create_node(
                NodeKind::ValuePhi,
                std::iter::once(phi_token).chain(resolved),
                [NodeOutputKind::OutputType(load_ty)],
            );
            let [out] = fg.graph.node_outputs_exact::<1>(value_phi)?;
            Ok(out)
        }
    }
}


#[cfg(test)]
mod tests;
