//! Detects function arguments and records them in the
//! [`strider_ir::Function::arg_index_to_values`] side-table.
//!
//! Runs as a post-pass after the main fixed-point loop converges.  Identifies
//! register-passed arg reads (`InitialVar(arg_reg)`) and stack-passed arg
//! reads (`Load[InitialVar(sp) + K]` unshadowed by any prior store) and
//! records each underlying node in the side-table keyed by the argument's
//! index in the calling convention.  The original `InitialVar` / `Load` nodes
//! survive as the registered carriers — no consumer rewiring, no new nodes.
//! (The shared memory-SSA walk used for the stack-arg shadow check may narrow
//! a stack-arg `Load`'s own memory input onto its nearest clobber; this never
//! changes which args are detected.)
//!
//! # Detection rules
//!
//! * **Register args** (no contiguity constraint).  For each register
//!   `R = cc.arg_passing_regs[i]`, if `InitialVar(R)` has live uses in the
//!   graph, register it as the carrier for arg `i` via
//!   `function.register_arg_value(i, initial_var_value)`.
//!
//! * **Stack args** (strict contiguity + no-shadow).  Collect all `Load`
//!   nodes whose address decomposes (via [`sp_expr::decompose_sp`]) to
//!   `InitialVar(sp) + K` with `K == cc.stack_arg_offsets[j]`.  Reject any
//!   whose memory input is reachable backward from a shadowing store — the
//!   walk is a DFS through memory predecessors that treats `MemPhi` as a
//!   fork where every predecessor must be non-disqualifying.  Disqualifying
//!   nodes: a stack-tagged `Store { offset: K }`, a `MemPhi` whose
//!   per-predecessor offsets contain `K`, and un-decomposed `Store` (may alias —
//!   conservative).  Non-disqualifying: `InitialMemory`, `Call`,
//!   `CallOther`, and stores at other offsets.  After
//!   filtering, emit only those indices that form a gap-free prefix starting
//!   at `first_stack_arg = arg_passing_regs.len()`; the first gap truncates.
//!
//! For the stack-arg multi-`Load` case, every `Load` at the same `sp+K`
//! offset (potentially at different widths) is registered into the side-table
//! for that arg index — the `Vec<ValueId>` per entry accommodates this.

use strider_ir::node::{NodeId, NodeKind, ValueId};

use crate::error::Result;
use crate::pipeline::{OptimizationResult, Optimizer};
use crate::sp_expr::{AddrClass, SpAliasOracle, SpExpr, SpExprMemo, decompose_sp};
use crate::worklist::seeded_kind;

/// Detects register-passed and stack-passed argument reads and records their
/// underlying carrier nodes in
/// [`strider_ir::Function::arg_index_to_values`] via
/// [`strider_ir::Function::register_arg_value`].  Intended to run once, as an
/// [`OptimizerPipeline::add_post_pass`][crate::OptimizerPipeline::add_post_pass]
/// after the fixed-point loop has converged.
///
/// The arg layout (register slots + stack-arg offsets) and stack-pointer
/// varnode are read from the function's own calling convention
/// (`Function::default_cc`) at apply time — the function is the single
/// source of truth, so the pass carries only its behaviour knobs.
#[derive(Clone, Default)]
pub struct FunctionArgDetect {
    /// Alias-analysis precision for the `mem_chain_is_dirty` shadow
    /// walk.  Default is
    /// [`crate::AliasMode::StackGlobalDisjoint`].
    alias_mode: crate::AliasMode,
    /// Whether a `Call` / `CallOther` on a stack-arg `Load`'s memory
    /// chain shadows the slot.  Default `false` (aggressive arg
    /// detection): a call does not, by itself, clobber a stack-arg slot,
    /// so a `Load` downstream of a call still qualifies.  Set `true` for
    /// the conservative reading where any call on the chain marks the
    /// slot dirty.
    ///
    /// The callee is opaque, so when the flag is off there is nothing to
    /// inspect about the call's arguments — it simply does not shadow.
    call_clobbers_args: bool,
}

impl FunctionArgDetect {
    /// Creates the pass with default knobs.  The arg layout and stack
    /// pointer come from the function under analysis.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Overrides the alias-analysis precision used by the shadow walk.
    /// See [`crate::AliasMode`] for the soundness/coverage trade-off.
    #[must_use]
    pub const fn alias_mode(mut self, mode: crate::AliasMode) -> Self {
        self.alias_mode = mode;
        self
    }

