//! Per-function overlay tables keyed by arena ids, grouped so
//! [`crate::Function::new`] defaults them in one line and
//! [`crate::Function::compact`] remaps them in one `SideTables::remap` call.

use std::cell::RefCell;

use cranelift_entity::{SecondaryMap, entity_impl};
use entity_utils::{EntityInterner, UnionDag};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::graph::NodeIdRemap;
use crate::node::{NodeId, ValueId};

/// Interner key for a distinct stack-pointer decomposition `(base, offset)`.
///
/// [`SpDecomp`] stores this id rather than the payload inline so the dense
/// `stack_offsets` map stays narrow (an id + tag).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct StackId(u32);
entity_impl!(StackId);

/// Cached result of the optimizer's SP decomposition over a value's address
/// cone.  The third state is what lets the memo cache a NEGATIVE: a non-SP
/// value is classified once, not re-walked on every query.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SpDecomp {
    /// Not yet computed (the map default).
    #[default]
    Unknown,
    NotStack,
    Stack(StackId),
}

/// Drains an `FxHashMap` and rebuilds it through a per-entry translation,
/// keeping only entries `f` maps to `Some((new_key, new_payload))`.
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

/// Per-function data keyed by arena ids (or the CC arg index) that is not part
/// of the graph's structural identity.  Every entry is remapped, or dropped,
/// when the arena is compacted.
#[derive(Default, Clone)]
pub struct SideTables {
    // Sparse: only CallOther nodes carry a name.
    pub(crate) call_other_names: FxHashMap<NodeId, String>,
    /// Machine-instruction addresses whose lifting or rewrite contributed to
    /// each node's value.
    // A deferred-union DAG so `extend_asm_fingerprint_from` (the hot path: a
    // rewrite absorbing its matched interior's proof) is O(1), linking the two
    // nodes instead of merging address lists.  A node's set is materialised
    // only on the rare read, by walking its ancestors.  Unions run into the
    // millions on large functions, so the old sorted-merge storage made each
    // absorb O(N).
    asm_fingerprints: UnionDag<NodeId, u64>,
    /// The varnode a value represents.  Two disjoint populations share the map,
    /// distinguished only by the producer's node kind:
    ///
    /// * A lift-time `Phi`'s output: the source-level varnode it tracks.  No
    ///   entry means an anonymous phi synthesised by an opt pass.
    /// * A `Call` / `CallOther` clobber output: the clobbered register.  Set
    ///   for every clobber output at build time, so recovery is one lookup
    ///   with no slot arithmetic.
    ///
    /// Keyed by `ValueId` so it remaps through `compact`'s value translation.
    /// The payload is an `InitialVnId`, not a raw `Vn`: a source-register tag
    /// is only meaningful for a TRACKED varnode, so an untracked one (e.g. a
    /// `CallOther` clobber outside the tracked set) is left untagged.  The id
    /// is stable across `compact` (the tracked-vn interner never renumbers).
    pub(crate) value_vn: FxHashMap<ValueId, crate::node::InitialVnId>,
    /// Per-`Call` override calling convention.  Sparse: a Call on the
    /// function-default CC has no entry.  Read through
    /// [`crate::Function::get_cc`], which falls back to the default.
    pub(crate) call_cc: FxHashMap<NodeId, strider_target::BuiltCallingConvention>,
    /// CC argument index to the carrier nodes' output values (`InitialVar` for
    /// register args, `Load` for stack args).  Each carrier has a single
    /// output, so the node is recoverable losslessly via `producer`.
    ///
    /// A `Vec` per index because one stack slot may be read by several `Load`s
    /// at the same `sp+K` with different widths; register args have exactly one.
    arg_index_to_values: FxHashMap<u32, Vec<ValueId>>,
    /// SP-decomposition memo keyed by the value whose address cone was
    /// analysed.  A [`SpDecomp::Stack`] resolves through `stack_interner` to
    /// `(base, offset)`, where `base` is the SP-derived terminal (`InitialVar(sp)`
    /// or an alignment-masked `sp & -16`).  The offset is meaningful only
    /// relative to that base: two accesses are the same slot iff they share both.
    ///
    /// Volatile during the optimizer's fixed point (cleared on graph mutation
    /// via [`Self::clear_stack_slots`]) and stably refilled once frozen.
    ///
    /// `RefCell` so the read-only decomposer, whose callers hold `&Function`,
    /// can memoize on a miss.  A pure cache: nothing observable changes but
    /// query latency, so the IR is not mutable through `&`.
    stack_offsets: RefCell<SecondaryMap<ValueId, SpDecomp>>,
    stack_interner: RefCell<EntityInterner<StackId, (ValueId, i128)>>,
    /// Per-output case addresses for a `Switch`: raw target addresses, no arena
    /// ids.
    // Sparse: only Switch nodes carry targets.
    switch_targets: FxHashMap<NodeId, Vec<u64>>,
    /// O(1) `InitialVnId` to `InitialVar(id)` node accelerator for
    /// indirect-resolve sites and the lifter's lazy `read_or_init_var`
    /// fallback.  Keys are stable across compaction (culling dead nodes does
    /// not change the tracked set), so `compact` remaps only the `NodeId`
    /// payload.
    ///
    /// Writers MUST guarantee the inserted node's kind is `InitialVar(id)` for
    /// the key `id`; the index is advisory and never re-checked.
    pub(crate) initial_var_index: FxHashMap<crate::node::InitialVnId, NodeId>,
}

