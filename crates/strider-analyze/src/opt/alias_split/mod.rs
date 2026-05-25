//! `AliasSplit` — converts the unified memory chain into a **forked
//! per-partition** memory SSA.
//!
//! After this pass runs, each [`AliasClass`] partition (Stack / Heap /
//! Unknown — Rom is read-only and has no chain) carries its own
//! independent chain of `Memory(Some(P))` tokens.  Operations in
//! partition `P` depend only on the previous op in `P`, so two stores
//! at disjoint partitions appear as parallel branches of the SSA graph
//! and the downstream stack-aware passes (`StackLoadForward`,
//! `CallStackArgCollect`, `FunctionArgDetect`, `LoadReadOnly`) can walk
//! exactly the partition they care about.
//!
//! # Forked vs linearised — the shape change
//!
//! ## Before AliasSplit
//!
//! ```text
//! InitialMemory → MemPhi(entry) → St@sp+0 → St@heap → St@sp+4 → Return
//!                                  (Stack)   (Heap)   (Stack)
//! ```
//!
//! ## After AliasSplit (forked, the new design)
//!
//! ```text
//!                ┌── MemPart[Stack]   → St@sp+0 → St@sp+4 ──┐
//! InitialMemory ─┤                                          ├── MemUnion → Return
//!                ├── MemPart[Heap]    → St@heap ────────────┤
//!                └── MemPart[Unknown] (passes through) ─────┘
//! ```
//!
//! `St@sp+4`'s `mem_input` is `St@sp+0`'s mem-output directly —
//! `St@heap` is bypassed because their partitions are disjoint.  The
//! per-partition chains reconverge at `MemUnion` only when a unified-
//! memory consumer (Return, full-clobber Call/CallOther,
//! IndirectBranch) needs everything.
//!
//! ## Per-Call clobber semantics
//!
//! A bare `Call` clobbers `[Heap, Unknown]` by default — the Stack
//! chain flows through `Call` unchanged so a stack store before the
//! call still feeds a stack load after the call directly:
//!
//! ```text
//! Stack:        St@sp+4 ──────────────────────────────→ Ld@sp+4
//! Heap/Unknown: St@heap → MemUnion → Call → MemPart[Heap] → Ld@heap
//! ```
//!
//! `CallOther`'s clobber set comes from
//! [`strider_target::call_other_abi::CallOtherAbi::mem_clobbers`] —
//! per-op data on the ABI table.  A CallOther with empty `mem_clobbers`
//! (e.g. `cpuid`, `rdtsc`) doesn't touch the memory chain at all;
//! `MEM_CLOBBER_HEAP_UNKNOWN` is the usual default for atomics /
//! barriers / port-I/O; `MEM_CLOBBER_FULL` is reserved for kernel-entry
//! paths (`syscall`, `swi`, `software_interrupt`) where the kernel can
//! also mutate the user stack frame.
//!
//! # v1 scope and assumptions
//!
//! * Only `Stack` vs `Unknown` is *address-classifiable* by this pass
//!   today — the `decompose_sp` test promotes SP-relative addresses to
//!   `Stack` and everything else falls back to `Unknown`.  `Heap` and
//!   `Rom` partition tags exist on the IR (and `Call` clobbers them
//!   distinctly) but address-range-based Heap/Rom detection is not in
//!   scope yet.
//! * **All three partitions (`Stack` / `Heap` / `Unknown`) are projected
//!   at function entry**, even if a particular function turns out not
//!   to touch one of them.  Conservatively over-projecting (never
//!   under-projecting) keeps the algorithm sound when a downstream pass
//!   inserts a new partition consumer; idle partition heads cost one
//!   `MemPartition` node each and become dead unless wired up.
//! * `MemPhi` joins (the lifter's per-Region memory-φ) are partitioned
//!   sibling-style: every non-entry `MemPhi` M with N CFG predecessors
//!   gets N+1 per-partition mirror `MemPhi[P]` nodes (one per active
//!   partition), all sharing M's `phi_token` from the owning `Region`,
//!   so each partition keeps its own SSA join across branches and
//!   loops.  The entry `MemPhi` (single arm = `InitialMemory`) stays
//!   the chain root and gets `MemPartition[P]` projections instead of
//!   sibling mirrors — degenerate single-arm partition phis would
//!   just be collapsed by `RedundantPhis`.
//! * Back-edges at loop-header `MemPhi`s are wired in a second pass:
//!   the first pass walks chain consumers in topological-by-memory
//!   order, leaves any pred-slot whose producer's per-partition heads
//!   haven't been computed yet as deferred, and the second pass closes
//!   those slots once every chain node has been processed.
//! * Idempotent: re-running on already-partitioned IR (any pre-existing
//!   `MemPartition` / `MemUnion`) is a no-op.

use entity_utils::DenseEntitySet;
use rustc_hash::FxHashMap;
use strider_ir::node::{NodeId, NodeKind, NodeOutputId, NodeOutputKind};
use strider_ir::{AliasClass, Function};
use strider_target::call_other_abi::{CallOtherClass, classify};

use crate::opt::error::Result;
use crate::opt::pipeline::{OptimizationResult, Optimizer};
use crate::opt::sp_expr::{SpExpr, SpExprMemo, decompose_sp};

