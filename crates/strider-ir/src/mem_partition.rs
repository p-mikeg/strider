//! Memory partition infrastructure for the Memory SSA model.
//!
//! A `MemPartitionId` is an entity-ref into a `PartitionTable` carried on
//! `Function`.  Each partition has an `AliasClass` (Stack / Heap / Rom /
//! Unknown) and a `read_only` flag.  The AliasSplit optimization pass
//! assigns memory-touching nodes to partitions and inserts MemPartition /
//! MemUnion boundary nodes between unified and partitioned memory.

use cranelift_entity::{entity_impl, PrimaryMap};

/// Opaque identifier for a memory partition within a function's
/// [`PartitionTable`].  Created by [`PartitionTable::create`]; never
/// invalidated once issued.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct MemPartitionId(u32);
entity_impl!(MemPartitionId);

/// Coarse classification of what kind of memory a partition covers.
///
/// MMIO is intentionally absent in this revision — added in a follow-up
/// when address-range-based MMIO detection lands.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub enum AliasClass {
    /// SP-relative stack frame memory.
    Stack,
    /// Heap / global addressable memory the analyzer can't further refine.
    Heap,
    /// Read-only memory backed by a ROM image (.text, .rodata, constants).
    Rom,
    /// Address completely unknown — partition aliases everything (used as a
    /// conservative fallback to avoid unsound forwarding).
    Unknown,
}

/// Per-partition metadata stored in a [`PartitionTable`].
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct PartitionInfo {
    pub alias_class: AliasClass,
    pub read_only: bool,
}

/// Per-function table of memory partitions.  Created empty by `Function::new`;
/// populated by the AliasSplit pass.
#[derive(Debug, Default)]
pub struct PartitionTable {
    info: PrimaryMap<MemPartitionId, PartitionInfo>,
}

impl PartitionTable {
    /// Create a new partition with the given alias class.  Read-only is
    /// derived from the class: `Rom` is read-only by definition; everything
    /// else is RW.  Returns the assigned [`MemPartitionId`].
    pub fn create(&mut self, alias_class: AliasClass) -> MemPartitionId {
        let read_only = matches!(alias_class, AliasClass::Rom);
        self.info.push(PartitionInfo {
            alias_class,
            read_only,
        })
    }

    /// Look up a partition's info.  Panics on out-of-range id (entity-ref
    /// usage discipline; ids are never invalidated).
    #[must_use]
    pub fn info(&self, id: MemPartitionId) -> &PartitionInfo {
        &self.info[id]
    }

    /// Iterate over all created partitions in id-order.
    pub fn iter(&self) -> impl Iterator<Item = (MemPartitionId, &PartitionInfo)> + '_ {
        self.info.iter()
    }

    /// Total count of partitions in the table.
    #[must_use]
    pub fn len(&self) -> usize {
        self.info.len()
    }

    /// True if no partitions have been created yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.info.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::{AliasClass, PartitionTable};

    #[test]
    fn partition_table_assigns_distinct_ids() {
        let mut t = PartitionTable::default();
        let p1 = t.create(AliasClass::Stack);
        let p2 = t.create(AliasClass::Heap);
        assert_ne!(p1, p2);
    }

    #[test]
    fn partition_table_lookups_round_trip() {
        let mut t = PartitionTable::default();
        let p_rom = t.create(AliasClass::Rom);
        let p_stack = t.create(AliasClass::Stack);
        assert_eq!(t.info(p_rom).alias_class, AliasClass::Rom);
        assert!(t.info(p_rom).read_only); // Rom is read-only
        assert_eq!(t.info(p_stack).alias_class, AliasClass::Stack);
        assert!(!t.info(p_stack).read_only); // Stack is RW
    }
}
