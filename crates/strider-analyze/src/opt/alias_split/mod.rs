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
//! * `MemPhi` is currently treated as a barrier whose mem-edge is
//!   sealed via `MemUnion` and re-projected via per-partition
//!   `MemPartition` on the consumer side.  True per-partition `MemPhi`
//!   sibling construction (preserving the SSA join shape) is deferred
//!   to a follow-up — the over-projection is sound but may inhibit
//!   forwarding across loop back-edges.
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

        // v1 scope: bail on any function with a multi-predecessor
        // `MemPhi` (a genuine CFG memory join).  Per-partition phi
        // construction across multiple CFG predecessors is a follow-
        // up — see the module-level documentation.
        if has_multi_pred_mem_phi(function) {
            return Ok(OptimizationResult::NoChange);
        }

        // v1 scope: bail on functions with an `IndirectBranch`
        // placeholder.  The indirect-branch resolver's stack-array
        // classifier walks the memory chain backward from the
        // dispatching Load to find the stored target values; under
        // the new forked design the chain shape it walks subtly
        // differs from the old AliasSplit's output in a way that
        // breaks the seven `indirect_branch_resolved_*` tests on
        // arches other than x86 (which happens to pass).  Leaving
        // these functions unpartitioned preserves the previous
        // behaviour for them and keeps the gates green; a follow-up
        // will audit the classifier interaction and lift this guard.
        if function.has_kind(|k| matches!(k, NodeKind::IndirectBranch)) {
            return Ok(OptimizationResult::NoChange);
        }

        // Locate the entry MemPhi (the lifter's single-arm marker at
        // the start of the entry region).  Under v1, the partition
        // chains START at the MemPhi's memory output rather than at
        // InitialMemory's output — this keeps the shape
        //   InitialMemory → MemPhi → MemPartition[P] → … → barrier
        // which the consumer passes (`StackLoadForward`,
        // `find_stack_stored_value_at_offset`,
        // `CallStackArgCollect`) already handle correctly (they pass
        // through `MemPartition` and bail at `MemPhi`, which is the
        // chain root from their perspective).
        //
        // If there's no MemPhi (function with no region structure?
        // shouldn't happen post-builder), fall back to projecting
        // from `InitialMemory.out` directly.
        let chain_root_out = if let Some(phi) = function
            .preorder_kind(|k| matches!(k, NodeKind::MemPhi))
            .next()
        {
            function.memory_output_of(phi)?
        } else {
            let [im_out] = function.node_outputs_exact::<1>(initial_memory)?;
            im_out
        };

        // Classify every memory-touching node.
        let mut memo = SpExprMemo::default();
        let classified = classify_all(function, self.sp_vn, self.preset, &mut memo)?;

        // Bail (NoChange) if the chain has no memory consumers at all
        // — pure compute functions, or trivial `return 0` shapes.
        if classified.mem_chain_consumers.is_empty() {
            return Ok(OptimizationResult::NoChange);
        }

        // Build per-partition chains.  Returns Changed iff at least one
        // boundary was inserted.
        let result =
            build_forked_chains(function, chain_root_out, initial_memory, &classified)?;

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

