//! Converts `Store` nodes whose address resolves to `InitialVar(stack_ptr) + K`
//! into dedicated [`NodeKind::StackStore`] / [`NodeKind::StackStorePhi`] nodes.
//!
//! Must be wired into the pipeline with the calling convention's stack-pointer
//! varnode (see [`StackStoreDetect::new`]).  A later pass
//! (`CallStackArgCollect`, run once after convergence) walks backward from each
//! `Call`'s memory input through these nodes to reconstruct stack-passed
//! arguments.

use ir::node::{NodeId, NodeInputId, NodeKind, NodeOutputId, NodeOutputKind};
use ir::{BuiltFunctionGraph, IntBinaryOp};

use crate::error::Result;
use crate::pipeline::{OptimizationResult, Optimizer};

/// Decomposed stack-pointer expression.
///
/// `Terminal` carries the output we treat as the SP base (either
/// `InitialVar(sp)` or a `ControlPhi(sp)` node whose predecessors couldn't be
/// fully reduced — e.g. a loop-header self-reference).  Tracking the base
/// explicitly keeps stores taken from different SP versions distinct.
pub(crate) enum SpExpr {
    /// `base + offset`, where `base` is an SP-rooted node.
    Terminal { base: NodeOutputId, offset: i64 },
    /// `ControlPhi(stack_ptr)` where every predecessor resolves to
    /// `InitialVar(stack_ptr) + offsets[j]`.
    Phi { phi_node: NodeId, offsets: Vec<i64> },
}

impl SpExpr {
    fn shifted(self, delta: i64) -> Self {
        match self {
            SpExpr::Terminal { base, offset } => SpExpr::Terminal {
                base,
                offset: offset.wrapping_add(delta),
            },
            SpExpr::Phi { phi_node, offsets } => SpExpr::Phi {
                phi_node,
                offsets: offsets.into_iter().map(|o| o.wrapping_add(delta)).collect(),
            },
        }
    }
}

/// True when `[a_off, a_off + a_size)` and `[b_off, b_off + b_size)` do not
/// overlap.  Used by shadow / forward walks in both
/// [`crate::stack_load_forward`] and [`crate::function_args`].
pub(crate) fn ranges_disjoint(a_off: i64, a_size: i64, b_off: i64, b_size: i64) -> bool {
    a_off + a_size <= b_off || b_off + b_size <= a_off
}

/// Reads an integer-constant output as a signed 64-bit value, sign-extended
/// from its declared bit width.  Returns `None` if `out` is not an integer
/// constant or its type isn't one of `U8`/`U16`/`U32`/`U64`.  SP arithmetic
/// happens modulo `2^width`, so a constant like `0xFFFFFFF8` in a U32 slot
/// represents `-8`, not `4294967288` — `NodeOutputType::get_signed_int` is the
/// single source of truth for that mapping.
fn int_const_signed(fg: &BuiltFunctionGraph, out: NodeOutputId) -> Option<i64> {
    let c = fg.int_const_val(out)?;
    fg.graph.output_kind(out).as_value()?.get_signed_int(c)
}

/// Recursively decomposes `out` into `InitialVar(sp) + K` or the per-branch
/// equivalent, walking through `Add`/`Sub` with a constant operand and
/// `ControlPhi(sp)` nodes.  Returns `None` when the expression cannot be
/// reduced to the stack pointer plus constants only.
///
/// `visiting` guards against data-flow cycles through `ControlPhi` back-edges
/// (e.g. a loop-header phi whose predecessor is `sp_phi - 4`).
pub(crate) fn decompose_sp(
    fg: &BuiltFunctionGraph,
    out: NodeOutputId,
    sp_vn: rsleigh::Vn,
    visiting: &mut std::collections::HashSet<NodeId>,
) -> Option<SpExpr> {
    let node = fg.graph.get_node_from_output(out);
    if !visiting.insert(node) {
        // Cycle: the expression refers to itself through a back-edge; we
        // cannot express it as `sp + K`.
        return None;
    }
    let result = match *fg.graph.node_kind(node) {
        NodeKind::InitialVar(vn) if vn == sp_vn => Some(SpExpr::Terminal {
            base: out,
            offset: 0,
        }),
        NodeKind::ControlPhi(vn) if vn == sp_vn => {
            // Try to resolve every predecessor to `InitialVar(sp) + K_j` so
            // that we can emit a `StackStorePhi`.  If any predecessor can't
            // be reduced (loop back-edge, nested phi, …), fall back to using
            // this phi itself as an opaque SP base — the store still lives
            // at a fixed offset from whatever SP value reaches this program
            // point, which is all `CallStackArgCollect` needs.
            let inputs = fg.graph.node_inputs(node);
            if inputs.len() < 2 {
                // Bare phi with no value predecessors: treat as opaque base.
                Some(SpExpr::Terminal {
                    base: out,
                    offset: 0,
                })
            } else {
                let mut offsets = Vec::with_capacity(inputs.len() - 1);
                let mut bases = Vec::with_capacity(inputs.len() - 1);
                let mut ok = true;
                for pred_input in inputs.into_iter().skip(1) {
                    match decompose_sp(fg, pred_input, sp_vn, visiting) {
                        Some(SpExpr::Terminal { base, offset }) => {
                            bases.push(base);
                            offsets.push(offset);
                        }
                        _ => {
                            ok = false;
                            break;
                        }
                    }
                }
                if ok {
                    // If every predecessor resolves to the same (base, offset)
                    // — e.g. two CFG edges feeding the phi both trace back to
                    // the same `sub esp, 4` node — the phi is structurally
                    // redundant.  Collapse to a plain Terminal so the store
                    // becomes a regular StackStore rather than a degenerate
                    // StackStorePhi([C, C, …]).
                    if bases.iter().all(|&b| b == bases[0])
                        && offsets.iter().all(|&o| o == offsets[0])
                    {
                        Some(SpExpr::Terminal {
                            base: bases[0],
                            offset: offsets[0],
                        })
                    } else {
                        Some(SpExpr::Phi {
                            phi_node: node,
                            offsets,
                        })
                    }
                } else {
                    // Phi is SP-rooted but has a cycle / unresolvable
                    // predecessor — treat it as an opaque base.
                    Some(SpExpr::Terminal {
                        base: out,
                        offset: 0,
                    })
                }
            }
        }
        NodeKind::IntBinaryOp(IntBinaryOp::Add) => {
            let inputs = fg.graph.node_inputs(node);
            if inputs.len() == 2 {
                let l = inputs[0];
                let r = inputs[1];
                if let Some(c) = int_const_signed(fg, r) {
                    decompose_sp(fg, l, sp_vn, visiting).map(|e| e.shifted(c))
                } else if let Some(c) = int_const_signed(fg, l) {
                    decompose_sp(fg, r, sp_vn, visiting).map(|e| e.shifted(c))
                } else {
                    None
                }
            } else {
                None
            }
        }
        NodeKind::IntBinaryOp(IntBinaryOp::Sub) => {
            let inputs = fg.graph.node_inputs(node);
            if inputs.len() == 2 {
                let l = inputs[0];
                let r = inputs[1];
                int_const_signed(fg, r).and_then(|c| {
                    decompose_sp(fg, l, sp_vn, visiting).map(|e| e.shifted(c.wrapping_neg()))
                })
            } else {
                None
            }
        }
        _ => None,
    };
    visiting.remove(&node);
    result
}