/// Per-call default clobber set: a bare `Call` clobbers `[Heap,
/// Unknown]` so the Stack chain flows through it.  Future calling-
/// convention metadata (`CcMetadata.no_memory_clobber`) could refine
/// this on a per-callee basis; for now every `Call` uses this set.
const CALL_DEFAULT_CLOBBERS: &[AliasClass] = &[AliasClass::Heap, AliasClass::Unknown];

/// Terminal-clobber set used at `Return` and `IndirectBranch` —
/// everything is "consumed" so any pending stores in any partition
/// must reach the terminator.
const TERMINAL_CLOBBERS: &[AliasClass] =
    &[AliasClass::Stack, AliasClass::Heap, AliasClass::Unknown];

/// Active partitions tracked by this pass (in canonical order).  Rom
/// is read-only and has no chain.
const ACTIVE_PARTITIONS: [AliasClass; 3] =
    [AliasClass::Stack, AliasClass::Heap, AliasClass::Unknown];

/// Splits the unified memory chain into one independent SSA chain per
/// alias-class partition.  See the module-level documentation for the
/// algorithm and IR shape.
#[derive(Clone)]
pub struct AliasSplit {
    /// Stack-pointer varnode used by [`decompose_sp`] to classify
    /// `Store` / `Load` addresses as Stack-class.
    sp_vn: rsleigh::Vn,
    /// Architecture preset — required to look up `CallOther` ABI
    /// entries via [`classify`] so each user-op's `mem_clobbers` set
    /// drives the per-partition clobber decision.
    preset: strider_target::ArchPreset,
}

impl AliasSplit {
    /// Creates a new pass for the given stack-pointer varnode and arch
    /// preset.  Convenience constructor for tests; production paths
    /// prefer [`Self::from_convention`].
    #[must_use]
    pub fn new(sp_vn: rsleigh::Vn, preset: strider_target::ArchPreset) -> Self {
        Self { sp_vn, preset }
    }

    /// Creates a new pass whose stack-pointer varnode is taken from
    /// the supplied calling convention and whose `ArchPreset` is taken
    /// from `arch`.
    #[must_use]
    pub fn from_convention(
        cc: &strider_target::BuiltCallingConvention,
        arch: &strider_target::SleighArch,
    ) -> Self {
        Self {
            sp_vn: cc.stack_ptr_vn,
            preset: arch.preset(),
        }
    }
}

impl Optimizer for AliasSplit {
    fn optimize(
        &self,
        function: &mut Function,
        _entry: NodeId,
    ) -> Result<OptimizationResult> {
        // Idempotency: any existing MemPartition / MemUnion ⇒ NoChange.
        if function.has_kind(|k| {
            matches!(k, NodeKind::MemPartition { .. } | NodeKind::MemUnion)
        }) {
            return Ok(OptimizationResult::NoChange);
        }

        // Locate the unique InitialMemory node.  No InitialMemory ⇒
        // nothing to partition.
        let Some(initial_memory) = function
            .preorder_kind(|k| matches!(k, NodeKind::InitialMemory))
            .next()
        else {
            return Ok(OptimizationResult::NoChange);
        };

        // Bail on functions with an `IndirectBranch` placeholder: the
        // indirect-branch resolver's stack-array classifier walks the
        // memory chain backward from the dispatching Load to find
        // stored target values and is sensitive to chain shape.  This
        // bail predates the multi-pred MemPhi work; a separate audit
        // tracks lifting it.
        if function.has_kind(|k| matches!(k, NodeKind::IndirectBranch)) {
            return Ok(OptimizationResult::NoChange);
        }

        // Locate the entry MemPhi — the single-arm `MemPhi` whose only
        // memory input is `InitialMemory.out`.  Partition chains START
        // at this MemPhi's memory output rather than at `InitialMemory`
        // directly, preserving the shape
        //   InitialMemory → MemPhi → MemPartition[P] → … → barrier
        // that consumer passes (`StackLoadForward`,
        // `find_stack_stored_value_at_offset`, `CallStackArgCollect`)
        // already handle.  Non-entry MemPhis are sibling-partitioned
        // by `build_forked_chains` — see the module-level docs.
        //
        // If no MemPhi exists (function with no region structure?
        // shouldn't happen post-builder), fall back to projecting from
        // `InitialMemory.out` directly; otherwise pick the first
        // MemPhi whose sole memory input is `InitialMemory.out`.
        let [im_out] = function.node_outputs_exact::<1>(initial_memory)?;
        let entry_mem_phi = find_entry_mem_phi(function, im_out);
        let chain_root_out = match entry_mem_phi {
            Some(phi) => function.memory_output_of(phi)?,
            None => im_out,
        };

        // Classify every memory-touching node.
        let mut memo = SpExprMemo::default();
        let classified =
            classify_all(function, self.sp_vn, self.preset, entry_mem_phi, &mut memo)?;

        // Bail (NoChange) if the chain has no memory consumers at all
        // — pure compute functions, or trivial `return 0` shapes.
        if classified.mem_chain_consumers.is_empty() {
            return Ok(OptimizationResult::NoChange);
        }

        // Build per-partition chains.  Returns Changed iff at least one
        // boundary was inserted.
        let result = build_forked_chains(
            function,
            chain_root_out,
            initial_memory,
            entry_mem_phi,
            &classified,
        )?;

        // Populate Function::stack_offsets for every Store/Load whose
        // address decomposed to a single concrete sp+K.  Done after the
        // chain rewrite so AliasSplit's structural changes don't race
        // with the side-table writes.
        for (node, offset) in &classified.concrete_stack_offsets {
            function.set_stack_offset(*node, *offset);
        }

        Ok(result)
    }
}