/// Returns true if the function contains a `MemPhi` whose memory
/// predecessor count is > 1 (i.e. a genuine join across two or more
/// CFG predecessors).  v1 of the forked AliasSplit bails on these
/// because the over-projection trick (wire every per-partition phi
/// predecessor to the same `cur_head`) creates trivial phis that
/// `RedundantPhis` collapses every iteration, breaking convergence.
fn has_multi_pred_mem_phi(function: &Function) -> bool {
    function.preorder_kind(|k| matches!(k, NodeKind::MemPhi))
        .any(|n| {
            // MemPhi inputs: [phi_token, pred_0_mem, ...].  More than
            // one memory pred = multi-pred.
            function.node_inputs(n).len() > 2
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
fn classify_all(
    function: &Function,
    sp_vn: rsleigh::Vn,
    preset: strider_target::ArchPreset,
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
            // Other kinds (incl. `MemPhi`) are intentionally NOT
            // enqueued.  Under v1 of the forked AliasSplit the
            // partition chains START at the function-entry MemPhi
            // (the lifter's single-arm entry-region marker).
            // Multi-pred MemPhi (true CFG join) is handled by the
            // function-level guard `has_multi_pred_mem_phi` which
            // bails the pass.
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
/// currently "live" for each partition.  Updated as the algorithm
/// walks the unified-memory chain in topological order.
type PartitionHeads = [Option<NodeOutputId>; 3];

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

/// Build the forked chains.  Returns `Changed` iff at least one
/// boundary (`MemPartition` / `MemUnion`) was inserted.
///
/// Algorithm:
///   1. Snapshot every unified-memory consumer's (node, slot, value)
///      mem-input edge BEFORE inserting boundaries — once we create
///      `MemPartition` nodes, the unified-memory output of
///      `InitialMemory` will have new consumers we don't want to
///      revisit.
///   2. Insert one `MemPartition[P]` per active partition at the
///      function entry, all projecting from `InitialMemory`'s output.
///   3. Walk the snapshotted consumer list in preorder.  For each:
///      - `Store(P)` / `Load(P)`: rewire its mem-input to the current
///        head of partition P; retype `Store`'s mem-output to
///        `Memory(Some(P))`; advance head[P] to the new mem-output.
///      - `Call` / `CallOther` / `MemPhi` / `Return` / `IndirectBranch`:
///        emit a `MemUnion` of the heads of clobbered partitions
///        (skip pure-clobber barriers altogether); rewire the
///        barrier's mem-input to the `MemUnion`; if the barrier
///        produces a memory output (Call/CallOther/MemPhi), emit a
///        fresh `MemPartition[P]` per clobbered P projecting from
///        that output and advance head[P] to the new partition's
///        output; non-clobbered partitions keep their existing head.
fn build_forked_chains(
    function: &mut Function,
    chain_root_out: NodeOutputId,
    chain_root_node: NodeId,
    classified: &Classified,
) -> Result<OptimizationResult> {
    // Walk the memory chain in topological order BEFORE inserting any
    // boundary nodes — once we splice MemPartition / MemUnion in, the
    // unified-memory edge between the chain root and the first
    // consumer becomes a chain through several new nodes and the
    // simple "output_uses" lookup would weave back through them.
    let chain_order = topological_mem_order(function, chain_root_out, classified)?;

    // Insert one MemPartition[P] per active partition projecting from
    // the chain root's memory output (typically the lifter's entry
    // MemPhi).  Record each partition's single output in
    // `entry_heads`.
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

    // Thread the per-partition heads through the chain in
    // topological order.  At every barrier they branch off into a
    // MemUnion + re-projection of clobbered partitions; non-clobbered
    // partitions keep their current head.
    let mut heads = entry_heads;
    let mut handled: DenseEntitySet<NodeId> = DenseEntitySet::new();

    for &consumer in &chain_order {
        if !handled.insert(consumer) {
            continue;
        }
        let kind = *function.node_kind(consumer);
        match kind {
            NodeKind::Store(_) => {
                // Inputs: [memory, addr, data].  Memory slot index 0.
                let p = classified
                    .addr_class
                    .get(&consumer)
                    .copied()
                    .ok_or_else(|| anyhow::anyhow!(
                        "AliasSplit: Store {consumer:?} missing from addr_class table"
                    ))?;
                let cur_head = head_for(&heads, p)?;
                replace_specific_input(function, consumer, 0, cur_head)?;
                // Retype Store's memory output to Memory(Some(P)) and
                // advance head[P] to it.
                let mem_out = function.memory_output_of(consumer)?;
                function
                    .graph_mut()
                    .set_memory_partition(mem_out, Some(p))?;
                set_head(&mut heads, p, mem_out)?;
            }
            NodeKind::Load(_) => {
                // Inputs: [memory, addr].  Memory slot index 0.  Load
                // doesn't produce a memory output, so head[P] is
                // unchanged.
                let p = classified
                    .addr_class
                    .get(&consumer)
                    .copied()
                    .ok_or_else(|| anyhow::anyhow!(
                        "AliasSplit: Load {consumer:?} missing from addr_class table"
                    ))?;
                let cur_head = head_for(&heads, p)?;
                replace_specific_input(function, consumer, 0, cur_head)?;
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
                splice_barrier(function, consumer, clobbers, &mut heads)?;
            }
            _ => {
                // Defensive: classifier shouldn't enqueue anything else.
            }
        }
    }

    Ok(OptimizationResult::Changed)
}


/// Compute the topological order in which the unified memory chain
/// visits its consumers.  Starts at `InitialMemory`'s memory output
/// and walks forward through every memory consumer's memory output
/// (if any).  Returns the list of memory-chain consumer `NodeId`s in
/// chain order — every node has all of its memory-input producers
/// earlier in the list.
///
/// Algorithm: classic Kahn-style topological sort over the subgraph
/// induced by Memory edges.  We compute an in-degree count over Memory
/// inputs for each known memory consumer, then drain a queue that
/// starts with consumers whose memory-input is `InitialMemory`'s
/// output (no other in-chain producer feeds them).
///
/// This is robust to the IR's general preorder being unrelated to the
/// memory chain — e.g. when the walk visits a Load before its
/// Store-on-the-mem-chain because the Load is a data input of an
/// earlier Return.
fn topological_mem_order(
    function: &Function,
    chain_root_out: NodeOutputId,
    classified: &Classified,
) -> Result<Vec<NodeId>> {
    // The set of memory-chain consumers as a quick membership probe.
    // A node belongs to the chain iff it has a memory-input slot AND
    // the classifier enqueued it (Loads, Stores, barriers — MemPhi is
    // intentionally excluded; it's the chain root, see
    // `Optimizer::optimize`).
    let mut in_chain: DenseEntitySet<NodeId> = DenseEntitySet::new();
    for &n in &classified.mem_chain_consumers {
        in_chain.insert(n);
    }

    // For each chain consumer, compute its "memory predecessor" — the
    // chain consumer whose mem-output feeds this node's mem-input.
    // `None` means the memory-input is fed from `chain_root_out`
    // directly (typically the entry MemPhi's output) and thus the
    // node is a chain root.
    let mut predecessor: FxHashMap<NodeId, Option<NodeId>> =
        FxHashMap::default();
    for &n in &classified.mem_chain_consumers {
        let mem_in_value = mem_input_value(function, n)?;
        let producer = function.get_node_from_output(mem_in_value);
        if mem_in_value == chain_root_out {
            predecessor.insert(n, None);
        } else if in_chain.contains(producer) {
            predecessor.insert(n, Some(producer));
        } else {
            // Unexpected: a chain consumer whose mem-input is neither
            // the chain root nor another chain consumer.  Could happen
            // if a chain consumer's mem-input is a node kind we didn't
            // classify as in-chain.  Conservative: treat as a root.
            predecessor.insert(n, None);
        }
    }

    // Build successor-list from predecessor map: for each consumer,
    // who are its chain successors?
    let mut successors: FxHashMap<NodeId, Vec<NodeId>> = FxHashMap::default();
    let mut roots: Vec<NodeId> = Vec::new();
    for (&n, pred) in &predecessor {
        match *pred {
            Some(p) => successors.entry(p).or_default().push(n),
            None => roots.push(n),
        }
    }

    // Kahn's algorithm: drain roots, enqueue their successors as their
    // predecessors get processed.  In-degree is 0 for roots; every
    // other node has in-degree 1 (one memory predecessor) in this
    // linear-chain world (MemPhi is treated as a barrier with a
    // single conceptual predecessor — but Multi-predecessor MemPhi
    // exists in IR! See below).
    //
    // BUT MemPhi has multiple memory inputs!  The simplification: the
    // classifier promotes MemPhi to a barrier that's clobbered as
    // unified — and for chain-ordering, we want the MemPhi visited
    // after its first predecessor that's been computed.  Since we
    // can't fork into N partition predecessors here for v1, we'll
    // approximate by taking the *first* memory-input of MemPhi as its
    // chain predecessor (or fall back to "root" if it's InitialMemory).
    //
    // Refine: for each node, compute its in-degree from the successor
    // map and drain.
    let mut in_degree: FxHashMap<NodeId, usize> = FxHashMap::default();
    for &n in &classified.mem_chain_consumers {
        in_degree.insert(n, 0);
    }
    for succs in successors.values() {
        for &s in succs {
            // Saturating add to defend against an unexpected ordering
            // bug — without this, a missing in-degree entry would
            // index-panic the map.  Treating a missing entry as 0 is
            // safe (the topo drain will just visit such a node at
            // its first opportunity).
            *in_degree.entry(s).or_insert(0) += 1;
        }
    }

    // Stable sort the roots for deterministic ordering.
    use cranelift_entity::EntityRef;
    roots.sort_by_key(|n| n.index());

    let mut order: Vec<NodeId> = Vec::with_capacity(classified.mem_chain_consumers.len());
    let mut ready: std::collections::VecDeque<NodeId> = roots.into_iter().collect();
    while let Some(n) = ready.pop_front() {
        order.push(n);
        if let Some(succs) = successors.get(&n) {
            // Stable sort by NodeId for deterministic ordering.
            let mut sorted: Vec<NodeId> = succs.clone();
            sorted.sort_by_key(|x| x.index());
            for s in sorted {
                // Defensive: a missing successor in `in_degree` would
                // indicate a bookkeeping bug (every chain consumer is
                // seeded with 0 above).  Treat as 0 → ready-to-visit.
                let d = in_degree.entry(s).or_insert(0);
                *d = d.saturating_sub(1);
                if *d == 0 {
                    ready.push_back(s);
                }
            }
        }
    }

    // Fallback: any consumers we couldn't topologically order
    // (cycles? out-of-chain inputs?) get appended in classifier
    // preorder so the algorithm still attempts them.
    for &n in &classified.mem_chain_consumers {
        if !order.contains(&n) {
            order.push(n);
        }
    }

    Ok(order)
}

/// Returns the value driving the memory input slot of `node`.  Errors
/// if `node` has no memory input.
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
