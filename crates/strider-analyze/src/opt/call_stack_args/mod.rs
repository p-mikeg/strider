//! Stack-argument collection post-pass. The shared SP-decomposition
//! machinery lives in [`crate::opt::sp_expr`].
//!
//! `CallStackArgCollect` — post-pass that walks the memory chain leading
//! into each `Call` node, collects positional `StackStore` data outputs, and
//! appends them as additional Call inputs.

use strider_ir::node::{NodeId, NodeKind, NodeOutputId};

use crate::opt::error::Result;
use crate::opt::pipeline::{OptimizationResult, Optimizer};
use crate::opt::sp_expr::{SpExprMemo, decompose_sp};

#[cfg(test)]
mod tests;

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
/// Plain `Store` nodes require alias analysis: if the store's address
/// is proven *not* to alias the stack-arg space (e.g. a global write
/// to a constant `.data` address), the walker continues through it.
/// This makes stack-arg collection robust against compiler-emitted
/// volatile global writes (`volatile int g = …;` barriers commonly
/// inserted by gcc/clang at `-O2`) interleaved between the actual
/// stack-arg pushes.  Any SP-rooted `Store` (whether in-arg-range or
/// not) and any `StackStorePhi` is treated conservatively as
/// chain-terminating.
///
/// Returns the *dense prefix* of filled slots: indices `0..k` where every
/// slot in that range got a value, stopping at the first hole.  Patterns
/// querying `arg(i)` rely on positional continuity, so a missing slot 0
/// suppresses every later slot too.
fn collect_stack_args_in_chain_order(
    ctx: crate::pattern::RewriteCtxView<'_>,
    mem: NodeOutputId,
    stack_arg_offsets: &[i64],
    stack_vn: rsleigh::Vn,
    sp_memo: &mut SpExprMemo,
    alias_mode: crate::opt::AliasMode,
) -> Vec<NodeOutputId> {
    if stack_arg_offsets.is_empty() {
        return Vec::new();
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
            // Fast path: consult `Function::stack_offsets`, populated
            // by `StackOffsetDetect` for every Store whose address
            // decomposes to a single concrete `sp + K`.  O(1)
            // side-table read.
            //
            // Slow path (side-table miss): call `decompose_sp` —
            // covers stores with Phi-SP or non-SP addresses.
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
                    // Fast path: side-table hit.  All side-table
                    // stores in a function share the same SP root by
                    // construction, so no per-store anchor_base check
                    // is needed.
                    (offset, space, inputs[2], prev)
                } else {
                    // Slow path: no side-table entry.
                    match decompose_sp(ctx.function_ref(), addr, stack_vn, sp_memo) {
                        None => match alias_mode {
                            // Strict: cross-class store may alias an
                            // outgoing stack-arg slot.  Bail.
                            crate::opt::AliasMode::Strict => return dense_prefix(slots),
                            // Permissive: an `IntConst` store address
                            // is assumed to live outside the stack
                            // region.  Step through; any other
                            // non-SP-rooted (Anchor) address still
                            // bails.
                            crate::opt::AliasMode::AssumeStackConstDisjoint => {
                                let addr_node = ctx.get_node_from_output(addr);
                                if matches!(ctx.node_kind(addr_node), NodeKind::IntConst(_)) {
                                    cur = prev;
                                    continue;
                                }
                                return dense_prefix(slots);
                            }
                        },
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
            // `MemPhi` (control-flow join), `StackStorePhi`, and any
            // other non-Store memory producer terminate the chain.
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
                let slot_i32 = i32::try_from(slot).unwrap_or(i32::MAX);
                if prefix_top >= 0 && slot_i32 > prefix_top + 1 {
                    return dense_prefix(slots);
                }
                if fill_slot_and_advance(&mut slots, slot, data, &mut prefix_top) {
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

/// Fills `slots[slot]` with `data`, advances `prefix_top` to cover the new
/// contiguous prefix, and returns whether the prefix is now complete.
///
/// Returns `true` when `prefix_top + 1 == slots.len()` (caller should return
/// the dense prefix immediately).  Returns `false` when the slot was filled but
/// more slots remain.
///
/// **Precondition:** `slots[slot]` is `None` and the monotonicity guard has
/// already been checked by the caller.
fn fill_slot_and_advance(
    slots: &mut [Option<NodeOutputId>],
    slot: usize,
    data: NodeOutputId,
    prefix_top: &mut i32,
) -> bool {
    slots[slot] = Some(data);
    let mut k = usize::try_from(*prefix_top + 1).unwrap_or(0);
    while k < slots.len() && slots[k].is_some() {
        k += 1;
    }
    *prefix_top = i32::try_from(k).unwrap_or(i32::MAX).saturating_sub(1);
    usize::try_from(*prefix_top + 1).unwrap_or(0) == slots.len()
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
    stack_vn: rsleigh::Vn,
    sp_memo: &mut SpExprMemo,
    alias_mode: crate::opt::AliasMode,
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
        stack_vn,
        sp_memo,
        alias_mode,
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
/// The walker tolerates disjoint SP-relative stores interleaved on the
/// chain (different offsets, ranges proven non-overlapping).  When
/// `Function::stack_offsets` is populated by `StackOffsetDetect`,
/// SP-relative stores are identified via an O(1) side-table read;
/// without it the walker falls back to
/// `crate::opt::sp_expr::decompose_sp`.  Under the default
/// `AliasMode::Strict`, non-SP-rooted stores (constant addresses,
/// opaque pointers) cannot be proven disjoint from the outgoing
/// stack-arg slots and terminate the walk.
#[derive(Clone)]
pub struct CallStackArgCollect {
    /// Stack-pointer varnode used by [`decompose_sp`] when classifying
    /// chain stores as SP-relative.  Extracted from the calling
    /// convention at construction time.
    stack_vn: rsleigh::Vn,
    /// Cached positional-arg layout derived from `cc` at construction
    /// time.  Single source of truth for "what is positional arg `i`?";
    /// keeps the pass's stack-arg-offsets read aligned with the
    /// canonical layout shared by [`crate::opt::FunctionArgDetect`].
    layout: strider_target::PositionalArgLayout,
    /// Alias-analysis precision for the backward chain walk.  Default
    /// is [`crate::opt::AliasMode::Strict`].
    alias_mode: crate::opt::AliasMode,
}

impl CallStackArgCollect {
    /// Creates a new pass for the given positional stack-arg offset table
    /// and stack-pointer varnode.  Convenience constructor; production
    /// paths prefer [`Self::from_convention`].
    #[must_use]
    pub fn new(stack_arg_offsets: Vec<i64>, stack_vn: rsleigh::Vn) -> Self {
        let cc = crate::opt::sp_pass_cc::minimal_cc(stack_vn, Vec::new(), stack_arg_offsets);
        Self::from_convention(&cc)
    }

    /// Creates a new pass whose positional stack-arg offset table and
    /// stack-pointer varnode are taken from the supplied calling convention.
    #[must_use]
    pub fn from_convention(cc: &strider_target::BuiltCallingConvention) -> Self {
        Self {
            stack_vn: cc.stack_vn,
            layout: strider_target::PositionalArgLayout::from_convention(cc),
            alias_mode: crate::opt::AliasMode::Strict,
        }
    }

    /// Overrides the alias-analysis precision used by the chain walk.
    /// See [`crate::opt::AliasMode`] for the soundness/coverage trade-off.
    #[must_use]
    pub const fn alias_mode(mut self, mode: crate::opt::AliasMode) -> Self {
        self.alias_mode = mode;
        self
    }
}

impl Optimizer for CallStackArgCollect {
    fn optimize(
        &self,
        function: &mut strider_ir::Function,
        entry: NodeId,
    ) -> Result<OptimizationResult> {
        let mut ctx = crate::pattern::RewriteCtx::new(function, entry);
        let calls: Vec<NodeId> = ctx
            .walk()
            .filter(|&n| matches!(ctx.node_kind(n), NodeKind::Call))
            .collect();
        let mut sp_memo: SpExprMemo = Default::default();
        let mut result = OptimizationResult::NoChange;
        let default_offsets = self.layout.stack_arg_offsets();
        let stack_vn = self.stack_vn;
        for call_id in calls {
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
                stack_vn,
                &mut sp_memo,
                self.alias_mode,
            )?;
        }
        Ok(result)
    }
}
