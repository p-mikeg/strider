use strider_ir::node::ValueId;

mod alias;
mod arg_window;
mod decompose;
mod frame;
mod frame_escape;
mod index;
mod query;
mod ring;
mod stamp;
mod walker;

pub(crate) use alias::Loc;
pub(crate) use arg_window::{ArgPrefix, merged_slots};
pub(crate) use decompose::{alignment_masked_operand, decompose};
pub(crate) use frame::entry_sp_span;
pub(crate) use query::{
    MemQuery, Policy, ReachingDef, anchored_store, narrow_load_to, store_value_byte_size,
};
pub(crate) use ring::{reduce, ring_overlap, wrap_to_addr_width};
pub use stamp::refresh_memory_classes;
pub(crate) use stamp::stamp_all;
#[cfg(test)]
pub(crate) use walker::WALK_STEPS;

/// Which memory region a decomposed base roots in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MemKind {
    /// `InitialVar(sp)` or an alignment-masked SP `And` output.
    Stack,
    /// A pure allocator's fresh return pointer.
    Heap,
    /// A `Phi` whose every arm is heap: may-alias against every heap base,
    /// itself included.
    HeapOpaque,
}

/// `base + offset`, where `base` is a memory terminal of the given `kind`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MemExpr {
    pub base: ValueId,
    pub offset: i128,
    pub kind: MemKind,
}

/// Coarse Load / Store address class.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum AddrClass {
    /// Offsets are comparable only within one `base`: distinct bases (say
    /// `sp` and `sp & -16`) differ by the caller-dependent `sp mod align`.
    StackRooted { base: ValueId, offset: i128 },
    /// Rooted at a pure allocator's return pointer. Distinct allocations
    /// are disjoint from each other, the stack and globals.
    HeapRooted { base: ValueId, offset: i128 },
    /// Rooted at a `Phi` of heap bases: an allocation, but which one is
    /// unknown, so its offsets compare with nothing.
    HeapOpaque,
    /// A constant address.
    Constant { addr: i128 },
    /// Anything else. Only `ValueId` equality proves two equal.
    Anchor { value: ValueId },
}

/// Verdict for one (load, store) address pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AliasVerdict {
    /// Same start address; the sizes are the caller's to check.
    Match,
    /// Provably non-overlapping.
    Disjoint,
    /// Neither provable.
    MayAlias,
}

/// Collapses region phis to the `InitialVar(sp) + k` terminals `decompose`
/// recognises, without `ConstantFold`, so test chains stay unflattened.
#[cfg(test)]
fn collapse_phis(fg: &mut strider_ir::Function) {
    crate::test_support::run_passes(fg, crate::test_support::COLLAPSE).expect("phi collapse");
}

#[cfg(test)]
mod tests;
