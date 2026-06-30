//! [`SideTables`] — the per-function overlay tables keyed by arena ids,
//! grouped so [`crate::Function::new`] defaults them in one line and
//! [`crate::Function::compact`] remaps them in one [`SideTables::remap`] call.

use cranelift_entity::SecondaryMap;
use rustc_hash::FxHashMap;

use crate::graph::{remap_node_keyed, NodeIdRemap};
use crate::node::{NodeId, ValueId};

/// Drains an `FxHashMap` and rebuilds it through a per-entry translation,
/// keeping only entries `f` maps to `Some((new_key, new_payload))`.
///
/// The Vn-keyed / `ValueId`-keyed / index-keyed overlay maps in
/// [`crate::Function::compact`] each remap a different facet (the key, the payload,
/// or a payload `Vec`), so they don't fit the `NodeId`-keyed
/// [`remap_node_keyed`] shape — this folds their shared drain-rebuild loop
/// behind a single closure.
fn remap_hashmap<K, V, NK, NV>(
    map: &mut FxHashMap<K, V>,
    mut f: impl FnMut(K, V) -> Option<(NK, NV)>,
) -> FxHashMap<NK, NV>
where
    NK: std::hash::Hash + Eq,
{
    let mut dst = FxHashMap::with_capacity_and_hasher(map.len(), Default::default());
    for (old_key, old_payload) in map.drain() {
        if let Some((new_key, new_payload)) = f(old_key, old_payload) {
            dst.insert(new_key, new_payload);
        }
    }
    dst
}

/// Per-function overlay tables keyed by arena ids (`NodeId`, or a node's output
/// `ValueId`) or the CC arg index — data attached to the graph but not part of
/// its structural identity.  Grouped into one struct so [`crate::Function::new`]
/// defaults them in a single line and [`crate::Function::compact`] remaps them in a
/// single [`SideTables::remap`] call; each is still surfaced through its own
/// typed accessor on [`crate::Function`].  All entries are remapped (or dropped) when
/// the arena is compacted.
#[derive(Default)]
pub(crate) struct SideTables {
    /// User-op name resolved from Sleigh for [`crate::node::NodeKind::CallOther`]
    /// nodes.
    pub(crate) call_other_names: SecondaryMap<NodeId, Option<String>>,
    /// Per-node sorted-deduplicated list of machine-instruction addresses
    /// whose lifting or rewrite contributed to the node's value.
    // `SmallVec<[u64; 2]>` because the common case is 1–2 contributor
    // addresses per node.  Inlining those avoids a heap allocation per
    // non-empty entry — on graphs with thousands of nodes this drops
    // thousands of small allocations from the lift+optimize pipeline.
    // The mutation API (`extend_asm_fingerprint`,
    // `extend_asm_fingerprint_from`) keeps using `&[u64]` /
    // `impl IntoIterator<Item = u64>` so callers are unaffected.
    pub(crate) asm_fingerprints: SecondaryMap<NodeId, smallvec::SmallVec<[u64; 2]>>,
    /// The varnode a value *represents*, keyed by [`ValueId`].  Two
    /// disjoint populations share this one map:
    ///
    /// * A lift-time [`crate::node::NodeKind::Phi`]'s single output value →
    ///   the source-level varnode the phi tracks.  Absent entries mark
    ///   anonymous phis synthesised by opt passes (and every non-phi,
    ///   non-clobber value).
    /// * A [`crate::node::NodeKind::Call`] / [`crate::node::NodeKind::CallOther`]
    ///   clobber output value → the register that call clobbers.  Set for
    ///   every clobber output at build time (both the function-default and
    ///   the override / implicit-write paths), so a clobber output's
    ///   varnode is recovered with a single lookup, no slot arithmetic.
    ///
    /// Keyed by `ValueId` (not `NodeId`) so it remaps through the
    /// `ValueId` translation that [`crate::Function::compact`] applies.
    pub(crate) value_vn: FxHashMap<ValueId, rsleigh::Vn>,
    /// Per-[`crate::node::NodeKind::Call`] override calling convention, recorded
    /// at build time for a Call built with a per-address CC override.  Sparse:
    /// a default Call (function-default CC) has no entry.  Read through
    /// [`crate::Function::get_cc`] (the Call's effective CC — the override here
    /// if present, else the function default — from which its stack-arg offsets
    /// derive).
    pub(crate) call_cc: FxHashMap<NodeId, strider_target::BuiltCallingConvention>,
    /// Maps each calling-convention argument index to the [`ValueId`](s) of
    /// the underlying carrier nodes' outputs:
    /// [`crate::node::NodeKind::InitialVar`] for register args,
    /// [`crate::node::NodeKind::Load`] for stack args.  Each carrier node has
    /// a single output, so the carrier node is recoverable losslessly via
    /// [`crate::graph::Graph::producer`].
    ///
    /// `Vec<ValueId>` per index because a stack slot may have multiple `Load`
    /// nodes at the same `sp+K` offset but different widths.  Register args
    /// have a `Vec` of size 1.
    ///
    /// Populated by `FunctionArgDetect`; empty until that pass runs.
    pub(crate) arg_index_to_values: FxHashMap<u32, Vec<ValueId>>,
    /// Stack slot for Store/Load nodes whose address decomposes to
    /// `base + K` for a single concrete `K`, where `base` is the SP-derived
    /// terminal node (`InitialVar(sp)` or an alignment-masked `sp & -16`).
    /// Stored as `(base, K)`: the offset `K` is only meaningful relative to
    /// its `base`, and two accesses are the same slot iff they share both.
    /// Populated by the `StackOffsetDetect` classifier.  The phi-of-offsets
    /// case (address is a phi of different constants per branch) is not
    /// recorded — consumers can re-decompose via `decompose_sp` if needed.
    pub(crate) stack_offsets: SecondaryMap<NodeId, Option<(ValueId, i128)>>,
    /// O(1) varnode → `InitialVar(vn)` node-id accelerator for
    /// indirect-resolve sites and the lifter's lazy `read_or_init_var`
    /// fallback.  Maintained at every canonical `InitialVar`
    /// creation site (the lift-time path and the orchestrator
    /// fallback) and remapped through [`NodeIdRemap`] by
    /// [`crate::Function::compact`].
    ///
    /// Writers must guarantee the inserted `node_id`'s kind is
    /// `NodeKind::InitialVar(vn)` for the key `vn` — the index is advisory and
    /// never re-checked.
    pub(crate) initial_var_index: FxHashMap<rsleigh::Vn, NodeId>,
}