/// Finds the entry `MemPhi` — the lifter's single-arm join at the
/// entry region whose only memory input is `InitialMemory`'s output.
///
/// Returns `None` if no such `MemPhi` exists (e.g. a synthesised IR
/// without region structure).  In that case the caller projects the
/// per-partition `MemPartition` nodes directly from `InitialMemory`.
///
/// We pick the entry `MemPhi` structurally (single mem input = IM.out)
/// rather than by preorder position because preorder may visit a
/// non-entry `MemPhi` first if the lifter's chain ordering differs.
fn find_entry_mem_phi(function: &Function, im_out: NodeOutputId) -> Option<NodeId> {
    function
        .preorder_kind(|k| matches!(k, NodeKind::MemPhi))
        .find(|&phi| {
            let inputs: Vec<_> = function.node_inputs(phi).into_iter().collect();
            // MemPhi inputs: [phi_token, mem_pred_0, ...].  Entry phi
            // has exactly one mem pred and it must be IM.out.
            inputs.len() == 2 && inputs[1] == im_out
        })
}

/// Per-node classification.
#[derive(Debug, Clone)]
struct Classified {
    /// `NodeId` → its single-partition address class.  Populated only
    /// for `Store` / `Load` (whose `addr` decomposes — or not — to
    /// SP+K).  Other kinds default to "no entry" and are looked up by
    /// `consumer_kind` instead.
    addr_class: FxHashMap<NodeId, AliasClass>,
    /// `NodeId` → its memory clobber set (only for barrier-shaped
    /// nodes: `Call`, `CallOther`, `Return`, `IndirectBranch`).
    barriers: FxHashMap<NodeId, &'static [AliasClass]>,
    /// Every node that consumes a `Memory(_)` edge, in preorder
    /// (i.e. reachable from `entry`).  Drives the topological walk in
    /// [`build_forked_chains`].
    mem_chain_consumers: Vec<NodeId>,
    /// Concrete stack offsets for Store/Load nodes whose address
    /// decomposes to `sp + K` (single Terminal).  Phi-of-offsets
    /// addresses are not recorded.  Applied to
    /// `Function::stack_offsets` by the `optimize` driver after
    /// `build_forked_chains` runs.
    concrete_stack_offsets: Vec<(NodeId, i64)>,
}

/// Walk the function once and classify every memory-touching node.
///
/// `entry_mem_phi`, if `Some`, names the chain root — that MemPhi is
/// EXCLUDED from `mem_chain_consumers` because the chain projects from
/// its output rather than passing through it.  Non-entry MemPhis ARE
/// enqueued: [`build_forked_chains`] processes each of them as a join
/// and emits per-partition mirror MemPhis.
fn classify_all(
    function: &Function,
    sp_vn: rsleigh::Vn,
    preset: strider_target::ArchPreset,
    entry_mem_phi: Option<NodeId>,
    memo: &mut SpExprMemo,
) -> Result<Classified> {
    let mut addr_class: FxHashMap<NodeId, AliasClass> = FxHashMap::default();
    let mut barriers: FxHashMap<NodeId, &'static [AliasClass]> = FxHashMap::default();
    let mut mem_chain_consumers: Vec<NodeId> = Vec::new();
    let mut concrete_stack_offsets: Vec<(NodeId, i64)> = Vec::new();

    for node in function.preorder() {
        let kind = *function.node_kind(node);
        match kind {
            NodeKind::Store(_) => {
                let [_, addr, _] = function.node_inputs_exact::<3>(node)?;
                let (cls, offset) = address_class_with_offset(function, addr, sp_vn, memo);
                addr_class.insert(node, cls);
                if let Some(off) = offset {
                    concrete_stack_offsets.push((node, off));
                }
                mem_chain_consumers.push(node);
            }
            NodeKind::Load(_) => {
                let [_, addr] = function.node_inputs_exact::<2>(node)?;
                let (cls, offset) = address_class_with_offset(function, addr, sp_vn, memo);
                addr_class.insert(node, cls);
                if let Some(off) = offset {
                    concrete_stack_offsets.push((node, off));
                }
                mem_chain_consumers.push(node);
            }
            NodeKind::Call => {
                barriers.insert(node, CALL_DEFAULT_CLOBBERS);
                mem_chain_consumers.push(node);
            }
            NodeKind::CallOther { .. } => {
                let clobbers: &'static [AliasClass] =
                    if let Some(name) = function.call_other_name(node) {
                        match classify(preset, name) {
                            Some(CallOtherClass::Call(abi)) => abi.mem_clobbers,
                            // NoOp / NoReturn / unknown name — leave
                            // the memory edge alone.  Unknown CallOther
                            // would have been rejected at lift time.
                            _ => &[],
                        }
                    } else {
                        // No name on the side-table: conservative
                        // default — treat as full-clobber so we don't
                        // forward across an unmodeled op.
                        TERMINAL_CLOBBERS
                    };
                if !clobbers.is_empty() {
                    barriers.insert(node, clobbers);
                    mem_chain_consumers.push(node);
                }
            }
            NodeKind::Return | NodeKind::IndirectBranch => {
                barriers.insert(node, TERMINAL_CLOBBERS);
                mem_chain_consumers.push(node);
            }
            NodeKind::MemPhi => {
                // Non-entry MemPhis are real CFG joins: every active
                // partition gets a sibling-mirror MemPhi at the same
                // Region.  The entry MemPhi is the chain root, not a
                // join, and is excluded so its mem output stays the
                // projection anchor for `MemPartition[P]` nodes.
                if Some(node) != entry_mem_phi {
                    mem_chain_consumers.push(node);
                }
            }
            _ => {}
        }
    }

    Ok(Classified {
        addr_class,
        barriers,
        mem_chain_consumers,
        concrete_stack_offsets,
    })
}