/// Rewrites one `Store` node into the matching `StackStore` / `StackStorePhi`
/// form when its address resolves to a known SP offset (or per-branch phi of
/// SP offsets).  Leaves the node untouched otherwise.
fn try_detect_stack_store(
    fg: &mut BuiltFunctionGraph,
    node_id: NodeId,
    sp_vn: rsleigh::Vn,
) -> Result<OptimizationResult> {
    let space = match *fg.graph.node_kind(node_id) {
        NodeKind::Store(space) => space,
        _ => return Ok(OptimizationResult::NoChange),
    };

    // Store inputs: [memory, addr, data].
    let [memory, addr, data] = fg.graph.node_inputs_exact::<3>(node_id)?;
    let [old_mem_out] = fg.graph.node_outputs_exact::<1>(node_id)?;

    let mut visiting = std::collections::HashSet::new();
    let Some(expr) = decompose_sp(fg, addr, sp_vn, &mut visiting) else {
        return Ok(OptimizationResult::NoChange);
    };

    let new_mem_out = match expr {
        SpExpr::Terminal { base, offset } => {
            let new_node = fg.graph.create_node(
                NodeKind::StackStore { space, offset },
                [memory, base, data],
                [NodeOutputKind::Memory],
            );
            fg.graph.node_outputs_exact::<1>(new_node)?[0]
        }
        SpExpr::Phi { phi_node, offsets } => {
            // The ControlPhi's inputs[0] is the dispatch token from its
            // owning ControlState — the same token `StackStorePhi` will
            // consume so that `RedundantPhis` collapses it when only one
            // predecessor is live.
            let phi_inputs = fg.graph.node_inputs(phi_node);
            if phi_inputs.is_empty() {
                return Ok(OptimizationResult::NoChange);
            }
            let phi_token = phi_inputs[0];
            let new_node = fg.graph.create_node(
                NodeKind::StackStorePhi { space },
                [phi_token, memory, data],
                [NodeOutputKind::Memory],
            );
            fg.graph.set_stack_phi_offsets(new_node, offsets);
            fg.graph.node_outputs_exact::<1>(new_node)?[0]
        }
    };

    fg.replace_all_uses(old_mem_out, new_mem_out)?;
    // Whether or not the memory output had consumers, the rewrite replaced
    // the node structurally — severing the original Store's inputs keeps the
    // graph tidy (and prevents a future pass from seeing it again).
    fg.graph.detach_node_inputs(node_id);
    // Even if `replace_all_uses` found no consumers, the store was still
    // rewritten structurally (the new node exists, the old one is detached),
    // so report the change.
    Ok(OptimizationResult::Changed)
}

/// Rewrites `Store` nodes whose address is a compile-time-known SP offset
/// into [`NodeKind::StackStore`] / [`NodeKind::StackStorePhi`] nodes.
///
/// Runs inside the main fixed-point loop so that address arithmetic folded by
/// `ConstantFold` and SP-phi collapses produced by `RedundantPhis` feed more
/// detections on each iteration.
pub struct StackStoreDetect {
    /// Varnode for the stack pointer register (e.g. `ESP`, `RSP`, `sp`).
    pub stack_ptr_vn: rsleigh::Vn,
}

