use std::cell::RefCell;

use cranelift_entity::{SecondaryMap, entity_impl};
use entity_utils::{EntityInterner, UnionDag};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::graph::NodeIdRemap;
use crate::node::{NodeId, ValueId};

/// Interner key for a distinct `(base, offset)` decomposition, stack or heap.
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

/// Everything the analyses derive from the graph, and nothing else.
///
/// What makes a field one of these is the clearing rule:
/// [`SideTables::invalidate_analyses`] replaces the whole struct with its
/// [`Default`], so a memo added here is dropped with the rest and no caller
/// has to remember it.  A field only belongs here if its `Default` means "not
/// yet computed" and the graph alone can recompute it; a fact the graph does
/// not derive belongs with the claims on `Function` instead.
#[derive(Clone, Default)]
struct AnalysisMemos {
    /// SP-decomposition memo keyed by the value whose address cone was
    /// analysed.  A [`MemDecomp::Stack`] resolves through `memory_interner` to
    /// `(base, offset)`, where `base` is the SP-derived terminal and the offset
    /// is in bytes relative to it: two accesses are the same slot iff they
    /// share both.
    memory_offsets: SecondaryMap<ValueId, MemDecomp>,
    memory_interner: EntityInterner<MemoryId, (ValueId, i128)>,
    /// Whole-function frame-escape verdict, or `None` when not computed.
    frame_escape: Option<bool>,
}

