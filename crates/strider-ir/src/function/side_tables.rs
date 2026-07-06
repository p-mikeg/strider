//! [`SideTables`] — the per-function overlay tables keyed by arena ids,
//! grouped so [`crate::Function::new`] defaults them in one line and
//! [`crate::Function::compact`] remaps them in one `SideTables::remap` call.

use cranelift_entity::{entity_impl, SecondaryMap};
use entity_utils::{EntityInterner, UnionDag};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::graph::NodeIdRemap;
use crate::node::{NodeId, ValueId};

/// Interner key for a distinct stack-pointer decomposition `(base, offset)`.
///
/// The per-value [`SpDecomp`] slots store this small id instead of the
/// `(ValueId, i128)` payload inline, so the dense `stack_offsets`
/// [`SecondaryMap`] stays narrow (an id + tag) while the handful of genuinely
/// distinct SP terminals live once in [`SideTables::stack_interner`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct StackId(u32);
entity_impl!(StackId);

/// Per-value stack-pointer decomposition verdict — the cached result of the
/// optimizer's `decompose` over a value's address cone.
///
/// `Unknown` (the [`SecondaryMap`] default) means "not yet decomposed"; the two
/// resolved states distinguish "provably not SP-rooted" from "SP-rooted at an
/// interned `(base, offset)`".  Keeping all three is what lets the memo cache a
/// negative result — a non-SP value is classified once, not re-walked on every
/// query.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SpDecomp {
    /// Not yet computed (the map default).
    #[default]
    Unknown,
    /// Computed: the value is provably NOT an SP-rooted terminal.
    NotStack,
    /// Computed: the value decomposes to the interned `(base, offset)`.
    Stack(StackId),
}