impl StackStoreDetect {
    /// Creates a new pass for the given stack-pointer varnode.
    pub fn new(stack_ptr_vn: rsleigh::Vn) -> Self {
        Self { stack_ptr_vn }
    }

    /// Creates a new pass whose stack-pointer varnode is taken from the
    /// supplied calling convention.
    pub fn from_convention(cc: &target::BuiltCallingConvention) -> Self {
        Self::new(cc.stack_ptr_vn)
    }
}

impl Optimizer for StackStoreDetect {
    fn optimize(&self, function: &mut BuiltFunctionGraph) -> Result<OptimizationResult> {
        let nodes: Vec<NodeId> = function.preorder().collect();
        let mut result = OptimizationResult::NoChange;
        for node_id in nodes {
            result |= try_detect_stack_store(function, node_id, self.stack_ptr_vn)?;
        }
        Ok(result)
    }
}

// Silence unused warning when no test uses NodeInputId in this module.
#[allow(dead_code)]
type _UnusedNodeInputId = NodeInputId;

// ─── CallStackArgCollect ────────────────────────────────────────────────────

/// Walks memory backward from `mem`, collecting `StackStore` data outputs as
/// positional call arguments *as long as each successive store in chain order
/// lands at the next expected arg slot*.
///
/// Returning data in chain-order contiguity is the key defense against
/// misidentifying **stack locals** as call arguments.  A typical cdecl
/// prologue writes a local buffer (e.g. `char buf[16] = {0}`) at the same
/// offsets that later become arg slots in an unrelated call; those buffer-
/// init stores appear on the memory chain but chronologically *before* the
/// arg pushes, so the chain walker sees them only *after* walking past the
/// real pushes.  Requiring chain-order contiguity — each next store must be
/// at `call_sp_adjust + stack_arg_offsets[next_arg]` — makes us stop at the
/// first such interloper instead of greedily scooping them up as args.
///
/// The first store on the chain anchors `call_sp_adjust` (the SP value at
/// the call site).  Whether it is *itself* the first arg depends on the
/// architecture:
///   * On x86 / x86-64 the `call` instruction pushes a return address, so
///     the most-recent store is the ret-addr push (not an arg) and
///     `stack_arg_offsets[0]` is `+4` / `+8`.
///   * On AArch64 / ARM (link-register calls) there is no implicit push and
///     the most-recent store *is* arg 0, so `stack_arg_offsets[0] == 0`.
///
/// Only merges stores that share the same SP base output: offsets mean
/// different absolute addresses across different SP versions, so mixing them
/// would be unsound.  The first base seen pins the chain; a store using a
/// different base terminates collection.
fn collect_stack_args_in_chain_order(
    fg: &BuiltFunctionGraph,
    mem: NodeOutputId,
    stack_arg_offsets: &[i64],
) -> Vec<NodeOutputId> {
    if stack_arg_offsets.is_empty() {
        return Vec::new();
    }
    let mut cur = mem;
    let mut anchor_base: Option<NodeOutputId> = None;
    let mut call_sp_adjust: Option<i64> = None;
    let mut args: Vec<NodeOutputId> = Vec::new();
    loop {
        let node = fg.graph.get_node_from_output(cur);
        let (offset, base, data, prev_mem) = match *fg.graph.node_kind(node) {
            NodeKind::StackStore { offset, .. } => {
                let inputs = fg.graph.node_inputs(node);
                (offset, inputs[1], inputs[2], inputs[0])
            }
            // Un-decomposed `Store` (may alias), `StackStorePhi` (ambiguous),
            // `MemPhi` (control-flow join), or anything else (entry memory,
            // an earlier `Call`) terminates the chain.
            _ => return args,
        };
        match anchor_base {
            None => anchor_base = Some(base),
            Some(b) if b == base => {}
            // Base changed mid-chain: stop rather than merge offsets
            // relative to different SP versions.
            _ => return args,
        }
        match call_sp_adjust {
            None => {
                call_sp_adjust = Some(offset);
                // On architectures where `stack_arg_offsets[0] == 0` the
                // first store on the chain is itself arg 0 (e.g. AArch64).
                if stack_arg_offsets[0] == 0 {
                    args.push(data);
                    if args.len() >= stack_arg_offsets.len() {
                        return args;
                    }
                }
            }
            Some(anchor) => {
                let expected = anchor + stack_arg_offsets[args.len()];
                if offset != expected {
                    // First out-of-pattern store: chain order broken, we
                    // have walked past the real args into frame locals.
                    return args;
                }
                args.push(data);
                if args.len() >= stack_arg_offsets.len() {
                    return args;
                }
            }
        }
        cur = prev_mem;
    }
}

