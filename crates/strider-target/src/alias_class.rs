//! Coarse alias-class classification for memory partitions.
//!
//! [`AliasClass`] is the closed enum used both on
//! [`crate::call_other_abi::CallOtherAbi::mem_clobbers`] (per-user-op
//! memory-clobber set) and on `strider_ir::NodeKind::MemProject` /
//! `NodeOutputKind::Memory(_)` (partition tag carried on the IR memory
//! edge).  It lives in `strider-target` (not `strider-ir`) because
//! `CallOtherAbi`'s clobber set is a target-description fact —
//! `strider-ir` re-exports it from here so downstream pattern code can
//! keep referring to `strider_ir::AliasClass` without changing.

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

/// Convenience clobber sets for common ABI shapes.  Keeping these as named
/// constants saves repetition in [`crate::call_other_abi`] and makes the
/// per-row intent self-documenting (e.g. `MEM_CLOBBER_FULL` for SYSCALL,
/// `MEM_CLOBBER_HEAP_UNKNOWN` for atomic RMW ops, `MEM_CLOBBER_NONE` for
/// pure compute).
pub const MEM_CLOBBER_NONE: &[AliasClass] = &[];

/// `[Heap, Unknown]` — atomic / barrier / port-I/O / external-effect ops.
/// Equivalent to the old `memory_edge: true` default for ops that may
/// disturb heap-resident or unmapped memory but NOT the stack frame.
pub const MEM_CLOBBER_HEAP_UNKNOWN: &[AliasClass] = &[AliasClass::Heap, AliasClass::Unknown];

/// `[Stack, Heap, Unknown]` — full-clobber: SYSCALL and any kernel-entry
/// path that can mutate the user-mode stack frame.
pub const MEM_CLOBBER_FULL: &[AliasClass] =
    &[AliasClass::Stack, AliasClass::Heap, AliasClass::Unknown];
