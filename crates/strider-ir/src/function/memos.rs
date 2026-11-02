use cranelift_entity::{SecondaryMap, entity_impl};
use entity_utils::EntityInterner;

use crate::Function;
use crate::graph::NodeIdRemap;
use crate::node::ValueId;

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

/// Everything the analyses derive from a [`Function`]'s graph, and nothing
/// else.
///
/// What makes a field one of these is the clearing rule:
/// [`Function::invalidate_analyses`] replaces the whole struct with its
/// [`Default`], so a memo added here is dropped with the rest and no caller
/// has to remember it.  A field only belongs here if its `Default` means "not
/// yet computed" and the graph alone can recompute it; a fact the graph does
/// not derive belongs with the claims on [`Function`] instead.
#[derive(Clone, Default)]
pub(crate) struct FunctionMemos {
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

impl FunctionMemos {
    /// Carries the memory classes through a compaction and drops the verdicts
    /// a compacted graph has to re-derive.
    pub(crate) fn remap(&mut self, remap: &NodeIdRemap) {
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
        let mut rebase = |id: MemoryId| {
            let &(old_base, off) = memory_interner.get(id)?;
            Some(new_interner.intern((remap.value_old_to_new(old_base)?, off)))
        };
        for (old_value, slot) in memory_offsets.iter() {
            let Some(new_value) = remap.value_old_to_new(old_value) else {
                continue;
            };
            let new_slot = match *slot {
                MemDecomp::Unknown => None,
                MemDecomp::NotMemory => Some(MemDecomp::NotMemory),
                MemDecomp::Stack(id) => rebase(id).map(MemDecomp::Stack),
                MemDecomp::Heap(id) => rebase(id).map(MemDecomp::Heap),
            };
            if let Some(new_slot) = new_slot {
                new_slots[new_value] = new_slot;
            }
        }
        *memory_offsets = new_slots;
        *memory_interner = new_interner;
        // Recomputed off the compacted graph on next demand.
        *frame_escape = None;
    }
}

/// The memo group is written through a shared `&Function`: an analysis holds
/// the function immutably while it fills them in.
impl Function {
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
        *self.memos.borrow_mut() = FunctionMemos::default();
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::function::test_function;
    use cranelift_entity::EntityRef;

    /// Class and resolved slot come out of one lookup and must agree in every
    /// state.
    #[test]
    fn memory_decomp_returns_class_and_slot_together() {
        let f = test_function();
        let base = ValueId::new(0);
        let stack = ValueId::new(1);
        let heap = ValueId::new(2);
        let not_mem = ValueId::new(3);
        let cold = ValueId::new(4);

        f.set_stack_slot(stack, base, -8);
        f.set_heap_slot(heap, base, 16);
        f.set_not_memory(not_mem);

        assert!(
            matches!(f.memory_decomp(stack), (MemDecomp::Stack(_), Some((b, -8))) if b == base)
        );
        assert!(matches!(f.memory_decomp(heap), (MemDecomp::Heap(_), Some((b, 16))) if b == base));
        assert_eq!(f.memory_decomp(not_mem), (MemDecomp::NotMemory, None));
        assert_eq!(f.memory_decomp(cold), (MemDecomp::Unknown, None));

        for v in [stack, heap, not_mem, cold] {
            let (class, slot) = f.memory_decomp(v);
            assert_eq!(class, f.memory_class(v));
            assert_eq!(slot, f.memory_slot_resolved(v));
        }
    }

    #[test]
    fn frame_escape_round_trips_and_clears() {
        let f = test_function();
        assert_eq!(f.frame_escape(), None, "cold: not yet computed");
        f.set_frame_escape(true);
        assert_eq!(f.frame_escape(), Some(true));
        f.set_frame_escape(false);
        assert_eq!(f.frame_escape(), Some(false), "overwrites a prior verdict");
        f.clear_frame_escape();
        assert_eq!(f.frame_escape(), None, "clear invalidates the memo");
    }

    #[test]
    fn invalidate_analyses_drops_the_memos() {
        let f = test_function();
        let (base, v) = (ValueId::new(0), ValueId::new(1));
        f.set_stack_slot(v, base, -8);
        f.set_frame_escape(true);
        f.invalidate_analyses();
        assert_eq!(f.memory_decomp(v), (MemDecomp::Unknown, None));
        assert_eq!(f.frame_escape(), None);
    }

    /// The reset is the whole group, not a list of fields, so a memo added to
    /// [`FunctionMemos`] is dropped without `invalidate_analyses` naming it.
    /// The destructuring stops compiling when a field joins the group, so the
    /// pinning below is extended with it.
    #[test]
    fn invalidate_analyses_resets_every_memo_field() {
        let f = test_function();
        let (base, v) = (ValueId::new(0), ValueId::new(1));
        f.set_stack_slot(v, base, -8);
        f.set_frame_escape(true);
        {
            let memos = f.memos.borrow();
            let FunctionMemos {
                memory_offsets,
                memory_interner,
                frame_escape,
            } = &*memos;
            assert!(memory_offsets.iter().any(|(_, c)| *c != MemDecomp::Unknown));
            assert!(!memory_interner.is_empty());
            assert!(frame_escape.is_some());
        }

        f.invalidate_analyses();

        let memos = f.memos.borrow();
        let FunctionMemos {
            memory_offsets,
            memory_interner,
            frame_escape,
        } = &*memos;
        assert!(memory_offsets.iter().all(|(_, c)| *c == MemDecomp::Unknown));
        // A surviving entry would outlive the class that named it.
        assert!(memory_interner.is_empty());
        assert!(frame_escape.is_none());
    }
}