/// Collects stack-passed arguments for one Call node.  Walks the memory chain
/// leading into the call, matches the convention's positional offset table,
/// and appends the discovered data values as additional Call inputs (in
/// positional order, stopping on the first missing slot).
fn try_collect_stack_args(
    fg: &mut BuiltFunctionGraph,
    call_id: NodeId,
    stack_arg_offsets: &[i64],
) -> Result<OptimizationResult> {
    if !matches!(fg.graph.node_kind(call_id), NodeKind::Call) {
        return Ok(OptimizationResult::NoChange);
    }
    if stack_arg_offsets.is_empty() {
        return Ok(OptimizationResult::NoChange);
    }
    let inputs = fg.graph.node_inputs(call_id);
    if inputs.len() < 2 {
        return Ok(OptimizationResult::NoChange);
    }
    let mem_in = inputs[1];

    let args = collect_stack_args_in_chain_order(fg, mem_in, stack_arg_offsets);
    if args.is_empty() {
        return Ok(OptimizationResult::NoChange);
    }
    for data in &args {
        fg.graph.add_node_input(call_id, *data)?;
    }
    Ok(OptimizationResult::Changed)
}

/// Walks backward from each `Call`'s memory input through `StackStore` nodes
/// to reconstruct stack-passed arguments and appends them as extra `Call`
/// inputs in positional order.  Intended to run *once*, as an
/// [`OptimizerPipeline::add_post_pass`][crate::OptimizerPipeline::add_post_pass]
/// after the fixed-point loop has converged.
pub struct CallStackArgCollect {
    /// Positional byte offsets of stack-passed arguments from call-time SP.
    /// Entry `i` is the offset of the `i`-th stack arg.
    pub stack_arg_offsets: Vec<i64>,
}

impl CallStackArgCollect {
    /// Creates a new pass for the given positional stack-arg offset table.
    pub fn new(stack_arg_offsets: Vec<i64>) -> Self {
        Self { stack_arg_offsets }
    }

    /// Creates a new pass whose positional stack-arg offset table is taken
    /// from the supplied calling convention.
    pub fn from_convention(cc: &target::BuiltCallingConvention) -> Self {
        Self::new(cc.stack_arg_offsets.clone())
    }
}

impl Optimizer for CallStackArgCollect {
    fn optimize(&self, function: &mut BuiltFunctionGraph) -> Result<OptimizationResult> {
        let calls: Vec<NodeId> = function
            .preorder()
            .filter(|&n| matches!(function.graph.node_kind(n), NodeKind::Call))
            .collect();
        let mut result = OptimizationResult::NoChange;
        for call_id in calls {
            result |= try_collect_stack_args(function, call_id, &self.stack_arg_offsets)?;
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;
    use crate::{ConstantFold, OptimizerPipeline, RedundantPhis};
    use ir::node::NodeOutputType;
    use ir::{FunctionBuilder, IntBinaryOp};

    fn sp_vn() -> rsleigh::Vn {
        // Use a fake stack-pointer varnode in the REGISTER space.  Width is
        // 4 bytes (u32), matching x86 ESP.
        rsleigh::Vn {
            addr: rsleigh::VnAddr {
                space: rsleigh::VnSpace::REGISTER,
                off: 0x20,
            },
            size: 4,
        }
    }

    /// Counts how many nodes in `fg` match the predicate.
    fn count<F: Fn(&NodeKind) -> bool>(fg: &BuiltFunctionGraph, pred: F) -> usize {
        fg.all_node_ids()
            .filter(|&n| pred(fg.graph.node_kind(n)))
            .count()
    }

    /// Simple straight-line program: `*(sp - 4) = 0x11; return *(sp - 4)`.  After
    /// `ConstantFold` reassociates the address to `sp + 0xFFFFFFFC`, the
    /// pass should replace the `Store` with a `StackStore { offset: -4 }`.
    /// The trailing `Load` keeps the memory chain alive so `RedundantPhis`
    /// doesn't detach the store as dead.
    #[test]
    fn simple_sp_minus_4_becomes_stack_store() -> Result<()> {
        let sp = sp_vn();
        let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);
        let sp_val = b.read_variable(&sp)?;
        let four = b.build_int_const(4, NodeOutputType::U32);
        let addr =
            b.build_int_binary_operation(sp_val, four, IntBinaryOp::Sub, NodeOutputType::U32)?;
        let data = b.build_int_const(0x11, NodeOutputType::U32);
        b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
        b.build_return(Some(loaded), &[])?;
        let mut fg = b.build()?;

        let mut pipeline = OptimizerPipeline::new();
        pipeline.add(ConstantFold);
        pipeline.add(RedundantPhis);
        pipeline.add(StackStoreDetect::new(sp));
        pipeline.run(&mut fg)?;

        let stack_stores = count(&fg, |k| {
            matches!(k, NodeKind::StackStore { offset: -4, .. })
        });
        assert_eq!(stack_stores, 1, "expected one StackStore at offset -4");
        // Every reachable Store must have been rewritten.
        let reachable: std::collections::HashSet<_> = fg.preorder().collect();
        let reachable_stores = fg
            .all_node_ids()
            .filter(|n| reachable.contains(n))
            .filter(|&n| matches!(fg.graph.node_kind(n), NodeKind::Store(_)))
            .count();
        assert_eq!(reachable_stores, 0, "no reachable Store must remain");
        Ok(())
    }

    /// `add esp, 0xFFFFFFFC` and `sub esp, 4` are two encodings of the same
    /// SP adjustment.  `decompose_sp` must recognise `Add(sp, 0xFFFFFFFC_U32)`
    /// as `sp + (-4)` via `int_const_signed`'s bit-width-aware sign extension,
    /// producing a `StackStore { offset: -4 }` directly — without relying on
    /// `ConstantFold` to reassociate the address first.
    #[test]
    fn add_sp_with_negative_unsigned_constant_becomes_stack_store() -> Result<()> {
        let sp = sp_vn();
        let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);
        let sp_val = b.read_variable(&sp)?;
        // 0xFFFFFFFC_U32 == -4 when sign-extended.
        let neg_four = b.build_int_const(0xFFFF_FFFC, NodeOutputType::U32);
        let addr = b.build_int_binary_operation(
            sp_val,
            neg_four,
            IntBinaryOp::Add,
            NodeOutputType::U32,
        )?;
        let data = b.build_int_const(0x11, NodeOutputType::U32);
        b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
        b.build_return(Some(loaded), &[])?;
        let mut fg = b.build()?;

        // Intentionally omit `ConstantFold` so the test exercises
        // `decompose_sp`'s handling of the alternate encoding in isolation.
        let mut pipeline = OptimizerPipeline::new();
        pipeline.add(RedundantPhis);
        pipeline.add(StackStoreDetect::new(sp));
        pipeline.run(&mut fg)?;

        let stack_stores = count(&fg, |k| {
            matches!(k, NodeKind::StackStore { offset: -4, .. })
        });
        assert_eq!(
            stack_stores, 1,
            "Add(sp, 0xFFFFFFFC_U32) must decompose to offset -4 without ConstantFold",
        );
        Ok(())
    }

