use std::cell::{Cell, RefCell};

use cranelift_entity::{SecondaryMap, entity_impl};
use entity_utils::{EntityInterner, UnionDag};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::graph::NodeIdRemap;
use crate::node::{NodeId, ValueId};

/// Interner key for a distinct stack-pointer decomposition `(base, offset)`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct MemoryId(u32);
entity_impl!(MemoryId);

/// A value's address cone reduced to a `base + offset` terminal. `Stack` and
/// `Heap` both resolve through `memory_interner` to `(base, offset)`; the
/// variant is the region.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MemDecomp {
    /// Not yet computed (the map default).
    #[default]
    Unknown,
    /// Provably neither stack- nor heap-rooted.
    NotMemory,
    /// Rooted at the entry SP (or an alignment-masked SP anchor).
    Stack(MemoryId),
    /// Rooted at a pure allocator's fresh return pointer.
    Heap(MemoryId),
}

/// The ABI class a function argument was passed in; stack arguments continue
/// the `Integer` index space after the register ones. Indices are positional
/// within a class, gaps preserved: an unregistered carrier leaves its index
/// empty rather than shifting the later ones down.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum ArgClass {
    Integer,
    Float,
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
/// of the graph's structural identity.
#[derive(Default, Clone)]
pub struct SideTables {
    pub(crate) call_other_names: FxHashMap<NodeId, String>,
    /// Machine-instruction addresses whose lifting or rewrite contributed to
    /// each node's value.
    asm_fingerprints: UnionDag<NodeId, u64>,
    /// The tracked varnode a value represents, at most one per value.
    pub(crate) value_vn: FxHashMap<ValueId, crate::node::InitialVnId>,
    /// Per-`Call` override calling convention.
    pub(crate) call_cc: FxHashMap<NodeId, strider_target::BuiltCallingConvention>,
    /// Per-class CC argument index to the carrier nodes' output values
    /// (`InitialVar` for register args, `Load` for stack args).
    arg_index_to_values: FxHashMap<(ArgClass, u32), Vec<ValueId>>,
    /// SP-decomposition memo keyed by the value whose address cone was
    /// analysed.  A [`MemDecomp::Stack`] resolves through `memory_interner` to
    /// `(base, offset)`, where `base` is the SP-derived terminal and the offset
    /// is in bytes relative to it: two accesses are the same slot iff they
    /// share both.
    memory_offsets: RefCell<SecondaryMap<ValueId, MemDecomp>>,
    memory_interner: RefCell<EntityInterner<MemoryId, (ValueId, i128)>>,
    /// Callee addresses of pure `noalias` heap allocators. A `Call` to one of
    /// these has a fresh heap-base return. Survives [`Self::clear_memory_slots`]
    /// and compaction: machine addresses, not arena ids.
    noalias_allocators: FxHashSet<u64>,
    /// Per-output case target addresses for a `Switch`: machine addresses, not
    /// arena ids.
    switch_targets: FxHashMap<NodeId, Vec<u64>>,
    /// `InitialVnId` to `InitialVar(id)` node index. Accessors trust it; the
    /// validator re-checks reachable entries against the node's kind
    /// (`StaleInitialVarIndex`).
    pub(crate) initial_var_index: FxHashMap<crate::node::InitialVnId, NodeId>,
    /// Whole-function frame-escape verdict, or `None` when not computed.
    /// Cleared by every changing pass of the optimizer's fixed-point loop and
    /// on compaction, NOT between post-passes: a post-pass that adds a `Call`
    /// input must clear it itself.
    frame_escape: Cell<Option<bool>>,
}

impl SideTables {
    #[inline]
    pub fn call_other_name(&self, node_id: NodeId) -> Option<&str> {
        self.call_other_names.get(&node_id).map(String::as_str)
    }

    #[inline]
    pub fn set_call_other_name(&mut self, node_id: NodeId, name: impl Into<String>) {
        self.call_other_names.insert(node_id, name.into());
    }

