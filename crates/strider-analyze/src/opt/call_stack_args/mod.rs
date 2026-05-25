//! Stack-argument collection post-pass. The shared SP-decomposition
//! machinery lives in [`crate::opt::sp_expr`].
//!
//! `CallStackArgCollect` — post-pass that walks the memory chain leading
//! into each `Call` node, collects positional `StackStore` data outputs, and
//! appends them as additional Call inputs.

use strider_ir::node::{NodeId, NodeKind, NodeOutputId};
use strider_ir::AliasClass;

use crate::opt::error::Result;
use crate::opt::pipeline::{OptimizationResult, Optimizer};
use crate::opt::sp_expr::{SpExprMemo, decompose_sp};
use crate::opt::stack_load_forward::is_stack_partition_input;

#[cfg(test)]
mod tests;

/// Returns `true` when the function contains at least one `MemProject`
/// node — the sign that `AliasSplit` successfully partitioned this function.
/// Pre-computed once per pass invocation to gate the fast path in
/// [`collect_stack_args_in_chain_order`].
#[inline]
fn was_partitioned(function: &strider_ir::Function) -> bool {
    function.has_kind(|k| matches!(k, NodeKind::MemProject { .. }))
}

/// Fast-path stack-arg collection for functions partitioned by `AliasSplit`.
///
/// Walks backward along the `Memory(Some(Stack))` chain starting from `mem`.
/// Unlike the unified-form walker, this path:
///
/// * Relies on `Function::stack_offsets` for O(1) offset lookup — no
///   `decompose_sp` call per Store.
/// * Terminates at `MemProject(Stack)` (the entry boundary inserted by
///   `AliasSplit`).
/// * Falls back to `None` for any unexpected node kind (MemPhi within the
///   Stack chain, a Store without a side-table entry, etc.), letting the
///   caller use the unified-form walker instead.
///
/// Applies the same anchor + prefix-monotonicity rules as the unified-form
/// walker so collection semantics are identical.
///
/// Returns `Some(args)` when the fast path succeeds, or `None` when it
/// encounters a shape it cannot handle (caller falls back to unified form).
fn collect_stack_args_partitioned(
    ctx: crate::pattern::RewriteCtxView<'_>,
    mem: NodeOutputId,
    stack_arg_offsets: &[i64],
) -> Option<Vec<NodeOutputId>> {
    if stack_arg_offsets.is_empty() {
        return Some(Vec::new());
    }

    // Resolve the starting point on the Stack partition chain.
    // The Call's mem input may be:
    //   (a) directly `Memory(Some(Stack))` — walk from there.
    //   (b) `Memory(None)` from a MemUnion — route through the MemUnion
    //       to find the Stack-partition input.
    //   (c) Something else — fall back.
    let start = {
        use strider_ir::node::NodeOutputKind;
        match ctx.function_ref().output_kind(mem) {
            NodeOutputKind::Memory(Some(AliasClass::Stack)) => mem,
            NodeOutputKind::Memory(None) => {
                let union_node = ctx.function_ref().get_node_from_output(mem);
                if !matches!(ctx.function_ref().node_kind(union_node), NodeKind::MemUnion) {
                    return None;
                }
                ctx.function_ref()
                    .node_inputs(union_node)
                    .iter()
                    .find(|&inp| is_stack_partition_input(ctx.function_ref(), inp))?
            }
            _ => return None,
        }
    };

    let mut cur = start;
    let mut anchor_space: Option<rsleigh::VnSpace> = None;
    let mut chain_anchor_offset: Option<i64> = None;
    let mut slots: Vec<Option<NodeOutputId>> = vec![None; stack_arg_offsets.len()];
    let mut prefix_top: i32 = -1;

    loop {
        let node = ctx.function_ref().get_node_from_output(cur);
        match *ctx.function_ref().node_kind(node) {
            NodeKind::MemProject { class: AliasClass::Stack } => {
                // Reached the function-entry Stack boundary — chain exhausted cleanly.
                return Some(dense_prefix(slots));
            }
            NodeKind::MemProject { .. } | NodeKind::MemPhi => {
                // Unexpected non-Stack partition or control-flow join inside the
                // Stack chain — not produced by v1 AliasSplit; fall back.
                return None;
            }
            NodeKind::Store(space) => {
                // Use the side-table offset — AliasSplit records it for every
                // SP-relative store when it partitions the function.
                let offset = ctx.function_ref().stack_offset(node)?;
                let inputs = ctx.function_ref().node_inputs(node);
                if inputs.len() != 3 {
                    return Some(dense_prefix(slots));
                }
                let data = inputs[2];

                // Space consistency check (same as unified-form walker).
                match anchor_space {
                    None => anchor_space = Some(space),
                    Some(s) if s == space => {}
                    _ => return Some(dense_prefix(slots)),
                }

                let is_first_store = chain_anchor_offset.is_none();
                let anchor = *chain_anchor_offset.get_or_insert(offset);
                let rel = offset - anchor;

                match stack_arg_offsets.iter().position(|&o| o == rel) {
                    Some(slot) if slots[slot].is_none() => {
                        let slot_i32 = i32::try_from(slot).unwrap_or(i32::MAX);
                        if prefix_top >= 0 && slot_i32 > prefix_top + 1 {
                            return Some(dense_prefix(slots));
                        }
                        slots[slot] = Some(data);
                        let mut k = usize::try_from(prefix_top + 1).unwrap_or(0);
                        while k < slots.len() && slots[k].is_some() {
                            k += 1;
                        }
                        prefix_top =
                            i32::try_from(k).unwrap_or(i32::MAX).saturating_sub(1);
                        if usize::try_from(prefix_top + 1).unwrap_or(0) == slots.len() {
                            return Some(dense_prefix(slots));
                        }
                    }
                    Some(_) => { /* slot already filled — stale write, skip */ }
                    None if is_first_store => { /* anchor outside slot table — continue */ }
                    None => return Some(dense_prefix(slots)),
                }

                cur = inputs[0]; // Store mem-predecessor is slot 0.
            }
            _ => {
                // InitialMemory, Call re-projection, or anything else —
                // terminate collection cleanly.
                return Some(dense_prefix(slots));
            }
        }
    }
}