    /// `*sp = X` where `sp` is an entry-only phi (single reachable predecessor):
    /// `RedundantPhis` collapses the phi inside the fixed-point loop, then
    /// `StackStoreDetect` picks up a straight InitialVar(sp) + 0.
    #[test]
    fn phi_sp_collapses_to_stack_store() -> Result<()> {
        let sp = sp_vn();
        let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
        // Two regions: entry → body.  Body reads sp (which is a phi of the
        // single entry predecessor) and stores at sp + 0.
        let entry = b.create_region()?;
        let body = b.create_region()?;
        b.set_entry_region(entry)?;
        b.set_region(entry);
        b.build_branch(body)?;
        b.set_region(body);
        let sp_val = b.read_variable(&sp)?;
        let data = b.build_int_const(0xAB, NodeOutputType::U32);
        b.build_store(sp_val, data, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(sp_val, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
        b.build_return(Some(loaded), &[])?;
        let mut fg = b.build()?;

        let mut pipeline = OptimizerPipeline::new();
        pipeline.add(ConstantFold);
        pipeline.add(RedundantPhis);
        pipeline.add(StackStoreDetect::new(sp));
        pipeline.run(&mut fg)?;

        let stack_stores = count(&fg, |k| matches!(k, NodeKind::StackStore { offset: 0, .. }));
        assert_eq!(
            stack_stores, 1,
            "phi-of-single-predecessor-sp must collapse then yield StackStore at 0"
        );
        Ok(())
    }

    /// Two reachable predecessors adjust SP by different amounts and merge
    /// at a block that stores through the SP-phi.  The address cannot be
    /// reduced to a single constant, so the rewrite produces
    /// `StackStorePhi { offsets: [-4, -8] }`.
    #[test]
    fn phi_of_offsets_becomes_stack_store_phi() -> Result<()> {
        let sp = sp_vn();
        let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
        let entry = b.create_region()?;
        let a = b.create_region()?;
        let bb = b.create_region()?;
        let c = b.create_region()?;
        b.set_entry_region(entry)?;

        // entry: if (true) goto a else goto b
        b.set_region(entry);
        let cond = b.build_boolean_const(true);
        b.build_if(cond, a, bb)?;

        // a: sp = sp - 4; goto c
        b.set_region(a);
        let sp_a = b.read_variable(&sp)?;
        let four = b.build_int_const(4, NodeOutputType::U32);
        let sp_a2 =
            b.build_int_binary_operation(sp_a, four, IntBinaryOp::Sub, NodeOutputType::U32)?;
        b.write_variable(&sp, sp_a2)?;
        b.build_branch(c)?;

        // b: sp = sp - 8; goto c
        b.set_region(bb);
        let sp_b = b.read_variable(&sp)?;
        let eight = b.build_int_const(8, NodeOutputType::U32);
        let sp_b2 =
            b.build_int_binary_operation(sp_b, eight, IntBinaryOp::Sub, NodeOutputType::U32)?;
        b.write_variable(&sp, sp_b2)?;
        b.build_branch(c)?;

        // c: *(sp) = 0xCC; load(sp); return loaded
        b.set_region(c);
        let sp_c = b.read_variable(&sp)?;
        let data = b.build_int_const(0xCC, NodeOutputType::U32);
        b.build_store(sp_c, data, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(sp_c, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
        b.build_return(Some(loaded), &[])?;
        let mut fg = b.build()?;

        let mut pipeline = OptimizerPipeline::new();
        pipeline.add(ConstantFold);
        pipeline.add(RedundantPhis);
        pipeline.add(StackStoreDetect::new(sp));
        pipeline.run(&mut fg)?;

        let phis: Vec<NodeId> = fg
            .all_node_ids()
            .filter(|&n| matches!(fg.graph.node_kind(n), NodeKind::StackStorePhi { .. }))
            .collect();
        assert_eq!(phis.len(), 1, "expected one StackStorePhi");
        let offsets = fg.graph.stack_phi_offsets(phis[0]);
        let mut sorted: Vec<i64> = offsets.to_vec();
        sorted.sort();
        assert_eq!(
            sorted,
            vec![-8, -4],
            "expected per-branch offsets -4 and -8"
        );
        Ok(())
    }

    /// Two reachable predecessors both adjust SP by the same amount and merge
    /// at a block that stores through the SP-phi.  Because every predecessor
    /// resolves to the same `(base, offset)`, the phi is structurally
    /// redundant — the rewrite must produce a plain `StackStore`, not a
    /// degenerate `StackStorePhi { offsets: [-4, -4] }`.
    #[test]
    fn phi_with_equal_offsets_collapses_to_stack_store() -> Result<()> {
        let sp = sp_vn();
        let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
        let entry = b.create_region()?;
        let a = b.create_region()?;
        let bb = b.create_region()?;
        let c = b.create_region()?;
        b.set_entry_region(entry)?;

        b.set_region(entry);
        let cond = b.build_boolean_const(true);
        b.build_if(cond, a, bb)?;

        // a: sp = sp - 4; goto c
        b.set_region(a);
        let sp_a = b.read_variable(&sp)?;
        let four = b.build_int_const(4, NodeOutputType::U32);
        let sp_a2 =
            b.build_int_binary_operation(sp_a, four, IntBinaryOp::Sub, NodeOutputType::U32)?;
        b.write_variable(&sp, sp_a2)?;
        b.build_branch(c)?;

        // b: sp = sp - 4; goto c  (same offset as a)
        b.set_region(bb);
        let sp_b = b.read_variable(&sp)?;
        let four2 = b.build_int_const(4, NodeOutputType::U32);
        let sp_b2 =
            b.build_int_binary_operation(sp_b, four2, IntBinaryOp::Sub, NodeOutputType::U32)?;
        b.write_variable(&sp, sp_b2)?;
        b.build_branch(c)?;

        // c: *(sp) = 0xCC; load(sp); return loaded
        b.set_region(c);
        let sp_c = b.read_variable(&sp)?;
        let data = b.build_int_const(0xCC, NodeOutputType::U32);
        b.build_store(sp_c, data, rsleigh::VnSpace::RAM)?;
        let loaded = b.build_load(sp_c, rsleigh::VnSpace::RAM, NodeOutputType::U32)?;
        b.build_return(Some(loaded), &[])?;
        let mut fg = b.build()?;

        let mut pipeline = OptimizerPipeline::new();
        pipeline.add(ConstantFold);
        pipeline.add(RedundantPhis);
        pipeline.add(StackStoreDetect::new(sp));
        pipeline.run(&mut fg)?;

        let stack_store_phis = count(&fg, |k| matches!(k, NodeKind::StackStorePhi { .. }));
        assert_eq!(
            stack_store_phis, 0,
            "phi with all-equal offsets must not produce a StackStorePhi"
        );
        let stack_stores = count(&fg, |k| {
            matches!(k, NodeKind::StackStore { offset: -4, .. })
        });
        assert_eq!(
            stack_stores, 1,
            "phi with all-equal offsets must collapse to a plain StackStore"
        );
        Ok(())
    }

    /// A prologue local-variable zero-init writes to offsets that happen to
    /// land in the arg-slot range for a later call, but *chronologically*
    /// before the real arg pushes.  In memory-chain order, the walker sees:
    ///   ret-push, arg 0 push, arg 1 push, buf-init stores, prologue saves, …
    /// The buf-init stores break chain-order contiguity (after arg 1 at
    /// `ret + 8` the next chain entry jumps to some much higher offset), so
    /// collection must stop after arg 1 rather than scoop up the zero-init
    /// writes as spurious args.  Reproduces the `hard_func` case where Call
    /// nodes ended up with 4× `const 0` + an `init EBX` tacked on.
    #[test]
    fn buf_init_does_not_leak_into_args() -> Result<()> {
        let sp = sp_vn();
        let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);

        // Simulate: `push ebx` + `sub esp, 16` + 4× zero-init + push arg1 +
        // push arg0 + implicit-call ret-push.
        let sp0 = b.read_variable(&sp)?;
        let four = b.build_int_const(4, NodeOutputType::U32);
        let sixteen = b.build_int_const(16, NodeOutputType::U32);

        // push ebx → [sp - 4] = init_ebx.
        let sp_after_push_ebx =
            b.build_int_binary_operation(sp0, four, IntBinaryOp::Sub, NodeOutputType::U32)?;
        b.write_variable(&sp, sp_after_push_ebx)?;
        let init_ebx = b.build_int_const(0xEB, NodeOutputType::U32);
        b.build_store(sp_after_push_ebx, init_ebx, rsleigh::VnSpace::RAM)?;

        // sub esp, 16 → reserve buf.
        let sp_after_sub = b.build_int_binary_operation(
            sp_after_push_ebx,
            sixteen,
            IntBinaryOp::Sub,
            NodeOutputType::U32,
        )?;
        b.write_variable(&sp, sp_after_sub)?;

        // 4× zero-init at buf[0..16] (esp+0, +4, +8, +12) = [-20, -16, -12, -8].
        let zero = b.build_int_const(0, NodeOutputType::U32);
        for k in 0..4 {
            let off = b.build_int_const((k * 4) as u64, NodeOutputType::U32);
            let addr = b.build_int_binary_operation(
                sp_after_sub,
                off,
                IntBinaryOp::Add,
                NodeOutputType::U32,
            )?;
            b.build_store(addr, zero, rsleigh::VnSpace::RAM)?;
        }

        // push arg1 = 1 → [sp - 24].
        let sp_push_arg1 = b.build_int_binary_operation(
            sp_after_sub,
            four,
            IntBinaryOp::Sub,
            NodeOutputType::U32,
        )?;
        b.write_variable(&sp, sp_push_arg1)?;
        let arg1 = b.build_int_const(1, NodeOutputType::U32);
        b.build_store(sp_push_arg1, arg1, rsleigh::VnSpace::RAM)?;

        // push arg0 = 42 → [sp - 28].
        let sp_push_arg0 = b.build_int_binary_operation(
            sp_push_arg1,
            four,
            IntBinaryOp::Sub,
            NodeOutputType::U32,
        )?;
        b.write_variable(&sp, sp_push_arg0)?;
        let arg0 = b.build_int_const(42, NodeOutputType::U32);
        b.build_store(sp_push_arg0, arg0, rsleigh::VnSpace::RAM)?;

        // implicit call ret-addr push at [sp - 32] — mimics x86 `call`.
        let sp_call = b.build_int_binary_operation(
            sp_push_arg0,
            four,
            IntBinaryOp::Sub,
            NodeOutputType::U32,
        )?;
        b.write_variable(&sp, sp_call)?;
        let retaddr = b.build_int_const(0x1234, NodeOutputType::U32);
        b.build_store(sp_call, retaddr, rsleigh::VnSpace::RAM)?;

        // call target.
        let target = b.build_int_const(0x1000, NodeOutputType::U32);
        b.build_call(target)?;
        b.build_return(None, &[])?;
        let mut fg = b.build()?;

        let mut pipeline = OptimizerPipeline::new();
        pipeline.add(ConstantFold);
        pipeline.add(RedundantPhis);
        pipeline.add(StackStoreDetect::new(sp));
        // x86 cdecl: ret addr at offset 0, args at +4, +8, +12, …
        pipeline.add_post_pass(CallStackArgCollect::new(vec![4, 8, 12, 16, 20, 24, 28, 32]));
        pipeline.run(&mut fg)?;

        let call_id = find_call(&fg)?;
        let inputs: Vec<NodeOutputId> = fg.graph.node_inputs(call_id).into_iter().collect();
        // ctrl + mem + target + exactly 2 args = 5 inputs.
        assert_eq!(
            inputs.len(),
            5,
            "buf-init and callee-save writes must not be mis-collected as args; got inputs={inputs:?}"
        );
        let arg0_kind = *fg.graph.node_kind(fg.graph.get_node_from_output(inputs[3]));
        let arg1_kind = *fg.graph.node_kind(fg.graph.get_node_from_output(inputs[4]));
        assert!(
            matches!(arg0_kind, NodeKind::IntConst(42)),
            "arg0 should be 42, got {arg0_kind:?}"
        );
        assert!(
            matches!(arg1_kind, NodeKind::IntConst(1)),
            "arg1 should be 1, got {arg1_kind:?}"
        );
        Ok(())
    }

    /// A non-stack store (address is an arbitrary integer constant) must be
    /// left completely untouched.
    #[test]
    fn non_stack_store_is_untouched() -> Result<()> {
        let sp = sp_vn();
        let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);
        let addr = b.build_int_const(0x1000, NodeOutputType::U32);
        let data = b.build_int_const(0x42, NodeOutputType::U32);
        b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
        b.build_return(None, &[])?;
        let mut fg = b.build()?;

        StackStoreDetect::new(sp).optimize(&mut fg)?;

        assert_eq!(
            count(&fg, |k| matches!(k, NodeKind::StackStore { .. })),
            0,
            "non-stack store must not become a StackStore"
        );
        assert_eq!(
            count(&fg, |k| matches!(k, NodeKind::Store(_))),
            1,
            "the original Store must remain"
        );
        Ok(())
    }

    // ── CallStackArgCollect tests ────────────────────────────────────────────

    /// Finds the unique Call node in `fg`.
    fn find_call(fg: &BuiltFunctionGraph) -> Result<NodeId> {
        fg.all_node_ids()
            .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::Call))
            .ok_or_else(|| ErrorKind::ExpectedNodeNotFound("Call", NodeKind::Call).into())
    }

