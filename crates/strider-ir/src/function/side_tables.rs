//! [`SideTables`] — the per-function overlay tables keyed by arena ids,
//! grouped so [`crate::Function::new`] defaults them in one line and
//! [`crate::Function::compact`] remaps them in one [`SideTables::remap`] call.

use cranelift_entity::SecondaryMap;
use rustc_hash::FxHashMap;

use crate::graph::{NodeIdRemap, remap_node_keyed};
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
#[derive(Default, Clone)]
pub struct SideTables {
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
    asm_fingerprints: SecondaryMap<NodeId, smallvec::SmallVec<[u64; 2]>>,
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
    ///
    /// The payload is a tracked-varnode id (`InitialVnId`), NOT a raw `Vn`: a
    /// value's source-register tag is only meaningful for a *tracked* varnode
    /// (one the function has a `VnId` for), so an untracked vn (e.g. a
    /// `CallOther` clobber register outside the tracked set) is simply not
    /// tagged. Stored as a 4-byte id, and stable across `compact` (the
    /// tracked-vn interner never renumbers).
    pub(crate) value_vn: FxHashMap<ValueId, crate::node::InitialVnId>,
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
    arg_index_to_values: FxHashMap<u32, Vec<ValueId>>,
    /// Stack slot for Store/Load nodes whose address decomposes to
    /// `base + K` for a single concrete `K`, where `base` is the SP-derived
    /// terminal node (`InitialVar(sp)` or an alignment-masked `sp & -16`).
    /// Stored as `(base, K)`: the offset `K` is only meaningful relative to
    /// its `base`, and two accesses are the same slot iff they share both.
    /// Populated by the `StackOffsetDetect` classifier.  The phi-of-offsets
    /// case (address is a phi of different constants per branch) is not
    /// recorded — consumers can re-decompose via `decompose_sp` if needed.
    stack_offsets: SecondaryMap<NodeId, Option<(ValueId, i128)>>,
    /// Per-output case addresses for a [`crate::node::NodeKind::Switch`]
    /// node: raw target addresses (no arena ids), one per switch output.
    switch_targets: SecondaryMap<NodeId, Vec<u64>>,
    /// O(1) [`crate::node::InitialVnId`] → `InitialVar(id)` node-id accelerator
    /// for indirect-resolve sites and the lifter's lazy `read_or_init_var`
    /// fallback.  Keyed by the tracked-varnode id (not the raw `rsleigh::Vn`)
    /// — the id is 4 bytes vs the varnode's 16, and every key is by
    /// construction a tracked varnode (an `InitialVar` payload).  Maintained
    /// at every canonical `InitialVar` creation site (the lift-time path and
    /// the orchestrator fallback).  The `InitialVnId` keys are stable across
    /// compaction (the tracked set doesn't change when dead nodes are culled),
    /// so [`crate::Function::compact`] remaps only the `NodeId` payload.
    ///
    /// Writers must guarantee the inserted `node_id`'s kind is
    /// `NodeKind::InitialVar(id)` for the key `id` — the index is advisory and
    /// never re-checked.
    pub(crate) initial_var_index: FxHashMap<crate::node::InitialVnId, NodeId>,
}

impl SideTables {
    // ── pure get/set accessors ────────────────────────────────────────────
    //
    // Each of these only reads or writes ONE side table with no cross-table or
    // interner resolution, so it lives here with the data.  Accessors that also
    // consult the `vn_interner` / `default_cc` (`get`/`set_vn_for_value`,
    // `get_cc`, `initial_sp`, `initial_var_value`) stay on `Function`, reached
    // through [`crate::Function::side_tables`] / `side_tables_mut`.

    /// Returns the user-op name associated with a
    /// [`crate::node::NodeKind::CallOther`] node, or `None` if no name has been
    /// recorded for that node.
    #[inline]
    pub fn call_other_name(&self, node_id: NodeId) -> Option<&str> {
        self.call_other_names[node_id].as_deref()
    }

    /// Records the user-op `name` for a [`crate::node::NodeKind::CallOther`]
    /// node.  The `CallOther` emitter is name-agnostic; the caller (the lifter,
    /// or a test) stamps the name here after building the node.
    #[inline]
    pub fn set_call_other_name(&mut self, node_id: NodeId, name: impl Into<String>) {
        self.call_other_names[node_id] = Some(name.into());
    }

    /// Records `cc` as the per-`Call` override calling convention for
    /// `node_id`.  Replaces any prior override.  Read back via
    /// [`crate::Function::get_cc`] (the Call's effective CC).
    #[inline]
    pub fn set_call_cc(&mut self, node_id: NodeId, cc: strider_target::BuiltCallingConvention) {
        self.call_cc.insert(node_id, cc);
    }