/// Address classifier — single source of truth used by both `Store` and
/// `Load` paths.  Returns the `AliasClass` and, when the address is a
/// `SpExpr::Terminal`, the concrete `i64` stack offset.  The
/// phi-of-offsets case (`SpExpr::Phi`) produces `AliasClass::Stack` but
/// no offset (consumers that need per-branch offsets can call
/// `decompose_sp` directly).
///
/// Heap detection would require address-range analysis; that is out of
/// scope for this pass, so everything non-SP-rooted falls back to Unknown.
fn address_class_with_offset(
    function: &Function,
    addr: NodeOutputId,
    sp_vn: rsleigh::Vn,
    memo: &mut SpExprMemo,
) -> (AliasClass, Option<i64>) {
    match decompose_sp(function, addr, sp_vn, memo) {
        Some(SpExpr::Terminal { offset, .. }) => (AliasClass::Stack, Some(offset)),
        Some(SpExpr::Phi { .. }) => (AliasClass::Stack, None),
        None => (AliasClass::Unknown, None),
    }
}

/// Per-partition head map: which partition-typed memory output is
/// "live" for each partition at a given chain point.  In the forked
/// algorithm this is held per memory-output `NodeOutputId` in
/// [`OutgoingHeadsMap`] — every chain node's mem-output is associated
/// with the per-partition heads that downstream consumers should see.
type PartitionHeads = [Option<NodeOutputId>; 3];

/// `outgoing_heads[output_id][P]` = the partition-P memory token that
/// flows out of the node defining `output_id`.  Seeded at the chain
/// root with `MemPartition[P]` projections, then advanced node-by-node
/// in [`build_forked_chains`] as each chain consumer's per-partition
/// outgoing tokens are computed.
type OutgoingHeadsMap = FxHashMap<NodeOutputId, PartitionHeads>;

/// Maps an active partition to its index in [`PartitionHeads`].
/// Returns an error when given `AliasClass::Rom` (read-only memory has
/// no chain — callers should never request a Rom head).
#[inline]
fn partition_index(p: AliasClass) -> Result<usize> {
    match p {
        AliasClass::Stack => Ok(0),
        AliasClass::Heap => Ok(1),
        AliasClass::Unknown => Ok(2),
        AliasClass::Rom => Err(anyhow::anyhow!(
            "AliasSplit: Rom has no memory chain; partition_index called with Rom"
        )),
    }
}

/// Returns the current memory-output head for partition `p`.  Errors
/// when the head hasn't been initialised yet (caller bug — every active
/// partition's head is seeded at entry by `build_forked_chains`).
#[inline]
fn head_for(heads: &PartitionHeads, p: AliasClass) -> Result<NodeOutputId> {
    let idx = partition_index(p)?;
    heads[idx].ok_or_else(|| {
        anyhow::anyhow!(
            "AliasSplit: partition {p:?}'s head was not initialised at entry"
        )
    })
}

#[inline]
fn set_head(heads: &mut PartitionHeads, p: AliasClass, value: NodeOutputId) -> Result<()> {
    let idx = partition_index(p)?;
    heads[idx] = Some(value);
    Ok(())
}

/// A back-edge deferred during pass 1 of [`build_forked_chains`]: at
/// the time a MemPhi M was processed, the partition-P heads flowing
/// out of one of its predecessor producers were not yet available
/// (the producer lives in M's own subtree — a loop back-edge).  Pass
/// 2 looks up `outgoing_heads[pred_value][P]` once every chain node
/// has been processed and closes the deferred input slot.
#[derive(Debug, Clone, Copy)]
struct DeferredBackEdge {
    /// The per-partition mirror MemPhi created for the unified MemPhi.
    partition_phi: NodeId,
    /// The active partition `P` for this mirror.
    partition: AliasClass,
    /// The unified MemPhi's input value at this pred slot — the
    /// "back-edge tail" mem-output whose per-partition head we'll
    /// look up once pass 1 finishes.
    pred_value: NodeOutputId,
}