    /// Sets whether a `Call` / `CallOther` on a stack-arg `Load`'s
    /// memory chain shadows the slot.  Default `false`.  See the
    /// [`call_clobbers_args`][Self::call_clobbers_args] field docs.
    #[must_use]
    pub const fn call_clobbers_args(mut self, clobbers: bool) -> Self {
        self.call_clobbers_args = clobbers;
        self
    }
}

impl Optimizer for FunctionArgDetect {
    fn apply(
        &self,
        ctx: &mut crate::RewriteCtx<'_>,
        _opt_ctx: &mut crate::OptCtx<'_>,
    ) -> Result<OptimizationResult> {
        // SSoT: derive the positional-arg layout and stack pointer from the
        // function's own calling convention.  `layout.register_args()`
        // yields slots in ABI order with canonical positional indices, and
        // `layout.first_stack_index()` gives the register-vs-stack boundary.
        let layout =
            strider_target::PositionalArgLayout::from_convention(ctx.function_ref().default_cc());
        let stack_vn = ctx.function_ref().default_cc().stack_vn;
        let arg_passing_regs: Vec<rsleigh::Vn> = layout.register_args().map(|(_, vn)| vn).collect();
        let stack_arg_offsets: Vec<i64> = layout.stack_args().map(|(_, o)| o).collect();
        let first_stack_arg = layout.first_stack_index() as usize;
        // Rebuild the side-table from scratch so the pass is idempotent when
        // re-run on the same function across stable iterations (otherwise
        // carrier ids would accumulate duplicates).
        ctx.clear_arg_values();
        detect_register_args(ctx, &arg_passing_regs)?;
        detect_stack_args(
            ctx,
            stack_vn,
            &stack_arg_offsets,
            first_stack_arg,
            self.alias_mode,
            self.call_clobbers_args,
        )?;
        // Arg detection only populates the arg_index_to_values side-table,
        // and the memory-SSA walk's narrowing only shortens stack-arg loads'
        // memory edges (idempotent, never changes which args are detected) —
        // so as a post-pass it reports `NoChange`: nothing here unlocks
        // further optimization that would require another fixed-point pass.
        Ok(OptimizationResult::NoChange)
    }
}

/// Rule D: for every register in `arg_passing_regs` whose `InitialVar` node
/// has live uses, register that `InitialVar` as the carrier for arg `i` in
/// `function.arg_index_to_values`.  No contiguity check — reading only arg 2
/// still labels it arg 2.
///
/// **Sub-register fallback.**  The IR builder doesn't always promote a
/// register read at function entry to the full container register: a `char`
/// or `int` parameter compiled on x86_64 SysV may surface as
/// `InitialVar(ECX size=4 at off=8)` rather than `InitialVar(RCX size=8
/// at off=8)`, and on AArch64-BE a 32-bit `int` parameter may surface as
/// `InitialVar(W3 size=4 at off=X3.off+4)` (BE places the 32-bit
/// sub-register in the high half of the 64-bit container).
///
/// When the exact-`Vn` lookup misses, fall back to any `InitialVar` whose
/// `Vn` lies fully within `reg`'s byte range
/// `[reg.addr_off, reg.addr_off + reg.size)` in the same address space.
/// If multiple candidates exist, pick the largest (the most specific
/// reading of `reg`'s state).  The registered node carries the actual
/// sub-register Vn, so downstream consumers see the width the function
/// actually reads.
/// Find the largest `(Vn, NodeId)` whose Vn is fully contained
/// in `reg`'s byte range.  Returns `None` if nothing's contained.
///
/// Binary-searches the pre-sorted per-space bucket to the first
/// candidate at `addr_off >= reg.addr_off`, then scans forward
/// while the candidate's `addr_off` stays below `reg`'s end.
fn largest_sub_in(
    initial_vars_by_space: &rustc_hash::FxHashMap<rsleigh::VnSpace, Vec<(rsleigh::Vn, NodeId)>>,
    reg: rsleigh::Vn,
) -> Option<(rsleigh::Vn, NodeId)> {
    let bucket = initial_vars_by_space.get(&reg.addr_space)?;
    let lo = reg.addr_off;
    let hi = reg.addr_off.checked_add(u64::from(reg.size))?;
    // First index whose `addr_off >= lo`.
    let start_idx = bucket.partition_point(|(vn, _)| vn.addr_off < lo);
    let mut best: Option<(rsleigh::Vn, NodeId)> = None;
    for (vn, n) in &bucket[start_idx..] {
        if vn.addr_off >= hi {
            break;
        }
        // Containment: `vn.addr_off >= lo` (guaranteed by start_idx)
        // and `vn.addr_off + vn.size <= hi`.
        if vn
            .addr_off
            .checked_add(u64::from(vn.size))
            .is_some_and(|e| e <= hi)
        {
            match best {
                Some((b, _)) if b.size >= vn.size => {}
                _ => best = Some((*vn, *n)),
            }
        }
    }
    best
}