/// Drains an `FxHashMap` and rebuilds it through a per-entry translation,
/// keeping only entries `f` maps to `Some((new_key, new_payload))`.
///
/// Every `NodeId`- / `ValueId`- / Vn- / index-keyed overlay map in
/// [`crate::Function::compact`] remaps a different facet (the key, the payload,
/// or both), so this folds their shared drain-rebuild loop behind a single
/// closure.
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
/// single `SideTables::remap` call; each is still surfaced through its own
/// typed accessor on [`crate::Function`].  All entries are remapped (or dropped) when
/// the arena is compacted.
#[derive(Default, Clone)]
pub struct SideTables {
    /// User-op name resolved from Sleigh for [`crate::node::NodeKind::CallOther`]
    /// nodes.
    // Sparse: only CallOther nodes carry a name.
    pub(crate) call_other_names: FxHashMap<NodeId, String>,
    /// Per-node set of machine-instruction addresses whose lifting or rewrite
    /// contributed to the node's value.
    // A deferred-union DAG so `extend_asm_fingerprint_from` (the hot path — a
    // rewrite absorbing its matched interior's proof) is O(1): it links the two
    // nodes instead of copying/merging address lists.  A node's full set is
    // materialised only on the rare read (`asm_fingerprint`) by walking its
    // ancestors.  The union count runs into the millions on large functions;
    // the old sorted-merge storage made each absorb O(N).
    asm_fingerprints: UnionDag<NodeId, u64>,
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
    /// The optimizer's stack-pointer decomposition memo, keyed by the *value*
    /// whose address cone was analysed: `value → SpDecomp`, where a
    /// [`SpDecomp::Stack`] resolves through [`Self::stack_interner`] to a
    /// `(base, offset)` — `base` is the SP-derived terminal (`InitialVar(sp)`
    /// or an alignment-masked `sp & -16`) and the offset is only meaningful
    /// relative to it (two accesses are the same slot iff they share both).
    ///
    /// This is the single home for SP-decomposition results: the optimizer's
    /// `decompose` writes it (caching negatives too), and the user-facing
    /// per-node accessor [`crate::Function::stack_offset`] derives a
    /// Store/Load's slot by looking up its address value here.  Volatile during
    /// the optimizer's fixed point (cleared on graph mutation) and stably
    /// (re)filled once the graph is frozen.
    stack_offsets: SecondaryMap<ValueId, SpDecomp>,
    /// Deduplicates the `(base, offset)` payloads that [`SpDecomp::Stack`] slots
    /// reference, so the dense `stack_offsets` map stores only a [`StackId`].
    stack_interner: EntityInterner<StackId, (ValueId, i128)>,
    /// Per-output case addresses for a [`crate::node::NodeKind::Switch`]
    /// node: raw target addresses (no arena ids), one per switch output.
    // Sparse: only Switch nodes carry targets — see `stack_offsets`.
    switch_targets: FxHashMap<NodeId, Vec<u64>>,
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
        self.call_other_names.get(&node_id).map(String::as_str)
    }

    /// Records the user-op `name` for a [`crate::node::NodeKind::CallOther`]
    /// node.  The `CallOther` emitter is name-agnostic; the caller (the lifter,
    /// or a test) stamps the name here after building the node.
    #[inline]
    pub fn set_call_other_name(&mut self, node_id: NodeId, name: impl Into<String>) {
        self.call_other_names.insert(node_id, name.into());
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

    /// The raw decomposition verdict cached for `value` (`Unknown` if the value
    /// has not been decomposed).
    #[inline]
    pub fn stack_slot(&self, value: ValueId) -> SpDecomp {
        self.stack_offsets[value]
    }

    /// The resolved stack slot `(base, offset)` for `value`, or `None` when it
    /// is unknown or provably not SP-rooted.  `base` is the SP-derived terminal
    /// the offset is relative to; offsets compare only when their bases match.
    #[inline]
    pub fn stack_slot_resolved(&self, value: ValueId) -> Option<(ValueId, i128)> {
        match self.stack_offsets[value] {
            SpDecomp::Stack(id) => self.stack_interner.get(id).copied(),
            SpDecomp::Unknown | SpDecomp::NotStack => None,
        }
    }

    /// Caches that `value` decomposes to the SP terminal `base` plus `offset`.
    #[inline]
    pub fn set_stack_slot(&mut self, value: ValueId, base: ValueId, offset: i128) {
        let id = self.stack_interner.intern((base, offset));
        self.stack_offsets[value] = SpDecomp::Stack(id);
    }

    /// Caches that `value` is provably NOT SP-rooted (a negative memo entry).
    #[inline]
    pub fn set_stack_slot_not(&mut self, value: ValueId) {
        self.stack_offsets[value] = SpDecomp::NotStack;
    }

    /// Returns the per-output case addresses recorded for a
    /// [`crate::node::NodeKind::Switch`] node, or `&[]` if none.
    #[inline]
    pub fn switch_targets(&self, id: NodeId) -> &[u64] {
        self.switch_targets.get(&id).map_or(&[], Vec::as_slice)
    }

    /// Records the per-output case addresses for a
    /// [`crate::node::NodeKind::Switch`] node.
    #[inline]
    pub fn set_switch_targets(&mut self, id: NodeId, targets: Vec<u64>) {
        self.switch_targets.insert(id, targets);
    }

    /// Returns the asm-instruction-address fingerprint of `id` as an unordered
    /// set (empty when nothing was recorded).  Materialised on demand by
    /// walking the deferred-union DAG; callers that need a stable order sort
    /// the result themselves.
    pub fn asm_fingerprint(&self, id: NodeId) -> FxHashSet<u64> {
        let mut set = FxHashSet::default();
        self.asm_fingerprints.for_each(id, |addr| {
            set.insert(addr);
        });
        set
    }

    /// Whether `id` has no recorded fingerprint.  O(1) — no materialisation.
    #[inline]
    pub fn asm_fingerprint_is_empty(&self, id: NodeId) -> bool {
        self.asm_fingerprints.is_empty(id)
    }

    /// Unions `contributors` into `node_id`'s fingerprint.  Existing entries
    /// are never removed (the no-shrink contract).  Empty `contributors` is a
    /// no-op.
    pub fn extend_asm_fingerprint(&mut self, node_id: NodeId, contributors: &[u64]) {
        for &addr in contributors {
            self.asm_fingerprints.extend(node_id, addr);
        }
    }

    /// Unions the fingerprint of `src` into `dst` in O(1) (a DAG link, no
    /// copy).  Self-extension (`src == dst`) is a no-op.
    pub fn extend_asm_fingerprint_from(&mut self, dst: NodeId, src: NodeId) {
        if dst == src {
            return;
        }
        self.asm_fingerprints.union(dst, src);
    }

    /// Remaps every arena-id key / value through `remap` after a
    /// `retain_reachable` compaction; an entry whose node or value did not
    /// survive is dropped.  Called once by [`crate::Function::compact`].
    pub(crate) fn remap(&mut self, remap: &NodeIdRemap) {
        // NodeId-keyed sparse maps: translate the key, drop pruned nodes.
        self.asm_fingerprints
            .remap(|old| remap.node_old_to_new(old));
        self.call_other_names = remap_hashmap(&mut self.call_other_names, |old, name| {
            remap.node_old_to_new(old).map(|n| (n, name))
        });
        self.switch_targets = remap_hashmap(&mut self.switch_targets, |old, targets| {
            remap.node_old_to_new(old).map(|n| (n, targets))
        });
        self.call_cc = remap_hashmap(&mut self.call_cc, |old, cc| {
            remap.node_old_to_new(old).map(|n| (n, cc))
        });
        // `stack_offsets`: ValueId-keyed, and each `Stack` slot references an
        // interned `(base, offset)` whose `base` is also a ValueId.  Rebuild the
        // interner (remapping every base; dropping a slot whose base or key did
        // not survive) and the dense map together, translating each surviving
        // key.
        let mut new_interner: EntityInterner<StackId, (ValueId, i128)> = EntityInterner::new();
        let mut new_slots: SecondaryMap<ValueId, SpDecomp> = SecondaryMap::new();
        for (old_value, slot) in self.stack_offsets.iter() {
            let Some(new_value) = remap.value_old_to_new(old_value) else {
                continue;
            };
            let new_slot = match *slot {
                SpDecomp::Unknown => continue,
                SpDecomp::NotStack => SpDecomp::NotStack,
                SpDecomp::Stack(id) => {
                    let Some(&(old_base, off)) = self.stack_interner.get(id) else {
                        continue;
                    };
                    match remap.value_old_to_new(old_base) {
                        Some(new_base) => SpDecomp::Stack(new_interner.intern((new_base, off))),
                        None => continue,
                    }
                }
            };
            new_slots[new_value] = new_slot;
        }
        self.stack_offsets = new_slots;
        self.stack_interner = new_interner;
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

#[cfg(test)]
mod tests {
    use super::*;
    use cranelift_entity::EntityRef;

    // The union accumulates each node's fingerprint as a deduplicated set
    // regardless of incoming order, folding new contributors into a large
    // existing set — never dropping an entry.
    #[test]
    fn extend_asm_fingerprint_unions_deduped() {
        let mut st = SideTables::default();
        let n = NodeId::new(3);

        // Empty → seed; contributors may arrive unsorted.
        st.extend_asm_fingerprint(n, &[40, 10, 30, 20, 50]);
        assert_eq!(st.asm_fingerprint(n), FxHashSet::from_iter([10, 20, 30, 40, 50]));

        // Merge into the (now non-empty) fp: unsorted, one duplicate (20), the
        // rest new — result is the full union (no shrink).
        st.extend_asm_fingerprint(n, &[35, 20, 5, 45]);
        assert_eq!(
            st.asm_fingerprint(n),
            FxHashSet::from_iter([5, 10, 20, 30, 35, 40, 45, 50])
        );

        // Merging a subset changes nothing (pure union, idempotent).
        st.extend_asm_fingerprint(n, &[10, 50]);
        assert_eq!(
            st.asm_fingerprint(n),
            FxHashSet::from_iter([5, 10, 20, 30, 35, 40, 45, 50])
        );

        // Empty contributors is a no-op.
        st.extend_asm_fingerprint(n, &[]);
        assert_eq!(
            st.asm_fingerprint(n),
            FxHashSet::from_iter([5, 10, 20, 30, 35, 40, 45, 50])
        );
    }
}