/// Build the forked chains.  Returns `Changed` iff at least one
/// boundary (`MemPartition` / `MemUnion`) was inserted.
///
/// Algorithm:
///   1. Compute the chain consumers' topological order over Memory
///      edges (MemPhi joins contribute one chain-edge per predecessor;
///      cycles fall to a deferred sweep).
///   2. Insert one `MemPartition[P]` per active partition at the
///      function entry, all projecting from the chain root's memory
///      output.  Seed `outgoing_heads[chain_root_out] = [MP[P]…]`.
///   3. Pass 1 — walk the topological order.  For each chain node:
///      - `Store(P)` / `Load(P)`: rewire its mem-input to the
///        partition-P head at the producer's outgoing heads; retype
///        `Store`'s mem-output to `Memory(Some(P))`; record the
///        Store's outgoing heads (P advanced; other partitions
///        passed through).  `Load` produces no mem-output, so the
///        next consumer reads the producer's heads directly.
///      - `Call` / `CallOther` / `Return` / `IndirectBranch`: emit a
///        `MemUnion` of every active partition head at the producer
///        and re-project clobbered partitions from the barrier's
///        mem-output.  Outgoing heads = producer's heads with
///        clobbered slots replaced.
///      - `MemPhi` (non-entry): emit one per-partition mirror MemPhi
///        per active partition, sharing the unified MemPhi's
///        `phi_token`.  Per-pred-slot input is the partition's head
///        from the producer's outgoing heads, or — if that producer
///        hasn't been processed yet (loop back-edge) — deferred to
///        pass 2.
///   4. Pass 2 — close back-edges.  Every `DeferredBackEdge` records
///      `(partition_phi, partition, pred_value)`; pass 1 guarantees
///      `outgoing_heads[pred_value]` is filled by the time the chain
///      walk finishes, so we look up the head and `add_node_input`.
fn build_forked_chains(
    function: &mut Function,
    chain_root_out: NodeOutputId,
    chain_root_node: NodeId,
    entry_mem_phi: Option<NodeId>,
    classified: &Classified,
) -> Result<OptimizationResult> {
    // Topologically order memory chain consumers BEFORE inserting any
    // boundary nodes.  MemPhi joins introduce true forks; cycles
    // (loop back-edges) get appended to the tail in classifier
    // preorder so they're visited at least once.
    let chain_order = topological_mem_order(function, chain_root_out, classified)?;

    // Insert one MemPartition[P] per active partition projecting from
    // the chain root's memory output (typically the lifter's entry
    // MemPhi).
    let mut entry_heads: PartitionHeads = [None; 3];
    for &p in &ACTIVE_PARTITIONS {
        let mp = function.create_node_attributed(
            NodeKind::MemPartition { class: p },
            [chain_root_out],
            [NodeOutputKind::Memory(Some(p))],
            &[chain_root_node],
        );
        let [mp_out] = function.node_outputs_exact::<1>(mp)?;
        set_head(&mut entry_heads, p, mp_out)?;
    }
    let mut outgoing_heads: OutgoingHeadsMap = FxHashMap::default();
    outgoing_heads.insert(chain_root_out, entry_heads);
    // If the chain root is the entry MemPhi's output, also map the
    // entry MemPhi's OUTPUT id (already chain_root_out) into the
    // table.  Belt-and-suspenders: a non-entry MemPhi whose only
    // mem-input is the chain root (unlikely but possible in a
    // synthesised IR) still finds its producer's heads.
    let _ = entry_mem_phi; // entry MemPhi is excluded from chain_order

    // Pass 1: thread per-partition outgoing heads through every chain
    // consumer in topological order.
    let mut handled: DenseEntitySet<NodeId> = DenseEntitySet::new();
    let mut deferred: Vec<DeferredBackEdge> = Vec::new();

    for &consumer in &chain_order {
        if !handled.insert(consumer) {
            continue;
        }
        let kind = *function.node_kind(consumer);
        match kind {
            NodeKind::Store(_) => {
                let p = classified
                    .addr_class
                    .get(&consumer)
                    .copied()
                    .ok_or_else(|| anyhow::anyhow!(
                        "AliasSplit: Store {consumer:?} missing from addr_class table"
                    ))?;
                let producer_value = mem_input_value(function, consumer)?;
                let producer_heads = lookup_outgoing_or_seed(
                    &outgoing_heads,
                    producer_value,
                    entry_heads,
                )?;
                let cur_head = head_for(&producer_heads, p)?;
                replace_specific_input(function, consumer, 0, cur_head)?;
                let mem_out = function.memory_output_of(consumer)?;
                function
                    .graph_mut()
                    .set_memory_partition(mem_out, Some(p))?;
                let mut store_heads = producer_heads;
                set_head(&mut store_heads, p, mem_out)?;
                outgoing_heads.insert(mem_out, store_heads);
            }
            NodeKind::Load(_) => {
                let p = classified
                    .addr_class
                    .get(&consumer)
                    .copied()
                    .ok_or_else(|| anyhow::anyhow!(
                        "AliasSplit: Load {consumer:?} missing from addr_class table"
                    ))?;
                let producer_value = mem_input_value(function, consumer)?;
                let producer_heads = lookup_outgoing_or_seed(
                    &outgoing_heads,
                    producer_value,
                    entry_heads,
                )?;
                let cur_head = head_for(&producer_heads, p)?;
                replace_specific_input(function, consumer, 0, cur_head)?;
                // Load produces no mem-output; no entry to record.
            }
            NodeKind::Call
            | NodeKind::CallOther { .. }
            | NodeKind::Return
            | NodeKind::IndirectBranch => {
                let clobbers = classified
                    .barriers
                    .get(&consumer)
                    .copied()
                    .ok_or_else(|| anyhow::anyhow!(
                        "AliasSplit: barrier {consumer:?} missing from barrier table"
                    ))?;
                let producer_value = mem_input_value(function, consumer)?;
                let producer_heads = lookup_outgoing_or_seed(
                    &outgoing_heads,
                    producer_value,
                    entry_heads,
                )?;
                let mut heads = producer_heads;
                splice_barrier(function, consumer, clobbers, &mut heads)?;
                if let Ok(mem_out) = function.memory_output_of(consumer) {
                    outgoing_heads.insert(mem_out, heads);
                }
            }
            NodeKind::MemPhi => {
                splice_mem_phi_join(
                    function,
                    consumer,
                    &mut outgoing_heads,
                    entry_heads,
                    &mut deferred,
                )?;
            }
            _ => {
                // Defensive: classifier shouldn't enqueue anything else.
            }
        }
    }

    // Pass 2: close back-edges.  Every deferred record refers to a
    // per-partition mirror MemPhi whose pred-i input wasn't wired in
    // pass 1 because the producer's outgoing heads were missing.
    // Pass 1 has now visited every chain node, so the producer's
    // heads are filled — unless the producer was outside the
    // classified chain (defensive fallback handled below).
    for entry in &deferred {
        let head = match outgoing_heads.get(&entry.pred_value) {
            Some(h) => head_for(h, entry.partition)?,
            // Producer wasn't classified — fall back to the
            // partition's entry head so the resulting MemPhi is at
            // least well-formed.  Loop back-edges whose body skipped
            // the chain (no Stores / Loads in the body) land here.
            None => head_for(&entry_heads, entry.partition)?,
        };
        function.graph_mut().add_node_input(entry.partition_phi, head)?;
    }

    Ok(OptimizationResult::Changed)
}

