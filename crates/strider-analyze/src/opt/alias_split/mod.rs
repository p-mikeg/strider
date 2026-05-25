//! `AliasSplit` — converts unified memory chains to partition-typed chains
//! by inserting `MemPartition` / `MemUnion` boundary nodes.
//!
//! After this pass runs, memory consumers operate on a typed
//! `Memory(Some(P))` edge restricted to a single alias class.  The
//! downstream stack-aware passes (`StackLoadForward`, `CallStackArgCollect`,
//! `FunctionArgDetect`, `LoadReadOnly`) can walk only the partition chain
//! they care about — no more per-load `decompose_sp` calls.
//!
//! # v1 scope
//!
//! * Only `AliasClass::Stack` vs `AliasClass::Unknown` is detected.
//!   Stack is identified by an `addr` that decomposes to
//!   `InitialVar(sp) + K` via [`decompose_sp`]; everything else is
//!   Unknown.  `Rom` partition + MMIO discovery deferred.
//! * `Call` / `CallOther(memory_edge=true)` / `IndirectBranch` / `Return`
//!   are treated as **barriers**: the chain into them is sealed with a
//!   `MemUnion`, and the chain out of them (where one exists) restarts
//!   from unified memory.
//! * `MemPhi` whose predecessors don't all agree on a single
//!   `AliasClass` is left unified — the conservative v1 stance.  A
//!   `MemPhi` whose predecessors all resolve to the same Stack
//!   partition is itself promoted.
//! * Idempotent: re-running on already-partitioned IR (any pre-existing
//!   `MemPartition` / `MemUnion` node) is a no-op.
//!
//! # Algorithm sketch
//!
//! 1. Idempotency guard: scan for any existing `MemPartition` /
//!    `MemUnion`; bail with `NoChange` if found.
//! 2. Locate the `InitialMemory` node and walk forward through every
//!    memory-edge-producing consumer (`Store`, `StackStore`,
//!    `StackStorePhi`, `MemPhi`) plus the address-classified
//!    address-consuming `Load`.  Classify each by `AliasClass`.
//! 3. Group the chain into segments delimited by barriers (`Call`,
//!    `Return`, `IndirectBranch`, `CallOther(memory_edge)`) — each
//!    segment is one contiguous unified-memory subgraph.
//! 4. For each segment whose memory-producing consumers are all
//!    Stack-classified, splice a `MemPartition(Stack)` at the entry and
//!    a `MemUnion` at the exit; retype every Stack-class producer's
//!    memory output to `Memory(Some(stack_partition))`.
//! 5. Segments containing any Unknown-classified consumer are left
//!    unified (no boundary insertion).
//!
//! Limitations carried forward to follow-up commits:
//!
//! * No Rom / Heap detection.
//! * `MemPhi` only promoted in the all-stack-predecessors case;
//!   loop-back `MemPhi` (self-referencing predecessor) is conservatively
//!   left unified.

use entity_utils::DenseEntitySet;
use rustc_hash::FxHashMap;
use strider_ir::node::{NodeId, NodeKind, NodeOutputId, NodeOutputKind};
use strider_ir::{AliasClass, Function};

use crate::opt::error::Result;
use crate::opt::pipeline::{OptimizationResult, Optimizer};
use crate::opt::sp_expr::{SpExprMemo, decompose_sp};

/// Splits unified memory chains into partition-typed chains.  See the
/// module-level documentation for the full algorithm.
#[derive(Clone)]
pub struct AliasSplit {
    /// Stack-pointer varnode used by [`decompose_sp`] to classify
    /// `Store` / `Load` addresses as Stack-class.
    sp_vn: rsleigh::Vn,
}

impl AliasSplit {
    /// Creates a new pass for the given stack-pointer varnode.
    #[must_use]
    pub fn new(sp_vn: rsleigh::Vn) -> Self {
        Self { sp_vn }
    }

    /// Creates a new pass whose stack-pointer varnode is taken from the
    /// supplied calling convention.
    #[must_use]
    pub fn from_convention(cc: &strider_target::BuiltCallingConvention) -> Self {
        Self { sp_vn: cc.stack_ptr_vn }
    }
}