    /// cdecl-style: `push arg1=22; push arg0=11; call target(0x1000)`.
    /// After optimization the Call's inputs should be extended with
    /// `[arg0, arg1]` in positional order.
    #[test]
    fn cdecl_two_stack_args_collected_in_order() -> Result<()> {
        let sp = sp_vn();
        let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);

        // push arg1 (= 22) at sp - 4
        let sp_v0 = b.read_variable(&sp)?;
        let four = b.build_int_const(4, NodeOutputType::U32);
        let sp_v1 =
            b.build_int_binary_operation(sp_v0, four, IntBinaryOp::Sub, NodeOutputType::U32)?;
        b.write_variable(&sp, sp_v1)?;
        let arg1 = b.build_int_const(22, NodeOutputType::U32);
        b.build_store(sp_v1, arg1, rsleigh::VnSpace::RAM)?;

        // push arg0 (= 11) at sp - 8
        let sp_v2 =
            b.build_int_binary_operation(sp_v1, four, IntBinaryOp::Sub, NodeOutputType::U32)?;
        b.write_variable(&sp, sp_v2)?;
        let arg0 = b.build_int_const(11, NodeOutputType::U32);
        b.build_store(sp_v2, arg0, rsleigh::VnSpace::RAM)?;

        // call 0x1000
        let target = b.build_int_const(0x1000, NodeOutputType::U32);
        b.build_call(target)?;
        b.build_return(None, &[])?;
        let mut fg = b.build()?;