fn detect_register_args(
    ctx: &mut crate::RewriteCtx<'_>,
    arg_passing_regs: &[rsleigh::Vn],
) -> Result<()> {
    // Single reachable-graph scan collects every InitialVar's Vn → NodeId.
    // `InitialVar` nodes are not hash-cached (see `NodeKind::is_cacheable`),
    // so we still rely on the builder's invariant of at most one InitialVar
    // per varnode.  Walking `preorder()` rather than `all_node_ids()` skips
    // detached zombies left by destructive passes (e.g. `PhiCollapse`),
    // matching every other pass in this crate.
    let mut initial_vars: rustc_hash::FxHashMap<rsleigh::Vn, NodeId> =
        rustc_hash::FxHashMap::default();
    // Scan the live `InitialVar` nodes into a `Vn`-keyed map.  Each
    // `InitialVar` carries a unique `Vn` (builder invariant), so the map is
    // insertion-order-independent — iterate the cached live set directly
    // (`live_of_kind`, no graph walk).
    for n in ctx.live_of_kind(|k| matches!(k, NodeKind::InitialVar(_))) {
        let NodeKind::InitialVar(vn) = *ctx.node_kind(n) else {
            unreachable!("live_of_kind seeded on InitialVar");
        };
        initial_vars.insert(vn, n);
    }

    // Per-space bucket sorted by `(addr_off ascending, size descending)`.
    // Lets `largest_sub_in` binary-search to the first vn with
    // `addr_off >= lo` and then scan forward while `addr_off < hi`
    // — the sort order means the first vn with `addr_off == lo` is
    // the widest one (size-descending), and the scan terminates as
    // soon as we walk past `hi`.  Hot-loop complexity becomes
    // O(log V + matches) per arg slot instead of O(V).
    let initial_vars_by_space: rustc_hash::FxHashMap<rsleigh::VnSpace, Vec<(rsleigh::Vn, NodeId)>> = {
        let mut by_space: rustc_hash::FxHashMap<rsleigh::VnSpace, Vec<(rsleigh::Vn, NodeId)>> =
            rustc_hash::FxHashMap::default();
        for (&vn, &n) in &initial_vars {
            by_space.entry(vn.addr_space).or_default().push((vn, n));
        }
        for bucket in by_space.values_mut() {
            bucket.sort_by_key(|(vn, _)| (vn.addr_off, std::cmp::Reverse(vn.size)));
        }
        by_space
    };

    for (i, reg) in arg_passing_regs.iter().enumerate() {
        // Exact match → use as-is.  Otherwise the largest sub-register
        // contained in `reg`'s byte range.
        let initial_var = if let Some(&n) = initial_vars.get(reg) {
            n
        } else if let Some((_, sub_n)) = largest_sub_in(&initial_vars_by_space, *reg) {
            sub_n
        } else {
            continue;
        };

        let [old_value] = ctx
            .node_outputs_exact::<1>(initial_var)
            .expect("InitialVar has 1 output per node signature");
        // Skip if the InitialVar has no consumers.
        if ctx.graph_ref().value_uses(old_value).next().is_none() {
            continue;
        }

        // Register the underlying InitialVar's value as the carrier for arg i.
        // The node stays in place; consumers are not rewired.  `old_value` is
        // the InitialVar's single output (computed above).
        ctx.register_arg_value(i as u32, old_value);
    }
    Ok(())
}

