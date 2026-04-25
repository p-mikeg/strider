//! Detects function arguments and replaces their reads with canonical
//! [`NodeKind::FunctionArg`] nodes.
//!
//! Runs as a post-pass after the main fixed-point loop converges.  Rewrites
//! register-passed arg reads (`InitialVar(arg_reg)`) and stack-passed arg
//! reads (`Load[InitialVar(sp) + K]` unshadowed by any prior store) into a
//! single canonical form keyed by the argument's index in the calling
//! convention.
//!
//! # Detection rules
//!
//! * **Register args** (no contiguity constraint).  For each register
//!   `R = cc.arg_passing_regs[i]`, if `InitialVar(R)` has live uses in the
//!   graph, emit one `FunctionArg { Register(R), i }` and rewire every use
//!   of `InitialVar(R)`'s output to point at the new node.
//!
//! * **Stack args** (strict contiguity + no-shadow).  Collect all `Load`
//!   nodes whose address decomposes (via [`sp_expr::decompose_sp`]) to
//!   `InitialVar(sp) + K` with `K == cc.stack_arg_offsets[j]`.  Reject any
//!   whose memory input is reachable backward from a shadowing store — the
//!   walk is a DFS through memory predecessors that treats `MemPhi` as a
//!   fork where every predecessor must be non-disqualifying.  Disqualifying
//!   nodes: `StackStore { offset: K }`, `StackStorePhi` whose per-predecessor
//!   offsets contain `K`, and un-decomposed `Store` (may alias —
//!   conservative).  Non-disqualifying: `InitialMemory`, `Call`,
//!   `PostCallMemState`, `CallOther`, and stores at other offsets.  After
//!   filtering, emit only those indices that form a gap-free prefix starting
//!   at `first_stack_arg = arg_passing_regs.len()`; the first gap truncates.
//!
//! Each emitted `FunctionArg` has an output width equal to the widest load
//! observed at that offset (register sources use the container register's
//! natural width); narrower reads are rewired through `Truncate`.

use ir::BuiltFunctionGraph;
use ir::node::{FunctionArgSource, NodeId, NodeKind, NodeOutputId, NodeOutputKind, NodeOutputType};

use crate::error::Result;
use crate::pipeline::{OptimizationResult, Optimizer};
use crate::sp_expr::{SpExpr, SpExprMemo, decompose_sp, ranges_disjoint};

/// Replaces register-passed and stack-passed argument reads with canonical
/// [`NodeKind::FunctionArg`][ir::node::NodeKind::FunctionArg] nodes.  Intended
/// to run once, as an
/// [`OptimizerPipeline::add_post_pass`][crate::OptimizerPipeline::add_post_pass]
/// after the fixed-point loop has converged.
pub struct FunctionArgDetect {
    /// Varnodes (in positional order) used by the calling convention to pass
    /// integer arguments in registers.  Entry `i` is arg `i`.
    pub arg_passing_regs: Vec<rsleigh::Vn>,
    /// Varnode of the stack pointer register (used to recognise SP-relative
    /// stack-arg loads).
    pub stack_ptr_vn: rsleigh::Vn,
    /// Positional byte offsets of stack-passed arguments from the entry-time
    /// stack pointer.  Entry `j` is the offset of stack-arg `j`, which has
    /// overall argument index `arg_passing_regs.len() + j`.
    pub stack_arg_offsets: Vec<i64>,
}

impl FunctionArgDetect {
    /// Creates a new pass with an explicit register list, stack-pointer
    /// varnode, and stack-arg offset table.
    #[must_use]
    pub fn new(
        arg_passing_regs: Vec<rsleigh::Vn>,
        stack_ptr_vn: rsleigh::Vn,
        stack_arg_offsets: Vec<i64>,
    ) -> Self {
        Self {
            arg_passing_regs,
            stack_ptr_vn,
            stack_arg_offsets,
        }
    }

    /// Creates a new pass whose parameters are taken from the supplied
    /// calling convention.
    #[must_use]
    pub fn from_convention(cc: &target::BuiltCallingConvention) -> Self {
        Self::new(
            cc.arg_passing_regs.clone(),
            cc.stack_ptr_vn,
            cc.stack_arg_offsets.clone(),
        )
    }
}