        let mut pipeline = OptimizerPipeline::new();
        pipeline.add(ConstantFold);
        pipeline.add(RedundantPhis);
        pipeline.add(StackStoreDetect::new(sp));
        pipeline.add_post_pass(CallStackArgCollect::new(vec![0, 4, 8, 12]));
        pipeline.run(&mut fg)?;

        let call_id = find_call(&fg)?;
        let inputs: Vec<NodeOutputId> = fg.graph.node_inputs(call_id).into_iter().collect();
        // inputs = [ctrl, memory, target, stack_arg_0, stack_arg_1] — no
        // arg-passing registers on cdecl, so indices 3 and 4 are the stack args.
        assert_eq!(
            inputs.len(),
            5,
            "expected ctrl+mem+target+2 stack args; got {inputs:?}"
        );

        let arg0_val = inputs[3];
        let arg1_val = inputs[4];
        let arg0_kind = *fg.graph.node_kind(fg.graph.get_node_from_output(arg0_val));
        let arg1_kind = *fg.graph.node_kind(fg.graph.get_node_from_output(arg1_val));
        assert!(
            matches!(arg0_kind, NodeKind::IntConst(11)),
            "arg0 should be 11, got {arg0_kind:?}"
        );
        assert!(
            matches!(arg1_kind, NodeKind::IntConst(22)),
            "arg1 should be 22, got {arg1_kind:?}"
        );
        Ok(())
    }

    /// Only slot 1 is populated (slot 0 is missing) — the pass must skip
    /// this call entirely rather than mis-assign the gap.
    #[test]
    fn missing_slot_zero_skips_collection() -> Result<()> {
        let sp = sp_vn();
        let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);

        // Only one push, at sp - 4.  If the convention expects [0, 4, 8, …]
        // then call_sp_adjust = -4 and slot_0 would be at -4.  But if we
        // designed a convention where stack_arg_offsets[0] != 0 we'd
        // effectively simulate a missing slot.  Here we instead use an
        // offset table that expects slot_0 = -4 and slot_1 = 0.  Since
        // there is no store at offset 0, collection must stop after slot_0.
        let sp_v0 = b.read_variable(&sp)?;
        let four = b.build_int_const(4, NodeOutputType::U32);
        let sp_v1 =
            b.build_int_binary_operation(sp_v0, four, IntBinaryOp::Sub, NodeOutputType::U32)?;
        b.write_variable(&sp, sp_v1)?;
        let only_arg = b.build_int_const(99, NodeOutputType::U32);
        b.build_store(sp_v1, only_arg, rsleigh::VnSpace::RAM)?;

        let target = b.build_int_const(0x1000, NodeOutputType::U32);
        b.build_call(target)?;
        b.build_return(None, &[])?;
        let mut fg = b.build()?;

        let mut pipeline = OptimizerPipeline::new();
        pipeline.add(ConstantFold);
        pipeline.add(RedundantPhis);
        pipeline.add(StackStoreDetect::new(sp));
        pipeline.add_post_pass(CallStackArgCollect::new(vec![0, 4]));
        pipeline.run(&mut fg)?;

        let call_id = find_call(&fg)?;
        let inputs: Vec<NodeOutputId> = fg.graph.node_inputs(call_id).into_iter().collect();
        // ctrl + memory + target + stack_arg_0 — only the one we have.
        assert_eq!(inputs.len(), 4, "only one stack arg could be collected");
        Ok(())
    }

    /// A call with no stack stores before it must not have any inputs
    /// added.
    #[test]
    fn call_with_no_stack_stores_unchanged() -> Result<()> {
        let sp = sp_vn();
        let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);

        let target = b.build_int_const(0x1000, NodeOutputType::U32);
        b.build_call(target)?;
        b.build_return(None, &[])?;
        let mut fg = b.build()?;

        let before_inputs = fg.graph.node_inputs(find_call(&fg)?).into_iter().count();

        let mut pipeline = OptimizerPipeline::new();
        pipeline.add(ConstantFold);
        pipeline.add(RedundantPhis);
        pipeline.add(StackStoreDetect::new(sp));
        pipeline.add_post_pass(CallStackArgCollect::new(vec![0, 4, 8]));
        pipeline.run(&mut fg)?;

        let after_inputs = fg.graph.node_inputs(find_call(&fg)?).into_iter().count();
        assert_eq!(
            before_inputs, after_inputs,
            "no args should have been collected"
        );
        Ok(())
    }
}