/// Rule (stack args): collect every `Load` node whose address decomposes to
/// `InitialVar(sp) + K` where `K` is one of the convention's stack-arg
/// offsets.  Group by `K`, then apply **strict contiguity** from position 0:
/// the first gap in the offset-set truncates, so surviving indices are a
/// gap-free prefix.  For each surviving group of qualifying `Load`s, register
/// every `Load` in the group into `function.arg_index_to_values` for that index.
///
/// The original `Load` nodes survive unchanged — no consumer rewiring.
/// Multiple `Load`s at the same `sp+K` offset (e.g. different widths) are all
/// registered into the side-table for that index.
/// Returns the `InitialVar(sp)` output (the entry stack pointer), or `None`
/// when the function never reads it.  Stack-arg detection requires every
/// candidate load's terminal base to equal this value.
fn entry_sp_value(
    ctx: &mut crate::RewriteCtx<'_>,
    stack_vn: rsleigh::Vn,
) -> Option<ValueId> {
    // Exactly one `InitialVar(stack_vn)` exists (builder invariant), so the
    // search is order-independent — iterate the cached live set directly
    // (`live_of_kind`, no graph walk).
    for n in ctx.live_of_kind(|k| matches!(k, NodeKind::InitialVar(_))) {
        if matches!(*ctx.node_kind(n), NodeKind::InitialVar(vn) if vn == stack_vn) {
            let [out] = ctx
                .node_outputs_exact::<1>(n)
                .expect("InitialVar has 1 output per node signature");
            return Some(out);
        }
    }
    None
}