    /// Replaces any prior override.
    #[inline]
    pub fn set_call_cc(&mut self, node_id: NodeId, cc: strider_target::BuiltCallingConvention) {
        self.call_cc.insert(node_id, cc);
    }

    #[inline]
    fn class_values(&self, class: ArgClass, index: u32) -> &[ValueId] {
        self.arg_index_to_values
            .get(&(class, index))
            .map_or(&[], Vec::as_slice)
    }

    #[inline]
    fn class_indices(&self, class: ArgClass) -> impl Iterator<Item = u32> + '_ {
        self.arg_index_to_values
            .keys()
            .filter(move |(c, _)| *c == class)
            .map(|(_, index)| *index)
    }

    /// Carriers of the `index`-th integer-class argument.
    #[inline]
    pub fn arg_index_to_values(&self, index: u32) -> &[ValueId] {
        self.class_values(ArgClass::Integer, index)
    }

    /// Carriers of the `index`-th float-class argument.
    #[inline]
    pub fn float_arg_index_to_values(&self, index: u32) -> &[ValueId] {
        self.class_values(ArgClass::Float, index)
    }

    /// Appends `value` to the carriers recorded for integer-class `index`.
    #[inline]
    pub fn register_arg_value(&mut self, index: u32, value: ValueId) {
        self.arg_index_to_values
            .entry((ArgClass::Integer, index))
            .or_default()
            .push(value);
    }

    /// Appends `value` to the carriers recorded for float-class `index`.
    #[inline]
    pub fn register_float_arg_value(&mut self, index: u32, value: ValueId) {
        self.arg_index_to_values
            .entry((ArgClass::Float, index))
            .or_default()
            .push(value);
    }

    /// Unordered.
    #[inline]
    pub fn iter_arg_indices(&self) -> impl Iterator<Item = u32> + '_ {
        self.class_indices(ArgClass::Integer)
    }

    /// Unordered.
    #[inline]
    pub fn iter_float_arg_indices(&self) -> impl Iterator<Item = u32> + '_ {
        self.class_indices(ArgClass::Float)
    }

    /// Every carrier value of either class. Unordered.
    #[inline]
    pub fn arg_carrier_values(&self) -> impl Iterator<Item = ValueId> + '_ {
        self.arg_index_to_values.values().flatten().copied()
    }

    #[inline]
    pub fn memory_class(&self, value: ValueId) -> MemDecomp {
        self.memory_offsets.borrow()[value]
    }

    /// `(base, byte offset from base)`.  `None` when unknown or provably not
    /// memory-rooted (neither stack- nor heap-rooted).
    #[inline]
    pub fn memory_slot_resolved(&self, value: ValueId) -> Option<(ValueId, i128)> {
        self.memory_decomp(value).1
    }

    /// The region tag and the `(base, byte offset)` it resolves to, from one
    /// lookup.
    #[inline]
    pub fn memory_decomp(&self, value: ValueId) -> (MemDecomp, Option<(ValueId, i128)>) {
        let class = self.memory_offsets.borrow()[value];
        let slot = match class {
            MemDecomp::Stack(id) | MemDecomp::Heap(id) => {
                self.memory_interner.borrow().get(id).copied()
            }
            MemDecomp::Unknown | MemDecomp::NotMemory => None,
        };
        (class, slot)
    }

    /// `offset` is in bytes from `base`.
    #[inline]
    pub fn set_stack_slot(&self, value: ValueId, base: ValueId, offset: i128) {
        let id = self.memory_interner.borrow_mut().intern((base, offset));
        self.memory_offsets.borrow_mut()[value] = MemDecomp::Stack(id);
    }

    /// `offset` is in bytes from the heap `base` (an allocator's return
    /// pointer). Distinct from [`Self::set_stack_slot`] only in the region tag.
    #[inline]
    pub fn set_heap_slot(&self, value: ValueId, base: ValueId, offset: i128) {
        let id = self.memory_interner.borrow_mut().intern((base, offset));
        self.memory_offsets.borrow_mut()[value] = MemDecomp::Heap(id);
    }

    /// Negative memo entry: `value` is provably not memory-rooted (neither
    /// stack- nor heap-rooted).
    #[inline]
    pub fn set_not_memory(&self, value: ValueId) {
        self.memory_offsets.borrow_mut()[value] = MemDecomp::NotMemory;
    }

    /// Drops every memoized decomposition, invalidating all [`MemoryId`]s.
    /// Must be called after any graph mutation.
    #[inline]
    pub fn clear_memory_slots(&self) {
        self.memory_offsets.borrow_mut().clear();
        *self.memory_interner.borrow_mut() = EntityInterner::new();
    }

    /// Replaces the set of pure-allocator callee addresses. Clears the
    /// decomposition memo, since the verdict for any heap address depends on it.
    pub fn set_noalias_allocators(&mut self, addrs: FxHashSet<u64>) {
        self.noalias_allocators = addrs;
        self.clear_memory_slots();
    }

    /// Whether `addr` is a configured pure-allocator callee.
    #[inline]
    pub fn is_noalias_allocator(&self, addr: u64) -> bool {
        self.noalias_allocators.contains(&addr)
    }

    /// Memoized frame-escape verdict, or `None` when not yet computed.
    #[inline]
    pub fn frame_escape(&self) -> Option<bool> {
        self.frame_escape.get()
    }

    #[inline]
    pub fn set_frame_escape(&self, escapes: bool) {
        self.frame_escape.set(Some(escapes));
    }

    /// Invalidates the memo. Must be called after any graph mutation.
    #[inline]
    pub fn clear_frame_escape(&self) {
        self.frame_escape.set(None);
    }

    #[inline]
    pub fn switch_targets(&self, id: NodeId) -> &[u64] {
        self.switch_targets.get(&id).map_or(&[], Vec::as_slice)
    }

    #[inline]
    pub fn set_switch_targets(&mut self, id: NodeId, targets: Vec<u64>) {
        self.switch_targets.insert(id, targets);
    }

    /// Unordered.
    pub fn asm_fingerprint(&self, id: NodeId) -> FxHashSet<u64> {
        let mut set = FxHashSet::default();
        self.asm_fingerprints.for_each(id, |addr| {
            set.insert(addr);
        });
        set
    }

    #[inline]
    pub fn asm_fingerprint_is_empty(&self, id: NodeId) -> bool {
        self.asm_fingerprints.is_empty(id)
    }

    /// Unions `contributors` in.  Existing entries are never removed.
    pub fn extend_asm_fingerprint(&mut self, node_id: NodeId, contributors: &[u64]) {
        for &addr in contributors {
            self.asm_fingerprints.extend(node_id, addr);
        }
    }

    /// Unions `src`'s fingerprint into `dst`.
    pub fn extend_asm_fingerprint_from(&mut self, dst: NodeId, src: NodeId) {
        if dst == src {
            return;
        }
        self.asm_fingerprints.union(dst, src);
    }

    /// Remaps every arena-id key / value after a compaction; an entry whose
    /// node or value did not survive is dropped.
    pub(crate) fn remap(&mut self, remap: &NodeIdRemap) {
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
        // Both the slot key and its interned base are ValueIds, so the map and
        // the interner have to be rebuilt together.
        let (new_slots, new_interner) = {
            let old_slots = self.memory_offsets.borrow();
            let old_interner = self.memory_interner.borrow();
            let mut new_interner: EntityInterner<MemoryId, (ValueId, i128)> = EntityInterner::new();
            let mut new_slots: SecondaryMap<ValueId, MemDecomp> = SecondaryMap::new();
            for (old_value, slot) in old_slots.iter() {
                let Some(new_value) = remap.value_old_to_new(old_value) else {
                    continue;
                };
                let new_slot = match *slot {
                    MemDecomp::Unknown => continue,
                    MemDecomp::NotMemory => MemDecomp::NotMemory,
                    MemDecomp::Stack(id) | MemDecomp::Heap(id) => {
                        let Some(&(old_base, off)) = old_interner.get(id) else {
                            continue;
                        };
                        let Some(new_base) = remap.value_old_to_new(old_base) else {
                            continue;
                        };
                        let new_id = new_interner.intern((new_base, off));
                        if matches!(*slot, MemDecomp::Heap(_)) {
                            MemDecomp::Heap(new_id)
                        } else {
                            MemDecomp::Stack(new_id)
                        }
                    }
                };
                new_slots[new_value] = new_slot;
            }
            (new_slots, new_interner)
        };
        *self.memory_offsets.get_mut() = new_slots;
        *self.memory_interner.get_mut() = new_interner;
        // `InitialVnId`s are stable across compaction, so only the arena-id
        // coordinate of the next two maps needs translating.
        self.value_vn = remap_hashmap(&mut self.value_vn, |old_value, vn_id| {
            remap
                .value_old_to_new(old_value)
                .map(|new_value| (new_value, vn_id))
        });
        self.initial_var_index = remap_hashmap(&mut self.initial_var_index, |vn_id, old_id| {
            remap.node_old_to_new(old_id).map(|new_id| (vn_id, new_id))
        });
        // Recomputed off the compacted graph on next demand.
        self.frame_escape.set(None);
        // An index whose carriers all vanish is dropped.
        self.arg_index_to_values =
            remap_hashmap(&mut self.arg_index_to_values, |key, old_values| {
                let mapped: Vec<ValueId> = old_values
                    .into_iter()
                    .filter_map(|old_value| remap.value_old_to_new(old_value))
                    .collect();
                (!mapped.is_empty()).then_some((key, mapped))
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cranelift_entity::EntityRef;

    /// Class and resolved slot come out of one lookup and must agree in every
    /// state.
    #[test]
    fn memory_decomp_returns_class_and_slot_together() {
        let st = SideTables::default();
        let base = ValueId::new(0);
        let stack = ValueId::new(1);
        let heap = ValueId::new(2);
        let not_mem = ValueId::new(3);
        let cold = ValueId::new(4);

        st.set_stack_slot(stack, base, -8);
        st.set_heap_slot(heap, base, 16);
        st.set_not_memory(not_mem);

        assert!(
            matches!(st.memory_decomp(stack), (MemDecomp::Stack(_), Some((b, -8))) if b == base)
        );
        assert!(matches!(st.memory_decomp(heap), (MemDecomp::Heap(_), Some((b, 16))) if b == base));
        assert_eq!(st.memory_decomp(not_mem), (MemDecomp::NotMemory, None));
        assert_eq!(st.memory_decomp(cold), (MemDecomp::Unknown, None));

        for v in [stack, heap, not_mem, cold] {
            let (class, slot) = st.memory_decomp(v);
            assert_eq!(class, st.memory_class(v));
            assert_eq!(slot, st.memory_slot_resolved(v));
        }
    }

    /// Wholesale replacement; the deny pins the writer to a `&mut` borrow.
    #[test]
    #[deny(unused_mut)]
    fn noalias_allocators_replace_through_mut() {
        let mut st = SideTables::default();
        assert!(!st.is_noalias_allocator(0x1000));
        st.set_noalias_allocators(FxHashSet::from_iter([0x1000, 0x2000]));
        assert!(st.is_noalias_allocator(0x1000));
        assert!(st.is_noalias_allocator(0x2000));
        st.set_noalias_allocators(FxHashSet::from_iter([0x2000]));
        assert!(!st.is_noalias_allocator(0x1000), "wholesale replacement");
        assert!(st.is_noalias_allocator(0x2000));
    }

    #[test]
    fn frame_escape_round_trips_and_clears() {
        let st = SideTables::default();
        assert_eq!(st.frame_escape(), None, "cold: not yet computed");
        st.set_frame_escape(true);
        assert_eq!(st.frame_escape(), Some(true));
        st.set_frame_escape(false);
        assert_eq!(st.frame_escape(), Some(false), "overwrites a prior verdict");
        st.clear_frame_escape();
        assert_eq!(st.frame_escape(), None, "clear invalidates the memo");
    }

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