impl Optimizer for FunctionArgDetect {
    fn optimize(&self, function: &mut BuiltFunctionGraph) -> Result<OptimizationResult> {
        let mut changed = OptimizationResult::NoChange;
        changed |= detect_register_args(function, &self.arg_passing_regs)?;
        changed |= detect_stack_args(
            function,
            self.stack_ptr_vn,
            &self.stack_arg_offsets,
            self.arg_passing_regs.len(),
        )?;
        // Replacing `Load[sp+K]` with `FunctionArg` orphans the address-
        // computation chain (`Add(sp, K)` and friends).  Those nodes remain in
        // the use-list of surviving producers like `InitialVar(sp)`, which
        // confuses downstream consumers that walk use-lists — e.g. the dot
        // renderer draws an edgeless `InitialVar(sp)` island.  Detach them.
        changed |= detach_unreachable_nodes(function);
        Ok(changed)
    }
}

/// Clears the inputs of every node not reachable from the function entry.
/// Mirrors [`crate::RedundantPhis`]' dead-block cleanup for the zombie nodes
/// this pass leaves behind.
fn detach_unreachable_nodes(fg: &mut BuiltFunctionGraph) -> OptimizationResult {
    let reachable: std::collections::HashSet<NodeId> = fg.preorder().collect();
    let mut changed = false;
    for node_id in fg.all_node_ids().collect::<Vec<_>>() {
        if !reachable.contains(&node_id) && !fg.graph.node_inputs(node_id).is_empty() {
            fg.graph.detach_node_inputs(node_id);
            changed = true;
        }
    }
    if changed {
        OptimizationResult::Changed
    } else {
        OptimizationResult::NoChange
    }
}

/// Rule D: for every register in `arg_passing_regs` whose `InitialVar` node
/// has live uses, emit one `FunctionArg { Register(reg), i }` and rewire all
/// those uses to it.  No contiguity check — reading only arg 2 still labels it
/// arg 2.
fn detect_register_args(
    fg: &mut BuiltFunctionGraph,
    arg_passing_regs: &[rsleigh::Vn],
) -> Result<OptimizationResult> {
    let mut result = OptimizationResult::NoChange;
    for (i, reg) in arg_passing_regs.iter().enumerate() {
        let Some(initial_var) = find_initial_var(fg, *reg) else {
            continue;
        };
        let [old_out] = fg.graph.node_outputs_exact::<1>(initial_var)?;
        // Skip if `InitialVar(reg)` has no consumers.
        if fg.graph.output_use_cursor(old_out).current().is_none() {
            continue;
        }

        let out_type = NodeOutputType::try_from(reg.size)?;
        let new_node = fg.graph.create_node(
            NodeKind::FunctionArg {
                source: FunctionArgSource::Register(*reg),
                index: i as u32,
            },
            [],
            [NodeOutputKind::OutputType(out_type)],
        );
        let [new_out] = fg.graph.node_outputs_exact::<1>(new_node)?;
        fg.replace_all_uses(old_out, new_out)?;
        result |= OptimizationResult::Changed;
    }
    Ok(result)
}