impl Optimizer for AliasSplit {
    fn optimize(
        &self,
        function: &mut Function,
        _entry: NodeId,
    ) -> Result<OptimizationResult> {
        // 1. Idempotency: any existing MemPartition / MemUnion ⇒ NoChange.
        if function.has_kind(|k| {
            matches!(k, NodeKind::MemPartition { .. } | NodeKind::MemUnion)
        }) {
            return Ok(OptimizationResult::NoChange);
        }

        // 2. Locate the unique InitialMemory node (validator guarantees
        //    uniqueness on a built function; we propagate a typed error
        //    if the invariant is broken).
        let initial_memory = function
            .preorder_kind(|k| matches!(k, NodeKind::InitialMemory))
            .next();
        let Some(initial_memory) = initial_memory else {
            // No InitialMemory ⇒ nothing to partition.
            return Ok(OptimizationResult::NoChange);
        };

        // 3. Classify every memory-edge-producing consumer reachable
        //    from InitialMemory and walk forward to identify chain
        //    segments.
        let mut memo = SpExprMemo::default();
        let mut classifier = ChainClassifier::new(self.sp_vn);
        classifier.classify_function(function, &mut memo)?;

        // 4. Splice boundaries for every Stack-only segment we found.
        let result = splice_stack_segments(function, initial_memory, &classifier)?;
        Ok(result)
    }
}

/// Per-output classification result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodeAliasClass {
    /// Address decomposes to `InitialVar(sp) + K` via [`decompose_sp`].
    Stack,
    /// Address doesn't decompose (or the node has no addr — e.g. a
    /// MemPhi whose predecessors disagree).  Conservative: aliases
    /// everything.
    Unknown,
}

/// Scans the function once to classify every memory-edge node by alias
/// class and remember which nodes are barriers.
struct ChainClassifier {
    sp_vn: rsleigh::Vn,
    /// `NodeId` → `NodeAliasClass`.  Populated for `Store` /
    /// `StackStore` / `StackStorePhi` / `Load` (Loads contribute the
    /// classification of their consumed memory — an Unknown-addr Load
    /// taints its containing segment).  `MemPhi` resolution is
    /// deferred to the splice step because it depends on already-
    /// classified predecessor verdicts.
    classes: FxHashMap<NodeId, NodeAliasClass>,
    /// `NodeId`s of barrier nodes encountered during the walk
    /// (`Call`, `Return`, `IndirectBranch`, `CallOther`).  We do NOT
    /// distinguish CallOther-with-memory-edge from CallOther-without
    /// here — the IR-level `CallOther` always carries a memory
    /// output; whether the ABI's `memory_edge` is true or false is a
    /// strider-target classification that's already been resolved into
    /// the IR shape (CallOther without a memory edge would not have
    /// produced a memory output).  For v1 we conservatively treat
    /// every CallOther with a memory output as a barrier.
    #[allow(dead_code)]
    barriers: DenseEntitySet<NodeId>,
}

impl ChainClassifier {
    fn new(sp_vn: rsleigh::Vn) -> Self {
        Self {
            sp_vn,
            classes: FxHashMap::default(),
            barriers: DenseEntitySet::new(),
        }
    }