impl AnalysisMemos {
    /// Carries the memory classes through a compaction and drops the verdicts
    /// a compacted graph has to re-derive.
    fn remap(&mut self, remap: &NodeIdRemap) {
        // Every field named, so one added later states what compaction does
        // to it.
        let Self {
            memory_offsets,
            memory_interner,
            frame_escape,
        } = self;
        // Both the slot key and its interned base are ValueIds, so the map and
        // the interner have to be rebuilt together.
        let mut new_interner: EntityInterner<MemoryId, (ValueId, i128)> = EntityInterner::new();
        let mut new_slots: SecondaryMap<ValueId, MemDecomp> = SecondaryMap::new();
        for (old_value, slot) in memory_offsets.iter() {
            let Some(new_value) = remap.value_old_to_new(old_value) else {
                continue;
            };
            let new_slot = match *slot {
                MemDecomp::Unknown => continue,
                MemDecomp::NotMemory => MemDecomp::NotMemory,
                MemDecomp::Stack(id) | MemDecomp::Heap(id) => {
                    let Some(&(old_base, off)) = memory_interner.get(id) else {
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
        *memory_offsets = new_slots;
        *memory_interner = new_interner;
        // Recomputed off the compacted graph on next demand.
        *frame_escape = None;
    }
}

/// Per-function data keyed by arena ids (or the CC arg index) that is not part
/// of the graph's structural identity, plus the memo group the analyses fill
/// in.
///
/// What makes a field one of the tables below is that compaction has to say
/// something about it: every one is keyed by an id the remap translates, and
/// nothing clears them.  The derived, clearable verdicts live in `memos`
/// instead.
#[derive(Clone)]
pub struct SideTables {
    /// Machine-instruction addresses whose lifting or rewrite contributed to
    /// each node's value.
    asm_fingerprints: UnionDag<NodeId, u64>,
    /// The tracked varnode a value represents, at most one per value.
    pub(crate) value_vn: FxHashMap<ValueId, crate::node::InitialVnId>,
    /// Per-class CC argument index to the carrier nodes' output values
    /// (`InitialVar` for register args, `Load` for stack args).
    arg_index_to_values: FxHashMap<(ArgClass, u32), Vec<ValueId>>,
    /// `InitialVnId` to `InitialVar(id)` node index. Accessors trust it; the
    /// validator re-checks reachable entries against the node's kind
    /// (`StaleInitialVarIndex`).
    pub(crate) initial_var_index: FxHashMap<crate::node::InitialVnId, NodeId>,
    memos: RefCell<AnalysisMemos>,
}

impl SideTables {
    /// Crate-private so no caller can wipe the tables through
    /// [`Function::side_tables_mut`](crate::Function::side_tables_mut).
    pub(crate) fn new() -> Self {
        Self {
            asm_fingerprints: UnionDag::default(),
            value_vn: FxHashMap::default(),
            arg_index_to_values: FxHashMap::default(),
            initial_var_index: FxHashMap::default(),
            memos: RefCell::default(),
        }
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

    /// Drops the integer-class carriers at `first_index` and above, keeping the
    /// register-passed ones below it.
    #[inline]
    pub fn clear_arg_values_from(&mut self, first_index: u32) {
        self.arg_index_to_values
            .retain(|(class, index), _| *class != ArgClass::Integer || *index < first_index);
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
        self.memos.borrow().memory_offsets[value]
    }

    /// `(base, byte offset from base)`.  `None` when unknown or provably not
    /// memory-rooted (neither stack- nor heap-rooted).
    #[inline]
    #[cfg(any(test, feature = "test-util"))]
    pub fn memory_slot_resolved(&self, value: ValueId) -> Option<(ValueId, i128)> {
        self.memory_decomp(value).1
    }

    /// The region tag and the `(base, byte offset)` it resolves to, from one
    /// lookup.
    #[inline]
    pub fn memory_decomp(&self, value: ValueId) -> (MemDecomp, Option<(ValueId, i128)>) {
        let memos = self.memos.borrow();
        let class = memos.memory_offsets[value];
        let slot = match class {
            MemDecomp::Stack(id) | MemDecomp::Heap(id) => memos.memory_interner.get(id).copied(),
            MemDecomp::Unknown | MemDecomp::NotMemory => None,
        };
        (class, slot)
    }

    /// `offset` is in bytes from `base`.
    #[inline]
    pub fn set_stack_slot(&self, value: ValueId, base: ValueId, offset: i128) {
        self.set_slot(value, base, offset, MemDecomp::Stack);
    }

    /// `offset` is in bytes from the heap `base` (an allocator's return
    /// pointer). Distinct from [`Self::set_stack_slot`] only in the region tag.
    #[inline]
    pub fn set_heap_slot(&self, value: ValueId, base: ValueId, offset: i128) {
        self.set_slot(value, base, offset, MemDecomp::Heap);
    }

    #[inline]
    fn set_slot(
        &self,
        value: ValueId,
        base: ValueId,
        offset: i128,
        region: fn(MemoryId) -> MemDecomp,
    ) {
        let memos = &mut *self.memos.borrow_mut();
        let id = memos.memory_interner.intern((base, offset));
        memos.memory_offsets[value] = region(id);
    }

    /// Negative memo entry: `value` is provably not memory-rooted (neither
    /// stack- nor heap-rooted).
    #[inline]
    pub fn set_not_memory(&self, value: ValueId) {
        self.memos.borrow_mut().memory_offsets[value] = MemDecomp::NotMemory;
    }

    /// Drops the memo group as a whole (the memory classes, invalidating all
    /// [`MemoryId`]s, and the frame-escape verdict), keeping the tables and
    /// the claims.  A caller that rewires an address value's producers owes
    /// the call.
    #[inline]
    pub fn invalidate_analyses(&self) {
        *self.memos.borrow_mut() = AnalysisMemos::default();
    }

    /// Memoized frame-escape verdict, or `None` when not yet computed.
    #[inline]
    pub fn frame_escape(&self) -> Option<bool> {
        self.memos.borrow().frame_escape
    }

    #[inline]
    pub fn set_frame_escape(&self, escapes: bool) {
        self.memos.borrow_mut().frame_escape = Some(escapes);
    }

    /// Invalidates the frame-escape verdict alone, keeping the memory classes.
    #[inline]
    pub fn clear_frame_escape(&self) {
        self.memos.borrow_mut().frame_escape = None;
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

    /// Links `src`'s fingerprint under `dst`'s. A live alias, not a copy:
    /// contributors added to `src` afterwards are `dst`'s too.
    pub(crate) fn extend_asm_fingerprint_from(&mut self, dst: NodeId, src: NodeId) {
        if dst == src {
            return;
        }
        self.asm_fingerprints.union(dst, src);
    }

    /// Remaps every arena-id key / value after a compaction; an entry whose
    /// node or value did not survive is dropped.
    pub(crate) fn remap(&mut self, remap: &NodeIdRemap) {
        // Every field named, so one added later states what compaction does
        // to it.
        let Self {
            asm_fingerprints,
            value_vn,
            arg_index_to_values,
            initial_var_index,
            memos,
        } = self;
        asm_fingerprints.remap(|old| remap.node_old_to_new(old));
        memos.get_mut().remap(remap);
        // `InitialVnId`s are stable across compaction, so only the arena-id
        // coordinate of the next two maps needs translating.
        *value_vn = remap_hashmap(value_vn, |old_value, vn_id| {
            remap
                .value_old_to_new(old_value)
                .map(|new_value| (new_value, vn_id))
        });
        *initial_var_index = remap_hashmap(initial_var_index, |vn_id, old_id| {
            remap.node_old_to_new(old_id).map(|new_id| (vn_id, new_id))
        });
        // An index whose carriers all vanish is dropped.
        *arg_index_to_values = remap_hashmap(arg_index_to_values, |key, old_values| {
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
        let st = SideTables::new();
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

    #[test]
    fn frame_escape_round_trips_and_clears() {
        let st = SideTables::new();
        assert_eq!(st.frame_escape(), None, "cold: not yet computed");
        st.set_frame_escape(true);
        assert_eq!(st.frame_escape(), Some(true));
        st.set_frame_escape(false);
        assert_eq!(st.frame_escape(), Some(false), "overwrites a prior verdict");
        st.clear_frame_escape();
        assert_eq!(st.frame_escape(), None, "clear invalidates the memo");
    }

    #[test]
    fn invalidate_analyses_drops_the_memos() {
        let st = SideTables::new();
        let (base, v) = (ValueId::new(0), ValueId::new(1));
        st.set_stack_slot(v, base, -8);
        st.set_frame_escape(true);
        st.invalidate_analyses();
        assert_eq!(st.memory_decomp(v), (MemDecomp::Unknown, None));
        assert_eq!(st.frame_escape(), None);
    }

    /// The reset is the whole group, not a list of fields, so a memo added to
    /// [`AnalysisMemos`] is dropped without `invalidate_analyses` naming it.
    /// The destructuring stops compiling when a field joins the group, so the
    /// pinning below is extended with it.
    #[test]
    fn invalidate_analyses_resets_every_memo_field() {
        let st = SideTables::new();
        let (base, v) = (ValueId::new(0), ValueId::new(1));
        st.set_stack_slot(v, base, -8);
        st.set_frame_escape(true);
        {
            let memos = st.memos.borrow();
            let AnalysisMemos {
                memory_offsets,
                memory_interner,
                frame_escape,
            } = &*memos;
            assert!(memory_offsets.iter().any(|(_, c)| *c != MemDecomp::Unknown));
            assert!(!memory_interner.is_empty());
            assert!(frame_escape.is_some());
        }

        st.invalidate_analyses();

        let memos = st.memos.borrow();
        let AnalysisMemos {
            memory_offsets,
            memory_interner,
            frame_escape,
        } = &*memos;
        assert!(memory_offsets.iter().all(|(_, c)| *c == MemDecomp::Unknown));
        // A surviving entry would outlive the class that named it.
        assert!(memory_interner.is_empty());
        assert!(frame_escape.is_none());
    }

    // The union is order-independent, deduplicating and never dropping an entry.
    #[test]
    fn extend_asm_fingerprint_unions_deduped() {
        let mut st = SideTables::new();
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
