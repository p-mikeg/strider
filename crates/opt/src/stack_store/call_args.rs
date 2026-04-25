//! `CallStackArgCollect` — post-pass that walks the memory chain leading
//! into each `Call` node, collects positional `StackStore` data outputs, and
//! appends them as additional Call inputs.

use ir::BuiltFunctionGraph;
use ir::node::{NodeId, NodeKind, NodeOutputId};

use crate::error::Result;
use crate::pipeline::{OptimizationResult, Optimizer};

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