impl SideTables {
    // Accessors here touch ONE table with no cross-table or interner
    // resolution.  Ones that also consult the `vn_interner` / `default_cc`
    // (`get`/`set_vn_for_value`, `get_cc`, `initial_sp`, `initial_var_value`)
    // stay on `Function`.

    #[inline]
    pub fn call_other_name(&self, node_id: NodeId) -> Option<&str> {
        self.call_other_names.get(&node_id).map(String::as_str)
    }

    /// The `CallOther` emitter is name-agnostic; the caller stamps the name
    /// here after building the node.
    #[inline]
    pub fn set_call_other_name(&mut self, node_id: NodeId, name: impl Into<String>) {
        self.call_other_names.insert(node_id, name.into());
    }

    /// Replaces any prior override.  Read back via [`crate::Function::get_cc`].
    #[inline]
    pub fn set_call_cc(&mut self, node_id: NodeId, cc: strider_target::BuiltCallingConvention) {
        self.call_cc.insert(node_id, cc);
    }

    /// Register args yield a slice of length 1; stack args may have several
    /// entries (different-width `Load`s at the same `sp+K`).
    #[inline]
    pub fn arg_index_to_values(&self, index: u32) -> &[ValueId] {
        self.arg_index_to_values
            .get(&index)
            .map_or(&[], Vec::as_slice)
    }

    /// `value` is a carrier node's single output.  Appends to the per-index `Vec`.
    #[inline]
    pub fn register_arg_value(&mut self, index: u32, value: ValueId) {
        self.arg_index_to_values
            .entry(index)
            .or_default()
            .push(value);
    }