/// Looks up the per-partition outgoing heads for `producer_value` in
/// `outgoing_heads`.  Falls back to `entry_heads` (the chain root's
/// partition projections) when `producer_value` was never recorded —
/// this happens when a chain consumer's mem-input traces to a
/// non-classified producer (a synthesised Region-bypass shape or a
/// non-MemPhi producer the classifier doesn't cover).  The fallback
/// is conservative: the resulting per-partition wiring sees the
/// function-entry head, which is sound (no writes happened) for any
/// real-binary lifter output.
fn lookup_outgoing_or_seed(
    outgoing_heads: &OutgoingHeadsMap,
    producer_value: NodeOutputId,
    entry_heads: PartitionHeads,
) -> Result<PartitionHeads> {
    if let Some(h) = outgoing_heads.get(&producer_value) {
        return Ok(*h);
    }
    Ok(entry_heads)
}

/// Emit per-partition mirror MemPhi nodes for the unified `mem_phi`.
///
/// Each mirror shares the unified MemPhi's `phi_token` (inputs[0])
/// and has one value-input per CFG predecessor — wired to that
/// predecessor's outgoing partition-P head (looked up in
/// `outgoing_heads`).  Predecessor producers whose heads aren't
/// populated yet (loop back-edges) are recorded in `deferred` for
/// the pass-2 sweep.
///
/// After this returns:
/// * `outgoing_heads[mem_phi.mem_out][P] = mirror[P].mem_out` so
///   downstream consumers see partition-P tokens at the join.
/// * Every per-partition mirror M[P] is structurally a valid
///   MemPhi-of-Memory(Some(P)): its `phi_token` input is the same
///   Region's phi_token; its value inputs are Memory(Some(P)) tokens
///   from each predecessor's partition-P chain.
fn splice_mem_phi_join(
    function: &mut Function,
    mem_phi: NodeId,
    outgoing_heads: &mut OutgoingHeadsMap,
    entry_heads: PartitionHeads,
    deferred: &mut Vec<DeferredBackEdge>,
) -> Result<()> {
    // Snapshot inputs so we can safely mutate the graph while still
    // iterating positionally over the unified MemPhi's pred values.
    let inputs: Vec<NodeOutputId> = function.node_inputs(mem_phi).into_iter().collect();
    if inputs.is_empty() {
        // Degenerate (no phi_token) — local typing would reject this;
        // skip to keep the pass robust mid-fixed-point.
        return Ok(());
    }
    let phi_token = inputs[0];
    let pred_values = &inputs[1..];

    // Build the per-partition mirror MemPhis.  Each starts with just
    // [phi_token] as inputs; value inputs are appended via
    // `add_node_input` so we can defer back-edge slots without
    // leaving the node mid-rewrite with the wrong arity.
    let unified_mem_out = function.memory_output_of(mem_phi)?;
    let mut new_heads: PartitionHeads = [None; 3];

    for &p in &ACTIVE_PARTITIONS {
        let mirror = function.create_node_attributed(
            NodeKind::MemPhi,
            [phi_token],
            [NodeOutputKind::Memory(Some(p))],
            &[mem_phi],
        );
        // Wire each pred slot.  If the pred's outgoing heads are
        // available, append the partition-P head as an input.  If
        // not, defer to pass 2 and record the deferred slot.
        for &pred_value in pred_values {
            match outgoing_heads.get(&pred_value) {
                Some(heads) => {
                    let h = head_for(heads, p)?;
                    function.graph_mut().add_node_input(mirror, h)?;
                }
                None => {
                    deferred.push(DeferredBackEdge {
                        partition_phi: mirror,
                        partition: p,
                        pred_value,
                    });
                }
            }
        }
        let [mirror_out] = function.node_outputs_exact::<1>(mirror)?;
        set_head(&mut new_heads, p, mirror_out)?;
    }

    // The unified MemPhi's mem-output now maps to the per-partition
    // mirror outputs; downstream consumers indexing into
    // `outgoing_heads` by this output id pick up the mirrors.
    outgoing_heads.insert(unified_mem_out, new_heads);

    // Defensive: when a barrier or store CHAIN-FORWARDS straight from
    // a unified MemPhi (rare but possible if the chain doesn't pass
    // through any partition-typed nodes in between), the old code
    // would have read partition heads from the chain-root projections
    // — which is unsound for non-entry MemPhis.  By recording
    // `outgoing_heads[unified_mem_out]` we ensure downstream lookups
    // use the per-MemPhi mirrors.
    let _ = entry_heads;
    Ok(())
}