/// Rule (stack args): collect every `Load` node whose address decomposes to
/// `InitialVar(sp) + K` where `K` is one of the convention's stack-arg
/// offsets.  Group by `K`, then apply **strict contiguity** from position 0:
/// the first gap in the offset-set truncates, so surviving indices are a
/// gap-free prefix.  For each surviving `K`, emit one `FunctionArg` and rewire
/// every qualifying load's uses to it.
///
/// Slice 4 extends this with memory-shadow disqualification; slice 5 extends
/// it with width merging.
fn detect_stack_args(
    fg: &mut BuiltFunctionGraph,
    sp_vn: rsleigh::Vn,
    stack_arg_offsets: &[i64],
    first_stack_arg: usize,
) -> Result<OptimizationResult> {
    if stack_arg_offsets.is_empty() {
        return Ok(OptimizationResult::NoChange);
    }

    // Group candidate loads by their position `j` in `stack_arg_offsets`.
    // A load qualifies only if (a) its address decomposes to `sp + K` where
    // `K` is a convention offset, and (b) nothing on its memory chain may
    // alias that slot (DFS shadow check).  If *any* load at offset K is
    // shadowed, the whole K-group is disqualified (conservative).
    let mut memo: SpExprMemo = Default::default();
    let mut groups: std::collections::HashMap<usize, Vec<NodeId>> =
        std::collections::HashMap::new();
    let mut disqualified: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let node_ids: Vec<NodeId> = fg.preorder().collect();
    for node_id in node_ids {
        if !matches!(fg.graph.node_kind(node_id), NodeKind::Load(_)) {
            continue;
        }
        let [memory, addr] = fg.graph.node_inputs_exact::<2>(node_id)?;
        let [load_out] = fg.graph.node_outputs_exact::<1>(node_id)?;
        let Some(load_ty) = fg.graph.output_kind(load_out).as_value() else {
            continue;
        };
        let load_size = load_ty.byte_size() as i64;
        let mut visiting = std::collections::HashSet::new();
        let Some(SpExpr::Terminal { base: _, offset }) =
            decompose_sp(fg, addr, sp_vn, &mut memo, &mut visiting)
        else {
            continue;
        };
        let Some(j) = stack_arg_offsets.iter().position(|&k| k == offset) else {
            continue;
        };
        if disqualified.contains(&j) {
            continue;
        }
        let mut seen = std::collections::HashSet::new();
        if mem_chain_is_dirty(fg, memory, offset, load_size, &mut seen) {
            disqualified.insert(j);
            groups.remove(&j);
            continue;
        }
        groups.entry(j).or_default().push(node_id);
    }

    // Strict contiguity from j=0 — first gap truncates.
    let mut max_j_plus_one = 0usize;
    while groups.contains_key(&max_j_plus_one) {
        max_j_plus_one += 1;
    }
    if max_j_plus_one == 0 {
        return Ok(OptimizationResult::NoChange);
    }

    let mut result = OptimizationResult::NoChange;
    for (j, &offset) in stack_arg_offsets.iter().enumerate().take(max_j_plus_one) {
        let index = (first_stack_arg + j) as u32;
        let Some(loads) = groups.remove(&j) else {
            continue;
        };

        // Space from first load (all loads in a K-group share the same memory
        // space).  Per-load output types may differ — pick the widest.
        let first = loads[0];
        let space = match *fg.graph.node_kind(first) {
            NodeKind::Load(s) => s,
            _ => continue,
        };
        // Collect (load, out_type) pairs and find the max byte size.
        let mut load_types: Vec<(NodeId, NodeOutputType)> = Vec::with_capacity(loads.len());
        for load in &loads {
            let [out] = fg.graph.node_outputs_exact::<1>(*load)?;
            let Some(ty) = fg.graph.output_kind(out).as_value() else {
                continue;
            };
            load_types.push((*load, ty));
        }
        let Some(max_type) = load_types.iter().map(|(_, t)| *t).max_by_key(|t| t.byte_size())
        else {
            continue;
        };

        let new_node = fg.graph.create_node(
            NodeKind::FunctionArg {
                source: FunctionArgSource::Stack { space, offset },
                index,
            },
            [],
            [NodeOutputKind::OutputType(max_type)],
        );
        let [new_out] = fg.graph.node_outputs_exact::<1>(new_node)?;

        for (load, load_ty) in load_types {
            let [old_out] = fg.graph.node_outputs_exact::<1>(load)?;
            if load_ty == max_type {
                fg.replace_all_uses(old_out, new_out)?;
            } else {
                // Narrower read: insert a Truncate from the wider FunctionArg.
                let trunc = fg.graph.create_node(
                    NodeKind::Truncate,
                    [new_out],
                    [NodeOutputKind::OutputType(load_ty)],
                );
                let [trunc_out] = fg.graph.node_outputs_exact::<1>(trunc)?;
                fg.replace_all_uses(old_out, trunc_out)?;
            }
            fg.graph.detach_node_inputs(load);
        }
        result |= OptimizationResult::Changed;
    }
    Ok(result)
}