/// Walks memory backward from `mem`, collecting `StackStore` data outputs as
/// positional call arguments by matching each store's offset against the
/// convention's slot table.
///
/// Two safety rules govern collection:
///
/// **Set membership.** Each chain `StackStore`'s `offset - anchor` must be
/// in the convention's `stack_arg_offsets` set.  An offset outside the set
/// terminates the walk — that's the local/saved-register guard.  This rule
/// assumes a frame's local-variable region and its outgoing-args region
/// occupy *disjoint* relative offsets from the anchor: in standard x86
/// cdecl, AAPCS, MIPS, etc., locals live at higher absolute SP-relative
/// offsets than the outgoing-args window, so the relative offsets land
/// outside `stack_arg_offsets`.  A pathological convention table that
/// includes offsets coinciding with the local region would break this
/// guarantee — none of the built-in `strider_target::CallingConvention` presets do.
///
/// **Prefix monotonicity.** Once a contiguous slot prefix `[0..=k]` has
/// formed, any further fill must land in `[0, k+1]`.  A new fill at slot
/// `> k+1` would require all of `(k+1)..slot` to be supplied by later
/// upstream stores; in real cdecl frames the prologue's local-init writes
/// translate to slots well above the actual arg-region top, so this rule
/// fires the moment the walker crosses out of the args-push window into
/// frame locals.  Until slot 0 is filled `prefix_top == -1` and this rule
/// is dormant — set membership is the only active guard in that window.
///
/// Together the two rules accept arg pushes in any program order — the
/// constraint earlier code mistakenly conflated with safety — while still
/// rejecting stale interleaved local-init writes.  Most-recent-wins for
/// repeated-slot writes falls out naturally: the first sighting on the
/// backward walk fills the slot; later sightings find the slot already
/// occupied and are skipped.
///
/// Earlier revisions enforced *chain-order monotonicity* (each next store
/// had to land at `anchor + stack_arg_offsets[args.len()]`).  That assumed
/// the compiler emitted arg pushes in slot-ascending order, which is false
/// on x86 cdecl with gcc/clang — both routinely store arg0 then arg1 in
/// program order, so arg1 ends up at the chain head and the in-order check
/// rejected every arg.  See the regression `cdecl_args_pushed_in_program_
/// order_collected` in this module's tests for the original repro from
/// the FreeBSD i386 10.0 `exec_free_args` function.
///
/// The first store on the chain anchors `chain_anchor_offset` (the byte
/// offset of that first store, used as the relative origin for slot
/// lookups).  Whether the anchor store is *itself* the first arg depends
/// on which calling pattern the compiler emitted:
///   * x86 / x86-64 `push arg`-style (older gcc, hand-written asm) — each
///     `push` decrements SP and stores; the chain head is the most-recent
///     `push`, anchor `rel == 0` matches `stack_arg_offsets[0] == 0` (when
///     the convention is configured for push-style; not the default cdecl
///     preset), filling the anchor as slot 0 immediately.
///   * x86 / x86-64 `mov [esp+K]`-style (gcc/clang -O2 default for cdecl
///     and SysV) — args are stored at fixed positive offsets from the
///     post-prologue SP, then the `call` instruction's implicit ret-addr
///     push lands at SP-4 (or SP-8) and Sleigh lifts that push as a
///     `Store`/`StackStore` node feeding the Call's memory input.  The
///     ret-addr push is the chain head, anchor `rel == 0` is not in
///     `stack_arg_offsets` (which starts at +4 / +8), and the
///     `is_first_store` exception lets the walker skip the OOW
///     termination and continue to the real args upstream.
///   * AArch64 / ARM (link-register calls) — no implicit push, the most-
///     recent store is arg 0, `stack_arg_offsets[0] == 0`, anchor fills
///     slot 0 immediately.
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
///
/// Returns the *dense prefix* of filled slots: indices `0..k` where every
/// slot in that range got a value, stopping at the first hole.  Patterns
/// querying `arg(i)` rely on positional continuity, so a missing slot 0
/// suppresses every later slot too.
///
/// When `partitioned` is `true` (the function was processed by `AliasSplit`),
/// this function first attempts the fast path via
/// [`collect_stack_args_partitioned`] which walks the Stack-only chain using
/// O(1) side-table offset lookups.  If the fast path bails (returns `None` for
/// an edge-case shape), the unified-form walker runs as fallback.
fn collect_stack_args_in_chain_order(
    ctx: crate::pattern::RewriteCtxView<'_>,
    mem: NodeOutputId,
    stack_arg_offsets: &[i64],
    stack_ptr_vn: rsleigh::Vn,
    sp_memo: &mut SpExprMemo,
    partitioned: bool,
) -> Vec<NodeOutputId> {
    if stack_arg_offsets.is_empty() {
        return Vec::new();
    }
    // When the function has been partitioned by AliasSplit, attempt the
    // simplified Stack-chain-only walk first.  If it returns `None` (an
    // edge case it cannot handle), fall through to the unified-form walker.
    if let Some(fast_args) = partitioned
        .then(|| collect_stack_args_partitioned(ctx, mem, stack_arg_offsets))
        .flatten()
    {
        return fast_args;
    }
    let mut cur = mem;
    // `anchor_base` tracks the SP root node used by decompose_sp-path stores.
    // All SP-relative stores in a function trace to the same SP root by
    // construction (InitialVar(sp) or the post-alignment And node), so when
    // the side-table path is used no per-store base check is needed.
    let mut anchor_base: Option<NodeOutputId> = None;
    let mut anchor_space: Option<rsleigh::VnSpace> = None;
    let mut chain_anchor_offset: Option<i64> = None;
    let mut slots: Vec<Option<NodeOutputId>> = vec![None; stack_arg_offsets.len()];
    // Largest k such that slots[0..=k] are all `Some`; -1 if slot 0 is empty.
    let mut prefix_top: i32 = -1;
    loop {
        let node = ctx.get_node_from_output(cur);
        let (offset, space, data, prev_mem) = match *ctx.node_kind(node) {
            // Raw `Store` — determine whether it is SP-relative.
            //
            // Fast path: consult `Function::stack_offsets`, populated by
            // `AliasSplit` for every Store whose address decomposes to a
            // single concrete `sp + K`.  O(1) side-table read, no
            // `decompose_sp` call.
            //
            // Slow path (side-table miss): call `decompose_sp` directly —
            // covers functions where `AliasSplit` has not run yet, and
            // stores with Phi-SP or non-SP addresses.
            NodeKind::Store(space) => {
                let inputs = ctx.node_inputs(node);
                // Store inputs: [memory, addr, data].  Skip if shape is
                // unexpected (defensive).
                if inputs.len() != 3 {
                    return dense_prefix(slots);
                }
                let addr = inputs[1];
                let prev = inputs[0];
                if let Some(offset) = ctx.function_ref().stack_offset(node) {
                    // Fast path: side-table hit.  All side-table stores in a
                    // function share the same SP root (by AliasSplit's
                    // construction), so no per-store anchor_base check is
                    // needed.
                    (offset, space, inputs[2], prev)
                } else {
                    // Slow path: no side-table entry.
                    match decompose_sp(ctx.function_ref(), addr, stack_ptr_vn, sp_memo) {
                        None => {
                            // Non-aliasing — pass through.
                            cur = prev;
                            continue;
                        }
                        Some(crate::opt::sp_expr::SpExpr::Terminal { base, offset }) => {
                            // SP-relative Store — treat like a stack-arg store.
                            match anchor_base {
                                None => anchor_base = Some(base),
                                Some(b) if b == base => {}
                                // Base changed mid-chain: stop rather than merge
                                // offsets relative to different SP versions.
                                _ => return dense_prefix(slots),
                            }
                            (offset, space, inputs[2], prev)
                        }
                        Some(crate::opt::sp_expr::SpExpr::Phi { .. }) => {
                            // SP-rooted Phi address: conservatively terminate.
                            return dense_prefix(slots);
                        }
                    }
                }
            }
            // `MemProject { class }` — boundary inserted by AliasSplit
            // that tags a unified memory edge with a single alias class.
            // The walker passes straight through to the single predecessor
            // (input 0, the unified-memory side).
            NodeKind::MemProject { .. } => {
                let inputs = ctx.node_inputs(node);
                if inputs.is_empty() {
                    return dense_prefix(slots);
                }
                cur = inputs[0];
                continue;
            }
            // `MemUnion` — merges N partition-typed edges back into a
            // single unified memory edge.  Only the Stack-partition input
            // is relevant for stack-arg collection; follow it and ignore
            // the rest.  If no Stack-partition input exists the chain is
            // opaque and we terminate.
            NodeKind::MemUnion => {
                let inputs = ctx.node_inputs(node);
                let stack_input = inputs
                    .iter()
                    .find(|&inp| is_stack_partition_input(ctx.function_ref(), inp));
                match stack_input {
                    Some(inp) => {
                        cur = inp;
                        continue;
                    }
                    None => return dense_prefix(slots),
                }
            }
            // `StackStorePhi` (ambiguous offsets), `MemPhi` (control-flow
            // join), or anything else (entry memory, an earlier `Call`,
            // `PostCallMemState`, …) terminates the chain.
            _ => return dense_prefix(slots),
        };
        match anchor_space {
            None => anchor_space = Some(space),
            Some(s) if s == space => {}
            // Space changed mid-chain: stop rather than mix args from
            // different SP-relative spaces.
            _ => return dense_prefix(slots),
        }
        let is_first_store = chain_anchor_offset.is_none();
        let anchor = *chain_anchor_offset.get_or_insert(offset);
        let rel = offset - anchor;
        match stack_arg_offsets.iter().position(|&o| o == rel) {
            Some(slot) if slots[slot].is_none() => {
                // Prefix-monotonicity check: once a `[0..=prefix_top]`
                // contiguous prefix exists, any new fill must land in
                // `[0, prefix_top + 1]`.  A jump beyond means we've
                // walked out of the args-push window and into frame
                // locals (or into args of an earlier, unrelated call —
                // which would normally be cut off by an intervening
                // chain-terminator, but defensive here).
                // `slot` is a `usize` index into the local `slots`
                // vec.  Use `i32::try_from` to surface overflow
                // explicitly (the convention's CC table caps slot
                // counts at a few dozen in practice, so this never
                // fires; `as i32` would silently wrap on a
                // future >2^31-slot table).
                let slot_i32 = i32::try_from(slot)
                    .unwrap_or(i32::MAX);
                if prefix_top >= 0 && slot_i32 > prefix_top + 1 {
                    return dense_prefix(slots);
                }
                slots[slot] = Some(data);
                // Extend `prefix_top` as far as the contiguous prefix
                // now reaches.
                let mut k = usize::try_from(prefix_top + 1).unwrap_or(0);
                while k < slots.len() && slots[k].is_some() {
                    k += 1;
                }
                prefix_top = i32::try_from(k).unwrap_or(i32::MAX).saturating_sub(1);
                if usize::try_from(prefix_top + 1).unwrap_or(0) == slots.len() {
                    return dense_prefix(slots);
                }
            }
            // Slot already filled by a more recent (closer to Call) write.
            // The newer write is what the callee sees; the older one is
            // stale and ignored.  Keep walking — there may be more args
            // at other slots upstream.
            Some(_) => {}
            // Offset is not a stack-arg slot under this convention.  On
            // architectures whose anchor is itself a non-arg push (x86
            // ret-addr push), the FIRST store legitimately has rel=0
            // outside the table — record the anchor and continue.  Any
            // later out-of-set offset is the local/interloper guard
            // firing.
            None if is_first_store => {}
            None => return dense_prefix(slots),
        }
        cur = prev_mem;
    }
}