/// Compute the topological order in which the memory chain visits
/// its consumers.
///
/// **MemPhis are visited first**, in classifier preorder, before
/// any non-MemPhi chain consumer.  This is load-bearing for the
/// forked-chain rewrite: a `Store` / `Load` / `Call` inside a loop
/// body reads from the loop-header `MemPhi`'s mem-output, so the
/// `MemPhi`'s per-partition mirrors must exist (and be recorded in
/// `outgoing_heads`) before the body consumers are processed.
/// `MemPhi`-to-`MemPhi` back-edges naturally fall to the deferred
/// sweep in pass 2 of [`build_forked_chains`] — pass 1 wires the
/// available forward inputs and queues the back-edge slots.
///
/// **Non-MemPhi consumers** are then ordered Kahn-style over their
/// mem-input edges, skipping `MemPhi` predecessors (already visited)
/// and the chain root.  Remaining cycles fall through to a final
/// classifier-preorder append so every consumer is visited at least
/// once.
fn topological_mem_order(
    function: &Function,
    chain_root_out: NodeOutputId,
    classified: &Classified,
) -> Result<Vec<NodeId>> {
    let mut order: Vec<NodeId> = Vec::with_capacity(classified.mem_chain_consumers.len());

    // Phase 1: all MemPhis first, in classifier preorder.  Each
    // MemPhi's value inputs may include back-edges; pass 1 wires the
    // forward edges (resolvable now) and defers back-edge slots.
    for &n in &classified.mem_chain_consumers {
        if matches!(function.node_kind(n), NodeKind::MemPhi) {
            order.push(n);
        }
    }

    // Phase 2: non-MemPhi consumers in Kahn topo order over mem-
    // input edges, restricted to non-MemPhi producers (MemPhis are
    // already in `order` and their mem-output's outgoing-heads
    // entries are populated by pass 1's MemPhi handler before any
    // non-MemPhi consumer runs).
    let mut in_chain: DenseEntitySet<NodeId> = DenseEntitySet::new();
    for &n in &classified.mem_chain_consumers {
        if !matches!(function.node_kind(n), NodeKind::MemPhi) {
            in_chain.insert(n);
        }
    }

    let mut predecessors: FxHashMap<NodeId, Vec<NodeId>> = FxHashMap::default();
    for &n in &classified.mem_chain_consumers {
        if matches!(function.node_kind(n), NodeKind::MemPhi) {
            continue;
        }
        let mem_inputs = mem_input_values(function, n);
        let mut preds: Vec<NodeId> = Vec::new();
        for mem_in_value in mem_inputs {
            if mem_in_value == chain_root_out {
                continue;
            }
            let producer = function.get_node_from_output(mem_in_value);
            if in_chain.contains(producer) {
                preds.push(producer);
            }
            // MemPhi producers and out-of-chain producers contribute
            // no in-degree (resolved via `outgoing_heads` lookup).
        }
        predecessors.insert(n, preds);
    }

    let mut successors: FxHashMap<NodeId, Vec<NodeId>> = FxHashMap::default();
    for (&n, preds) in &predecessors {
        for &p in preds {
            successors.entry(p).or_default().push(n);
        }
    }

    let mut in_degree: FxHashMap<NodeId, usize> = FxHashMap::default();
    for (&n, preds) in &predecessors {
        in_degree.insert(n, preds.len());
    }

    use cranelift_entity::EntityRef;
    let mut roots: Vec<NodeId> = in_degree
        .iter()
        .filter_map(|(&n, &d)| if d == 0 { Some(n) } else { None })
        .collect();
    roots.sort_by_key(|n| n.index());

    let mut ready: std::collections::VecDeque<NodeId> = roots.into_iter().collect();
    while let Some(n) = ready.pop_front() {
        order.push(n);
        if let Some(succs) = successors.get(&n) {
            let mut sorted: Vec<NodeId> = succs.clone();
            sorted.sort_by_key(|x| x.index());
            for s in sorted {
                let d = in_degree.entry(s).or_insert(0);
                *d = d.saturating_sub(1);
                if *d == 0 {
                    ready.push_back(s);
                }
            }
        }
    }

    // Cycle fallback: any non-MemPhi consumers still unvisited
    // (Store-to-Store back-edges? rare) get appended in classifier
    // preorder.  Pass 1 of `build_forked_chains` falls back to
    // entry-heads when a pred-value isn't in `outgoing_heads` yet,
    // which is sound for the loop-body-into-loop-header back-edge
    // shape — `outgoing_heads[loop_header_MemPhi.out]` was populated
    // in Phase 1.
    for &n in &classified.mem_chain_consumers {
        if !order.contains(&n) {
            order.push(n);
        }
    }

    Ok(order)
}

/// Returns the value driving the FIRST memory input slot of `node`.
/// Errors if `node` has no memory input.  Use [`mem_input_values`]
/// for MemPhi (which has multiple mem inputs).
fn mem_input_value(function: &Function, node: NodeId) -> Result<NodeOutputId> {
    let inputs = function.node_inputs(node);
    let graph = function.graph();
    for input_value in inputs {
        if matches!(graph.output_kind(input_value), NodeOutputKind::Memory(_)) {
            return Ok(input_value);
        }
    }
    Err(anyhow::anyhow!(
        "node {node:?} has no Memory input — classifier mistakenly enqueued a non-memory node"
    ))
}

