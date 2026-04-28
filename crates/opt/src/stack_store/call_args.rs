//! `CallStackArgCollect` — post-pass that walks the memory chain leading
//! into each `Call` node, collects positional `StackStore` data outputs, and
//! appends them as additional Call inputs.

use ir::BuiltFunctionGraph;
use ir::node::{NodeId, NodeKind, NodeOutputId};

use crate::error::Result;
use crate::pipeline::{OptimizationResult, Optimizer};
use crate::sp_expr::{SpExprMemo, decompose_sp};

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
/// at `chain_anchor_offset + stack_arg_offsets[next_arg]` — makes us stop at
/// the first such interloper instead of greedily scooping them up as args.
///
/// The first store on the chain anchors `chain_anchor_offset` (the byte
/// offset of that first store, used as the relative origin for subsequent
/// arg-slot expectations).  Whether the anchor store is *itself* the first
/// arg depends on the architecture:
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
///
/// Plain `Store` nodes (those not rewritten to `StackStore` by
/// `StackStoreDetect`) require alias analysis: if the store's address is
/// proven *not* to alias the stack-arg space (e.g. a global write to a
/// constant `.data` address), the walker continues through it.  This makes
/// stack-arg collection robust against compiler-emitted volatile global
/// writes (`volatile int g = …;` barriers commonly inserted by gcc/clang at
/// `-O2`) interleaved between the actual stack-arg pushes.  Any SP-rooted
/// `Store` (whether in-arg-range or not) and any `StackStorePhi` is treated
/// conservatively as chain-terminating.
fn collect_stack_args_in_chain_order(
    fg: &BuiltFunctionGraph,
    mem: NodeOutputId,
    stack_arg_offsets: &[i64],
    stack_ptr_vn: rsleigh::Vn,
    sp_memo: &mut SpExprMemo,
) -> Vec<NodeOutputId> {
    if stack_arg_offsets.is_empty() {
        return Vec::new();
    }
    let mut cur = mem;
    let mut anchor_base: Option<NodeOutputId> = None;
    let mut anchor_space: Option<rsleigh::VnSpace> = None;
    let mut chain_anchor_offset: Option<i64> = None;
    let mut args: Vec<NodeOutputId> = Vec::new();
    loop {
        let node = fg.graph.get_node_from_output(cur);
        let (offset, space, base, data, prev_mem) = match *fg.graph.node_kind(node) {
            NodeKind::StackStore { offset, space } => {
                let inputs = fg.graph.node_inputs(node);
                (offset, space, inputs[1], inputs[2], inputs[0])
            }
            // A plain `Store` survived `StackStoreDetect` either because
            // its address didn't decompose to `sp + K` (so it doesn't
            // alias the stack-arg space) or because it has a different
            // SP base.  Decompose to decide:
            //   * `None`: provably non-aliasing — walk through the
            //     Store's memory predecessor and keep collecting.
            //   * `Some(_)`: SP-rooted but somehow still a `Store` (rare;
            //     would mean a different SP version or a non-canonical
            //     form) — terminate conservatively.
            NodeKind::Store(_) => {
                let inputs = fg.graph.node_inputs(node);
                // Store inputs: [memory, addr, data].  Skip if shape is
                // unexpected (defensive).
                if inputs.len() != 3 {
                    return args;
                }
                let addr = inputs[1];
                let prev = inputs[0];
                let mut visiting = rustc_hash::FxHashSet::default();
                match decompose_sp(&fg.graph, addr, stack_ptr_vn, sp_memo, &mut visiting) {
                    None => {
                        // Non-aliasing — pass through.
                        cur = prev;
                        continue;
                    }
                    Some(_) => return args,
                }
            }
            // `StackStorePhi` (ambiguous offsets), `MemPhi` (control-flow
            // join), or anything else (entry memory, an earlier `Call`,
            // `PostCallMemState`, …) terminates the chain.
            _ => return args,
        };
        match anchor_base {
            None => anchor_base = Some(base),
            Some(b) if b == base => {}
            // Base changed mid-chain: stop rather than merge offsets
            // relative to different SP versions.
            _ => return args,
        }
        match anchor_space {
            None => anchor_space = Some(space),
            Some(s) if s == space => {}
            // Space changed mid-chain: stop rather than mix args from
            // different SP-relative spaces.
            _ => return args,
        }
        match chain_anchor_offset {
            None => {
                chain_anchor_offset = Some(offset);
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
    stack_ptr_vn: rsleigh::Vn,
    sp_memo: &mut SpExprMemo,
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

    let args =
        collect_stack_args_in_chain_order(fg, mem_in, stack_arg_offsets, stack_ptr_vn, sp_memo);
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
///
/// The walker tolerates non-stack-aliasing `Store` nodes interleaved on the
/// chain (e.g. compiler-emitted volatile global writes that gcc/clang at
/// `-O2` are free to schedule between stack-arg pushes).  Such stores are
/// detected via [`crate::sp_expr::decompose_sp`] returning `None` for their
/// address; SP-rooted stores remain chain-terminating.
pub struct CallStackArgCollect {
    /// Positional byte offsets of stack-passed arguments from call-time SP.
    /// Entry `i` is the offset of the `i`-th stack arg.
    pub stack_arg_offsets: Vec<i64>,
    /// Varnode for the stack-pointer register (matches the calling
    /// convention's `stack_ptr_vn`).  Used by the alias-discrimination
    /// branch when the walker encounters a plain `Store`.
    pub stack_ptr_vn: rsleigh::Vn,
}

impl CallStackArgCollect {
    /// Creates a new pass for the given positional stack-arg offset table
    /// and stack-pointer varnode.
    #[must_use]
    pub fn new(stack_arg_offsets: Vec<i64>, stack_ptr_vn: rsleigh::Vn) -> Self {
        Self {
            stack_arg_offsets,
            stack_ptr_vn,
        }
    }

    /// Creates a new pass whose positional stack-arg offset table and
    /// stack-pointer varnode are taken from the supplied calling convention.
    #[must_use]
    pub fn from_convention(cc: &target::BuiltCallingConvention) -> Self {
        Self::new(cc.stack_arg_offsets.clone(), cc.stack_ptr_vn)
    }
}

impl Optimizer for CallStackArgCollect {
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

impl CallStackArgCollect {
    fn optimize_built(&self, function: &mut BuiltFunctionGraph) -> Result<OptimizationResult> {
        let calls: Vec<NodeId> = function
            .preorder()
            .filter(|&n| matches!(function.graph.node_kind(n), NodeKind::Call))
            .collect();
        // Share the SP-decomposition memo across all Call sites in the
        // function — many stack pushes near each other share the same
        // intermediate `sp - K` outputs, and decompose_sp is the hot path
        // when the function has many calls or many stack args.
        let mut sp_memo: SpExprMemo = Default::default();
        let mut result = OptimizationResult::NoChange;
        for call_id in calls {
            result |= try_collect_stack_args(
                function,
                call_id,
                &self.stack_arg_offsets,
                self.stack_ptr_vn,
                &mut sp_memo,
            )?;
        }
        Ok(result)
    }
}