/// Returns the longest dense prefix of `slots` (indices `0..k` where
/// every entry is `Some(_)`, stopping at the first `None`).  Patterns
/// querying `arg(i)` rely on positional continuity, so a missing slot 0
/// suppresses every later slot too.
fn dense_prefix(slots: Vec<Option<NodeOutputId>>) -> Vec<NodeOutputId> {
    let mut out = Vec::with_capacity(slots.len());
    for s in slots {
        match s {
            Some(v) => out.push(v),
            None => break,
        }
    }
    out
}

/// Collects stack-passed arguments for one Call node.  Walks the memory chain
/// leading into the call, matches the convention's positional offset table,
/// and appends the discovered data values as additional Call inputs (in
/// positional order, stopping on the first missing slot).
fn try_collect_stack_args(
    ctx: &mut crate::pattern::RewriteCtx<'_>,
    call_id: NodeId,
    stack_arg_offsets: &[i64],
    stack_ptr_vn: rsleigh::Vn,
    sp_memo: &mut SpExprMemo,
    partitioned: bool,
) -> Result<OptimizationResult> {
    if !matches!(ctx.node_kind(call_id), NodeKind::Call) {
        return Ok(OptimizationResult::NoChange);
    }
    if stack_arg_offsets.is_empty() {
        return Ok(OptimizationResult::NoChange);
    }
    let inputs = ctx.node_inputs(call_id);
    if inputs.len() < 2 {
        return Ok(OptimizationResult::NoChange);
    }
    let mem_in = inputs[1];

    let args = collect_stack_args_in_chain_order(
        ctx.as_view(),
        mem_in,
        stack_arg_offsets,
        stack_ptr_vn,
        sp_memo,
        partitioned,
    );
    if args.is_empty() {
        return Ok(OptimizationResult::NoChange);
    }
    for data in &args {
        ctx.add_node_input(call_id, *data)?;
    }
    Ok(OptimizationResult::Changed)
}