    fn classify_function(&mut self, function: &Function, memo: &mut SpExprMemo) -> Result<()> {
        for node in function.preorder() {
            let kind = *function.node_kind(node);
            match kind {
                NodeKind::Store(_) => {
                    // Store inputs: [memory, addr, data].
                    let [_, addr, _] = function.node_inputs_exact::<3>(node)?;
                    let cls = if decompose_sp(function, addr, self.sp_vn, memo).is_some() {
                        NodeAliasClass::Stack
                    } else {
                        NodeAliasClass::Unknown
                    };
                    self.classes.insert(node, cls);
                }
                NodeKind::Load(_) => {
                    // Load inputs: [memory, addr].
                    let [_, addr] = function.node_inputs_exact::<2>(node)?;
                    let cls = if decompose_sp(function, addr, self.sp_vn, memo).is_some() {
                        NodeAliasClass::Stack
                    } else {
                        NodeAliasClass::Unknown
                    };
                    self.classes.insert(node, cls);
                }
                NodeKind::Call
                | NodeKind::Return
                | NodeKind::IndirectBranch
                | NodeKind::CallOther { .. } => {
                    self.barriers.insert(node);
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// Walks the unified-memory chain starting at `initial_memory`'s output
/// and inserts `MemPartition` / `MemUnion` boundaries around every
/// contiguous Stack-only segment.  Returns `Changed` if at least one
/// boundary was inserted.
///
/// Iterative driver — accumulates segment seeds in a work stack to
/// avoid recursion depth proportional to the number of sequential
/// barriers (a real binary may have hundreds of calls along a single
/// memory chain).
fn splice_stack_segments(
    function: &mut Function,
    initial_memory: NodeId,
    classifier: &ChainClassifier,
) -> Result<OptimizationResult> {
    let mut combined = OptimizationResult::NoChange;
    let [im_out] = function.node_outputs_exact::<1>(initial_memory)?;
    let mut work: Vec<(NodeOutputId, NodeId)> = vec![(im_out, initial_memory)];
    let mut seen_seeds: DenseEntitySet<NodeOutputId> = DenseEntitySet::new();
    seen_seeds.insert(im_out);

    while let Some((segment_in, producer_node)) = work.pop() {
        let new_seeds = process_segment(function, segment_in, producer_node, classifier)?;
        combined |= new_seeds.result;
        for (next_seed, next_producer) in new_seeds.next_segments {
            if seen_seeds.insert(next_seed) {
                work.push((next_seed, next_producer));
            }
        }
    }

    Ok(combined)
}

/// Output of [`process_segment`]: the rewrite verdict plus the list of
/// barrier-produced memory outputs that should be processed as new
/// segment seeds in the driver loop.
struct SegmentOutcome {
    result: OptimizationResult,
    next_segments: Vec<(NodeOutputId, NodeId)>,
}

/// Process a single segment of unified memory starting at `segment_in`
/// (the unified-memory output emerging from `producer_node` — either
/// `InitialMemory` or a barrier's memory output slot).
///
/// Returns a [`SegmentOutcome`] describing the rewrite verdict plus the
/// list of barrier-produced memory outputs that the iterative driver
/// in [`splice_stack_segments`] should enqueue as the next segment
/// seeds.  Recursion was retired so that a binary with hundreds of
/// sequential `Call`s along a single memory chain doesn't blow the
/// stack.
fn process_segment(
    function: &mut Function,
    segment_in: NodeOutputId,
    producer_node: NodeId,
    classifier: &ChainClassifier,
) -> Result<SegmentOutcome> {
    let mut walk = SegmentWalk::default();
    walk.collect(function, segment_in, classifier)?;

    // Collect next-segment seeds (barrier-produced memory outputs)
    // regardless of whether THIS segment splices — a later segment may
    // still be partition-eligible even if the current one bails.
    let next_segments: Vec<(NodeOutputId, NodeId)> = walk
        .barriers
        .iter()
        .filter_map(|&b| barrier_memory_output(function, b).map(|mo| (mo, b)))
        .collect();

    if walk.bailed
        || (walk.stack_producers.is_empty() && walk.stack_loads.is_empty())
    {
        // Either an Unknown-class consumer was seen (bail) or there is
        // no Stack activity in this segment — leave it unified.
        return Ok(SegmentOutcome {
            result: OptimizationResult::NoChange,
            next_segments,
        });
    }

    // Create the Stack partition (lazy; one per spliced segment is
    // fine — partitions never get unified across segments in v1).
    let stack_partition = function.partitions_mut().create(AliasClass::Stack);

    // 1. Insert MemPartition right after `segment_in`.  Wire every
    //    existing consumer of `segment_in` to consume from the new
    //    MemPartition's output instead.  (The MemPartition itself
    //    consumes from `segment_in`.)
    let part_node = function.create_node_attributed(
        NodeKind::MemPartition {
            partition: stack_partition,
        },
        [segment_in],
        [NodeOutputKind::Memory(Some(stack_partition))],
        &[producer_node],
    );
    let [part_out] = function.node_outputs_exact::<1>(part_node)?;
    rewire_consumers_except(function, segment_in, part_out, part_node)?;

    // 2. Retype every stack-class producer's memory output.
    for &producer in &walk.stack_producers {
        let mem_out = function.memory_output_of(producer)?;
        function
            .graph_mut()
            .set_memory_partition(mem_out, Some(stack_partition))?;
    }

    // 3. For every MemPhi we promoted, retype its output too.
    for &phi in &walk.stack_phis {
        let mem_out = function.memory_output_of(phi)?;
        function
            .graph_mut()
            .set_memory_partition(mem_out, Some(stack_partition))?;
    }

    // 4. For every barrier-input edge we collected, insert a MemUnion
    //    that bundles the segment's partition-typed memory back into
    //    unified, then rewire the barrier's memory input to consume
    //    from the MemUnion.
    for &(barrier, mem_input_idx, mem_input_value) in &walk.barrier_mem_edges {
        let union_node = function.create_node_attributed(
            NodeKind::MemUnion,
            [mem_input_value],
            [NodeOutputKind::Memory(None)],
            &[barrier, producer_node],
        );
        let [union_out] = function.node_outputs_exact::<1>(union_node)?;
        replace_specific_input(function, barrier, mem_input_idx, union_out)?;
    }

    Ok(SegmentOutcome {
        result: OptimizationResult::Changed,
        next_segments,
    })
}

/// Per-segment forward walk state.
#[derive(Default)]
struct SegmentWalk {
    /// `Store` / `StackStore` / `StackStorePhi` nodes whose memory
    /// outputs need retyping to `Memory(Some(stack))`.
    stack_producers: Vec<NodeId>,
    /// `MemPhi` nodes whose every predecessor resolves to stack-class
    /// — promotable.
    stack_phis: Vec<NodeId>,
    /// Stack-class `Load` nodes encountered (they don't produce
    /// memory but they're part of the segment's classification).
    stack_loads: Vec<NodeId>,
    /// `(barrier_node, input_idx, mem_input_value)` for each barrier-
    /// memory-edge we need to wrap in a `MemUnion`.
    barrier_mem_edges: Vec<(NodeId, u32, NodeOutputId)>,
    /// Barrier nodes encountered (used for recursion into their
    /// outgoing memory output).
    barriers: Vec<NodeId>,
    /// True if the walk encountered an Unknown-class consumer.
    bailed: bool,
}

impl SegmentWalk {
    /// Forward-walk every memory consumer of `seed` (and the resulting
    /// chain), collecting producers / barriers / etc.  Populates the
    /// `bailed` flag if any Unknown-class consumer is encountered.
    fn collect(
        &mut self,
        function: &Function,
        seed: NodeOutputId,
        classifier: &ChainClassifier,
    ) -> Result<()> {
        // Set of memory tokens we've already enqueued for inspection.
        // `seen` tracks every NodeOutputId we've REACHED during the
        // walk — this lets `MemPhi` promotion check whether every
        // predecessor is also in-segment.
        let mut seen: DenseEntitySet<NodeOutputId> = DenseEntitySet::new();
        let mut stack: Vec<NodeOutputId> = vec![seed];
        seen.insert(seed);
        // `pending_phis` collects MemPhis whose predecessor verdicts
        // we can't resolve at sighting time (forward walk hasn't
        // visited every pred yet).  We resolve them in a second pass
        // after the main walk converges.
        let mut pending_phis: Vec<NodeId> = Vec::new();

        while let Some(mem_out) = stack.pop() {
            // For every consumer of `mem_out`, look at the consuming
            // node's kind to decide what to do.
            let consumers: Vec<(NodeId, u32)> = function.output_uses(mem_out).collect();
            for (consumer, input_idx) in consumers {
                let consumer_kind = *function.node_kind(consumer);
                match consumer_kind {
                    NodeKind::Store(_) => {
                        let cls = classifier
                            .classes
                            .get(&consumer)
                            .copied()
                            .unwrap_or(NodeAliasClass::Unknown);
                        if matches!(cls, NodeAliasClass::Stack) {
                            self.stack_producers.push(consumer);
                            let out = function.memory_output_of(consumer)?;
                            if seen.insert(out) {
                                stack.push(out);
                            }
                        } else {
                            self.bailed = true;
                        }
                    }
                    NodeKind::Load(_) => {
                        let cls = classifier
                            .classes
                            .get(&consumer)
                            .copied()
                            .unwrap_or(NodeAliasClass::Unknown);
                        if matches!(cls, NodeAliasClass::Stack) {
                            self.stack_loads.push(consumer);
                        } else {
                            self.bailed = true;
                        }
                    }
                    NodeKind::MemPhi => {
                        // Treat the MemPhi as a chain producer and
                        // continue walking through its output — but
                        // remember to verify (after the walk
                        // converges) that EVERY predecessor input is
                        // in `seen` (a within-segment value).  If any
                        // pred is an out-of-segment value, the phi
                        // joins memory from a different region we
                        // can't reason about ⇒ bail.
                        self.stack_phis.push(consumer);
                        pending_phis.push(consumer);
                        let out = function.memory_output_of(consumer)?;
                        if seen.insert(out) {
                            stack.push(out);
                        }
                    }
                    NodeKind::Call
                    | NodeKind::Return
                    | NodeKind::IndirectBranch
                    | NodeKind::CallOther { .. } => {
                        self.barriers.push(consumer);
                        self.barrier_mem_edges.push((consumer, input_idx, mem_out));
                    }
                    // MemPartition / MemUnion shouldn't appear (idempotency
                    // guard handles pre-existing ones).  Other consumers
                    // (Region, etc.) don't consume Memory outputs by
                    // signature.
                    _ => {
                        self.bailed = true;
                    }
                }
            }
        }

        // Verify every MemPhi we provisionally promoted: its
        // predecessor inputs (input slots [1..] — slot 0 is the phi
        // token) must all be in `seen`.  An out-of-segment pred means
        // the phi joins memory we can't reason about ⇒ bail.
        if !self.bailed {
            for &phi in &pending_phis {
                let preds: Vec<NodeOutputId> = function
                    .node_inputs(phi)
                    .into_iter()
                    .skip(1)
                    .collect();
                for pred in preds {
                    if !seen.contains(pred) {
                        self.bailed = true;
                        break;
                    }
                }
                if self.bailed {
                    break;
                }
            }
        }

        // Dedup barriers (a Call can be reached twice if its memory
        // input appears via two distinct chain heads — shouldn't
        // happen in a well-formed graph but defend anyway).
        use cranelift_entity::EntityRef;
        self.barriers.sort_by_key(|n| n.index());
        self.barriers.dedup();
        self.stack_phis.sort_by_key(|n| n.index());
        self.stack_phis.dedup();

        Ok(())
    }
}

/// Returns the `Memory(_)` output slot of `node_id` if it has one
/// (post-call memory token).  `None` for `Return` and `IndirectBranch`
/// which terminate without producing memory.
fn barrier_memory_output(function: &Function, node_id: NodeId) -> Option<NodeOutputId> {
    function.memory_output_of(node_id).ok()
}

/// Rewires every consumer of `old_out` to consume `new_out`, EXCEPT
/// consumers belonging to `exempt_node` (the freshly created
/// `MemPartition` that wraps `old_out` as its input — we don't want it
/// to consume its own output).
fn rewire_consumers_except(
    function: &mut Function,
    old_out: NodeOutputId,
    new_out: NodeOutputId,
    exempt_node: NodeId,
) -> Result<()> {
    // Snapshot consumer list (replace_all_uses-style cursor invalidates
    // mid-iteration if we mutate the use-list).
    let consumers: Vec<(NodeId, u32)> = function.output_uses(old_out).collect();
    for (consumer_node, input_idx) in consumers {
        if consumer_node == exempt_node {
            continue;
        }
        replace_specific_input(function, consumer_node, input_idx, new_out)?;
    }
    Ok(())
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
