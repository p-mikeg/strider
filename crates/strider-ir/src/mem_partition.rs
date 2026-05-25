//! Alias-class classification for memory partitions.
//!
//! [`AliasClass`] is stored directly on [`crate::node::NodeKind::MemPartition`]
//! and [`crate::node::NodeOutputKind::Memory`] so pass code can branch on the
//! class without any table lookup.

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

impl AliasClass {
    /// Short human-readable name for rendering (e.g. in dot labels).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            AliasClass::Stack => "Stack",
            AliasClass::Heap => "Heap",
            AliasClass::Rom => "Rom",
            AliasClass::Unknown => "Unknown",
        }
    }
}