fn detect_stack_args(
    ctx: &mut crate::RewriteCtx<'_>,
    stack_vn: rsleigh::Vn,
    stack_arg_offsets: &[i64],
    first_stack_arg: usize,
    alias_mode: crate::AliasMode,
    call_clobbers_args: bool,
) -> Result<()> {
    if stack_arg_offsets.is_empty() {
        return Ok(());
    }

    // Incoming stack args live at fixed offsets from the *entry* stack
    // pointer.  Pin `InitialVar(sp)` up front: a candidate load's terminal
    // base must equal it, so a load rooted at a *different* SP terminal —
    // e.g. an alignment-masked `sp & mask`, which addresses a frame local —
    // is rejected even when its offset coincides with a convention slot.
    // With no entry-SP read there can be no stack args.
    let Some(initial_sp) = entry_sp_value(ctx, stack_vn) else {
        return Ok(());
    };

    // Group candidate loads by their position `j` in `stack_arg_offsets`.
    // A load qualifies only if (a) its address decomposes to `sp + K` where
    // `K` is a convention offset, `sp` is the entry SP, and (b) nothing on
    // its memory chain may alias that slot (DFS shadow check).  If *any*
    // load at offset K is shadowed, the whole K-group is disqualified
    // (conservative).
    let mut memo: SpExprMemo = SpExprMemo::default();
    let mut shadow_memo: ShadowMemo = ShadowMemo::default();
    let mut groups: rustc_hash::FxHashMap<usize, Vec<NodeId>> = rustc_hash::FxHashMap::default();
    let mut disqualified: rustc_hash::FxHashSet<usize> = rustc_hash::FxHashSet::default();
    let mut work = seeded_kind(ctx, |k| matches!(k, NodeKind::Load(_)));
    while let Some(node_id) = work.dequeue() {
        let [memory, addr] = ctx
            .graph_ref()
            .node_inputs_exact::<2>(node_id)
            .expect("Load has 2 inputs per node signature");
        let [load_value] = ctx
            .node_outputs_exact::<1>(node_id)
            .expect("Load has 1 output per node signature");
        // A `Load` always produces a value output (validated signature).
        let load_ty = ctx
            .value_kind(load_value)
            .as_value()
            .expect("Load output is a value");
        let load_size = load_ty.byte_size() as i64;
        let Some(SpExpr { base, offset }) =
            decompose_sp(ctx.function_ref(), addr, stack_vn, &mut memo)
        else {
            continue;
        };
        // Only loads rooted at the entry SP are incoming stack args.
        if base != initial_sp {
            continue;
        }
        let Some(j) = stack_arg_offsets.iter().position(|&k| k == offset) else {
            continue;
        };
        if disqualified.contains(&j) {
            continue;
        }
        let dirty = mem_chain_is_dirty(
            ctx,
            node_id,
            memory,
            base,
            offset,
            load_size,
            &mut memo,
            &mut shadow_memo,
            alias_mode,
            call_clobbers_args,
        )?;
        if dirty {
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
        return Ok(());
    }

    for (j, _offset) in stack_arg_offsets.iter().enumerate().take(max_j_plus_one) {
        let index = (first_stack_arg + j) as u32;
        let Some(loads) = groups.remove(&j) else {
            continue;
        };

        // Guard: every load in this K-group must share the same memory space.
        // The grouping logic above keys only on `j` (the offset slot), not on
        // space, so a multi-space lifter could in principle place two loads at
        // the same offset in different spaces.  Skip the whole group on
        // mismatch rather than silently merging.  Every member came from the
        // `Load`-seeded worklist, so the kind is a structural invariant here.
        let first = loads[0];
        let NodeKind::Load(space) = *ctx.node_kind(first) else {
            unreachable!("group members are seeded from Load nodes");
        };
        if loads
            .iter()
            .any(|&l| !matches!(*ctx.node_kind(l), NodeKind::Load(s) if s == space))
        {
            continue;
        }

        // Register every qualifying Load's value as a carrier for arg `index`.
        // Each Load stays in place; consumers are not rewired.
        // Multiple Loads at the same offset (different widths) are all
        // recorded — the Vec<ValueId> per index accommodates this.
        for load in loads {
            let [load_value] = ctx
                .node_outputs_exact::<1>(load)
                .expect("Load has 1 output per node signature");
            ctx.register_arg_value(index, load_value);
        }
    }
    Ok(())
}

/// Per-pass-call memo for [`mem_chain_is_dirty`]. Keyed on `(memory_token,
/// base, offset, load_size)`. Threaded through `detect_stack_args` so that
/// two stack-arg-load candidates sharing the same memory predecessor, SP
/// base, and slot reuse the walk's verdict.
type ShadowMemo = rustc_hash::FxHashMap<(ValueId, ValueId, i64, i64), bool>;

/// Walks the memory chain backward from `mem` looking for any def that
/// may shadow the byte range `[offset, offset + load_size)`.  Returns
/// `true` if any path through the chain may overwrite bytes in the
/// load's range.
///
/// Delegates the traversal (cycle-guarded, MemPhi-forking, stack-safe at
/// any chain depth) to [`may_clobber`]; the per-def shadow verdict comes
/// from the shared [`SpAliasOracle`] with the candidate load's
/// `AddrClass::SpRooted { base, offset }` class.  Memoised per pass-call
/// on `(mem, base, offset, load_size)`.
#[allow(clippy::too_many_arguments)]
fn mem_chain_is_dirty(
    ctx: &mut crate::RewriteCtx<'_>,
    load: NodeId,
    mem: ValueId,
    base: ValueId,
    offset: i64,
    load_size: i64,
    sp_memo: &mut SpExprMemo,
    memo: &mut ShadowMemo,
    alias_mode: crate::AliasMode,
    call_clobbers_args: bool,
) -> Result<bool> {
    let entry_key = (mem, base, offset, load_size);
    if let Some(&cached) = memo.get(&entry_key) {
        return Ok(cached);
    }

    let mut oracle = SpAliasOracle {
        load_class: AddrClass::SpRooted { base, offset },
        load_size,
        sp_memo,
        alias_mode,
        call_clobbers: call_clobbers_args,
    };
    // Walk from the def that produced the load's memory input.  The oracle
    // does not consult the load node (the slot range is carried by
    // `offset`/`load_size`), but `may_clobber` uses it to narrow the load's
    // memory edge onto the nearest clobber.  The chain is dirty iff that
    // nearest clobber is anything but the clean `InitialMemory` root.
    let start = ctx.function_ref().producer(mem);
    let clobber = crate::memory_ssa::may_clobber(ctx, &mut oracle, load, start);
    let result = !matches!(ctx.node_kind(clobber), NodeKind::InitialMemory);
    memo.insert(entry_key, result);
    Ok(result)
}

#[cfg(test)]
mod tests;