/// Returns every value driving a memory input slot of `node`.
/// `Store`, `Load`, `Call`, `CallOther`, `Return`, `IndirectBranch`
/// have exactly one mem input; `MemPhi` has one per CFG predecessor
/// (its `inputs[0]` is a `PhiToken`, not Memory, and is skipped).
fn mem_input_values(function: &Function, node: NodeId) -> Vec<NodeOutputId> {
    let inputs = function.node_inputs(node);
    let graph = function.graph();
    inputs
        .into_iter()
        .filter(|&v| matches!(graph.output_kind(v), NodeOutputKind::Memory(_)))
        .collect()
}

/// Wire a barrier's memory input through a `MemUnion` of **all
/// active partition heads** and, for each *clobbered* partition,
/// re-project a fresh `MemPartition[P]` from the barrier's memory
/// output (when the barrier has one).
///
/// Including non-clobbered partition heads in the `MemUnion` keeps the
/// stack-aware consumer passes (`CallStackArgCollect`,
/// `StackLoadForward`, `FunctionArgDetect`) able to find the
/// stack-partition head at the barrier's program point — they walk
/// backward from `barrier.mem_input` through the `MemUnion` to the
/// `Stack`-tagged input.  The non-clobbered partitions' heads are
/// **not** advanced past the barrier: a post-barrier consumer of
/// partition Q (Q ∉ clobbers) walks straight back to the pre-barrier
/// `heads[Q]` node, bypassing the barrier entirely.
fn splice_barrier(
    function: &mut Function,
    barrier: NodeId,
    clobbers: &'static [AliasClass],
    heads: &mut PartitionHeads,
) -> Result<()> {
    if clobbers.is_empty() {
        // No memory effect — barrier shouldn't have been enqueued.
        return Ok(());
    }

    // Build the MemUnion from every active partition's head so that
    // consumer passes can probe the union for any partition's view at
    // this program point.  See the doc comment above for the
    // motivation.
    let union_inputs: Vec<NodeOutputId> = ACTIVE_PARTITIONS
        .iter()
        .map(|&p| head_for(heads, p))
        .collect::<Result<Vec<_>>>()?;

    // Memory input slot on barrier nodes — different per kind.
    let mem_in_slot = barrier_memory_input_slot(function, barrier)?;

    // Special case: MemUnion with a single input is degenerate — for
    // tests on single-partition functions where only one partition is
    // ever clobbered we'd emit redundant 1-input MemUnion nodes, which
    // the IR validator rejects (MemUnion expects ≥1 partition-typed
    // input but the dot renderer / consumer passes assume the
    // multi-input shape).  Still emit it for shape uniformity; the
    // IR's MemUnion signature accepts 0 or more variadic inputs and
    // single-input is valid.
    let union_node = function.create_node_attributed(
        NodeKind::MemUnion,
        union_inputs,
        [NodeOutputKind::Memory(None)],
        &[barrier],
    );
    let [union_out] = function.node_outputs_exact::<1>(union_node)?;
    replace_specific_input(function, barrier, mem_in_slot, union_out)?;

    // Re-project clobbered partitions from the barrier's mem-output
    // (if any).  Return / IndirectBranch don't produce a memory
    // output — terminate the chain there.
    let mem_out = function.memory_output_of(barrier).ok();
    if let Some(barrier_mem_out) = mem_out {
        for &p in clobbers {
            let mp = function.create_node_attributed(
                NodeKind::MemPartition { class: p },
                [barrier_mem_out],
                [NodeOutputKind::Memory(Some(p))],
                &[barrier],
            );
            let [mp_out] = function.node_outputs_exact::<1>(mp)?;
            set_head(heads, p, mp_out)?;
        }
    }
    // For terminal barriers (no mem output), the relevant heads stay
    // pointing to their pre-barrier values — but since nothing past
    // the terminator can consume them, the dangling heads are inert.

    Ok(())
}


/// Returns the input-slot index of the memory edge on `barrier`.
///
/// Walks the signature-provided input kinds — the lone `Memory` input.
fn barrier_memory_input_slot(function: &Function, barrier: NodeId) -> Result<u32> {
    let n_inputs = function.node_inputs(barrier).len();
    let graph = function.graph();
    for i in 0..n_inputs {
        let input_id = function.node_input_id_at(barrier, i)?;
        let output_id = graph.input_output_id(input_id);
        if matches!(graph.output_kind(output_id), NodeOutputKind::Memory(_)) {
            // Convert to u32 — node input counts are bounded.  An
            // overflow here would indicate a malformed graph.
            return u32::try_from(i).map_err(|_| anyhow::anyhow!(
                "AliasSplit: node {barrier:?} input index {i} overflows u32"
            ));
        }
    }
    Err(anyhow::anyhow!(
        "node {barrier:?} has no Memory input slot — classifier enqueued a non-memory node"
    ))
}

/// Replaces the input at position `input_idx` of `node_id` to consume
/// `new_out` instead.  Uses the public `update_input` helper which
/// maintains use-list invariants and evicts cache entries for
/// cacheable owners.
fn replace_specific_input(
    function: &mut Function,
    node_id: NodeId,
    input_idx: u32,
    new_out: NodeOutputId,
) -> Result<()> {
    let input_id = function.node_input_id_at(node_id, input_idx as usize)?;
    function.graph_mut().update_input(input_id, new_out);
    Ok(())
}

#[cfg(test)]
mod tests;
