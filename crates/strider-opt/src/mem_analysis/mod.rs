use strider_ir::node::ValueId;

use AddrClass::*;

mod alias;
mod analyzer;
mod arg_window;
mod decompose;
mod frame;
mod frame_escape;
mod index;
mod ring;
mod stamp;
mod walker;

pub(crate) use analyzer::{MemAnalyzer, MemOptions, MemPurpose, store_value_byte_size};
pub(crate) use arg_window::{ArgStoreScan, SlotReach};
pub(crate) use decompose::{alignment_masked_operand, decompose};
pub(crate) use ring::{reduce, ring_overlap, slot_key, wrap_to_addr_width};
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
    /// Some allocation, which one unknown: a `Phi` whose every arm is heap.
    /// Carries no usable identity, so it is may-alias against every heap base
    /// including itself.
    HeapOpaque,
}

/// `base + offset`, where `base` is a memory terminal of the given `kind`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MemExpr {
    pub base: ValueId,
    pub offset: i128,
    pub kind: MemKind,
}

/// Coarse Load / Store address class; [`alias::alias_verdict`] is keyed on the
/// `(load_class, store_class)` pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum AddrClass {
    /// Offsets are only comparable within one `base`.  Distinct bases (say
    /// `InitialVar(sp)` vs `sp & -16`) differ by the caller-dependent
    /// `sp mod align`, so their offsets live in different coordinate systems
    /// and must be treated as may-alias.
    StackRooted { base: ValueId, offset: i128 },
    /// Rooted at a pure allocator's return pointer. Two distinct allocation
    /// bases never overlap (noalias), unlike SP bases, and no allocation ever
    /// coincides with the stack or a global.
    HeapRooted { base: ValueId, offset: i128 },
    /// Rooted at a `Phi` of heap bases: provably an allocation, but the base
    /// is one of several, so it may be any of them and offsets from it are not
    /// comparable with anything.
    HeapOpaque,
    /// Literal `.data` / `.rodata` / `.bss` / MMIO pointer.
    Constant { addr: i128 },
    /// Anything else.  Only `ValueId` equality proves two of these equal;
    /// distinct ids can still compute the same runtime address.
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

impl AddrClass {
    fn offset(self) -> Option<i128> {
        match self {
            StackRooted { offset, .. } | HeapRooted { offset, .. } => Some(offset),
            Constant { addr } => Some(addr),
            HeapOpaque | Anchor { .. } => None,
        }
    }
}

/// Collapses region phis to the bare `InitialVar(sp) + k` terminals
/// `decompose` recognises, the post-`PhiCollapse` state in which
/// `LoadForward` runs the analysis.  `ConstantFold` is left out so the
/// deep-chain and memo tests keep their un-flattened structure.
#[cfg(test)]
fn collapse_phis(fg: &mut strider_ir::Function) {
    let mut p = crate::OptimizerPipeline::new();
    p.add(crate::PhiCollapse);
    p.add(crate::RegionCollapse);
    p.run(fg, &mut crate::OptCtx::new(None))
        .expect("phi collapse");
}

#[cfg(test)]
mod tests;