/// DFS through memory predecessors looking for a store that may shadow the
/// byte range `[offset, offset + load_size)`.  Treats `MemPhi` as a fork
/// where **every** value predecessor must be clean; `Call` / `PostCallMemState`
/// / `CallOther` as pass-throughs (a caller cannot alias the callee's
/// incoming stack-arg area through a nested call).
///
/// Returns `true` if any path through the chain *may* overwrite bytes in the
/// load's range.  A `StackStore` or `StackStorePhi` whose byte range overlaps
/// the load's is treated as a shadow; one whose range is strictly disjoint is
/// walked past.
///
/// `StackStorePhi` offsets are per-predecessor and stored in
/// `Graph::stack_phi_offsets`.  They are relative to `InitialVar(sp)` by
/// construction (the only place that populates them is `StackStoreDetect`).
fn mem_chain_is_dirty(
    fg: &BuiltFunctionGraph,
    mem: NodeOutputId,
    offset: i64,
    load_size: i64,
    seen: &mut std::collections::HashSet<NodeOutputId>,
) -> bool {
    if !seen.insert(mem) {
        // Already visited this edge — loop back-edge, treat as clean here
        // (other edges in the traversal will surface any real shadow).
        return false;
    }
    let node = fg.graph.get_node_from_output(mem);
    match *fg.graph.node_kind(node) {
        NodeKind::InitialMemory => false,
        NodeKind::StackStore { offset: k, .. } => {
            let inputs = fg.graph.node_inputs(node);
            // StackStore inputs: [MEM, SP, DATA].
            if inputs.len() < 3 {
                return false;
            }
            let Some(store_size) = value_byte_size(fg, inputs[2]) else {
                return true;
            };
            if !ranges_disjoint(k, store_size, offset, load_size) {
                return true;
            }
            mem_chain_is_dirty(fg, inputs[0], offset, load_size, seen)
        }
        NodeKind::StackStorePhi { .. } => {
            let inputs = fg.graph.node_inputs(node);
            // StackStorePhi inputs: [PHI, MEM, DATA].
            if inputs.len() < 3 {
                return false;
            }
            let Some(store_size) = value_byte_size(fg, inputs[2]) else {
                return true;
            };
            let any_overlap = fg
                .graph
                .stack_phi_offsets(node)
                .iter()
                .any(|&k| !ranges_disjoint(k, store_size, offset, load_size));
            if any_overlap {
                return true;
            }
            mem_chain_is_dirty(fg, inputs[1], offset, load_size, seen)
        }
        NodeKind::MemPhi => {
            // Inputs: [PHI, MEM, MEM, ...].  Every value predecessor must be
            // clean for the phi to be clean.
            let inputs = fg.graph.node_inputs(node);
            inputs
                .into_iter()
                .skip(1)
                .any(|pred| mem_chain_is_dirty(fg, pred, offset, load_size, seen))
        }
        NodeKind::Call | NodeKind::CallOther { .. } => {
            // Inputs: [CTRL, MEM, ...args].  Recurse on pre-call memory.
            let inputs = fg.graph.node_inputs(node);
            if inputs.len() < 2 {
                false
            } else {
                mem_chain_is_dirty(fg, inputs[1], offset, load_size, seen)
            }
        }
        NodeKind::PostCallMemState => {
            // Inputs: [CTRL] — the CTRL output of the originating Call.
            // Walk through it to the Call, then recurse on its MEM input.
            let inputs = fg.graph.node_inputs(node);
            if inputs.is_empty() {
                return false;
            }
            let call_node = fg.graph.get_node_from_output(inputs[0]);
            match *fg.graph.node_kind(call_node) {
                NodeKind::Call | NodeKind::CallOther { .. } => {
                    let call_inputs = fg.graph.node_inputs(call_node);
                    if call_inputs.len() < 2 {
                        false
                    } else {
                        mem_chain_is_dirty(fg, call_inputs[1], offset, load_size, seen)
                    }
                }
                // Unexpected producer for PostCallMemState's CTRL — be
                // conservative.
                _ => true,
            }
        }
        // Any other memory-producing node we don't recognise: be conservative.
        _ => true,
    }
}

/// Byte size of a value output, or `None` if it is not a value-typed output
/// (e.g. `Control` / `Memory`).
fn value_byte_size(fg: &BuiltFunctionGraph, out: NodeOutputId) -> Option<i64> {
    fg.graph.output_kind(out).as_value().map(|t| t.byte_size() as i64)
}

/// Locates the unique `InitialVar(reg)` node in the graph, if any.
///
/// `InitialVar` nodes are not hash-cached (see `NodeKind::is_cacheable`), so
/// they're found by linear scan; however the builder only creates one per
/// variable at entry-region setup, so at most one candidate exists.
fn find_initial_var(fg: &BuiltFunctionGraph, reg: rsleigh::Vn) -> Option<ir::node::NodeId> {
    fg.all_node_ids()
        .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::InitialVar(v) if *v == reg))
}

#[cfg(test)]
mod tests;
