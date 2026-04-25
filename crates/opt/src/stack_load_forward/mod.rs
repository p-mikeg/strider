//! Forwards the value of a `StackStore{offset: K}` to a subsequent
//! `Load[sp + K]` when the load's memory input traces back to that store with
//! no aliasing writes in between.  When a `MemPhi` sits between store and
//! load and every predecessor resolves to a store at the same offset, the
//! load is replaced with a synthesized [`NodeKind::ValuePhi`] sharing the
//! `MemPhi`'s phi-token.
//!
//! Must be wired into the pipeline with the calling convention's stack-pointer
//! varnode (see [`StackLoadForward::new`]).

use ir::BuiltFunctionGraph;
use ir::node::{NodeId, NodeKind, NodeOutputId, NodeOutputKind};

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
}

impl StackLoadForward {
    /// Creates a new pass for the given stack-pointer varnode.
    #[must_use]
    pub fn new(stack_ptr_vn: rsleigh::Vn) -> Self {
        Self { stack_ptr_vn }
    }

    /// Creates a new pass whose stack-pointer varnode is taken from the
    /// supplied calling convention.
    #[must_use]
    pub fn from_convention(cc: &target::BuiltCallingConvention) -> Self {
        Self::new(cc.stack_ptr_vn)
    }
}

impl Optimizer for StackLoadForward {
    fn optimize(&self, function: &mut BuiltFunctionGraph) -> Result<OptimizationResult> {
        let loads: Vec<NodeId> = function
            .preorder()
            .filter(|&n| matches!(function.graph.node_kind(n), NodeKind::Load(_)))
            .collect();
        let mut memo: SpExprMemo = Default::default();
        let mut result = OptimizationResult::NoChange;
        for load in loads {
            result |= try_forward_load(function, load, self.stack_ptr_vn, &mut memo)?;
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
    memo: &mut SpExprMemo,
) -> Result<OptimizationResult> {
    // Load inputs: [memory, addr].
    let [mem, addr] = fg.graph.node_inputs_exact::<2>(load)?;
    let [load_out] = fg.graph.node_outputs_exact::<1>(load)?;
    let Some(load_ty) = fg.graph.output_kind(load_out).as_value() else {
        return Ok(OptimizationResult::NoChange);
    };

    let mut visiting = std::collections::HashSet::new();
    let Some(SpExpr::Terminal { base: _, offset }) =
        decompose_sp(fg, addr, sp_vn, memo, &mut visiting)
    else {
        return Ok(OptimizationResult::NoChange);
    };

    let load_size = load_ty.byte_size() as i64;
    let mut visited = std::collections::HashSet::new();
    let Some(forwarded) = resolve(fg, mem, offset, load_size, load_ty, &mut visited) else {
        return Ok(OptimizationResult::NoChange);
    };

    let changed = fg.replace_all_uses(load_out, forwarded)?;
    if changed {
        fg.graph.detach_node_inputs(load);
    }
    Ok(OptimizationResult::from_changed(changed))
}

/// Walks memory backward from `mem` looking for a provable source of the
/// bytes `[offset, offset + load_size)` at type `load_ty`.  Returns
/// `Some(value_output)` when we can pin them to a stored value; `None`
/// otherwise (conservative bail).  When the walk crosses a [`NodeKind::MemPhi`]
/// and every predecessor resolves to a value, a fresh [`NodeKind::ValuePhi`]
/// is synthesized sharing the `MemPhi`'s phi-token and returned.
fn resolve(
    fg: &mut BuiltFunctionGraph,
    mem: NodeOutputId,
    offset: i64,
    load_size: i64,
    load_ty: ir::node::NodeOutputType,
    visited: &mut std::collections::HashSet<NodeOutputId>,
) -> Option<NodeOutputId> {
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
                    Some(data)
                } else if data_ty.is_integer()
                    && load_ty.is_integer()
                    && load_ty.byte_size() < data_ty.byte_size()
                {
                    // Narrow-load-from-wider-store at matching offset: on
                    // little-endian targets the load's bytes are exactly the
                    // low `load_size` bytes of the stored value, so a
                    // `Truncate` captures them.  Every calling-convention
                    // preset currently wired to this pass is LE; if a BE
                    // preset is added, this arm must be gated on endianness.
                    let trunc = fg.graph.create_node(
                        NodeKind::Truncate,
                        [data],
                        [NodeOutputKind::OutputType(load_ty)],
                    );
                    fg.graph.node_outputs(trunc).into_iter().next()
                } else {
                    None
                }
            } else if ranges_disjoint(k, store_size, offset, load_size) {
                let prev_mem = inputs[0];
                resolve(fg, prev_mem, offset, load_size, load_ty, visited)
            } else {
                None
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
            let inputs_vec: Vec<NodeOutputId> = fg.graph.node_inputs(node).into_iter().collect();
            if inputs_vec.len() < 2 {
                return None;
            }
            let phi_token = inputs_vec[0];
            let mut resolved: Vec<NodeOutputId> = Vec::with_capacity(inputs_vec.len() - 1);
            for pred_mem in &inputs_vec[1..] {
                let v = resolve(fg, *pred_mem, offset, load_size, load_ty, visited)?;
                resolved.push(v);
            }
            // Dedup: if all per-predecessor results are identical, skip the
            // ValuePhi — returning the common value directly keeps the graph
            // smaller and exposes it to later passes more cleanly.
            if resolved.iter().all(|v| *v == resolved[0]) {
                return Some(resolved[0]);
            }
            // Synthesize a ValuePhi [phi_token, val_0, val_1, ...].
            let value_phi = fg.graph.create_node(
                NodeKind::ValuePhi,
                std::iter::once(phi_token).chain(resolved),
                [NodeOutputKind::OutputType(load_ty)],
            );
            let outputs = fg.graph.node_outputs(value_phi);
            Some(outputs.into_iter().next()?)
        }
        _ => None,
    }
}


#[cfg(test)]
mod tests;