    /// Unordered.
    #[inline]
    pub fn iter_arg_indices(&self) -> impl Iterator<Item = u32> + '_ {
        self.arg_index_to_values.keys().copied()
    }

    #[inline]
    pub fn stack_slot(&self, value: ValueId) -> SpDecomp {
        self.stack_offsets.borrow()[value]
    }

    /// `None` when unknown or provably not SP-rooted.  Offsets compare only
    /// when their bases match.
    #[inline]
    pub fn stack_slot_resolved(&self, value: ValueId) -> Option<(ValueId, i128)> {
        match self.stack_offsets.borrow()[value] {
            SpDecomp::Stack(id) => self.stack_interner.borrow().get(id).copied(),
            SpDecomp::Unknown | SpDecomp::NotStack => None,
        }
    }

    /// Takes `&self` (interior mutability) so the read-only decomposer can
    /// memoize through a shared `&Function`.
    #[inline]
    pub fn set_stack_slot(&self, value: ValueId, base: ValueId, offset: i128) {
        let id = self.stack_interner.borrow_mut().intern((base, offset));
        self.stack_offsets.borrow_mut()[value] = SpDecomp::Stack(id);
    }

    /// Negative memo entry: `value` is provably not SP-rooted.
    #[inline]
    pub fn set_stack_slot_not(&self, value: ValueId) {
        self.stack_offsets.borrow_mut()[value] = SpDecomp::NotStack;
    }

    /// The optimizer calls this after a graph mutation so a memoized verdict
    /// never outlives the graph it was computed against.  The interner resets
    /// too; its ids are referenced only by the now-cleared slots.
    #[inline]
    pub fn clear_stack_slots(&self) {
        self.stack_offsets.borrow_mut().clear();
        *self.stack_interner.borrow_mut() = EntityInterner::new();
    }

    #[inline]
    pub fn switch_targets(&self, id: NodeId) -> &[u64] {
        self.switch_targets.get(&id).map_or(&[], Vec::as_slice)
    }

    #[inline]
    pub fn set_switch_targets(&mut self, id: NodeId, targets: Vec<u64>) {
        self.switch_targets.insert(id, targets);
    }

    /// Unordered, and materialised on demand by walking the union DAG; callers
    /// needing a stable order sort the result themselves.
    pub fn asm_fingerprint(&self, id: NodeId) -> FxHashSet<u64> {
        let mut set = FxHashSet::default();
        self.asm_fingerprints.for_each(id, |addr| {
            set.insert(addr);
        });
        set
    }

    /// O(1); no materialisation.
    #[inline]
    pub fn asm_fingerprint_is_empty(&self, id: NodeId) -> bool {
        self.asm_fingerprints.is_empty(id)
    }

    /// Unions `contributors` in.  Existing entries are never removed: the
    /// fingerprint contract is superset-only.
    pub fn extend_asm_fingerprint(&mut self, node_id: NodeId, contributors: &[u64]) {
        for &addr in contributors {
            self.asm_fingerprints.extend(node_id, addr);
        }
    }

    /// O(1): a DAG link, no copy.
    pub fn extend_asm_fingerprint_from(&mut self, dst: NodeId, src: NodeId) {
        if dst == src {
            return;
        }
        self.asm_fingerprints.union(dst, src);
    }

    /// Remaps every arena-id key / value after a `retain_reachable`
    /// compaction; an entry whose node or value did not survive is dropped.
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
        // `stack_offsets` is ValueId-keyed AND each `Stack` slot references an
        // interned `(base, offset)` whose base is also a ValueId, so both
        // coordinates need translating.  Rebuild the interner and the dense map
        // together, dropping a slot whose key or base did not survive.
        let (new_slots, new_interner) = {
            let old_slots = self.stack_offsets.borrow();
            let old_interner = self.stack_interner.borrow();
            let mut new_interner: EntityInterner<StackId, (ValueId, i128)> = EntityInterner::new();
            let mut new_slots: SecondaryMap<ValueId, SpDecomp> = SecondaryMap::new();
            for (old_value, slot) in old_slots.iter() {
                let Some(new_value) = remap.value_old_to_new(old_value) else {
                    continue;
                };
                let new_slot = match *slot {
                    SpDecomp::Unknown => continue,
                    SpDecomp::NotStack => SpDecomp::NotStack,
                    SpDecomp::Stack(id) => {
                        let Some(&(old_base, off)) = old_interner.get(id) else {
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
            (new_slots, new_interner)
        };
        *self.stack_offsets.get_mut() = new_slots;
        *self.stack_interner.get_mut() = new_interner;
        // `value_vn`: translate keys only; the `InitialVnId` payload is stable
        // across compaction.
        self.value_vn = remap_hashmap(&mut self.value_vn, |old_value, vn_id| {
            remap
                .value_old_to_new(old_value)
                .map(|new_value| (new_value, vn_id))
        });
        // `initial_var_index`: keys stable (the tracked-vn set is unchanged), so
        // remap only the NodeId payload; drop keys whose node died.
        self.initial_var_index = remap_hashmap(&mut self.initial_var_index, |vn_id, old_id| {
            remap.node_old_to_new(old_id).map(|new_id| (vn_id, new_id))
        });
        // `arg_index_to_values`: filter-map the carriers, dropping an index
        // whose carriers all vanish.
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

    // The union is order-independent, deduplicating and never dropping an entry.
    #[test]
    fn extend_asm_fingerprint_unions_deduped() {
        let mut st = SideTables::default();
        let n = NodeId::new(3);

        // Seed from empty; contributors may arrive unsorted.
        st.extend_asm_fingerprint(n, &[40, 10, 30, 20, 50]);
        assert_eq!(
            st.asm_fingerprint(n),
            FxHashSet::from_iter([10, 20, 30, 40, 50])
        );

        // Merge into a non-empty fp: unsorted, one duplicate (20), rest new.
        st.extend_asm_fingerprint(n, &[35, 20, 5, 45]);
        assert_eq!(
            st.asm_fingerprint(n),
            FxHashSet::from_iter([5, 10, 20, 30, 35, 40, 45, 50])
        );

        // Idempotent: merging a subset changes nothing.
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