/// Walks backward from each `Call`'s memory input through `StackStore` nodes
/// to reconstruct stack-passed arguments and appends them as extra `Call`
/// inputs in positional order.  Intended to run *once*, as an
/// [`OptimizerPipeline::add_post_pass`][crate::opt::OptimizerPipeline::add_post_pass]
/// after the fixed-point loop has converged.
///
/// The walker tolerates non-stack-aliasing `Store` nodes interleaved on the
/// chain (e.g. compiler-emitted volatile global writes that gcc/clang at
/// `-O2` are free to schedule between stack-arg pushes).  When
/// `Function::stack_offsets` is populated by `AliasSplit`, SP-relative
/// stores are identified via an O(1) side-table read; without it the walker
/// falls back to `crate::opt::sp_expr::decompose_sp`.  Non-SP-rooted stores
/// (side-table miss + decompose returns `None`) are passed through; SP-rooted
/// stores remain chain-terminating.
#[derive(Clone)]
pub struct CallStackArgCollect {
    /// Calling convention this pass was built from.  See the comment
    /// on `StackStoreDetect::cc` for the per-pass-shared-Arc rationale.
    /// Consults `cc.stack_arg_offsets` and `cc.stack_ptr_vn`.
    cc: std::sync::Arc<strider_target::BuiltCallingConvention>,
    /// Cached positional-arg layout derived from `cc` at construction
    /// time.  Single source of truth for "what is positional arg `i`?";
    /// keeps the pass's stack-arg-offsets read aligned with the
    /// canonical layout shared by [`crate::opt::FunctionArgDetect`].
    layout: strider_target::PositionalArgLayout,
}

