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
//!   nodes whose address decomposes (via [`stack_store::decompose_sp`]) to
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
use crate::stack_store::{SpExpr, decompose_sp};

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
    let mut groups: std::collections::HashMap<usize, Vec<NodeId>> =
        std::collections::HashMap::new();
    let mut disqualified: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let node_ids: Vec<NodeId> = fg.preorder().collect();
    for node_id in node_ids {
        if !matches!(fg.graph.node_kind(node_id), NodeKind::Load(_)) {
            continue;
        }
        let [memory, addr] = fg.graph.node_inputs_exact::<2>(node_id)?;
        let mut visiting = std::collections::HashSet::new();
        let Some(SpExpr::Terminal { base: _, offset }) =
            decompose_sp(fg, addr, sp_vn, &mut visiting)
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
        if mem_chain_is_dirty(fg, memory, offset, &mut seen) {
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

/// DFS through memory predecessors looking for a store that may shadow slot
/// `offset`.  Treats `MemPhi` as a fork where **every** value predecessor must
/// be clean; `Call`/`PostCallMemState`/`CallOther` as pass-throughs (a caller
/// cannot alias the callee's incoming stack-arg area through a nested call).
///
/// Returns `true` if any path through the chain *may* overwrite `offset`.
///
/// `StackStorePhi` offsets are per-predecessor and stored in
/// `Graph::stack_phi_offsets`.  They are relative to `InitialVar(sp)` by
/// construction (the only place that populates them is `StackStoreDetect`),
/// so comparing directly against `offset` is sound.
fn mem_chain_is_dirty(
    fg: &BuiltFunctionGraph,
    mem: NodeOutputId,
    offset: i64,
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
            if k == offset {
                return true;
            }
            let inputs = fg.graph.node_inputs(node);
            // StackStore inputs: [MEM, SP, DATA].
            if inputs.is_empty() {
                false
            } else {
                mem_chain_is_dirty(fg, inputs[0], offset, seen)
            }
        }
        NodeKind::StackStorePhi { .. } => {
            if fg.graph.stack_phi_offsets(node).contains(&offset) {
                return true;
            }
            let inputs = fg.graph.node_inputs(node);
            // StackStorePhi inputs: [PHI, MEM, DATA].
            if inputs.len() < 2 {
                false
            } else {
                mem_chain_is_dirty(fg, inputs[1], offset, seen)
            }
        }
        NodeKind::Store(_) => true,
        NodeKind::MemPhi => {
            // Inputs: [PHI, MEM, MEM, ...].  Every value predecessor must be
            // clean for the phi to be clean.
            let inputs = fg.graph.node_inputs(node);
            inputs
                .into_iter()
                .skip(1)
                .any(|pred| mem_chain_is_dirty(fg, pred, offset, seen))
        }
        NodeKind::Call | NodeKind::CallOther { .. } => {
            // Inputs: [CTRL, MEM, ...args].  Recurse on pre-call memory.
            let inputs = fg.graph.node_inputs(node);
            if inputs.len() < 2 {
                false
            } else {
                mem_chain_is_dirty(fg, inputs[1], offset, seen)
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
                        mem_chain_is_dirty(fg, call_inputs[1], offset, seen)
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
mod tests {
    use super::*;
    use ir::node::{FunctionArgSource, NodeKind, NodeOutputType};
    use ir::{FunctionBuilder, IntBinaryOp};

    fn rdi_like_vn() -> rsleigh::Vn {
        // Fake 8-byte register to stand in for x86_64 RDI in tests.
        rsleigh::Vn {
            addr: rsleigh::VnAddr {
                space: rsleigh::VnSpace::REGISTER,
                off: 0x38,
            },
            size: 8,
        }
    }

    fn sp_vn() -> rsleigh::Vn {
        // Fake stack pointer; distinct offset so it doesn't alias an arg reg.
        rsleigh::Vn {
            addr: rsleigh::VnAddr {
                space: rsleigh::VnSpace::REGISTER,
                off: 0x20,
            },
            size: 8,
        }
    }

    fn count<F: Fn(&NodeKind) -> bool>(fg: &BuiltFunctionGraph, pred: F) -> usize {
        fg.all_node_ids()
            .filter(|&n| pred(fg.graph.node_kind(n)))
            .count()
    }

    /// Slice 1: x86_64-like convention passes arg 0 in a register.  A function
    /// that reads that register once should, after `FunctionArgDetect` runs,
    /// contain exactly one `FunctionArg { Register(rdi), 0 }` node, and the
    /// original `InitialVar(rdi)` use should have been rewired to it.
    #[test]
    fn reads_rdi_emits_function_arg_0() -> Result<()> {
        let rdi = rdi_like_vn();
        let sp = sp_vn();
        // new_raw(all_vns, callee_saved, ret_val_regs, arg_passing_regs, ...)
        let mut b = FunctionBuilder::new_raw(vec![rdi, sp], &[], &[rdi], &[rdi], None, 0)?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);

        // Build a trivial function that reads rdi and returns it.
        let v = b.read_variable(&rdi)?;
        b.build_return(Some(v), &[])?;
        let mut fg = b.build()?;

        let pass = FunctionArgDetect::new(vec![rdi], sp, vec![]);
        pass.optimize(&mut fg)?;

        let n_fa = count(&fg, |k| {
            matches!(
                k,
                NodeKind::FunctionArg {
                    source: FunctionArgSource::Register(r),
                    index: 0,
                } if *r == rdi
            )
        });
        assert_eq!(
            n_fa, 1,
            "expected exactly one FunctionArg {{ Register(rdi), 0 }}"
        );

        // The original InitialVar(rdi) should have no remaining live uses
        // (the Return should now source from the FunctionArg output).
        let reachable: std::collections::HashSet<_> = fg.preorder().collect();
        let reachable_initial_rdi = fg
            .all_node_ids()
            .filter(|n| reachable.contains(n))
            .filter(|&n| matches!(fg.graph.node_kind(n), NodeKind::InitialVar(v) if *v == rdi))
            .count();
        assert_eq!(
            reachable_initial_rdi, 0,
            "InitialVar(rdi) should be detached after rewiring"
        );
        Ok(())
    }

    /// Fake 4-byte SP for x86-cdecl-like scenarios.
    fn sp32_vn() -> rsleigh::Vn {
        rsleigh::Vn {
            addr: rsleigh::VnAddr {
                space: rsleigh::VnSpace::REGISTER,
                off: 0x20,
            },
            size: 4,
        }
    }

    /// Slice 2: x86 cdecl reads its first stack arg at `[sp + 4]`.  With no
    /// register args in the convention, the `Load[sp+4]` should be rewritten
    /// to a single `FunctionArg { Stack{offset:4}, 0 }` node and all consumers
    /// of the load rewired to it.
    #[test]
    fn reads_stack_arg_0_on_x86_cdecl() -> Result<()> {
        use crate::{ConstantFold, OptimizerPipeline};

        let sp = sp32_vn();
        let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);

        // addr = sp + 4; load[addr]; return loaded
        let sp_val = b.read_variable(&sp)?;
        let four = b.build_int_const(4, NodeOutputType::U32);
        let addr =
            b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, NodeOutputType::U32)?;
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
        b.build_return(Some(loaded), &[])?;
        let mut fg = b.build()?;

        // ConstantFold normalises the address; FunctionArgDetect runs after.
        let mut pipeline = OptimizerPipeline::new();
        pipeline.add(ConstantFold);
        pipeline.add_post_pass(FunctionArgDetect::new(vec![], sp, vec![4]));
        pipeline.run(&mut fg)?;

        let n_fa = count(&fg, |k| {
            matches!(
                k,
                NodeKind::FunctionArg {
                    source: FunctionArgSource::Stack { offset: 4, .. },
                    index: 0,
                }
            )
        });
        assert_eq!(
            n_fa, 1,
            "expected exactly one FunctionArg {{ Stack{{+4}}, 0 }}"
        );

        // The original Load should no longer be reachable (its single consumer,
        // the Return, now sources from the FunctionArg).
        let reachable: std::collections::HashSet<_> = fg.preorder().collect();
        let reachable_loads = fg
            .all_node_ids()
            .filter(|n| reachable.contains(n))
            .filter(|&n| matches!(fg.graph.node_kind(n), NodeKind::Load(_)))
            .count();
        assert_eq!(
            reachable_loads, 0,
            "Load[sp+4] should be detached after rewiring"
        );
        Ok(())
    }

    /// Builds `load[sp + offset]` reading a U32 value.  Returns the loaded output.
    fn build_sp_load(
        b: &mut FunctionBuilder,
        sp: &rsleigh::Vn,
        offset: u32,
    ) -> Result<ir::node::NodeOutputId> {
        let sp_val = b.read_variable(sp)?;
        let off_const = b.build_int_const(offset as u64, NodeOutputType::U32);
        let addr =
            b.build_int_binary_operation(sp_val, off_const, IntBinaryOp::Add, NodeOutputType::U32)?;
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
        Ok(loaded)
    }

    /// Slice 3: loads at sp+4 and sp+12, but **not** sp+8 — only the contiguous
    /// prefix (sp+4 → arg 0) is labelled.  The sp+12 load remains unchanged
    /// (i.e. it does **not** get FunctionArg index 2), and no gap-index node
    /// is emitted.
    #[test]
    fn stack_arg_gap_truncates() -> Result<()> {
        use crate::{ConstantFold, OptimizerPipeline};

        let sp = sp32_vn();
        let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);

        let a = build_sp_load(&mut b, &sp, 4)?;
        let c = build_sp_load(&mut b, &sp, 12)?;
        // Combine both loads so neither is dead.
        let sum = b.build_int_binary_operation(a, c, IntBinaryOp::Add, NodeOutputType::U32)?;
        b.build_return(Some(sum), &[])?;
        let mut fg = b.build()?;

        let mut pipeline = OptimizerPipeline::new();
        pipeline.add(ConstantFold);
        pipeline.add_post_pass(FunctionArgDetect::new(vec![], sp, vec![4, 8, 12]));
        pipeline.run(&mut fg)?;

        // Only arg 0 emitted; arg 1 absent (gap) and arg 2 MUST NOT be emitted.
        let arg0 = count(&fg, |k| {
            matches!(
                k,
                NodeKind::FunctionArg {
                    source: FunctionArgSource::Stack { offset: 4, .. },
                    index: 0,
                }
            )
        });
        let arg1 = count(&fg, |k| {
            matches!(
                k,
                NodeKind::FunctionArg {
                    source: FunctionArgSource::Stack { offset: 8, .. },
                    ..
                }
            )
        });
        let arg2 = count(&fg, |k| {
            matches!(
                k,
                NodeKind::FunctionArg {
                    source: FunctionArgSource::Stack { offset: 12, .. },
                    ..
                }
            )
        });
        assert_eq!(arg0, 1, "arg 0 (sp+4) should be emitted");
        assert_eq!(arg1, 0, "arg 1 (sp+8) is absent");
        assert_eq!(arg2, 0, "arg 2 (sp+12) must be truncated by the gap");

        // The sp+12 load must still exist and be reachable.
        let reachable: std::collections::HashSet<_> = fg.preorder().collect();
        let reachable_loads = fg
            .all_node_ids()
            .filter(|n| reachable.contains(n))
            .filter(|&n| matches!(fg.graph.node_kind(n), NodeKind::Load(_)))
            .count();
        assert_eq!(
            reachable_loads, 1,
            "sp+12 Load should remain (sp+4 Load replaced)"
        );
        Ok(())
    }

    /// Slice 4: a prior `StackStore{+4}` shadows the `Load[sp+4]` — the load
    /// reads the stored value, not the caller's arg.  No FunctionArg emitted.
    #[test]
    fn prior_stackstore_shadows() -> Result<()> {
        use crate::{ConstantFold, OptimizerPipeline, RedundantPhis, StackStoreDetect};

        let sp = sp32_vn();
        let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);

        // *(sp + 4) = 0x11; return *(sp + 4)
        let sp_val = b.read_variable(&sp)?;
        let four = b.build_int_const(4, NodeOutputType::U32);
        let addr =
            b.build_int_binary_operation(sp_val, four, IntBinaryOp::Add, NodeOutputType::U32)?;
        let data = b.build_int_const(0x11, NodeOutputType::U32);
        b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
        b.build_return(Some(loaded), &[])?;
        let mut fg = b.build()?;

        let mut pipeline = OptimizerPipeline::new();
        pipeline.add(ConstantFold);
        pipeline.add(RedundantPhis);
        pipeline.add(StackStoreDetect::new(sp));
        pipeline.add_post_pass(FunctionArgDetect::new(vec![], sp, vec![4]));
        pipeline.run(&mut fg)?;

        let any_fa = count(&fg, |k| matches!(k, NodeKind::FunctionArg { .. }));
        assert_eq!(
            any_fa, 0,
            "Load[sp+4] is shadowed by StackStore{{+4}}, not a function arg"
        );
        Ok(())
    }

    /// Slice 4 (audit B2 blocker): if-branch where the true side does
    /// `StackStore{+4}`, false side does nothing — their join is a `MemPhi`,
    /// and a later `Load[sp+4]` from the phi must be disqualified.  The DFS
    /// treats `MemPhi` as a fork where **every** predecessor must be clean.
    #[test]
    fn memphi_shadow_disqualifies() -> Result<()> {
        use crate::{ConstantFold, OptimizerPipeline, RedundantPhis, StackStoreDetect};

        let sp = sp32_vn();
        let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
        let entry = b.create_region()?;
        let true_br = b.create_region()?;
        let false_br = b.create_region()?;
        let join = b.create_region()?;
        b.set_entry_region(entry)?;

        // entry: if (<const true>) goto true_br else false_br
        //   (use a boolean const so the MemPhi has TWO predecessors in the
        //    graph even though DeadBranchElimination could collapse it — we
        //    skip that pass here to preserve the phi.)
        b.set_region(entry);
        let cond = b.build_boolean_const(true);
        b.build_if(cond, true_br, false_br)?;

        // true_br: *(sp+4) = 0x22; goto join
        b.set_region(true_br);
        let sp_t = b.read_variable(&sp)?;
        let four_t = b.build_int_const(4, NodeOutputType::U32);
        let addr_t = b.build_int_binary_operation(
            sp_t,
            four_t,
            IntBinaryOp::Add,
            NodeOutputType::U32,
        )?;
        let data = b.build_int_const(0x22, NodeOutputType::U32);
        b.build_store(addr_t, data, rsleigh::VnSpace::RAM)?;
        b.build_branch(join)?;

        // false_br: fallthrough to join
        b.set_region(false_br);
        b.build_branch(join)?;

        // join: return *(sp+4)
        b.set_region(join);
        let sp_j = b.read_variable(&sp)?;
        let four_j = b.build_int_const(4, NodeOutputType::U32);
        let addr_j = b.build_int_binary_operation(
            sp_j,
            four_j,
            IntBinaryOp::Add,
            NodeOutputType::U32,
        )?;
        let loaded = b.build_load(addr_j, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
        b.build_return(Some(loaded), &[])?;
        let mut fg = b.build()?;

        let mut pipeline = OptimizerPipeline::new();
        pipeline.add(ConstantFold);
        pipeline.add(RedundantPhis);
        pipeline.add(StackStoreDetect::new(sp));
        pipeline.add_post_pass(FunctionArgDetect::new(vec![], sp, vec![4]));
        pipeline.run(&mut fg)?;

        let any_fa = count(&fg, |k| matches!(k, NodeKind::FunctionArg { .. }));
        assert_eq!(
            any_fa, 0,
            "Load[sp+4] reaches a MemPhi with a shadowing branch — disqualified"
        );
        Ok(())
    }

    /// 8-byte SP varnode for aarch64-like scenarios.
    fn sp64_vn() -> rsleigh::Vn {
        rsleigh::Vn {
            addr: rsleigh::VnAddr {
                space: rsleigh::VnSpace::REGISTER,
                off: 0x40,
            },
            size: 8,
        }
    }

    /// Slice 5 (audit I2): if the same stack-arg slot is read at multiple
    /// widths — e.g. aarch64 reading both `x0` (8 bytes) and `w0` (4 bytes)
    /// from `sp+0` — emit **one** `FunctionArg` at the widest observed width
    /// and route narrower reads through `Truncate(FunctionArg)`.
    #[test]
    fn narrower_load_at_arg_slot_uses_truncate() -> Result<()> {
        use crate::{ConstantFold, OptimizerPipeline};

        let sp = sp64_vn();
        let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);

        // Read sp+0 as U32, then sp+0 as U64.  Combine so neither is dead.
        let sp_val = b.read_variable(&sp)?;
        let narrow = b.build_load(sp_val, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
        let wide = b.build_load(sp_val, rsleigh::VnSpace::RAM, NodeOutputType::U64)?;
        let narrow_ext =
            b.extend_if_needed(narrow, NodeOutputType::U64, ir::ExtendOp::ZeroExtend)?;
        let sum =
            b.build_int_binary_operation(narrow_ext, wide, IntBinaryOp::Add, NodeOutputType::U64)?;
        b.build_return(Some(sum), &[])?;
        let mut fg = b.build()?;

        let mut pipeline = OptimizerPipeline::new();
        pipeline.add(ConstantFold);
        pipeline.add_post_pass(FunctionArgDetect::new(vec![], sp, vec![0]));
        pipeline.run(&mut fg)?;

        // Exactly one FunctionArg at offset 0.
        let fa_count = count(&fg, |k| {
            matches!(
                k,
                NodeKind::FunctionArg {
                    source: FunctionArgSource::Stack { offset: 0, .. },
                    index: 0,
                }
            )
        });
        assert_eq!(fa_count, 1, "exactly one FunctionArg at offset 0");

        // That one FunctionArg must be at U64 (the widest observed load).
        let fa_node = fg
            .all_node_ids()
            .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::FunctionArg { .. }))
            .expect("FunctionArg exists");
        let [fa_out] = fg.graph.node_outputs_exact::<1>(fa_node)?;
        assert_eq!(
            fg.graph.output_kind(fa_out).as_value(),
            Some(NodeOutputType::U64),
            "FunctionArg output width should match widest load (U64)"
        );

        // The narrow (U32) use must be re-routed through a `Truncate` node
        // whose input is the FunctionArg's output.
        let reachable: std::collections::HashSet<_> = fg.preorder().collect();
        let trunc_from_fa = fg
            .all_node_ids()
            .filter(|n| reachable.contains(n))
            .filter(|&n| matches!(fg.graph.node_kind(n), NodeKind::Truncate))
            .filter(|&n| {
                let inputs = fg.graph.node_inputs(n);
                inputs.len() == 1 && inputs[0] == fa_out
            })
            .count();
        assert_eq!(
            trunc_from_fa, 1,
            "expected one Truncate consuming the FunctionArg output"
        );
        Ok(())
    }

    /// Audit I4: an `InitialVar(arg_reg)` with no live uses must not produce a
    /// `FunctionArg` node.  The pass is not pinning unreferenced registers.
    /// `FunctionArgDetect` runs after the fixed-point loop, so the setup here
    /// includes `RedundantPhis` to strip phantom phi consumers the builder
    /// creates during variable tracking.
    #[test]
    fn unused_register_arg_yields_no_node() -> Result<()> {
        use crate::{OptimizerPipeline, RedundantPhis};

        let rdi = rdi_like_vn();
        let sp = sp_vn();
        let mut b = FunctionBuilder::new_raw(vec![rdi, sp], &[], &[rdi], &[rdi], None, 0)?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);

        // Return a constant — rdi is never read.
        let c = b.build_int_const(0, NodeOutputType::U64);
        b.build_return(Some(c), &[])?;
        let mut fg = b.build()?;

        let mut pipeline = OptimizerPipeline::new();
        pipeline.add(RedundantPhis);
        pipeline.add_post_pass(FunctionArgDetect::new(vec![rdi], sp, vec![]));
        pipeline.run(&mut fg)?;

        let n_fa = count(&fg, |k| matches!(k, NodeKind::FunctionArg { .. }));
        assert_eq!(
            n_fa, 0,
            "unused InitialVar(rdi) must not be labelled as FunctionArg"
        );
        Ok(())
    }

    /// x86_64-like: two register args (rdi, rsi) and a stack arg at `sp+8`
    /// (i.e. arg 6 in SysV; for this test arg 2).  All three should become
    /// `FunctionArg` nodes, indexed 0, 1, and 2 respectively.
    #[test]
    fn x86_64_mixed_reg_and_stack() -> Result<()> {
        use crate::{ConstantFold, OptimizerPipeline};

        let rdi = rdi_like_vn();
        let rsi = rsleigh::Vn {
            addr: rsleigh::VnAddr {
                space: rsleigh::VnSpace::REGISTER,
                off: 0x30,
            },
            size: 8,
        };
        let sp = sp_vn();
        let mut b = FunctionBuilder::new_raw(
            vec![rdi, rsi, sp],
            &[],
            &[rdi],
            &[rdi, rsi],
            None,
            0,
        )?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);

        let a = b.read_variable(&rdi)?;
        let bb = b.read_variable(&rsi)?;
        let sp_val = b.read_variable(&sp)?;
        let eight = b.build_int_const(8, NodeOutputType::U64);
        let addr =
            b.build_int_binary_operation(sp_val, eight, IntBinaryOp::Add, NodeOutputType::U64)?;
        let c = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U64)?;
        let ab = b.build_int_binary_operation(a, bb, IntBinaryOp::Add, NodeOutputType::U64)?;
        let abc = b.build_int_binary_operation(ab, c, IntBinaryOp::Add, NodeOutputType::U64)?;
        b.build_return(Some(abc), &[])?;
        let mut fg = b.build()?;

        let mut pipeline = OptimizerPipeline::new();
        pipeline.add(ConstantFold);
        pipeline.add_post_pass(FunctionArgDetect::new(vec![rdi, rsi], sp, vec![8]));
        pipeline.run(&mut fg)?;

        let fa_reg0 = count(&fg, |k| {
            matches!(
                k,
                NodeKind::FunctionArg {
                    source: FunctionArgSource::Register(r),
                    index: 0,
                } if *r == rdi
            )
        });
        let fa_reg1 = count(&fg, |k| {
            matches!(
                k,
                NodeKind::FunctionArg {
                    source: FunctionArgSource::Register(r),
                    index: 1,
                } if *r == rsi
            )
        });
        let fa_stack2 = count(&fg, |k| {
            matches!(
                k,
                NodeKind::FunctionArg {
                    source: FunctionArgSource::Stack { offset: 8, .. },
                    index: 2,
                }
            )
        });
        assert_eq!(fa_reg0, 1, "rdi → FunctionArg index 0");
        assert_eq!(fa_reg1, 1, "rsi → FunctionArg index 1");
        assert_eq!(fa_stack2, 1, "sp+8 → FunctionArg index 2");
        Ok(())
    }

    /// Slice 3: an isolated high-offset load (sp+12) with no sp+4 or sp+8
    /// produces no FunctionArg at all — nothing starts the contiguous prefix.
    #[test]
    fn isolated_high_offset_load_dropped() -> Result<()> {
        use crate::{ConstantFold, OptimizerPipeline};

        let sp = sp32_vn();
        let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);

        let v = build_sp_load(&mut b, &sp, 12)?;
        b.build_return(Some(v), &[])?;
        let mut fg = b.build()?;

        let mut pipeline = OptimizerPipeline::new();
        pipeline.add(ConstantFold);
        pipeline.add_post_pass(FunctionArgDetect::new(vec![], sp, vec![4, 8, 12]));
        pipeline.run(&mut fg)?;

        let any_fa = count(&fg, |k| matches!(k, NodeKind::FunctionArg { .. }));
        assert_eq!(
            any_fa, 0,
            "isolated sp+12 load must not be labelled without arg 0/1"
        );
        Ok(())
    }
}