    /// All carrier output [`ValueId`]s registered for argument `index`, or
    /// `&[]` if none.  Register args have a slice of length 1; stack args may
    /// have multiple entries (different-width `Load`s at the same `sp+K`).
    #[inline]
    pub fn arg_index_to_values(&self, index: u32) -> &[ValueId] {
        self.arg_index_to_values
            .get(&index)
            .map_or(&[], Vec::as_slice)
    }

    /// Register `value` (a carrier node's single output) as a carrier for
    /// argument `index`.  Appends to the per-index `Vec`.
    #[inline]
    pub fn register_arg_value(&mut self, index: u32, value: ValueId) {
        self.arg_index_to_values
            .entry(index)
            .or_default()
            .push(value);
    }

    /// Iterate over all registered argument indices (unordered).
    #[inline]
    pub fn iter_arg_indices(&self) -> impl Iterator<Item = u32> + '_ {
        self.arg_index_to_values.keys().copied()
    }

    /// Returns the stack slot `(base, offset)` recorded for a Store/Load node,
    /// or `None`.  `base` is the SP-derived terminal node the offset is
    /// relative to; offsets compare only when their bases match.
    #[inline]
    pub fn stack_offset(&self, id: NodeId) -> Option<(ValueId, i128)> {
        self.stack_offsets[id]
    }

    /// Records a concrete stack slot `(base, offset)` for a Store/Load node.
    #[inline]
    pub fn set_stack_offset(&mut self, id: NodeId, base: ValueId, offset: i128) {
        self.stack_offsets[id] = Some((base, offset));
    }

    /// Returns the per-output case addresses recorded for a
    /// [`crate::node::NodeKind::Switch`] node, or `&[]` if none.
    #[inline]
    pub fn switch_targets(&self, id: NodeId) -> &[u64] {
        self.switch_targets[id].as_slice()
    }

    /// Records the per-output case addresses for a
    /// [`crate::node::NodeKind::Switch`] node.
    #[inline]
    pub fn set_switch_targets(&mut self, id: NodeId, targets: Vec<u64>) {
        self.switch_targets[id] = targets;
    }

    /// Returns the asm-instruction-address fingerprint of `id` as a
    /// sorted-deduplicated slice (empty when nothing was recorded).
    #[inline]
    pub fn asm_fingerprint(&self, id: NodeId) -> &[u64] {
        self.asm_fingerprints[id].as_slice()
    }

    /// Unions `contributors` into `node_id`'s fingerprint, kept sorted and
    /// deduplicated.  Existing entries are never removed (the no-shrink
    /// contract).  Empty `contributors` is a no-op.
    pub fn extend_asm_fingerprint(&mut self, node_id: NodeId, contributors: &[u64]) {
        if contributors.is_empty() {
            return;
        }
        // Both existing and `contributors` are tiny (a handful of addresses),
        // so extend + sort + dedup is the right wheel.
        let fp = &mut self.asm_fingerprints[node_id];
        fp.extend_from_slice(contributors);
        fp.sort_unstable();
        fp.dedup();
    }

    /// Unions the fingerprint of `src` into `dst`.  Self-extension
    /// (`src == dst`) is a no-op.
    pub fn extend_asm_fingerprint_from(&mut self, dst: NodeId, src: NodeId) {
        if dst == src {
            return;
        }
        let src_slice: smallvec::SmallVec<[u64; 4]> =
            self.asm_fingerprints[src].iter().copied().collect();
        self.extend_asm_fingerprint(dst, &src_slice);
    }

    /// Remaps every arena-id key / value through `remap` after a
    /// `retain_reachable` compaction; an entry whose node or value did not
    /// survive is dropped.  Called once by [`crate::Function::compact`].
    pub(crate) fn remap(&mut self, remap: &NodeIdRemap) {
        // NodeId-keyed tables: translate the key, drop pruned nodes.
        remap_node_keyed(&mut self.call_other_names, remap);
        remap_node_keyed(&mut self.asm_fingerprints, remap);
        remap_node_keyed(&mut self.switch_targets, remap);
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
        // The `InitialVnId` payload is stable across compaction, so it passes
        // through unchanged.
        self.value_vn = remap_hashmap(&mut self.value_vn, |old_value, vn_id| {
            remap
                .value_old_to_new(old_value)
                .map(|new_value| (new_value, vn_id))
        });
        // `initial_var_index`: `InitialVnId`-keyed with a NodeId payload. The
        // `InitialVnId` keys are stable across compaction (the tracked-vn set
        // is unchanged), so only the NodeId payload is remapped; a key whose
        // node did not survive is dropped.
        self.initial_var_index = remap_hashmap(&mut self.initial_var_index, |vn_id, old_id| {
            remap.node_old_to_new(old_id).map(|new_id| (vn_id, new_id))
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