impl CallStackArgCollect {
    /// Creates a new pass for the given positional stack-arg offset table
    /// and stack-pointer varnode.  Convenience constructor; production
    /// paths prefer [`Self::from_convention`].
    #[must_use]
    pub fn new(stack_arg_offsets: Vec<i64>, stack_ptr_vn: rsleigh::Vn) -> Self {
        let cc = crate::opt::sp_pass_cc::minimal_cc(stack_ptr_vn, Vec::new(), stack_arg_offsets);
        Self::from_convention(&cc)
    }

    /// Creates a new pass whose positional stack-arg offset table and
    /// stack-pointer varnode are taken from the supplied calling convention.
    #[must_use]
    pub fn from_convention(cc: &strider_target::BuiltCallingConvention) -> Self {
        let layout = strider_target::PositionalArgLayout::from_convention(cc);
        Self {
            cc: std::sync::Arc::new(cc.clone()),
            layout,
        }
    }

    /// Convention this pass was built with.
    #[must_use]
    pub fn calling_convention(&self) -> &strider_target::BuiltCallingConvention {
        &self.cc
    }
}

impl Optimizer for CallStackArgCollect {
    fn optimize(
        &self,
        function: &mut strider_ir::Function,
        entry: NodeId,
    ) -> Result<OptimizationResult> {
        // Pre-compute once: did AliasSplit partition this function?
        // When true, each Call's memory chain is routed through a MemUnion
        // (or arrives directly on the Stack-partition chain), and every
        // stack Store has a concrete offset in `Function::stack_offsets`.
        // The fast path in `collect_stack_args_partitioned` exploits this.
        let partitioned = was_partitioned(function);
        let mut ctx = crate::pattern::RewriteCtx::new(function, entry);
        let calls: Vec<NodeId> = ctx
            .preorder()
            .filter(|&n| matches!(ctx.node_kind(n), NodeKind::Call))
            .collect();
        // Share the SP-decomposition memo across all Call sites in the
        // function.  When `AliasSplit` has populated `Function::stack_offsets`,
        // the walker uses the side-table (O(1)) and the memo is only consulted
        // for stores not in the side-table.  Without `AliasSplit`, many stack
        // pushes near each other share intermediate `sp − K` outputs that hit
        // the memo on the second access.
        let mut sp_memo: SpExprMemo = Default::default();
        let mut result = OptimizationResult::NoChange;
        let default_offsets = self.layout.stack_arg_offsets();
        let stack_ptr_vn = self.cc.stack_ptr_vn;
        for call_id in calls {
            // Consult the per-Call stack-arg-offsets override recorded at
            // lift time when a per-address CC was in effect.  Falls back to
            // the function-default layout when no override is present.
            let override_offsets: Option<Vec<i64>> = ctx
                .function_ref()
                .call_stack_arg_offsets_override(call_id)
                .map(|s| s.to_vec());
            let stack_arg_offsets: &[i64] = override_offsets
                .as_deref()
                .unwrap_or(&default_offsets);
            result |= try_collect_stack_args(
                &mut ctx,
                call_id,
                stack_arg_offsets,
                stack_ptr_vn,
                &mut sp_memo,
                partitioned,
            )?;
        }
        Ok(result)
    }
}