impl SideTables {
    /// Remaps every arena-id key / value through `remap` after a
    /// `retain_reachable` compaction; an entry whose node or value did not
    /// survive is dropped.  Called once by [`crate::Function::compact`].
    pub(crate) fn remap(&mut self, remap: &NodeIdRemap) {
        // NodeId-keyed tables: translate the key, drop pruned nodes.
        remap_node_keyed(&mut self.call_other_names, remap);
        remap_node_keyed(&mut self.asm_fingerprints, remap);
        self.call_cc = remap_hashmap(&mut self.call_cc, |old, cc| {
            remap.node_old_to_new(old).map(|n| (n, cc))
        });
        // `stack_offsets`: the only NodeId-keyed table whose VALUE also
        // references a node (the slot `base`, a `ValueId`); remap both.
        let mut new_stack_offsets: SecondaryMap<NodeId, Option<(ValueId, i128)>> =
            SecondaryMap::new();
        for (old_id, slot) in self.stack_offsets.iter() {
            let Some((old_base, off)) = *slot else {
                continue;
            };
            if let (Some(new_id), Some(new_base)) = (
                remap.node_old_to_new(old_id),
                remap.value_old_to_new(old_base),
            ) {
                new_stack_offsets[new_id] = Some((new_base, off));
            }
        }
        self.stack_offsets = new_stack_offsets;
        // `value_vn`: ValueId-keyed (a phi / clobber output); translate keys.
        self.value_vn = remap_hashmap(&mut self.value_vn, |old_value, vn| {
            remap
                .value_old_to_new(old_value)
                .map(|new_value| (new_value, vn))
        });
        // `initial_var_index`: Vn-keyed with a NodeId payload; remap the value.
        self.initial_var_index = remap_hashmap(&mut self.initial_var_index, |vn, old_id| {
            remap.node_old_to_new(old_id).map(|new_id| (vn, new_id))
        });
        // `arg_index_to_values`: index-keyed with a `Vec<ValueId>` payload;
        // filter-map the carriers, dropping an index whose carriers all vanish.
        self.arg_index_to_values =
            remap_hashmap(&mut self.arg_index_to_values, |index, old_values| {
                let mapped: Vec<ValueId> = old_values
                    .into_iter()
                    .filter_map(|old_value| remap.value_old_to_new(old_value))
                    .collect();
                (!mapped.is_empty()).then_some((index, mapped))
            });
    }
}
